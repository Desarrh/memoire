//! Capture d'écran : image figée du bureau virtuel + sélection interactive.
//! Pixels physiques, coordonnées du bureau virtuel (peuvent être négatives).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect { pub l: i32, pub t: i32, pub r: i32, pub b: i32 }

pub struct Frame {
    pub x: i32, pub y: i32, pub w: u32, pub h: u32,
    pub bgra: Vec<u8>,
    /// Ordre Z, dessus d'abord.
    pub windows: Vec<Rect>,
    pub monitors: Vec<Rect>,
}

#[derive(Clone, Copy)]
pub enum Mode { Region, Window, Screen }

pub struct Shot { pub rgba: Vec<u8>, pub w: u32, pub h: u32 }

impl Rect {
    fn w(&self) -> i32 { self.r - self.l }
    fn h(&self) -> i32 { self.b - self.t }
    fn contains(&self, x: i32, y: i32) -> bool { x >= self.l && x < self.r && y >= self.t && y < self.b }
    /// Intersection, None si vide.
    fn inter(self, o: Rect) -> Option<Rect> {
        let r = Rect { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) };
        (r.r > r.l && r.b > r.t).then_some(r)
    }
    fn of(r: RECT) -> Rect { Rect { l: r.left, t: r.top, r: r.right, b: r.bottom } }
    fn win(self) -> RECT { RECT { left: self.l, top: self.t, right: self.r, bottom: self.b } }
}

fn frame_rect(f: &Frame) -> Rect {
    Rect { l: f.x, t: f.y, r: f.x + f.w as i32, b: f.y + f.h as i32 }
}

/// Rectangle glissé normalisé (coins dans n'importe quel ordre).
fn norm(a: (i32, i32), b: (i32, i32)) -> Rect {
    Rect { l: a.0.min(b.0), t: a.1.min(b.1), r: a.0.max(b.0), b: a.1.max(b.1) }
}

/// Une sélection de moins de 4 px est traitée comme un clic.
fn is_click(r: Rect) -> bool { r.w() < 4 || r.h() < 4 }

fn window_at(f: &Frame, x: i32, y: i32) -> Option<Rect> { f.windows.iter().copied().find(|r| r.contains(x, y)) }
fn monitor_at(f: &Frame, x: i32, y: i32) -> Option<Rect> { f.monitors.iter().copied().find(|r| r.contains(x, y)) }

/// Ce qui est surligné sous la souris selon le mode.
fn hover_rect(f: &Frame, m: Mode, x: i32, y: i32) -> Option<Rect> {
    match m {
        Mode::Screen => monitor_at(f, x, y),
        Mode::Region | Mode::Window => window_at(f, x, y).or_else(|| monitor_at(f, x, y)),
    }
}

/// Découpe `r` (coordonnées bureau) dans l'image, rognée aux bords. BGRA → RGBA, alpha forcé à 255.
pub fn crop(frame: &Frame, r: Rect) -> Option<Shot> {
    if frame.bgra.len() != frame.w as usize * frame.h as usize * 4 { return None; }
    let i = r.inter(frame_rect(frame))?;
    let (w, h) = (i.w() as usize, i.h() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in i.t..i.b {
        let o = ((y - frame.y) as usize * frame.w as usize + (i.l - frame.x) as usize) * 4;
        for p in frame.bgra[o..o + w * 4].chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    Some(Shot { rgba, w: w as u32, h: h as u32 })
}

// ---------------------------------------------------------------- GDI

/// DC mémoire + DIB 32 bpp top-down. Libère tout dans Drop (SelectObject restauré avant DeleteObject).
struct Surf { dc: HDC, bmp: HBITMAP, old: HGDIOBJ, bits: *mut u8 }

impl Surf {
    unsafe fn new(w: i32, h: i32) -> Option<Surf> {
        let mut s = Surf { dc: CreateCompatibleDC(null_mut()), bmp: null_mut(), old: null_mut(), bits: null_mut() };
        if s.dc.is_null() { return None; }
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB as _;
        let mut bits: *mut c_void = null_mut();
        s.bmp = CreateDIBSection(s.dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if s.bmp.is_null() || bits.is_null() { return None; }
        s.bits = bits as *mut u8;
        s.old = SelectObject(s.dc, s.bmp);
        if s.old.is_null() { return None; }
        Some(s)
    }
}

impl Drop for Surf {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_null() { SelectObject(self.dc, self.old); }
            if !self.bmp.is_null() { DeleteObject(self.bmp); }
            if !self.dc.is_null() { DeleteDC(self.dc); }
        }
    }
}

// ---------------------------------------------------------------- grab

/// Image figée du bureau virtuel (BitBlt + CAPTUREBLT depuis le DC écran) + fenêtres visibles
/// (DWMWA_EXTENDED_FRAME_BOUNDS, sans les fenêtres cloaked, minimisées, sans titre, ni celles du
/// processus courant) + moniteurs.
pub fn grab() -> Result<Frame, String> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2); // échec ignoré (déjà réglé)
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 { return Err("bureau virtuel vide".into()); }
        let s = Surf::new(w, h).ok_or_else(|| "CreateDIBSection a échoué".to_string())?;
        let scr = GetDC(null_mut());
        if scr.is_null() { return Err("GetDC(écran) a échoué".into()); }
        let ok = BitBlt(s.dc, 0, 0, w, h, scr, x, y, SRCCOPY | CAPTUREBLT);
        ReleaseDC(null_mut(), scr);
        if ok == 0 { return Err(format!("BitBlt a échoué ({})", GetLastError())); }
        GdiFlush();
        let bgra = std::slice::from_raw_parts(s.bits, w as usize * h as usize * 4).to_vec();
        drop(s);

        let mut windows: Vec<Rect> = Vec::new();
        EnumWindows(Some(enum_win), &mut windows as *mut _ as LPARAM);
        let mut monitors: Vec<Rect> = Vec::new();
        EnumDisplayMonitors(null_mut(), null(), Some(enum_mon), &mut monitors as *mut _ as LPARAM);
        Ok(Frame { x, y, w: w as u32, h: h as u32, bgra, windows, monitors })
    }
}

