//! Historique : un journal chiffré en ajout seul (`journal.bin`), relu en entier au démarrage.
//! Chaque enregistrement = [u32 longueur][nonce + JSON chiffré]. Une modification ajoute une nouvelle
//! version de l'entrée ; le journal est réécrit (compacté) quand il contient trop de versions mortes.
//! Images : `img/<id>.bin`, PNG chiffré. Statistiques (comptes seulement, sans contenu) : `stats.json`.

use crate::crypt::Crypt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    Text,
    Image,
    Files,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Entry {
    pub id: u64,
    pub kind: Kind,
    /// texte copié, chemins (un par ligne) ou texte reconnu sur l'image
    pub text: String,
    /// dernière utilisation (secondes Unix)
    pub t: i64,
    pub created: i64,
    /// exe d'origine (« code.exe »), ou « Capture »
    pub src: String,
    pub pinned: bool,
    pub secret: bool,
    pub w: u32,
    pub h: u32,
    /// octets sur le disque (image) ou du texte
    pub size: u64,
    pub ocr_done: bool,
    /// fichier PNG enregistré (capture), pour « Afficher dans l'explorateur »
    pub path: String,
    pub hash: u64,
}

#[derive(Serialize, Deserialize)]
enum Op {
    Put(Entry),
    Del(u64),
}

pub struct Store {
    dir: PathBuf,
    crypt: Crypt,
    file: std::fs::File,
    /// triées du plus récent au plus ancien
    pub entries: Vec<Entry>,
    ops: usize,
    next_id: u64,
    /// incrémenté à chaque changement (l'interface reconstruit son index)
    pub generation: u64,
}

pub const MAX_TEXT: usize = 1 << 20;

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// FNV-1a : dédoublonnage des copies identiques
pub fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100000001b3))
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, String> {
        Self::open_with(dir, true)
    }

    /// Lecture seule (CLI pendant que l'app tourne) : ni création, ni compaction, ni écriture.
    pub fn open_read(dir: &Path) -> Result<Self, String> {
        if !dir.join("cle.bin").exists() || !dir.join("journal.bin").exists() {
            return Err("historique vide".into());
        }
        Self::open_with(dir, false)
    }

    fn open_with(dir: &Path, write: bool) -> Result<Self, String> {
        if write {
            std::fs::create_dir_all(dir.join("img")).map_err(|e| format!("dossier {} : {e}", dir.display()))?;
        }
        let crypt = Crypt::open(&dir.join("cle.bin"))?;
        let path = dir.join("journal.bin");
        let data = std::fs::read(&path).unwrap_or_default();
        let mut map: BTreeMap<u64, Entry> = BTreeMap::new();
        let (mut i, mut ops) = (0usize, 0usize);
        while i + 4 <= data.len() {
            let n = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
            if i + 4 + n > data.len() {
                break; // fin tronquée (coupure pendant une écriture) : ignorée
            }
            if let Ok(plain) = crypt.open_box(&data[i + 4..i + 4 + n]) {
                match serde_json::from_slice::<Op>(&plain) {
                    Ok(Op::Put(e)) => {
                        map.insert(e.id, e);
                    }
                    Ok(Op::Del(id)) => {
                        map.remove(&id);
                    }
                    Err(_) => {}
                }
            }
            ops += 1;
            i += 4 + n;
        }
        let mut entries: Vec<Entry> = map.into_values().collect();
        entries.sort_by(|a, b| b.t.cmp(&a.t).then(b.id.cmp(&a.id)));
        let next_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        let file = if write { std::fs::OpenOptions::new().create(true).append(true).open(&path) } else { std::fs::File::open(&path) }.map_err(|e| format!("journal : {e}"))?;
        let mut s = Store { dir: dir.to_path_buf(), crypt, file, entries, ops, next_id, generation: 1 };
        if write && s.ops > 1000 && s.ops > s.entries.len() * 2 {
            let _ = s.compact();
        }
        Ok(s)
    }

    fn write(&mut self, op: &Op) {
        let Ok(json) = serde_json::to_vec(op) else { return };
        let sealed = self.crypt.seal(&json);
        let mut rec = (sealed.len() as u32).to_le_bytes().to_vec();
        rec.extend(sealed);
        let _ = self.file.write_all(&rec);
        self.ops += 1;
        self.generation += 1;
    }

    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    /// Ajoute ou remonte une entrée. Une copie identique à une entrée existante la remonte en tête.
    /// Renvoie l'id et `true` si c'est une nouvelle entrée.
    pub fn add(&mut self, mut e: Entry) -> (u64, bool) {
        if let Some(pos) = self.entries.iter().position(|x| x.hash == e.hash && x.kind == e.kind) {
            let mut old = self.entries.remove(pos);
            old.t = e.t;
            if !e.src.is_empty() {
                old.src = e.src;
            }
            let id = old.id;
            self.write(&Op::Put(old.clone()));
            self.entries.insert(0, old);
            return (id, false);
        }
        if e.id == 0 {
            e.id = self.new_id();
        }
        let id = e.id;
        self.write(&Op::Put(e.clone()));
        self.entries.insert(0, e);
        (id, true)
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Modifie une entrée (épingle, OCR…) ; `bump` la remonte en tête.
    pub fn update(&mut self, id: u64, bump: bool, f: impl FnOnce(&mut Entry)) {
        let Some(pos) = self.entries.iter().position(|e| e.id == id) else { return };
        let mut e = self.entries.remove(pos);
        f(&mut e);
        if bump {
            e.t = now();
        }
        self.write(&Op::Put(e.clone()));
        if bump { self.entries.insert(0, e) } else { self.entries.insert(pos, e) }
    }

    pub fn delete(&mut self, id: u64) {
        if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
            let e = self.entries.remove(pos);
            if e.kind == Kind::Image {
                let _ = std::fs::remove_file(self.img_path(id));
                let _ = std::fs::remove_file(self.dir.join("img").join(format!("{id}.t.bin")));
            }
            self.write(&Op::Del(id));
        }
    }

    fn img_path(&self, id: u64) -> PathBuf {
        img_path(&self.dir, id)
    }

    pub fn load_image(&self, id: u64) -> Result<(Vec<u8>, u32, u32), String> {
        load_image(&self.crypt, &self.dir, id)
    }

    /// Réécrit le journal avec les seules entrées vivantes.
    pub fn compact(&mut self) -> Result<(), String> {
        let mut buf = Vec::new();
        for e in self.entries.iter().rev() {
            let Ok(json) = serde_json::to_vec(&Op::Put(e.clone())) else { continue };
            let sealed = self.crypt.seal(&json);
            buf.extend((sealed.len() as u32).to_le_bytes());
            buf.extend(sealed);
        }
        let path = self.dir.join("journal.bin");
        socle::paths::write_atomic(&path, &buf)?;
        self.file = std::fs::OpenOptions::new().append(true).open(&path).map_err(|e| format!("journal : {e}"))?;
        self.ops = self.entries.len();
        Ok(())
    }

    /// Rétention : textes et fichiers plus vieux que `text_days`, images plus vieilles que `img_days`,
    /// puis les plus vieilles images tant que le total dépasse `max_bytes`. Les épingles restent.
    pub fn prune(&mut self, text_days: i64, img_days: i64, max_bytes: u64) -> usize {
        let t = now();
        let mut gone: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| !e.pinned && t - e.t > 86400 * if e.kind == Kind::Image { img_days } else { text_days })
            .map(|e| e.id)
            .collect();
        let mut total: u64 = self.entries.iter().filter(|e| !gone.contains(&e.id)).map(|e| e.size).sum();
        for e in self.entries.iter().rev() {
            if total <= max_bytes {
                break;
            }
            if !e.pinned && !gone.contains(&e.id) {
                total = total.saturating_sub(e.size);
                gone.push(e.id);
            }
        }
        for id in &gone {
            self.delete(*id);
        }
        gone.len()
    }
}

