//! Glisser-déposer natif (OLE) depuis l'historique : texte (CF_UNICODETEXT) et fichiers (CF_HDROP).
//! Objet de données du shell (`SHCreateDataObject`) et source de glisser par défaut (`SHDoDragDrop`).

use std::path::PathBuf;
use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, OleInitialize};
use windows::Win32::UI::Shell::{SHCreateDataObject, SHDoDragDrop};

const CF_UNICODETEXT: u16 = 13;
const CF_HDROP: u16 = 15;

unsafe fn put(obj: &IDataObject, cf: u16, bytes: &[u8]) -> windows::core::Result<()> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        let fmt = FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let med = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: HGLOBAL(h.0) }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
        obj.SetData(&fmt, &med, true)
    }
}

/// Contenu d'un CF_HDROP : DROPFILES (20 octets) puis les chemins en UTF-16, terminés par un double nul.
pub fn dropfiles(files: &[PathBuf]) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [20i32, 0, 0, 0, 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for f in files {
        for u in f.to_string_lossy().encode_utf16().chain([0]) {
            b.extend_from_slice(&u.to_le_bytes());
        }
    }
    b.extend_from_slice(&[0, 0]);
    b
}

/// Lance le glisser (bloquant jusqu'au dépôt ou à l'abandon).
pub fn run(hwnd: isize, text: Option<&str>, files: &[PathBuf]) -> Result<(), String> {
    unsafe {
        let _ = OleInitialize(None);
        let obj: IDataObject = SHCreateDataObject(None, None, None).map_err(|e| e.to_string())?;
        if let Some(t) = text {
            let b: Vec<u8> = t.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
            put(&obj, CF_UNICODETEXT, &b).map_err(|e| e.to_string())?;
        }
        if !files.is_empty() {
            put(&obj, CF_HDROP, &dropfiles(files)).map_err(|e| e.to_string())?;
        }
        let _ = SHDoDragDrop(Some(HWND(hwnd as _)), &obj, None, DROPEFFECT_COPY);
    }
    Ok(())
}
