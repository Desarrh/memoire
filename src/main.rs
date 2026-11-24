#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annotate;
mod app;
mod capture;
mod clip;
mod crypt;
mod drag;
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
    /// cachée par perte du focus : un clic sur l'icône juste après doit fermer, pas rouvrir
    hidden_at: Instant,
    dragging: bool,
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
        if let (Some(on), Some(g)) = (out.topmost, &self.gfx) {
            g.window.set_window_level(if on { winit::window::WindowLevel::AlwaysOnTop } else { winit::window::WindowLevel::Normal });
        }
        if let Some((text, files)) = out.drag {
            self.drag(text, files);
        }
    }

    /// Glisser-déposer OLE (boucle modale de Windows jusqu'au dépôt).
    fn drag(&mut self, text: Option<String>, files: Vec<PathBuf>) {
        if socle::testmode::enabled() {
            return;
        }
        let Some(g) = self.gfx.as_mut() else { return };
        self.dragging = true;
        let res = drag::run(hwnd_of(&g.window), text.as_deref(), &files);
        self.dragging = false;
        // le relâchement du bouton a été pris par la boucle de glisser : egui ne l'a pas vu
        let raw = g.egui.egui_input_mut();
        let pos = self.ctx.pointer_latest_pos().unwrap_or_default();
        raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        raw.events.push(egui::Event::PointerGone);
        g.window.request_redraw();
        if let (Err(m), Some(a)) = (res, self.app.as_mut()) {
            a.toast(format!("Glisser impossible : {m}"), true);
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
                let pinned = self.app.as_ref().is_some_and(|a| a.on_top);
                if self.visible && !editing && !pinned && !self.dragging && !socle::testmode::enabled() && self.shown_at.elapsed().as_millis() > 300 {
                    self.hide();
                    self.hidden_at = Instant::now();
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
                    } else if self.hidden_at.elapsed().as_millis() < 400 {
                        // clic sur l'icône : la fenêtre vient de se cacher en perdant le focus, on la laisse fermée
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

/// `--chercher` : même recherche floue que la fenêtre (texte et OCR), sans jamais les secrets.
pub fn cli_search(dir: &std::path::Path, q: &str, max: usize) -> String {
    let entries: Vec<store::Entry> = store::Store::open_read(dir).map(|s| s.entries).unwrap_or_default().into_iter().filter(|e| !e.secret).collect();
    let picked: Vec<&store::Entry> = if q.trim().is_empty() {
        entries.iter().take(max).collect()
    } else {
        let mut idx = socle::fuzzy::Index::new();
        for (i, e) in entries.iter().enumerate() {
            let boost = if e.pinned { 40 } else { 0 } + (20 - (i / 500).min(20) as i32);
            idx.push(&app::App::search_text(e), boost);
        }
        idx.search(q, max).into_iter().filter_map(|h| entries.get(h.index)).collect()
    };
    let res: Vec<serde_json::Value> = picked
        .iter()
        .map(|e| {
            let line = e.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let apercu = match e.kind {
                store::Kind::Image if line.is_empty() => format!("Image {} × {}", e.w, e.h),
                store::Kind::Image => format!("Image : {line}"),
                _ => line,
            };
            let apercu: String = apercu.chars().take(120).collect();
            let t = if e.kind == store::Kind::Image { "image" } else { "texte" };
            serde_json::json!({"id": e.id.to_string(), "type": t, "apercu": apercu, "quand": e.t})
        })
        .collect();
    serde_json::json!({"v": 1, "resultats": res}).to_string()
}

/// `--copier <id>` : remet l'entrée dans le presse-papiers ; refusé pour un secret.
fn cli_copy(dir: &std::path::Path, id: u64) -> bool {
    let Ok(st) = store::Store::open_read(dir) else { return false };
    let Some(e) = st.get(id).filter(|e| !e.secret) else { return false };
    let data = match e.kind {
        store::Kind::Text => clip::Data::Text(e.text.clone()),
        store::Kind::Files => clip::Data::Files(e.text.lines().map(String::from).collect()),
        store::Kind::Image => match st.load_image(id) {
            Ok((rgba, w, h)) => clip::Data::Image { rgba, w, h },
            Err(_) => return false,
        },
    };
    clip::write(std::ptr::null_mut(), &data).is_ok()
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
    // Pour le Lanceur : recherche et copie, en lecture seule, sans fenêtre
    if let Some(i) = args.iter().position(|a| a == "--chercher") {
        let q = args.get(i + 1).map(String::as_str).unwrap_or("");
        let max = args.iter().position(|a| a == "--max").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(20usize);
        println!("{}", cli_search(&data_dir(), q, max));
        std::process::exit(0);
    }
    if let Some(i) = args.iter().position(|a| a == "--copier") {
        let ok = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()).is_some_and(|id| cli_copy(&data_dir(), id));
        std::process::exit(if ok { 0 } else { 2 });
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
    let mut runner = Runner { ctx, gfx: None, app: None, next_repaint: None, proxy, visible: false, shown_at: Instant::now(), capturing: false, start_hidden, hidden_at: Instant::now() - std::time::Duration::from_secs(5), dragging: false };
    let _ = event_loop.run_app(&mut runner);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--chercher` pour le Lanceur : secrets jamais listés, aperçu sur une ligne, rapide sur 5 000 entrées.
    #[test]
    fn cli_chercher() {
        let dir = store::tests::tmp("cli");
        {
            let mut st = store::Store::open(&dir).unwrap();
            for i in 0..5000u64 {
                let text = format!("note numéro {i}\nsuite {}", "x".repeat(200));
                st.add(store::Entry { text, hash: i + 10, t: store::now(), ..Default::default() });
            }
            st.add(store::Entry { text: "motdepasse ghp_secretabc".into(), secret: true, hash: 1, t: store::now(), ..Default::default() });
            st.add(store::Entry { text: "facture électricité".into(), hash: 2, t: store::now(), ..Default::default() });
        }
        let t0 = std::time::Instant::now();
        let out = cli_search(&dir, "factur", 20);
        let ms = t0.elapsed().as_millis();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["resultats"][0]["apercu"], "facture électricité");
        assert_eq!(v["resultats"][0]["type"], "texte");
        assert!(!cli_search(&dir, "motdepasse", 20).contains("ghp_"), "secret jamais listé");
        assert!(!cli_search(&dir, "", 5000).contains("ghp_"));
        let a = cli_search(&dir, "note", 3);
        let v: serde_json::Value = serde_json::from_str(&a).unwrap();
        let ap = v["resultats"][0]["apercu"].as_str().unwrap();
        assert!(ap.chars().count() <= 120 && !ap.contains('\n'));
        assert!(ms < 1500, "recherche en {ms} ms");
        eprintln!("cli_chercher : {ms} ms sur 5 002 entrées");
    }
}