unsafe extern "system" fn enum_win(h: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp as *mut Vec<Rect>);
    if IsWindowVisible(h) == 0 || IsIconic(h) != 0 || GetWindowTextLengthW(h) == 0 || h == GetShellWindow() {
        return 1;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    if pid == GetCurrentProcessId() { return 1; }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(h, DWMWA_CLOAKED as _, &mut cloaked as *mut _ as *mut c_void, 4) == 0 && cloaked != 0 {
        return 1;
    }
    let mut r: RECT = std::mem::zeroed();
    let hr = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS as _, &mut r as *mut _ as *mut c_void, size_of::<RECT>() as u32);
    if hr != 0 && GetWindowRect(h, &mut r) == 0 { return 1; }
    let r = Rect::of(r);
    if r.w() > 0 && r.h() > 0 { v.push(r); }
    1
}

unsafe extern "system" fn enum_mon(_: HMONITOR, _: HDC, r: *mut RECT, lp: LPARAM) -> BOOL {
    let v = &mut *(lp as *mut Vec<Rect>);
    if !r.is_null() { v.push(Rect::of(*r)); }
    1
}

// ---------------------------------------------------------------- select

/// Sélection interactive BLOQUANTE, sur un thread dédié (crée sa fenêtre, pompe ses messages). None si annulé.
pub fn select(frame: &Frame, start: Mode) -> Option<Shot> {
    let r = std::thread::scope(|s| s.spawn(|| unsafe { run_overlay(frame, start) }).join().ok().flatten())?;
    crop(frame, r)
}

const BORDER: i32 = 2;
const COL_BORDER: u32 = 0x00FF_A86F; // #6FA8FF en COLORREF (0x00BBGGRR)
const COL_LABEL: u32 = 0x0020_2020;

struct Ov<'a> {
    frame: &'a Frame,
    cw: i32, ch: i32,
    mode: Mode,
    mouse: (i32, i32),                         // bureau
    hover: Option<Rect>,                       // bureau
    drag: Option<((i32, i32), (i32, i32))>,    // bureau
    done: Option<Option<Rect>>,
    bright: Surf, dark: Surf, back: Surf,
    brush_border: HBRUSH, brush_label: HBRUSH,
    font: HFONT, old_font: HGDIOBJ,
    lw: i32, lh: i32,
}

impl Drop for Ov<'_> {
    fn drop(&mut self) {
        unsafe {
            if !self.old_font.is_null() { SelectObject(self.back.dc, self.old_font); }
            if !self.font.is_null() { DeleteObject(self.font); }
            if !self.brush_border.is_null() { DeleteObject(self.brush_border); }
            if !self.brush_label.is_null() { DeleteObject(self.brush_label); }
        } // puis les Surf se libèrent d'elles-mêmes
    }
}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

