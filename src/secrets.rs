//! Détection de secrets dans un texte copié (heuristiques, std uniquement).

/// Vrai si le texte (4 premiers Ko) ressemble à un secret.
pub fn is_secret(s: &str) -> bool {
    let mut n = s.len().min(4096);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    let t = s[..n].trim();
    if t.is_empty() {
        return false;
    }
    pem(t) || tokens(t) || connexion(t) || env_line(t) || mot_de_passe(t)
}

fn pem(t: &str) -> bool {
    t.contains("-----BEGIN ") && t.contains("PRIVATE KEY-----")
}

fn b64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// JWT et préfixes de clés connus, sur chaque « mot » du texte.
fn tokens(t: &str) -> bool {
    // JWT : eyJ… avec 3 segments base64url
    for w in t.split(|c: char| !(b64url(c) || c == '.')) {
        if w.starts_with("eyJ") && w.len() >= 20 {
            let p: Vec<&str> = w.split('.').collect();
            if p.len() == 3 && p.iter().all(|x| !x.is_empty()) {
                return true;
            }
        }
    }
    const PREF: [(&str, usize); 13] = [
        ("ghp_", 20), ("gho_", 20), ("ghu_", 20), ("ghs_", 20), ("ghr_", 20),
        ("github_pat_", 20), ("glpat-", 20), ("sk_live_", 10), ("rk_live_", 10),
        ("sk_test_", 10), ("sk-", 20), ("AIza", 35), ("npm_", 30),
    ];
    for w in t.split(|c: char| !b64url(c)) {
        let aws = (w.starts_with("AKIA") || w.starts_with("ASIA"))
            && w.len() == 20
            && w[4..].bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if aws {
            return true;
        }
        // Slack : xox[abpr]-…
        let b = w.as_bytes();
        if w.starts_with("xox") && b.len() > 14 && b"abpr".contains(&b[3]) && b[4] == b'-' {
            return true;
        }
        if PREF.iter().any(|(p, min)| w.strip_prefix(p).is_some_and(|r| r.len() >= *min)) {
            return true;
        }
    }
    false
}

/// password=…, pwd=…, ou ://user:pass@hôte.
fn connexion(t: &str) -> bool {
    let l = t.to_lowercase();
    for k in ["password=", "pwd="] {
        let mut from = 0;
        while let Some(i) = l[from..].find(k) {
            let v = &l[from + i + k.len()..];
            if v.chars().next().is_some_and(|c| !c.is_whitespace() && c != ';' && c != '&') {
                return true;
            }
            from += i + k.len();
        }
    }
    let mut from = 0;
    while let Some(i) = t[from..].find("://") {
        let r = &t[from + i + 3..];
        let auth = r.split(|c: char| c == '/' || c.is_whitespace()).next().unwrap_or("");
        if let Some((cred, _)) = auth.split_once('@') {
            if let Some((u, p)) = cred.split_once(':') {
                if !u.is_empty() && !p.is_empty() {
                    return true;
                }
            }
        }
        from += i + 3;
    }
    false
}

/// Ligne `.env` : CLE_SECRET=valeur.
fn env_line(t: &str) -> bool {
    t.lines().any(|l| {
        let l = l.trim();
        let l = l.strip_prefix("export ").unwrap_or(l);
        let Some((k, v)) = l.split_once('=') else { return false };
        let k = k.trim();
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        let ku = k.to_uppercase();
        !v.is_empty()
            && !k.is_empty()
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && ["SECRET", "TOKEN", "PASSWORD", "API_KEY"].iter().any(|m| ku.contains(m))
    })
}

fn entropie(t: &str) -> f64 {
    let mut m: std::collections::HashMap<char, f64> = std::collections::HashMap::new();
    let mut n: f64 = 0.0;
    for c in t.chars() {
        *m.entry(c).or_insert(0.0) += 1.0;
        n += 1.0;
    }
    -m.values().map(|c| c / n * (c / n).log2()).sum::<f64>()
}