fn img_path(dir: &Path, id: u64) -> PathBuf {
    dir.join("img").join(format!("{id}.bin"))
}

fn load_image(c: &Crypt, dir: &Path, id: u64) -> Result<(Vec<u8>, u32, u32), String> {
    let data = std::fs::read(img_path(dir, id)).map_err(|e| format!("image introuvable : {e}"))?;
    let png = c.open_box(&data)?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| format!("image abîmée : {e}"))?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}

/// Lecture et écriture des images depuis un autre thread (même clé que le journal).
pub struct ImageReader {
    dir: PathBuf,
    crypt: Crypt,
}

impl ImageReader {
    pub fn new(dir: &Path) -> Result<Self, String> {
        Ok(ImageReader { dir: dir.to_path_buf(), crypt: Crypt::open(&dir.join("cle.bin"))? })
    }

    pub fn load(&self, id: u64) -> Result<(Vec<u8>, u32, u32), String> {
        load_image(&self.crypt, &self.dir, id)
    }

    fn thumb_path(&self, id: u64) -> PathBuf {
        self.dir.join("img").join(format!("{id}.t.bin"))
    }

    /// Miniature (360 px au plus) depuis le cache, sinon calculée depuis l'image et mise en cache.
    pub fn thumb(&self, id: u64) -> Result<(Vec<u8>, u32, u32), String> {
        if let Ok(data) = std::fs::read(self.thumb_path(id)) {
            if let Ok(png) = self.crypt.open_box(&data) {
                if let Ok(img) = image::load_from_memory_with_format(&png, image::ImageFormat::Png) {
                    let img = img.to_rgba8();
                    let (w, h) = img.dimensions();
                    return Ok((img.into_raw(), w, h));
                }
            }
        }
        let (rgba, w, h) = self.load(id)?;
        self.thumb_from(id, &rgba, w, h)
    }