unsafe fn run_overlay(frame: &Frame, mode: Mode) -> Option<Rect> {
    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let (cw, ch) = (frame.w as i32, frame.h as i32);
    let len = frame.w as usize * frame.h as usize * 4;
    if cw <= 0 || ch <= 0 || frame.bgra.len() != len { return None; }

    // Image claire + version assombrie (~50 %), calculées une fois.
    let bright = Surf::new(cw, ch)?;
    let dark = Surf::new(cw, ch)?;
    let back = Surf::new(cw, ch)?;
    std::ptr::copy_nonoverlapping(frame.bgra.as_ptr(), bright.bits, len);
    let d = std::slice::from_raw_parts_mut(dark.bits, len);
    for (o, i) in d.iter_mut().zip(&frame.bgra) { *o = i >> 1; }

    let hinst = GetModuleHandleW(null());
    let class = wide("MemoireCaptureOverlay");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0, cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: LoadCursorW(null_mut(), IDC_CROSS),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class.as_ptr(),
        hIconSm: null_mut(),
    };
    if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS { return None; }

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    let ov = Box::into_raw(Box::new(Ov {
        frame, cw, ch, mode,
        mouse: (pt.x, pt.y),
        hover: hover_rect(frame, mode, pt.x, pt.y),
        drag: None, done: None,
        bright, dark, back,
        brush_border: CreateSolidBrush(COL_BORDER),
        brush_label: CreateSolidBrush(COL_LABEL),
        font: null_mut(), old_font: null_mut(),
        lw: 160, lh: 24,
    }));

    let title = wide("Mémoire — capture");
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class.as_ptr(), title.as_ptr(), WS_POPUP,
        frame.x, frame.y, cw, ch, null_mut(), null_mut(), hinst, null(),
    );
    if hwnd.is_null() {
        drop(Box::from_raw(ov));
        UnregisterClassW(class.as_ptr(), hinst);
        return None;
    }

    // Police à l'échelle du DPI + mesure de l'étiquette la plus large.
    {
        let o = &mut *ov;
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let face = wide("Segoe UI");
        o.font = CreateFontW(-(11 * dpi / 72), 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, 5, 0, face.as_ptr());
        if !o.font.is_null() { o.old_font = SelectObject(o.back.dc, o.font); }
        SetBkMode(o.back.dc, TRANSPARENT as _);
        SetTextColor(o.back.dc, 0x00FF_FFFF);
        let sample: Vec<u16> = "00000 × 00000".encode_utf16().collect();
        let mut sz = SIZE { cx: 0, cy: 0 };
        if GetTextExtentPoint32W(o.back.dc, sample.as_ptr(), sample.len() as i32, &mut sz) != 0 {
            o.lw = sz.cx + 12;
            o.lh = sz.cy + 6;
        }
    }

    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ov as isize);
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    SetFocus(hwnd);

    let mut msg: MSG = std::mem::zeroed();
    loop {
        let r = GetMessageW(&mut msg, null_mut(), 0, 0);
        if r == 0 || r == -1 { break; }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    DestroyWindow(hwnd);
    let ov = Box::from_raw(ov);
    let r = ov.done.flatten();
    drop(ov);
    UnregisterClassW(class.as_ptr(), hinst);
    r
}

/// Ajoute `r` à la région `dst` selon `mode` (région temporaire libérée aussitôt).
unsafe fn rgn_op(dst: HRGN, r: Rect, mode: RGN_COMBINE_MODE) {
    let t = CreateRectRgn(r.l, r.t, r.r, r.b);
    if !t.is_null() {
        CombineRgn(dst, dst, t, mode);
        DeleteObject(t);
    }
}

/// Cadre dessiné à l'intérieur de la sélection (reste visible même sur les bords d'écran).
fn border_strips(s: Rect) -> [Rect; 4] {
    let b = BORDER;
    [
        Rect { l: s.l, t: s.t, r: s.r, b: s.t + b },
        Rect { l: s.l, t: s.b - b, r: s.r, b: s.b },
        Rect { l: s.l, t: s.t + b, r: s.l + b, b: s.b - b },
        Rect { l: s.r - b, t: s.t + b, r: s.r, b: s.b - b },
    ]
}

