//! Thread de veille : une fenêtre cachée (jamais affichée) qui reçoit
//! - WM_CLIPBOARDUPDATE (AddClipboardFormatListener, sans sondage),
//! - les raccourcis globaux (RegisterHotKey),
//! - l'icône de la zone de notification et son menu,
//! - le changement d'app au premier plan (SetWinEventHook) : Alt+V est relâché quand l'app au premier
//!   plan est dans la liste « laisser Alt+V », pour ne jamais lui voler son raccourci.

use crate::clip;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::DataExchange::AddClipboardFormatListener;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub enum Event {
    Copied(clip::Copied),
    ShowHistory,
    Capture,
    Quit,
    TogglePause,
    /// raccourci refusé par Windows (déjà pris) : message pour l'utilisateur
    Warn(String),
}

pub struct Config {
    pub history: String,
    pub fallback: String,
    pub capture: String,
    /// exe pour lesquels Alt+V (le raccourci d'historique) est laissé à l'app
    pub passthrough: Vec<String>,
}

type Sink = Box<dyn Fn(Event) + Send>;
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
/// fenêtre de veille (pour PostMessage depuis l'interface)
pub static HWND_WATCH: AtomicIsize = AtomicIsize::new(0);

const WM_TRAY: u32 = WM_APP + 1;
/// wParam = numéro de séquence du presse-papiers à effacer dans 30 s
pub const WM_CLEAR_LATER: u32 = WM_APP + 2;
const WM_RELOAD: u32 = WM_APP + 3;
const HK_HISTORY: i32 = 1;
const HK_CAPTURE: i32 = 2;
const TIMER_CLEAR: usize = 7;
static CLEAR_SEQ: AtomicIsize = AtomicIsize::new(0);
static HISTORY_ON: AtomicIsize = AtomicIsize::new(0);

fn send(e: Event) {
    if let Ok(s) = SINK.lock() {
        if let Some(f) = s.as_ref() {
            f(e);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// « Ctrl+Maj+S », « Alt+V », « Win+F2 » → (modificateurs, touche virtuelle)
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let mut m = MOD_NOREPEAT;
    let mut key = None;
    for p in s.split('+').map(|p| p.trim().to_lowercase()) {
        match p.as_str() {
            "ctrl" | "control" => m |= MOD_CONTROL,
            "alt" => m |= MOD_ALT,
            "maj" | "shift" => m |= MOD_SHIFT,
            "win" => m |= MOD_WIN,
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => key = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => key = Some(VK_F1 as u32 + k[1..].parse::<u32>().ok()? - 1),
            "espace" | "space" => key = Some(VK_SPACE as u32),
            "impr" | "printscreen" => key = Some(VK_SNAPSHOT as u32),
            _ => return None,
        }
    }
    Some((m, key?))
}

fn register(hwnd: HWND, id: i32, s: &str) -> bool {
    parse_hotkey(s).is_some_and(|(m, k)| unsafe { RegisterHotKey(hwnd, id, m, k) } != 0)
}

/// (Ré)enregistre les raccourcis selon la config et l'app au premier plan.
fn apply_hotkeys(hwnd: HWND, warn: bool) {
    let Ok(c) = CONFIG.lock() else { return };
    let Some(c) = c.as_ref() else { return };
    let fg = clip::exe_of_window(unsafe { GetForegroundWindow() });
    let leave = !fg.is_empty() && c.passthrough.iter().any(|p| p.eq_ignore_ascii_case(&fg));
    unsafe { UnregisterHotKey(hwnd, HK_HISTORY) };
    HISTORY_ON.store(0, Ordering::Relaxed);
    if !leave {
        if register(hwnd, HK_HISTORY, &c.history) {
            HISTORY_ON.store(1, Ordering::Relaxed);
        } else if register(hwnd, HK_HISTORY, &c.fallback) {
            if warn {
                send(Event::Warn(format!("{} est déjà pris par une autre app : historique sur {}", c.history, c.fallback)));
            }
            HISTORY_ON.store(1, Ordering::Relaxed);
        } else if warn {
            send(Event::Warn(format!("Raccourcis {} et {} indisponibles (réglages)", c.history, c.fallback)));
        }
    }
    if warn {
        unsafe { UnregisterHotKey(hwnd, HK_CAPTURE) };
        if !register(hwnd, HK_CAPTURE, &c.capture) {
            send(Event::Warn(format!("{} est déjà pris : capture par l'icône de la zone de notification", c.capture)));
        }
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    let h = HWND_WATCH.load(Ordering::Relaxed) as HWND;
    let has_pass = CONFIG.lock().ok().is_some_and(|c| c.as_ref().is_some_and(|c| !c.passthrough.is_empty()));
    if has_pass {
        apply_hotkeys(h, false);
    }
}

fn tray(hwnd: HWND, add: bool) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _);
        if nid.hIcon.is_null() {
            nid.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
        }
        let tip = wide("Mémoire — presse-papiers et captures");
        nid.szTip[..tip.len().min(127)].copy_from_slice(&tip[..tip.len().min(127)]);
        Shell_NotifyIconW(if add { NIM_ADD } else { NIM_DELETE }, &nid);
    }
}

