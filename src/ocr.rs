//! OCR Windows.Media.Ocr : fichier autonome.
use std::cell::RefCell;
use std::collections::HashSet;
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
    core::HSTRING,
};

thread_local! {
    // Moteurs créés une seule fois par thread.
    static ENGINES: RefCell<Option<Vec<OcrEngine>>> = const { RefCell::new(None) };
}

const OVERLAP: u32 = 64;

fn engines() -> Result<Vec<OcrEngine>, String> {
    // Déjà initialisé (S_FALSE / mode différent) : toléré.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let mut v = Vec::new();
    for tag in ["fr", "en"] {
        if let Ok(l) = Language::CreateLanguage(&HSTRING::from(tag)) {
            if let Ok(e) = OcrEngine::TryCreateFromLanguage(&l) {
                v.push(e);
            }
        }
    }
    if v.is_empty() {
        v.push(OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| format!("OCR indisponible : {e}"))?);
    }
    Ok(v)
}

fn score(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphanumeric()).count()
}

/// Texte reconnu dans une image RGBA 8 bits (lignes séparées par '\n'). Bloquant : thread de fond.
pub fn recognize(rgba: &[u8], w: u32, h: u32) -> Result<String, String> {
    if w == 0 || h == 0 || rgba.len() != w as usize * h as usize * 4 {
        return Err(format!("taille incohérente : {} octets pour {w}x{h}", rgba.len()));
    }
    // RGBA -> BGRA
    let mut buf: Vec<u8> = rgba.to_vec();
    buf.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    let (mut w, mut h) = (w, h);
    if h < 40 {
        // Agrandissement x2 (pixels dupliqués).
        let (nw, nh) = (w * 2, h * 2);
        let mut big = Vec::with_capacity(nw as usize * nh as usize * 4);
        for y in 0..nh {
            for x in 0..nw {
                let i = ((y / 2) as usize * w as usize + (x / 2) as usize) * 4;
                big.extend_from_slice(&buf[i..i + 4]);
            }
        }
        (buf, w, h) = (big, nw, nh);
    }
    ENGINES.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(engines()?);
        }
        let mut best = String::new();
        for e in c.as_deref().unwrap_or(&[]) {
            let t = run(e, &buf, w, h).map_err(|e| format!("OCR : {e}"))?;
            if score(&t) > score(&best) {
                best = t;
            }
        }
        Ok(best)
    })
}

/// Un moteur sur toute l'image, en tuiles chevauchantes (haut en bas, gauche à droite).
fn run(engine: &OcrEngine, bgra: &[u8], w: u32, h: u32) -> windows::core::Result<String> {
    let tile = OcrEngine::MaxImageDimension()?.max(2 * OVERLAP);
    let step = tile - OVERLAP;
    let single = tile >= w.max(h);
    let mut seen = HashSet::new(); // shortcut : dédoublonnage des lignes du chevauchement par texte exact
    let mut out = Vec::new();
    let mut y0 = 0;
    loop {
        let y1 = (y0 + tile).min(h);
        let mut x0 = 0;
        loop {
            let x1 = (x0 + tile).min(w);
            let (tw, th) = (x1 - x0, y1 - y0);
            let mut px = Vec::with_capacity(tw as usize * th as usize * 4);
            for y in y0..y1 {
                let i = (y as usize * w as usize + x0 as usize) * 4;
                px.extend_from_slice(&bgra[i..i + tw as usize * 4]);
            }
            let wr = DataWriter::new()?;
            wr.WriteBytes(&px)?;
            let bmp = SoftwareBitmap::CreateCopyFromBuffer(&wr.DetachBuffer()?, BitmapPixelFormat::Bgra8, tw as i32, th as i32)?;
            for l in engine.RecognizeAsync(&bmp)?.join()?.Lines()? {
                let t = l.Text()?.to_string();
                if single || seen.insert(t.clone()) {
                    out.push(t);
                }
            }
            if x1 == w {
                break;
            }
            x0 += step;
        }
        if y1 == h {
            break;
        }
        y0 += step;
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;
    use std::time::Instant;
    use windows_sys::Win32::Graphics::Gdi::*;

    /// Dessin GDI en mémoire (aucune fenêtre, aucune capture) : fond blanc, Segoe UI 24 px noir.
    fn draw(w: i32, h: i32, textes: &[(i32, i32, &str)]) -> Vec<u8> {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // top-down
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!bmp.is_null() && !bits.is_null());
            SelectObject(dc, bmp);
            let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
            let font = CreateFontW(-24, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 4, 0, face.as_ptr());
            SelectObject(dc, font);
            let n = w as usize * h as usize * 4;
            std::slice::from_raw_parts_mut(bits as *mut u8, n).fill(255);
            SetBkMode(dc, 1);
            SetTextColor(dc, 0);
            for (x, y, t) in textes {
                let u: Vec<u16> = t.encode_utf16().collect();
                TextOutW(dc, *x, *y, u.as_ptr(), u.len() as i32);
            }
            GdiFlush();
            let mut v = std::slice::from_raw_parts(bits as *const u8, n).to_vec();
            v.chunks_exact_mut(4).for_each(|p| {
                p.swap(0, 2); // BGRA -> RGBA
                p[3] = 255;
            });
            DeleteObject(font);
            DeleteObject(bmp);
            DeleteDC(dc);
            v
        }
    }

    #[test]
    fn francais_accents() {
        let img = draw(1000, 100, &[(20, 30, "Réunion à l'école : le déjeuner est prévu demain après-midi")]);
        let t = recognize(&img, 1000, 100).unwrap_or_else(|e| panic!("{e}")).to_lowercase();
        eprintln!("FR: {t}");
        let ok = ["réunion", "école", "déjeuner", "prévu", "demain", "après"].iter().filter(|m| t.contains(*m)).count();
        assert!(ok >= 4, "trop peu de mots reconnus : {t}");
    }

    #[test]
    fn large_gauche_et_droite() {
        let img = draw(5120, 1440, &[(40, 100, "Bonjour tout le monde"), (4700, 1200, "Fin du document")]);
        let t = recognize(&img, 5120, 1440).unwrap_or_else(|e| panic!("{e}")).to_lowercase();
        eprintln!("LARGE: {t}");
        assert!(t.contains("bonjour") && t.contains("document"), "{t}");
    }

    #[test]
    fn vide_et_incoherent() {
        let blanc = vec![255u8; 200 * 100 * 4];
        assert_eq!(recognize(&blanc, 200, 100).unwrap_or_else(|e| panic!("{e}")).trim(), "");
        assert!(recognize(&blanc, 300, 100).is_err());
        assert!(recognize(&[], 0, 0).is_err());
    }

    #[test]
    fn temps_1080p() {
        let lignes: Vec<String> = (0..30).map(|i| format!("Ligne {i} : le rapport trimestriel est disponible dans le dossier partagé")).collect();
        let t: Vec<(i32, i32, &str)> = lignes.iter().enumerate().map(|(i, l)| (60, 20 + i as i32 * 34, l.as_str())).collect();
        let img = draw(1920, 1080, &t);
        let t0 = Instant::now();
        let a = recognize(&img, 1920, 1080).unwrap_or_else(|e| panic!("{e}"));
        let d1 = t0.elapsed();
        let t1 = Instant::now();
        let _ = recognize(&img, 1920, 1080).unwrap_or_else(|e| panic!("{e}"));
        eprintln!("TEMPS 1080p : 1er appel {d1:?}, 2e appel {:?}, {} lignes", t1.elapsed(), a.lines().count());
        assert!(a.lines().count() >= 20);
    }
}