impl Ov<'_> {
    /// Sélection courante (bureau) : rectangle glissé s'il n'est pas un clic, sinon surlignage.
    fn current(&self) -> Option<Rect> {
        if let Some((a, b)) = self.drag {
            let r = norm(a, b);
            if !is_click(r) { return Some(r); }
        }
        self.hover
    }

    /// Sélection en coordonnées client, rognée à la fenêtre.
    fn sel_client(&self) -> Option<Rect> {
        let r = self.current()?;
        let (fx, fy) = (self.frame.x, self.frame.y);
        Rect { l: r.l - fx, t: r.t - fy, r: r.r - fx, b: r.b - fy }.inter(Rect { l: 0, t: 0, r: self.cw, b: self.ch })
    }

    fn label_box(&self, s: Rect) -> Rect {
        let x = s.l.min(self.cw - self.lw).max(0);
        let y = if s.t - self.lh - 4 >= 0 { s.t - self.lh - 4 } else { s.t + 4 };
        Rect { l: x, t: y, r: x + self.lw, b: y + self.lh }
    }

    fn finish(&mut self, r: Option<Rect>) {
        if self.done.is_none() {
            self.done = Some(r);
            unsafe { PostQuitMessage(0) };
        }
    }

    /// Applique `f` puis invalide uniquement ce qui a changé.
    unsafe fn update(&mut self, h: HWND, f: impl FnOnce(&mut Self)) {
        let old = self.sel_client();
        f(self);
        let new = self.sel_client();
        if old == new { return; }
        let rgn = CreateRectRgn(0, 0, 0, 0);
        if rgn.is_null() { InvalidateRect(h, null(), 0); return; }
        // Zone qui bascule clair/sombre = XOR des deux rectangles.
        for s in [old, new].into_iter().flatten() { rgn_op(rgn, s, RGN_XOR); }
        // + les cadres et étiquettes, ancien et nouveau.
        for s in [old, new].into_iter().flatten() {
            for b in border_strips(s) { rgn_op(rgn, b, RGN_OR); }
            rgn_op(rgn, self.label_box(s), RGN_OR);
        }
        InvalidateRgn(h, rgn, 0);
        DeleteObject(rgn);
    }

    fn set_mode(&mut self, m: Mode) {
        self.mode = m;
        if self.drag.is_none() { self.hover = hover_rect(self.frame, m, self.mouse.0, self.mouse.1); }
    }

    /// Double tampon : compose dans `back` (rogné à la région à repeindre) puis copie à l'écran.
    unsafe fn paint(&mut self, h: HWND) {
        let rgn = CreateRectRgn(0, 0, 0, 0);
        let clipped = !rgn.is_null() && GetUpdateRgn(h, rgn, 0) > NULLREGION;
        if clipped { SelectClipRgn(self.back.dc, rgn); }
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(h, &mut ps);
        if !hdc.is_null() {
            let p = Rect::of(ps.rcPaint);
            let back = self.back.dc;
            BitBlt(back, p.l, p.t, p.w(), p.h(), self.dark.dc, p.l, p.t, SRCCOPY);
            if let Some(s) = self.sel_client() {
                if let Some(i) = s.inter(p) {
                    BitBlt(back, i.l, i.t, i.w(), i.h(), self.bright.dc, i.l, i.t, SRCCOPY);
                }
                for b in border_strips(s) { FillRect(back, &b.win(), self.brush_border); }
                let lb = self.label_box(s);
                FillRect(back, &lb.win(), self.brush_label);
                let txt: Vec<u16> = format!("{} × {}", s.w(), s.h()).encode_utf16().collect();
                TextOutW(back, lb.l + 6, lb.t + 3, txt.as_ptr(), txt.len() as i32);
            }
            BitBlt(hdc, p.l, p.t, p.w(), p.h(), back, p.l, p.t, SRCCOPY);
            EndPaint(h, &ps);
        }
        if clipped { SelectClipRgn(self.back.dc, null_mut()); }
        if !rgn.is_null() { DeleteObject(rgn); }
    }
}

