//! Transformations de texte pour le presse-papiers.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transform { JsonPretty, JsonCompact, Upper, Lower, Title, WinToUnix, UnixToWin, Trim, OneLine }

impl Transform {
    pub const ALL: [Transform; 9] = [
        Transform::JsonPretty, Transform::JsonCompact, Transform::Upper, Transform::Lower,
        Transform::Title, Transform::WinToUnix, Transform::UnixToWin, Transform::Trim, Transform::OneLine,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Transform::JsonPretty => "JSON indenté",
            Transform::JsonCompact => "JSON compact",
            Transform::Upper => "MAJUSCULES",
            Transform::Lower => "minuscules",
            Transform::Title => "Initiales Majuscules",
            Transform::WinToUnix => "Chemin Windows → Unix",
            Transform::UnixToWin => "Chemin Unix → Windows",
            Transform::Trim => "Nettoyer les espaces",
            Transform::OneLine => "Tout sur une ligne",
        }
    }
}

pub fn apply(t: Transform, s: &str) -> Result<String, String> {
    Ok(match t {
        Transform::JsonPretty => return json(s, true),
        Transform::JsonCompact => return json(s, false),
        Transform::Upper => s.to_uppercase(),
        Transform::Lower => s.to_lowercase(),
        Transform::Title => title(s),
        Transform::WinToUnix => s.split('\n').map(win_to_unix).collect::<Vec<_>>().join("\n"),
        Transform::UnixToWin => s.split('\n').map(unix_to_win).collect::<Vec<_>>().join("\n"),
        Transform::Trim => trim(s),
        Transform::OneLine => s.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

fn title(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut debut = true;
    for c in s.chars() {
        if c.is_whitespace() {
            debut = true;
            o.push(c);
        } else if debut {
            o.extend(c.to_uppercase());
            debut = false;
        } else {
            o.extend(c.to_lowercase());
        }
    }
    o
}

fn win_to_unix(l: &str) -> String {
    let b = l.as_bytes();
    if l.starts_with("\\\\") {
        return l.replace('\\', "/");
    }
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/') {
        return format!("/{}{}", (b[0] as char).to_ascii_lowercase(), l[2..].replace('\\', "/"));
    }
    l.replace('\\', "/")
}

fn unix_to_win(l: &str) -> String {
    let b = l.as_bytes();
    if l.starts_with("//") {
        return l.replace('/', "\\");
    }
    if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/') {
        let r = if b.len() == 2 { "\\".to_string() } else { l[2..].replace('/', "\\") };
        return format!("{}:{}", (b[1] as char).to_ascii_uppercase(), r);
    }
    l.replace('/', "\\")
}

/// Espaces de bord retirés par ligne ; lignes vides consécutives réduites à une, sans vide en tête/queue.
fn trim(s: &str) -> String {
    let mut o: Vec<&str> = vec![];
    for l in s.lines().map(str::trim) {
        if l.is_empty() && o.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        o.push(l);
    }
    while o.last().is_some_and(|l| l.is_empty()) {
        o.pop();
    }
    o.join("\n")
}

/// Ré-indente (2 espaces) ou compacte du JSON, sans serde.
fn json(s: &str, joli: bool) -> Result<String, String> {
    let t = s.trim();
    if !t.starts_with('{') && !t.starts_with('[') {
        return Err("Ce n'est pas du JSON : il doit commencer par « { » ou « [ ».".into());
    }
    let nl = |o: &mut String, d: usize| {
        if joli {
            o.push('\n');
            o.push_str(&"  ".repeat(d));
        }
    };
    let mut o = String::new();
    let mut pile: Vec<char> = vec![];
    let mut it = t.chars().peekable();
    let mut fini = false;
    while let Some(c) = it.next() {
        if fini && !c.is_whitespace() {
            return Err("Du texte suit la fin du JSON.".into());
        }
        match c {
            '"' => {
                o.push(c);
                let mut clos = false;
                while let Some(x) = it.next() {
                    o.push(x);
                    if x == '\\' {
                        match it.next() {
                            Some(e) => o.push(e),
                            None => break,
                        }
                    } else if x == '"' {
                        clos = true;
                        break;
                    }
                }
                if !clos {
                    return Err("Chaîne de caractères non terminée : il manque un guillemet fermant.".into());
                }
            }
            '{' | '[' => {
                let fin = if c == '{' { '}' } else { ']' };
                o.push(c);
                while it.peek().is_some_and(|x| x.is_whitespace()) {
                    it.next();
                }
                if it.peek() == Some(&fin) {
                    it.next();
                    o.push(fin);
                    fini = pile.is_empty();
                } else {
                    pile.push(fin);
                    nl(&mut o, pile.len());
                }
            }
            '}' | ']' => {
                if pile.pop() != Some(c) {
                    return Err(format!("« {c} » inattendu : accolades ou crochets déséquilibrés."));
                }
                nl(&mut o, pile.len());
                o.push(c);
                fini = pile.is_empty();
            }
            ',' => {
                o.push(c);
                nl(&mut o, pile.len());
            }
            ':' => o.push_str(if joli { ": " } else { ":" }),
            c if c.is_whitespace() => {}
            c => o.push(c),
        }
    }
    match pile.last() {
        Some(f) => Err(format!("JSON incomplet : il manque « {f} » avant la fin.")),
        None => Ok(o),
    }
}

#[cfg(test)]
mod tests {
    use super::{Transform::*, *};

    fn ok(t: Transform, s: &str) -> String {
        apply(t, s).unwrap()
    }

    #[test]
    fn json_pretty() {
        assert_eq!(
            ok(JsonPretty, r#"{"a":1,"b":[1,2],"c":{},"d":"x, {y}\" :"}"#),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {},\n  \"d\": \"x, {y}\\\" :\"\n}"
        );
    }

    #[test]
    fn json_compact() {
        assert_eq!(ok(JsonCompact, "{\n  \"a\": \"x y\",\n  \"b\": [ 1, 2 ]\n}"), r#"{"a":"x y","b":[1,2]}"#);
    }

    #[test]
    fn json_erreurs() {
        assert!(apply(JsonPretty, "bonjour").unwrap_err().contains("commencer"));
        assert!(apply(JsonPretty, "{\"a\": [1, 2}").is_err());
        assert!(apply(JsonCompact, "{\"a\": 1").unwrap_err().contains("manque"));
        assert!(apply(JsonPretty, "{\"a\": \"x}").unwrap_err().contains("guillemet"));
        assert!(apply(JsonPretty, "{} x").is_err());
    }

    #[test]
    fn casse() {
        assert_eq!(ok(Upper, "été à Paris"), "ÉTÉ À PARIS");
        assert_eq!(ok(Lower, "ÉCOLE Ça"), "école ça");
        assert_eq!(ok(Title, "élan vital\nDE la vie"), "Élan Vital\nDe La Vie");
    }

    #[test]
    fn chemins() {
        assert_eq!(ok(WinToUnix, "C:\\Users\\x\n\\\\srv\\p\\f"), "/c/Users/x\n//srv/p/f");
        assert_eq!(ok(UnixToWin, "/c/Users/x\n//srv/p/f"), "C:\\Users\\x\n\\\\srv\\p\\f");
        assert_eq!(ok(WinToUnix, "D:"), "/d");
        assert_eq!(ok(UnixToWin, "/d"), "D:\\");
    }

    #[test]
    fn espaces() {
        assert_eq!(ok(Trim, "\n\n  a  \n\n\n\t b\t\n\n"), "a\n\nb");
        assert_eq!(ok(OneLine, "  a   b\n\n c\t d "), "a b c d");
    }

    #[test]
    fn libelles() {
        assert_eq!(Transform::ALL.len(), 9);
        assert!(Transform::ALL.iter().all(|t| !t.label().is_empty()));
        assert_eq!(WinToUnix.label(), "Chemin Windows → Unix");
    }
}
