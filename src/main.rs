#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annotate;
mod app;
mod capture;
mod clip;
mod crypt;
mod ocr;
mod secrets;
mod store;
mod transform;
mod watch;

use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Rendu CPU (comme l'Explorateur et Vigie) : pas de contexte GPU, ~0 % au repos.
struct Gfx {
    window: Rc<Window>,
    egui: egui_winit::State,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    renderer: EguiSoftwareRender,
}

enum UserEvent {
    RepaintAt(Instant),
    Watch(watch::Event),
    Job(app::Job),
    /// une 2e instance a été lancée : montrer l'historique
    Front,
}

struct Runner {
    ctx: egui::Context,
    gfx: Option<Gfx>,
    app: Option<app::App>,
    next_repaint: Option<Instant>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    visible: bool,
    shown_at: Instant,
    capturing: bool,
    start_hidden: bool,
}

fn data_dir() -> PathBuf {
    // tests et essais : jamais les données réelles de l'utilisateur
    if let Some(d) = std::env::var_os("MEMOIRE_DIR") {
        return PathBuf::from(d);
    }
    socle::paths::cache_dir("Memoire")
}

/// Thread de fond en priorité basse (OCR, encodage).
pub fn low_thread_priority() {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

/// Démarrage avec Windows : HKCU\Software\Microsoft\Windows\CurrentVersion\Run, valeur « Memoire ».
pub fn autostart(on: bool) {
    use windows_sys::Win32::System::Registry::*;
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0".encode_utf16().collect();
    let name: Vec<u16> = "Memoire\0".encode_utf16().collect();
    unsafe {
        let mut h = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_SET_VALUE, &mut h) != 0 {
            return;
        }
        if on {
            if let Ok(exe) = std::env::current_exe() {
                let v: Vec<u16> = format!("\"{}\" --demarrage", exe.display()).encode_utf16().chain([0]).collect();
                RegSetValueExW(h, name.as_ptr(), 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32);
            }
        } else {
            RegDeleteValueW(h, name.as_ptr());
        }
        RegCloseKey(h);
    }
}

fn hwnd_of(w: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// Zone de travail de l'écran sous la souris (pixels physiques).
fn work_area() -> (i32, i32, i32, i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        GetCursorPos(&mut p);
        let m = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(m, &mut mi);
        let r = mi.rcWork;
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }
}

impl Runner {
    fn paint(&mut self) {
        let (Some(gfx), Some(app)) = (self.gfx.as_mut(), self.app.as_mut()) else { return };
        let size = gfx.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let mut raw = gfx.egui.take_egui_input(&gfx.window);
        socle::testmode::prepare_input(&mut raw);
        let out = self.ctx.run_ui(raw, |ui| app.frame(ui));
        gfx.egui.handle_platform_output(&gfx.window, out.platform_output);
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        if gfx.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = gfx.surface.buffer_mut() else { return };
        let bg = app.pal.bg;
        buf.fill(((bg.r() as u32) << 16) | ((bg.g() as u32) << 8) | bg.b() as u32);
        {
            let px: &mut [[u8; 4]] = bytemuck::cast_slice_mut(&mut buf[..]);
            let mut target = BufferMutRef::new(px, w.get() as usize, h.get() as usize);
            gfx.renderer.render(&mut target, &prims, &out.textures_delta, out.pixels_per_point);
        }
        let _ = buf.present();
        self.after_frame();
    }

    /// Applique ce que l'interface a demandé (cacher, capturer, redimensionner, quitter).
    fn after_frame(&mut self) {
        let Some(app) = self.app.as_mut() else { return };
        let out = std::mem::take(&mut app.out);
        if out.quit {
            app.save_settings();
            watch::stop();
            std::process::exit(0);
        }
        if let Some(sz) = out.resize {
            self.place(Some(sz));
        }
        if out.hide {
            self.hide();
        }
        if out.capture {
            self.start_capture();
        }
    }

    fn hide(&mut self) {
        if let Some(g) = &self.gfx {
            if !socle::testmode::enabled() {
                g.window.set_visible(false);
            }
        }
        self.visible = false;
    }

    /// Place la fenêtre au centre de l'écran sous la souris ; `content` = taille voulue (éditeur).
    fn place(&mut self, content: Option<egui::Vec2>) {
        let Some(g) = &self.gfx else { return };
        let (x, y, w, h) = work_area();
        let sf = g.window.scale_factor();
        let (cw, ch) = match content {
            Some(v) => ((v.x as f64 + 24.0 * sf).min(w as f64 * 0.92).max(520.0 * sf), (v.y as f64 + 70.0 * sf).min(h as f64 * 0.92).max(320.0 * sf)),
            None => ((980.0 * sf).min(w as f64 * 0.9), (620.0 * sf).min(h as f64 * 0.85)),
        };
        let _ = g.window.request_inner_size(PhysicalSize::new(cw as u32, ch as u32));
        g.window.set_outer_position(PhysicalPosition::new(x + (w - cw as i32) / 2, y + (h - ch as i32) / 3));
    }

    fn show(&mut self, history: bool) {
        if history {
            if let Some(a) = self.app.as_mut() {
                if a.editing() {
                    return self.focus();
                }
                a.on_show();
            }
            self.place(None);
        }
        self.focus();
    }

    fn focus(&mut self) {
        let Some(g) = &self.gfx else { return };
        if !socle::testmode::enabled() {
            g.window.set_visible(true);
            g.window.focus_window();
        }
        self.visible = true;
        self.shown_at = Instant::now();
        g.window.request_redraw();
    }

    /// Capture : on cache la fenêtre, on fige l'écran puis l'overlay de sélection tourne sur son thread.
    fn start_capture(&mut self) {
        if self.capturing || socle::testmode::enabled() {
            return;
        }
        if self.app.as_ref().is_some_and(|a| a.editing()) {
            return self.focus();
        }
        let was_visible = self.visible;
        self.hide();
        self.capturing = true;
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            // laisser à Windows le temps d'effacer notre fenêtre (animation de fermeture comprise)
            if was_visible {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            let shot = match capture::grab() {
                Ok(f) => capture::select(&f, capture::Mode::Region),
                Err(e) => {
                    let _ = p.send_event(UserEvent::Job(app::Job::Info(format!("Capture impossible : {e}"), true)));
                    None
                }
            };
            let _ = p.send_event(UserEvent::Job(app::Job::Shot(shot)));
        });
    }
}

