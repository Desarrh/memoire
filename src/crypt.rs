//! Chiffrement local : une clé aléatoire de 32 octets, protégée par DPAPI (liée au compte Windows),
//! chiffre chaque enregistrement en ChaCha20-Poly1305. DPAPI une seule fois au démarrage, pas par entrée :
//! déchiffrer 100 000 entrées par DPAPI coûterait des secondes.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::path::Path;

pub struct Crypt(ChaCha20Poly1305);

const NONCE: usize = 12;

impl Crypt {
    /// Lit (ou crée) la clé dans `path`, protégée par DPAPI.
    pub fn open(path: &Path) -> Result<Self, String> {
        let key = match std::fs::read(path) {
            Ok(b) => dpapi(&b, false)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let k = ChaCha20Poly1305::generate_key(&mut OsRng).to_vec();
                socle::paths::write_atomic(path, &dpapi(&k, true)?).map_err(|e| format!("clé non enregistrée : {e}"))?;
                k
            }
            Err(e) => return Err(format!("clé illisible : {e}")),
        };
        if key.len() != 32 {
            return Err("clé abîmée".into());
        }
        Ok(Crypt(ChaCha20Poly1305::new(Key::from_slice(&key))))
    }

    /// nonce (12) + texte chiffré
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut out = nonce.to_vec();
        // n'échoue que pour des tailles > 256 Go
        out.extend(self.0.encrypt(&nonce, plain).unwrap_or_default());
        out
    }

    pub fn open_box(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < NONCE + 16 {
            return Err("bloc trop court".into());
        }
        self.0.decrypt(Nonce::from_slice(&data[..NONCE]), &data[NONCE..]).map_err(|_| "bloc illisible".into())
    }
}

/// CryptProtectData / CryptUnprotectData (portée utilisateur).
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY : blobs valides le temps de l'appel ; la sortie est libérée par LocalFree
    let ok = unsafe {
        if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 {
        return Err("DPAPI a refusé la clé (autre compte Windows ?)".into());
    }
    // SAFETY : DPAPI a alloué cbData octets
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as _) };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aller_retour() {
        let dir = crate::store::tests::tmp("crypt");
        let p = dir.join("cle.bin");
        let c = Crypt::open(&p).unwrap();
        let b = c.seal(b"bonjour");
        assert_ne!(&b[NONCE..], b"bonjour");
        // même clé relue depuis le disque
        let c2 = Crypt::open(&p).unwrap();
        assert_eq!(c2.open_box(&b).unwrap(), b"bonjour");
        let mut bad = b.clone();
        bad[NONCE] ^= 1;
        assert!(c2.open_box(&bad).is_err());
    }
}