    pub fn thumb_from(&self, id: u64, rgba: &[u8], w: u32, h: u32) -> Result<(Vec<u8>, u32, u32), String> {
        let img = image::RgbaImage::from_raw(w, h, rgba.to_vec()).ok_or("image incohérente")?;
        let k = (360.0 / w.max(h) as f32).min(1.0);
        let (tw, th) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
        let t = image::imageops::thumbnail(&img, tw, th);
        let mut png = Vec::new();
        t.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| format!("PNG : {e}"))?;
        let _ = socle::paths::write_atomic(&self.thumb_path(id), &self.crypt.seal(&png));
        Ok((t.into_raw(), tw, th))
    }

    /// Enregistre l'image (PNG chiffré) ; renvoie sa taille sur le disque.
    pub fn save(&self, id: u64, rgba: &[u8], w: u32, h: u32) -> Result<u64, String> {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
            .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("PNG : {e}"))?;
        let sealed = self.crypt.seal(&png);
        socle::paths::write_atomic(&img_path(&self.dir, id), &sealed)?;
        Ok(sealed.len() as u64)
    }
}

// ---------- statistiques (pour Bilan) ----------

/// jour local « AAAA-MM-JJ » → [copies, captures]
pub type Stats = BTreeMap<String, [u32; 2]>;

pub fn count(dir: &Path, capture: bool) {
    let p = dir.join("stats.json");
    let mut s: Stats = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    s.entry(day).or_default()[capture as usize] += 1;
    // garder ~2 ans
    while s.len() > 800 {
        let Some(k) = s.keys().next().cloned() else { break };
        s.remove(&k);
    }
    if let Ok(b) = serde_json::to_vec(&s) {
        let _ = socle::paths::write_atomic(&p, &b);
    }
}

/// `--stats-semaine --json [--semaine AAAA-MM-JJ]` : semaine du lundi au dimanche qui contient la date.
pub fn week_json(dir: &Path, date: Option<&str>) -> Result<String, String> {
    use chrono::{Datelike, Duration, NaiveDate};
    let day = match date {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| format!("date invalide : {d} (attendu AAAA-MM-JJ)"))?,
        None => chrono::Local::now().date_naive(),
    };
    let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
    let s: Stats = std::fs::read(dir.join("stats.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let (mut copies, mut captures, mut par_jour) = (0u32, 0u32, [0u32; 7]);
    for (i, pj) in par_jour.iter_mut().enumerate() {
        let k = (monday + Duration::days(i as i64)).format("%Y-%m-%d").to_string();
        let [c, k2] = s.get(&k).copied().unwrap_or_default();
        copies += c;
        captures += k2;
        *pj = c + k2;
    }
    Ok(serde_json::json!({"v": 1, "debut": monday.format("%Y-%m-%d").to_string(), "copies": copies, "captures": captures, "par_jour": par_jour}).to_string())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("memoire-test-{name}-{}-{}", std::process::id(), now()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn txt(s: &str) -> Entry {
        Entry { text: s.into(), hash: hash(s.as_bytes()), t: now(), created: now(), size: s.len() as u64, ..Default::default() }
    }

    #[test]
    fn journal_relu_dedoublonne_compacte() {
        let d = tmp("store");
        {
            let mut s = Store::open(&d).unwrap();
            let (a, new) = s.add(txt("un"));
            assert!(new);
            s.add(txt("deux"));
            let (a2, new2) = s.add(txt("un"));
            assert_eq!((a, new2), (a2, false));
            assert_eq!(s.entries[0].text, "un");
            s.update(a, false, |e| e.pinned = true);
            let id = s.new_id();
            let mut img = txt("");
            img.id = id;
            img.kind = Kind::Image;
            img.hash = 7;
            let px = vec![200u8; 4 * 3 * 2];
            img.size = ImageReader::new(&d).unwrap().save(id, &px, 3, 2).unwrap();
            s.add(img);
            assert_eq!(s.load_image(id).unwrap(), (px, 3, 2));
            s.delete(s.entries.iter().find(|e| e.text == "deux").unwrap().id);
        }
        let mut s = Store::open(&d).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert!(s.get(1).unwrap().pinned);
        assert!(!std::fs::read(d.join("journal.bin")).unwrap().windows(4).any(|w| w == b"deux"), "en clair sur le disque");
        s.compact().unwrap();
        let s = Store::open(&d).unwrap();
        assert_eq!(s.entries.len(), 2);
    }

    #[test]
    fn retention_et_stats() {
        let d = tmp("prune");
        let mut s = Store::open(&d).unwrap();
        let mut old = txt("vieux");
        old.t = now() - 86400 * 200;
        s.add(old.clone());
        let mut pin = txt("vieux épinglé");
        pin.t = old.t;
        pin.pinned = true;
        s.add(pin);
        s.add(txt("récent"));
        assert_eq!(s.prune(180, 90, u64::MAX), 1);
        assert_eq!(s.entries.len(), 2);
        count(&d, false);
        count(&d, false);
        count(&d, true);
        let v: serde_json::Value = serde_json::from_str(&week_json(&d, None).unwrap()).unwrap();
        assert_eq!((v["v"].as_u64(), v["copies"].as_u64(), v["captures"].as_u64()), (Some(1), Some(2), Some(1)));
        let v: serde_json::Value = serde_json::from_str(&week_json(&d, Some("2026-11-26")).unwrap()).unwrap();
        assert_eq!(v["debut"], "2026-11-23");
        assert!(week_json(&d, Some("hier")).is_err());
    }
}