/// Identifiant fait de mots (snake/camel/kebab) : segments alphabétiques ≥ 3 ou numériques.
fn identifiant(t: &str) -> bool {
    if !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return false;
    }
    let mut segs: Vec<String> = vec![];
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in t.chars() {
        let sep = c == '_' || c == '-';
        let coupe = sep
            || prev.is_some_and(|p| {
                (p.is_ascii_lowercase() && c.is_ascii_uppercase()) || (p.is_ascii_digit() != c.is_ascii_digit())
            });
        if coupe && !cur.is_empty() {
            segs.push(std::mem::take(&mut cur));
        }
        if !sep {
            cur.push(c);
        }
        prev = Some(c);
    }
    segs.push(cur);
    segs.iter().all(|s| {
        s.bytes().all(|b| b.is_ascii_digit()) || (s.len() >= 3 && s.bytes().all(|b| b.is_ascii_alphabetic()))
    })
}

fn mot_de_passe(t: &str) -> bool {
    let n = t.chars().count();
    if !(10..=64).contains(&n) || t.contains(char::is_whitespace) {
        return false;
    }
    let classes = [
        t.chars().any(|c| c.is_lowercase()),
        t.chars().any(|c| c.is_uppercase()),
        t.chars().any(|c| c.is_ascii_digit()),
        t.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    if classes < 3 || entropie(t) < 3.3 {
        return false;
    }
    let b = t.as_bytes();
    let lt = t.to_lowercase();
    let url = t.contains("://") || lt.starts_with("www.") || lt.starts_with("mailto:");
    let chemin = t.starts_with('/') || t.starts_with("./") || t.starts_with("../") || t.starts_with('~')
        || t.contains('\\') || (b[1] == b':' && b[0].is_ascii_alphabetic());
    let email = t.matches('@').count() == 1 && {
        let (l, d) = t.split_once('@').unwrap_or(("", ""));
        !l.is_empty() && d.contains('.') && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    };
    let nombre_date = t.chars().all(|c| c.is_ascii_digit() || "-:/.,TZ+ ".contains(c));
    let hex = t.bytes().all(|b| b.is_ascii_hexdigit());
    let guid = t.len() == 36
        && t.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() });
    let fichier = t.rsplit_once('.').is_some_and(|(p, e)| {
        !p.is_empty() && (2..=5).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase())
    });
    !(url || chemin || email || nombre_date || hex || guid || fichier || identifiant(t))
}

#[cfg(test)]
mod tests {
    use super::is_secret as s;

    #[test]
    fn vrais_positifs() {
        assert!(s("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----"));
        assert!(s("-----BEGIN RSA PRIVATE KEY-----\nMIIE"));
        assert!(s("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"));
        assert!(s("AKIAIOSFODNN7EXAMPLE"));
        assert!(s("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(s("xoxb-123456789012-abcdefghij"));
        assert!(s("sk_live_51HabcdefghIJKL"));
        assert!(s("sk-ant-api03-abcdefghijklmnopqrstuvwxyz"));
        assert!(s("AIzaSyA1234567890abcdefghijklmnopqrstuv"));
        assert!(s("Server=db;User=sa;Password=Hunter2!;"));
        assert!(s("postgres://admin:s3cr3t@db.local:5432/app"));
        assert!(s("export STRIPE_API_KEY=\"abc123\"\nDEBUG=1"));
        assert!(s("kP9#vL2$xQ7!mZ"));
    }

    #[test]
    fn faux_positifs() {
        assert!(!s(""));
        assert!(!s("Bonjour,\nvoici le compte rendu de la réunion d'hier.\nCordialement, Paul"));
        assert!(!s("https://example.com/Path/To?Id=42&x=Y"));
        assert!(!s("C:\\Users\\Paul\\Documents\\Rapport2024.docx"));
        assert!(!s("/home/paul/projets/Memoire-v2"));
        assert!(!s("paul.martin@example.com"));
        assert!(!s("getUserNameFromCache2"));
        assert!(!s("max_retry_count_10"));
        assert!(!s("1234567890123"));
        assert!(!s("2024-11-24T10:30:00Z"));
        assert!(!s("9d6bdaa3f1c2e4b5a6978c0d1e2f3a4b5c6d7e8f"));
        assert!(!s("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!s("Rapport_Final.v2.pdf"));
        assert!(!s("un texte avec password sans égal et token"));
        assert!(!s("sk-learn"));
    }

    #[test]
    fn tronque_et_vite() {
        let long = "mot ".repeat(5000);
        assert!(!s(&long));
        let t = std::time::Instant::now();
        for _ in 0..100 {
            s(&long);
        }
        assert!(t.elapsed().as_micros() / 100 < 5000); // marge large (build debug)
    }
}