/// Coordonnées client signées du lParam → bureau.
fn desk(o: &Ov, lp: LPARAM) -> (i32, i32) {
    let x = (lp & 0xFFFF) as u16 as i16 as i32;
    let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x + o.frame.x, y + o.frame.y)
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == WM_ERASEBKGND { return 1; } // tout est peint dans WM_PAINT
    let p = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Ov;
    if p.is_null() { return DefWindowProcW(h, m, wp, lp); }
    let o = &mut *p;
    match m {
        WM_PAINT => o.paint(h),
        WM_MOUSEMOVE => {
            let pt = desk(o, lp);
            o.update(h, |o| {
                o.mouse = pt;
                match o.drag {
                    Some((a, _)) => o.drag = Some((a, pt)),
                    None => o.hover = hover_rect(o.frame, o.mode, pt.0, pt.1),
                }
            });
        }
        WM_LBUTTONDOWN => {
            let pt = desk(o, lp);
            o.update(h, |o| {
                o.mouse = pt;
                o.drag = Some((pt, pt));
                o.hover = hover_rect(o.frame, o.mode, pt.0, pt.1);
            });
            SetCapture(h);
        }
        WM_LBUTTONUP => {
            if o.drag.is_some() {
                let r = o.current(); // < 4 px → surlignage (clic simple)
                o.finish(r);
            }
            ReleaseCapture();
        }
        WM_CAPTURECHANGED => {
            if o.done.is_none() && o.drag.is_some() { o.update(h, |o| o.drag = None); }
        }
        WM_RBUTTONDOWN | WM_CLOSE => o.finish(None),
        WM_KEYDOWN => match wp as u16 {
            VK_ESCAPE => o.finish(None),
            VK_RETURN => { let r = o.hover; o.finish(r) }
            VK_F => o.update(h, |o| o.set_mode(Mode::Window)),
            VK_E => o.update(h, |o| o.set_mode(Mode::Screen)),
            VK_R => o.update(h, |o| o.set_mode(Mode::Region)),
            _ => {}
        },
        _ => return DefWindowProcW(h, m, wp, lp),
    }
    0
}

// ---------------------------------------------------------------- tests (logique pure, aucun appel Win32)

#[cfg(test)]
mod tests {
    use super::*;

    /// Image synthétique : pixel (i, j) = BGRA [i, j, 7, 0].
    fn frame() -> Frame {
        let (x, y, w, h) = (-100, -50, 200, 100);
        let mut bgra = Vec::new();
        for j in 0..h { for i in 0..w { bgra.extend_from_slice(&[i as u8, j as u8, 7, 0]); } }
        Frame {
            x, y, w, h, bgra,
            windows: vec![Rect { l: -20, t: -20, r: 20, b: 20 }, Rect { l: -90, t: -40, r: 90, b: 40 }],
            monitors: vec![Rect { l: -100, t: -50, r: 0, b: 50 }, Rect { l: 0, t: -50, r: 100, b: 50 }],
        }
    }

    #[test]
    fn crop_negatif_et_debordement() {
        let f = frame();
        let s = crop(&f, Rect { l: -150, t: -60, r: -98, b: -48 }).expect("intersection non vide");
        assert_eq!((s.w, s.h), (2, 2));
        // Coin haut-gauche de l'image : i=0, j=0 ; BGRA→RGBA et alpha 255.
        assert_eq!(&s.rgba[0..4], &[7, 0, 0, 255]);
        assert_eq!(&s.rgba[4..8], &[7, 0, 1, 255]);
        assert_eq!(&s.rgba[8..12], &[7, 1, 0, 255]);
        let s = crop(&f, Rect { l: 90, t: 40, r: 500, b: 500 }).expect("bord bas-droit");
        assert_eq!((s.w, s.h), (10, 10));
        assert_eq!(&s.rgba[0..4], &[7, 90, 190, 255]);
        assert!(crop(&f, Rect { l: 100, t: 0, r: 200, b: 10 }).is_none());
        assert!(crop(&f, Rect { l: 0, t: 0, r: 0, b: 10 }).is_none());
    }

    #[test]
    fn crop_image_incoherente() {
        let mut f = frame();
        f.bgra.pop();
        assert!(crop(&f, Rect { l: 0, t: 0, r: 10, b: 10 }).is_none());
    }

    #[test]
    fn normalisation_glisse() {
        let r = Rect { l: -30, t: -5, r: 10, b: 20 };
        assert_eq!(norm((10, -5), (-30, 20)), r);
        assert_eq!(norm((-30, 20), (10, -5)), r);
        assert!(is_click(norm((5, 5), (8, 100))));
        assert!(!is_click(norm((5, 5), (9, 9))));
    }

    #[test]
    fn fenetre_et_ecran_sous_point() {
        let f = frame();
        // Ordre Z : la petite fenêtre est dessus.
        assert_eq!(window_at(&f, 0, 0), Some(f.windows[0]));
        assert_eq!(window_at(&f, 50, 0), Some(f.windows[1]));
        assert_eq!(window_at(&f, 95, 45), None);
        assert_eq!(hover_rect(&f, Mode::Window, 95, 45), Some(f.monitors[1]));
        assert_eq!(hover_rect(&f, Mode::Region, 0, 0), Some(f.windows[0]));
        assert_eq!(hover_rect(&f, Mode::Screen, 0, 0), Some(f.monitors[1]));
        assert_eq!(hover_rect(&f, Mode::Screen, -1, 0), Some(f.monitors[0]));
        assert_eq!(monitor_at(&f, 100, 0), None); // bord droit exclu
    }
}