impl ApplicationHandler<UserEvent> for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let test = socle::testmode::enabled();
        let attrs = Window::default_attributes()
            .with_title("Mémoire")
            .with_inner_size(LogicalSize::new(980.0, 620.0))
            .with_min_inner_size(LogicalSize::new(520.0, 320.0))
            .with_window_icon(window_icon())
            .with_visible(false)
            .with_active(false);
        let window = Rc::new(el.create_window(attrs).expect("création de la fenêtre"));
        let hwnd = hwnd_of(&window);
        if test {
            socle::testmode::hide(hwnd);
        }
        if let Ok(mut o) = watch::OWN.lock() {
            o.push(hwnd);
        }
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");
        let egui = egui_winit::State::new(self.ctx.clone(), egui::ViewportId::ROOT, &*window, Some(window.scale_factor() as f32), None, Some(8192));
        let renderer = EguiSoftwareRender::new(ColorFieldOrder::Bgra);
        if self.app.is_none() {
            let s: app::Settings = socle::settings::load("Memoire");
            let px = self.proxy.clone();
            let wake: app::Wake = std::sync::Arc::new(move |j| {
                let _ = px.send_event(UserEvent::Job(j));
            });
            let mut a = app::App::new(&self.ctx, s, data_dir(), wake);
            a.hwnd = hwnd;
            if let Some(msg) = socle::crash::pending("Memoire") {
                a.toast(msg, true);
            }
            if !test {
                autostart(a.s.demarrage_windows);
                let px = self.proxy.clone();
                watch::start(
                    watch::Config { history: a.s.raccourci_historique.clone(), fallback: a.s.raccourci_secours.clone(), capture: a.s.raccourci_capture.clone(), passthrough: a.s.laisser_raccourci.clone() },
                    Box::new(move |e| {
                        let _ = px.send_event(UserEvent::Watch(e));
                    }),
                );
            }
            self.app = Some(a);
        }
        self.gfx = Some(Gfx { window, egui, _context: context, surface, renderer });
        if test || !self.start_hidden {
            self.show(true);
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        let resp = gfx.egui.on_window_event(&gfx.window, &event);
        match event {
            WindowEvent::CloseRequested => {
                if let Some(a) = self.app.as_mut() {
                    a.save_settings();
                }
                self.hide();
            }
            // comme Win+V : l'historique se ferme quand on clique ailleurs
            WindowEvent::Focused(false) => {
                let editing = self.app.as_ref().is_some_and(|a| a.editing());
                if self.visible && !editing && !socle::testmode::enabled() && self.shown_at.elapsed().as_millis() > 300 {
                    self.hide();
                }
            }
            WindowEvent::RedrawRequested => self.paint(),
            _ => {
                if resp.repaint {
                    gfx.window.request_redraw();
                }
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::RepaintAt(t) => {
                if t <= Instant::now() {
                    if let Some(g) = &self.gfx {
                        g.window.request_redraw();
                    }
                } else {
                    self.next_repaint = Some(self.next_repaint.map_or(t, |n| n.min(t)));
                }
            }
            UserEvent::Front => self.show(true),
            UserEvent::Watch(e) => match e {
                watch::Event::Copied(c) => {
                    if let Some(a) = self.app.as_mut() {
                        a.on_copied(c);
                    }
                    if self.visible {
                        if let Some(g) = &self.gfx {
                            g.window.request_redraw();
                        }
                    }
                }
                watch::Event::ShowHistory => {
                    if self.visible && !self.app.as_ref().is_some_and(|a| a.editing()) {
                        self.hide();
                    } else {
                        self.show(true);
                    }
                }
                watch::Event::Capture => self.start_capture(),
                watch::Event::Quit => {
                    if let Some(a) = self.app.as_mut() {
                        a.save_settings();
                    }
                    watch::stop();
                    std::process::exit(0);
                }
                watch::Event::TogglePause => {
                    if let Some(a) = self.app.as_mut() {
                        a.paused = !a.paused;
                    }
                }
                watch::Event::Warn(m) => {
                    if let Some(a) = self.app.as_mut() {
                        a.toast(m, true);
                    }
                }
            },
            UserEvent::Job(j) => {
                let shot = matches!(j, app::Job::Shot(_));
                if shot {
                    self.capturing = false;
                }
                let has = matches!(&j, app::Job::Shot(Some(_)));
                if let Some(a) = self.app.as_mut() {
                    a.on_job(j);
                }
                if has {
                    self.after_frame();
                    self.focus();
                } else if self.visible {
                    if let Some(g) = &self.gfx {
                        g.window.request_redraw();
                    }
                }
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        match self.next_repaint {
            Some(t) if t <= Instant::now() => {
                self.next_repaint = None;
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
                el.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if let Some(a) = self.app.as_mut() {
            a.save_settings();
        }
        watch::stop();
    }
}

fn window_icon() -> Option<winit::window::Icon> {
    let img = image::load_from_memory(include_bytes!("../assets/icon64.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

fn main() {
    socle::crash::install("Memoire", env!("CARGO_PKG_VERSION"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Pour Bilan : statistiques de la semaine, sans contenu, en lecture seule
    if args.iter().any(|a| a == "--stats-semaine") {
        let date = args.iter().position(|a| a == "--semaine").and_then(|i| args.get(i + 1)).map(String::as_str);
        match store::week_json(&data_dir(), date) {
            Ok(j) => {
                println!("{j}");
                std::process::exit(0)
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2)
            }
        }
    }
    let start_hidden = args.iter().any(|a| a == "--demarrage");
    let mut instance = None;
    if socle::testmode::enabled() {
        socle::testmode::low_priority();
    } else {
        match socle::instance::start("Memoire", &args) {
            socle::instance::Start::Forwarded => return,
            socle::instance::Start::Primary(i) => instance = Some(i),
            socle::instance::Start::Alone => {}
        }
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("boucle d’événements");
    let proxy = event_loop.create_proxy();
    if let Some(i) = instance {
        let p = proxy.clone();
        i.listen(move |a| {
            if !a.iter().any(|x| x == "--demarrage") {
                let _ = p.send_event(UserEvent::Front);
            }
        });
    }
    let ctx = egui::Context::default();
    let p2 = proxy.clone();
    ctx.set_request_repaint_callback(move |info| {
        let _ = p2.send_event(UserEvent::RepaintAt(Instant::now() + info.delay));
    });
    let mut runner = Runner { ctx, gfx: None, app: None, next_repaint: None, proxy, visible: false, shown_at: Instant::now(), capturing: false, start_hidden };
    let _ = event_loop.run_app(&mut runner);
}