fn menu(hwnd: HWND) {
    unsafe {
        let m = CreatePopupMenu();
        let (hist, cap) = CONFIG.lock().ok().and_then(|c| c.as_ref().map(|c| (c.history.clone(), c.capture.clone()))).unwrap_or_default();
        AppendMenuW(m, MF_STRING, 1, wide(&format!("Historique\t{hist}")).as_ptr());
        AppendMenuW(m, MF_STRING, 2, wide(&format!("Capture\t{cap}")).as_ptr());
        AppendMenuW(m, MF_STRING, 3, wide("Mettre en pause / reprendre").as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(m, MF_STRING, 4, wide("Quitter Mémoire").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd); // nécessaire pour que le menu se ferme au clic à côté
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        DestroyMenu(m);
        match cmd {
            1 => send(Event::ShowHistory),
            2 => send(Event::Capture),
            3 => send(Event::TogglePause),
            4 => send(Event::Quit),
            _ => {}
        }
    }
}

/// fenêtres à nous dont les copies sont ignorées (l'interface y ajoute la sienne)
pub static OWN: Mutex<Vec<isize>> = Mutex::new(Vec::new());

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    static TASKBAR: AtomicIsize = AtomicIsize::new(0);
    match msg {
        WM_CLIPBOARDUPDATE => {
            let own: Vec<HWND> = OWN.lock().map(|v| v.iter().map(|&h| h as HWND).collect()).unwrap_or_default();
            if let Some(c) = clip::read(hwnd, &own) {
                send(Event::Copied(c));
            }
            0
        }
        WM_HOTKEY => {
            match wp as i32 {
                HK_HISTORY => send(Event::ShowHistory),
                HK_CAPTURE => send(Event::Capture),
                _ => {}
            }
            0
        }
        WM_TRAY => {
            match (lp as u32) & 0xffff {
                WM_LBUTTONUP => send(Event::ShowHistory),
                WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
                _ => {}
            }
            0
        }
        WM_CLEAR_LATER => {
            CLEAR_SEQ.store(wp as isize, Ordering::Relaxed);
            unsafe { SetTimer(hwnd, TIMER_CLEAR, 30_000, None) };
            0
        }
        WM_TIMER if wp == TIMER_CLEAR => {
            unsafe { KillTimer(hwnd, TIMER_CLEAR) };
            clip::clear_if(hwnd, CLEAR_SEQ.load(Ordering::Relaxed) as u32);
            0
        }
        WM_RELOAD => {
            apply_hotkeys(hwnd, true);
            0
        }
        _ => {
            // l'Explorateur a redémarré : remettre l'icône
            if msg != 0 && msg as isize == TASKBAR.load(Ordering::Relaxed) {
                tray(hwnd, true);
            }
            if msg == WM_CREATE {
                TASKBAR.store(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) } as isize, Ordering::Relaxed);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

/// Démarre le thread de veille. `sink` reçoit les événements (depuis ce thread).
pub fn start(cfg: Config, sink: Sink) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(cfg);
    std::thread::Builder::new()
        .name("veille".into())
        .spawn(|| unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let class = wide("MemoireVeille");
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            // fenêtre de premier niveau jamais affichée (une fenêtre « message-only » ne reçoit pas TaskbarCreated)
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), wide("Mémoire").as_ptr(), WS_POPUP, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), inst, std::ptr::null());
            if hwnd.is_null() {
                send(Event::Warn("veille du presse-papiers impossible".into()));
                return;
            }
            HWND_WATCH.store(hwnd as isize, Ordering::Relaxed);
            if let Ok(mut o) = OWN.lock() {
                o.push(hwnd as isize);
            }
            AddClipboardFormatListener(hwnd);
            apply_hotkeys(hwnd, true);
            tray(hwnd, true);
            SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, std::ptr::null_mut(), Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            tray(hwnd, false);
        })
        .ok();
}

pub fn post(msg: u32, wp: usize) {
    let h = HWND_WATCH.load(Ordering::Relaxed);
    if h != 0 {
        unsafe { PostMessageW(h as HWND, msg, wp, 0) };
    }
}

/// À la fermeture : retirer l'icône tout de suite.
pub fn stop() {
    let h = HWND_WATCH.load(Ordering::Relaxed);
    if h != 0 {
        tray(h as HWND, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raccourcis() {
        assert_eq!(parse_hotkey("Alt+V"), Some((MOD_NOREPEAT | MOD_ALT, b'V' as u32)));
        assert_eq!(parse_hotkey("Ctrl+Maj+S"), Some((MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT, b'S' as u32)));
        assert_eq!(parse_hotkey("Win+F2").map(|x| x.1), Some(VK_F2 as u32));
        assert_eq!(parse_hotkey("Ctrl+"), None);
        assert_eq!(parse_hotkey("Hyper+V"), None);
    }
}
