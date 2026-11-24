//! Lecture et écriture du presse-papiers Windows (texte, image DIB, fichiers CF_HDROP).

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::Shell::DragQueryFileW;

pub enum Data {
    Text(String),
    Image { rgba: Vec<u8>, w: u32, h: u32 },
    Files(Vec<String>),
}

pub struct Copied {
    pub data: Data,
    /// exe de la fenêtre propriétaire (« opera.exe »)
    pub src: String,
    /// titre de la fenêtre au premier plan (détection de la navigation privée)
    pub title: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn fmt(name: &str) -> u32 {
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

/// Ouvre le presse-papiers, en réessayant un peu (une autre app peut l'avoir ouvert).
fn open(hwnd: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

/// Le contenu demande-t-il à ne pas être historisé ? (gestionnaires de mots de passe, etc.)
fn excluded() -> bool {
    unsafe {
        if IsClipboardFormatAvailable(fmt("ExcludeClipboardContentFromMonitorProcessing")) != 0 || IsClipboardFormatAvailable(fmt("Clipboard Viewer Ignore")) != 0 {
            return true;
        }
        for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
            let f = fmt(name);
            if name.starts_with("CanInclude") && IsClipboardFormatAvailable(f) != 0 {
                let h = GetClipboardData(f);
                if !h.is_null() {
                    let p = GlobalLock(h) as *const u32;
                    let v = if p.is_null() { 1 } else { *p };
                    GlobalUnlock(h);
                    if v == 0 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub fn exe_of_window(w: HWND) -> String {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if w.is_null() {
        return String::new();
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, &mut pid);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut n);
        windows_sys::Win32::Foundation::CloseHandle(h);
        if ok == 0 {
            return String::new();
        }
        let p = String::from_utf16_lossy(&buf[..n as usize]);
        p.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}

pub fn window_title(w: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(w, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Lit le presse-papiers. `None` : exclu, vide, ou écrit par nous (`own`).
pub fn read(hwnd: HWND, own: &[HWND]) -> Option<Copied> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let owner = GetClipboardOwner();
        if !owner.is_null() && own.contains(&owner) {
            return None;
        }
        let fg = GetForegroundWindow();
        let src = { let s = exe_of_window(owner); if s.is_empty() { exe_of_window(fg) } else { s } };
        let title = window_title(fg);
        if !open(hwnd) {
            return None;
        }
        let data = if excluded() { None } else { read_open() };
        CloseClipboard();
        Some(Copied { data: data?, src, title })
    }
}

unsafe fn read_open() -> Option<Data> {
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP as u32) != 0 {
            let h = GetClipboardData(CF_HDROP as u32);
            if !h.is_null() {
                let n = DragQueryFileW(h as _, u32::MAX, std::ptr::null_mut(), 0);
                let mut v = Vec::new();
                for i in 0..n {
                    let len = DragQueryFileW(h as _, i, std::ptr::null_mut(), 0);
                    let mut buf = vec![0u16; len as usize + 1];
                    DragQueryFileW(h as _, i, buf.as_mut_ptr(), buf.len() as u32);
                    v.push(String::from_utf16_lossy(&buf[..len as usize]));
                }
                if !v.is_empty() {
                    return Some(Data::Files(v));
                }
            }
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) != 0 {
            let h = GetClipboardData(CF_UNICODETEXT as u32);
            if !h.is_null() {
                let p = GlobalLock(h) as *const u16;
                if !p.is_null() {
                    let max = GlobalSize(h) / 2;
                    let mut n = 0;
                    while n < max && *p.add(n) != 0 {
                        n += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                    GlobalUnlock(h);
                    if !s.trim().is_empty() {
                        return Some(Data::Text(s));
                    }
                }
            }
        }
        if IsClipboardFormatAvailable(CF_DIB as u32) != 0 {
            let h = GetClipboardData(CF_DIB as u32);
            if !h.is_null() {
                let p = GlobalLock(h) as *const u8;
                if !p.is_null() {
                    let img = dib_to_rgba(std::slice::from_raw_parts(p, GlobalSize(h)));
                    GlobalUnlock(h);
                    if let Some((rgba, w, h)) = img {
                        return Some(Data::Image { rgba, w, h });
                    }
                }
            }
        }
        None
    }
}

/// DIB 24 ou 32 bits (BI_RGB / BI_BITFIELDS standard) → RGBA.
pub fn dib_to_rgba(d: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let rd = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    let hsize = rd(0)? as usize;
    let w = rd(4)? as i32;
    let h = rd(8)? as i32;
    let bpp = u16::from_le_bytes(d.get(14..16)?.try_into().ok()?);
    let comp = rd(16)?;
    if w <= 0 || h == 0 || w > 30000 || h.abs() > 30000 || !(bpp == 24 || bpp == 32) || !(comp == 0 || comp == 3) {
        return None;
    }
    let (w, ah) = (w as usize, h.unsigned_abs() as usize);
    // BI_BITFIELDS avec un en-tête de 40 octets : 3 masques après l'en-tête
    let off = hsize + if comp == 3 && hsize == 40 { 12 } else { 0 };
    let stride = (w * bpp as usize / 8 + 3) & !3;
    let px = d.get(off..off + stride * ah)?;
    let mut out = vec![0u8; w * ah * 4];
    let mut any_alpha = false;
    for y in 0..ah {
        let sy = if h > 0 { ah - 1 - y } else { y };
        let row = &px[sy * stride..];
        for x in 0..w {
            let s = &row[x * bpp as usize / 8..];
            let o = (y * w + x) * 4;
            out[o] = s[2];
            out[o + 1] = s[1];
            out[o + 2] = s[0];
            out[o + 3] = if bpp == 32 { s[3] } else { 255 };
            any_alpha |= out[o + 3] != 0;
        }
    }
    if !any_alpha {
        out.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Some((out, w as u32, ah as u32))
}

fn global(bytes: &[u8]) -> HANDLE {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if h.is_null() {
            return std::ptr::null_mut();
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        h
    }
}

/// Remplace le presse-papiers. `hwnd` : une fenêtre à nous (le propriétaire, pour ignorer notre propre copie).
pub fn write(hwnd: HWND, data: &Data) -> Result<(), String> {
    let mut parts: Vec<(u32, Vec<u8>)> = Vec::new();
    match data {
        Data::Text(s) => parts.push((CF_UNICODETEXT as u32, s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect())),
        Data::Files(v) => {
            // DROPFILES (20 octets) + chemins UTF-16 séparés par \0, terminés par \0\0
            let mut b = vec![0u8; 20];
            b[0] = 20;
            b[16] = 1; // fWide
            for p in v {
                b.extend(p.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()));
            }
            b.extend([0, 0]);
            parts.push((CF_HDROP as u32, b));
            parts.push((CF_UNICODETEXT as u32, v.join("\r\n").encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()));
        }
        Data::Image { rgba, w, h } => {
            let mut b = Vec::with_capacity(40 + rgba.len());
            for v in [40u32, *w, (-(*h as i32)) as u32] {
                b.extend(v.to_le_bytes());
            }
            b.extend(1u16.to_le_bytes());
            b.extend(32u16.to_le_bytes());
            b.extend([0u8; 24]);
            for p in rgba.chunks_exact(4) {
                b.extend([p[2], p[1], p[0], p[3]]);
            }
            parts.push((CF_DIB as u32, b));
        }
    }
    unsafe {
        if !open(hwnd) {
            return Err("presse-papiers occupé par une autre app".into());
        }
        EmptyClipboard();
        let mut ok = true;
        for (f, b) in parts {
            let h = global(&b);
            if h.is_null() || SetClipboardData(f, h).is_null() {
                if !h.is_null() {
                    GlobalFree(h);
                }
                ok = false;
            }
        }
        // pas d'historique Windows (Win+V) ni de cloud pour ce que nous recopions : c'est déjà chez nous
        CloseClipboard();
        if ok { Ok(()) } else { Err("écriture du presse-papiers refusée".into()) }
    }
}

/// Vide le presse-papiers si son contenu n'a pas changé depuis `seq` (effacement des secrets).
pub fn clear_if(hwnd: HWND, seq: u32) {
    unsafe {
        if GetClipboardSequenceNumber() == seq && open(hwnd) {
            EmptyClipboard();
            CloseClipboard();
        }
    }
}

pub fn seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dib_24_et_32() {
        // 2×1, 24 bits, de bas en haut : stride 8
        let mut d = vec![0u8; 40];
        d[0] = 40;
        d[4] = 2;
        d[8] = 1;
        d[12] = 1;
        d[14] = 24;
        d.extend([1, 2, 3, 4, 5, 6, 0, 0]);
        let (px, w, h) = super::dib_to_rgba(&d).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, vec![3, 2, 1, 255, 6, 5, 4, 255]);
        d[14] = 32;
        d.truncate(40);
        d.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        let (px, _, _) = super::dib_to_rgba(&d).unwrap();
        assert_eq!(px, vec![3, 2, 1, 255, 6, 5, 4, 255]); // alpha nul partout → opaque
        assert!(super::dib_to_rgba(&d[..20]).is_none());
    }
}
