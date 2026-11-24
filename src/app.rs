//! Interface : fenêtre d'historique (recherche floue, aperçu, épingles) et éditeur de capture.

use crate::annotate::{self, Mark, Tool};
use crate::capture::Shot;
use crate::clip::{self, Data};
use crate::store::{self, Entry, Kind, Store};
use crate::{secrets, transform};
use egui::{Color32, FontId, Key, Modifiers, Rect, Sense, Stroke, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};
use socle::icons::{self, Icon, Look};
use socle::theme::{self, Palette, taille};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub raccourci_historique: String,
    pub raccourci_secours: String,
    pub raccourci_capture: String,
    /// exe (« code.exe ») au premier plan desquels le raccourci d'historique est laissé à l'app
    pub laisser_raccourci: Vec<String>,
    /// exe dont les copies ne sont jamais gardées
    pub apps_exclues: Vec<String>,
    pub jours_texte: i64,
    pub jours_images: i64,
    pub limite_mo: u64,
    pub demarrage_windows: bool,
    pub effacer_secrets_30s: bool,
    pub ocr: bool,
    pub sombre: Option<bool>,
    pub dossier_captures: Option<String>,
    /// ancien comportement : la fenêtre se ferme après une copie
    pub fermer_apres_copie: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            raccourci_historique: "Alt+V".into(),
            raccourci_secours: "Ctrl+Maj+V".into(),
            raccourci_capture: "Ctrl+Maj+S".into(),
            laisser_raccourci: Vec::new(),
            apps_exclues: Vec::new(),
            jours_texte: 183,
            jours_images: 92,
            limite_mo: 2048,
            demarrage_windows: true,
            effacer_secrets_30s: true,
            ocr: true,
            sombre: None,
            dossier_captures: None,
            fermer_apres_copie: false,
        }
    }
}

/// Travail de fond terminé (renvoyé par les threads).
pub enum Job {
    Saved(u64, u64),
    Ocr(u64, String),
    Preview(u64, Vec<u8>, u32, u32),
    Thumb(u64, Vec<u8>, u32, u32),
    Shot(Option<Shot>),
    Info(String, bool),
}

pub type Wake = Arc<dyn Fn(Job) + Send + Sync>;

/// Ce que la boucle principale doit faire après une image.
#[derive(Default)]
pub struct Out {
    pub hide: bool,
    pub quit: bool,
    pub capture: bool,
    pub resize: Option<Vec2>,
    /// glisser-déposer à lancer : texte et/ou fichiers
    pub drag: Option<(Option<String>, Vec<PathBuf>)>,
    /// fenêtre toujours au premier plan (et plus fermée quand elle perd le focus)
    pub topmost: Option<bool>,
}

struct Editor {
    base: Vec<u8>,
    w: u32,
    h: u32,
    marks: Vec<Mark>,
    tool: Tool,
    drag: Option<(f32, f32)>,
    tex: egui::TextureHandle,
}

const PRIVATE: [&str; 8] = ["inprivate", "navigation privée", "private browsing", "incognito", "(private)", "fenêtre privée", "mode privé", "private window"];
const FILTERS: [&str; 6] = ["Tout", "Texte", "Liens", "Images", "Fichiers", "Épinglés"];

/// Lien web ou adresse : un seul mot qui commence comme une URL.
pub fn is_link(t: &str) -> bool {
    let t = t.trim();
    let l = t.to_ascii_lowercase();
    !t.is_empty() && !t.contains(char::is_whitespace) && (l.starts_with("http://") || l.starts_with("https://") || l.starts_with("www.") || l.starts_with("mailto:") || l.starts_with("ftp://"))
}

pub struct App {
    pub pal: Palette,
    pub s: Settings,
    saver: socle::settings::Saver,
    pub store: Option<Store>,
    dir: PathBuf,
    index: socle::fuzzy::Index,
    index_ids: Vec<u64>,
    index_gen: u64,
    query: String,
    results: Vec<u64>,
    results_key: (String, usize, u64),
    sel: usize,
    /// multi-sélection (Ctrl+clic, Maj+clic), dans l'ordre du clic
    multi: Vec<u64>,
    pub on_top: bool,
    filter: usize,
    revealed: HashSet<u64>,
    toasts: socle::ui::Toasts,
    palette: socle::ui::Palette,
    help: bool,
    preview: Option<(u64, egui::TextureHandle)>,
    preview_wanted: u64,
    thumbs: std::collections::HashMap<u64, Option<egui::TextureHandle>>,
    thumb_tx: std::sync::mpsc::Sender<u64>,
    /// lignes dessinées à la dernière image (tests)
    pub rows: Vec<Rect>,
    editor: Option<Editor>,
    pub hwnd: isize,
    wake: Wake,
    ocr_tx: std::sync::mpsc::Sender<(u64, Option<(Vec<u8>, u32, u32)>)>,
    pub paused: bool,
    focus_search: bool,
    scroll_to_sel: bool,
    pub out: Out,
}

fn now() -> i64 {
    store::now()
}

impl App {
    pub fn new(ctx: &egui::Context, s: Settings, dir: PathBuf, wake: Wake) -> Self {
        theme::install_fonts(ctx);
        let dark = s.sombre.unwrap_or_else(|| theme::system_dark(ctx));
        let pal = theme::palette(dark);
        theme::apply(ctx, &pal);
        let mut toasts = socle::ui::Toasts::default();
        let store = match Store::open(&dir) {
            Ok(mut st) => {
                st.prune(s.jours_texte, s.jours_images, s.limite_mo << 20);
                Some(st)
            }
            Err(e) => {
                toasts.error(format!("Historique illisible : {e}"));
                None
            }
        };
        let ocr_tx = spawn_worker(&dir, wake.clone(), s.ocr);
        let dir2 = dir.clone();
        // fichiers temporaires des glisser-déposer de la session précédente
        let _ = std::fs::remove_dir_all(dir.join("glisser"));
        let a = App {
            pal,
            s,
            saver: socle::settings::Saver::debounced(std::time::Duration::from_millis(300), std::time::Duration::from_secs(2)),
            store,
            dir,
            index: socle::fuzzy::Index::new(),
            index_ids: Vec::new(),
            index_gen: 0,
            query: String::new(),
            results: Vec::new(),
            results_key: (String::new(), usize::MAX, 0),
            sel: 0,
            multi: Vec::new(),
            on_top: false,
            filter: 0,
            revealed: HashSet::new(),
            toasts,
            palette: socle::ui::Palette::default(),
            help: false,
            preview: None,
            preview_wanted: 0,
            thumbs: Default::default(),
            thumb_tx: spawn_thumbs(&dir2, wake.clone()),
            rows: Vec::new(),
            editor: None,
            hwnd: 0,
            wake,
            ocr_tx,
            paused: false,
            focus_search: true,
            scroll_to_sel: false,
            out: Out::default(),
        };
        // images dont le texte n'a pas encore été lu (app fermée pendant l'OCR)
        if a.s.ocr {
            if let Some(st) = &a.store {
                for e in st.entries.iter().filter(|e| e.kind == Kind::Image && !e.ocr_done).take(200) {
                    let _ = a.ocr_tx.send((e.id, None));
                }
            }
        }
        a
    }

    pub fn toast(&mut self, msg: impl Into<String>, err: bool) {
        if err { self.toasts.error(msg) } else { self.toasts.info(msg) }
    }

    pub fn save_settings(&mut self) {
        let _ = self.saver.flush("Memoire", &self.s);
    }

    /// La fenêtre vient d'être (ré)affichée pour l'historique.
    pub fn on_show(&mut self) {
        self.query.clear();
        self.sel = 0;
        self.multi.clear();
        self.filter = 0;
        self.revealed.clear();
        self.focus_search = true;
        self.scroll_to_sel = true;
    }

    pub fn editing(&self) -> bool {
        self.editor.is_some()
    }

    // ---------- arrivées ----------

    pub fn on_copied(&mut self, c: clip::Copied) {
        if self.paused {
            return;
        }
        let title = c.title.to_lowercase();
        if PRIVATE.iter().any(|p| title.contains(p)) || self.s.apps_exclues.iter().any(|x| x.eq_ignore_ascii_case(&c.src)) {
            return;
        }
        let Some(st) = self.store.as_mut() else { return };
        let t = now();
        let mut e = Entry { t, created: t, src: c.src, ..Default::default() };
        let mut image = None;
        match c.data {
            Data::Text(mut s) => {
                if s.len() > store::MAX_TEXT {
                    let mut n = store::MAX_TEXT;
                    while !s.is_char_boundary(n) {
                        n -= 1;
                    }
                    s.truncate(n);
                }
                e.secret = secrets::is_secret(&s);
                e.hash = store::hash(s.as_bytes());
                e.size = s.len() as u64;
                e.text = s;
            }
            Data::Files(v) => {
                e.kind = Kind::Files;
                e.text = v.join("\n");
                e.hash = store::hash(e.text.as_bytes());
                e.size = e.text.len() as u64;
            }
            Data::Image { rgba, w, h } => {
                e.kind = Kind::Image;
                e.w = w;
                e.h = h;
                e.hash = store::hash(&rgba) ^ ((w as u64) << 32 | h as u64);
                image = Some((rgba, w, h));
            }
        }
        let secret = e.secret;
        let (id, new) = st.add(e);
        if new && !secret {
            store::count(&self.dir, false);
        }
        if secret && self.s.effacer_secrets_30s {
            crate::watch::post(crate::watch::WM_CLEAR_LATER, clip::seq() as usize);
        }
        if let (true, Some(img)) = (new, image) {
            let _ = self.ocr_tx.send((id, Some(img)));
        }
    }

    pub fn on_job(&mut self, j: Job) {
        match j {
            Job::Saved(id, size) => {
                if let Some(st) = self.store.as_mut() {
                    st.update(id, false, |e| e.size = size);
                }
            }
            Job::Ocr(id, text) => {
                if let Some(st) = self.store.as_mut() {
                    st.update(id, false, |e| {
                        e.text = text;
                        e.ocr_done = true;
                    });
                }
            }
            Job::Thumb(id, rgba, w, h) => {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                let t = CTX.with(|c| c.borrow().as_ref().map(|ctx| ctx.load_texture("miniature", img, egui::TextureOptions::LINEAR)));
                self.thumbs.insert(id, t);
            }
            Job::Preview(id, rgba, w, h) => {
                if id == self.preview_wanted {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    self.preview = Some((id, self.palette_ctx_tex(img)));
                }
            }
            Job::Shot(Some(shot)) => self.open_editor(shot),
            Job::Shot(None) => self.out.hide = true,
            Job::Info(m, err) => self.toast(m, err),
        }
    }

    fn palette_ctx_tex(&self, img: egui::ColorImage) -> egui::TextureHandle {
        CTX.with(|c| c.borrow().as_ref().map(|ctx| ctx.load_texture("apercu", img, egui::TextureOptions::LINEAR))).expect("contexte egui")
    }

    fn open_editor(&mut self, shot: Shot) {
        let img = egui::ColorImage::from_rgba_unmultiplied([shot.w as usize, shot.h as usize], &shot.rgba);
        let tex = CTX.with(|c| c.borrow().as_ref().map(|ctx| ctx.load_texture("capture", img, egui::TextureOptions::LINEAR)));
        let Some(tex) = tex else { return };
        // fenêtre à la taille de l'image (bornée par la boucle principale à l'écran)
        self.out.resize = Some(vec2(shot.w as f32, shot.h as f32));
        self.editor = Some(Editor { base: shot.rgba, w: shot.w, h: shot.h, marks: Vec::new(), tool: Tool::Arrow, drag: None, tex });
    }

    // ---------- index et résultats ----------

    fn search_text(e: &Entry) -> String {
        let cut = |s: &str| s.chars().take(2000).collect::<String>();
        match e.kind {
            _ if e.secret => format!("secret {}", e.src),
            Kind::Text => format!("{} {}", cut(&e.text), e.src),
            Kind::Image => format!("image {} {}", cut(&e.text), e.src),
            Kind::Files => format!("fichier {} {}", cut(&e.text), e.src),
        }
    }

    fn refresh(&mut self) {
        let Some(st) = &self.store else { return };
        if self.index_gen != st.generation {
            self.index_gen = st.generation;
            let mut idx = socle::fuzzy::Index::new();
            self.index_ids.clear();
            for (i, e) in st.entries.iter().enumerate() {
                // les plus récentes d'abord à score égal
                let boost = if e.pinned { 40 } else { 0 } + (20 - (i / 500).min(20) as i32);
                idx.push(&Self::search_text(e), boost);
                self.index_ids.push(e.id);
            }
            self.index = idx;
        }
        let key = (self.query.clone(), self.filter, st.generation);
        if key == self.results_key {
            return;
        }
        let keep = |e: &Entry| match self.filter {
            1 => e.kind == Kind::Text,
            2 => e.kind == Kind::Text && !e.secret && is_link(&e.text),
            3 => e.kind == Kind::Image,
            4 => e.kind == Kind::Files,
            5 => e.pinned,
            _ => true,
        };
        self.results = if self.query.trim().is_empty() {
            st.entries.iter().filter(|e| keep(e)).map(|e| e.id).collect()
        } else {
            self.index.search(&self.query, 500).into_iter().filter_map(|h| self.index_ids.get(h.index).copied()).filter(|id| st.get(*id).is_some_and(keep)).collect()
        };
        if self.results_key.0 != self.query || self.results_key.1 != self.filter {
            self.sel = 0;
            self.multi.clear();
            self.scroll_to_sel = true;
        }
        self.sel = self.sel.min(self.results.len().saturating_sub(1));
        self.results_key = key;
    }

    fn selected(&self) -> Option<&Entry> {
        let id = *self.results.get(self.sel)?;
        self.store.as_ref()?.get(id)
    }

    // ---------- actions ----------

    fn copy_entry(&mut self, id: u64, tf: Option<transform::Transform>) {
        let Some(st) = self.store.as_ref() else { return };
        let Some(e) = st.get(id).cloned() else { return };
        let data = match e.kind {
            Kind::Text => match tf {
                Some(t) => match transform::apply(t, &e.text) {
                    Ok(s) => Data::Text(s),
                    Err(m) => return self.toast(m, true),
                },
                None => Data::Text(e.text.clone()),
            },
            Kind::Files => Data::Files(e.text.lines().map(String::from).collect()),
            Kind::Image => match st.load_image(id) {
                Ok((rgba, w, h)) => Data::Image { rgba, w, h },
                Err(m) => return self.toast(m, true),
            },
        };
        match clip::write(self.hwnd as _, &data) {
            Ok(()) => {
                if let Some(st) = self.store.as_mut() {
                    st.update(id, true, |_| {});
                }
                if e.secret && self.s.effacer_secrets_30s {
                    crate::watch::post(crate::watch::WM_CLEAR_LATER, clip::seq() as usize);
                }
                self.copied();
            }
            Err(m) => self.toast(m, true),
        }
    }

    fn copied(&mut self) {
        if self.s.fermer_apres_copie && !self.on_top {
            self.out.hide = true;
        } else {
            self.toast("Copié", false);
        }
    }

    /// Entrées visées : la multi-sélection si elle existe, sinon l'entrée choisie.
    fn targets(&self) -> Vec<u64> {
        if self.multi.is_empty() { self.results.get(self.sel).copied().into_iter().collect() } else { self.multi.clone() }
    }

    /// Image en PNG lisible par les autres apps : `<données>\glisser\<id>\Capture … .png` (vidé au démarrage).
    fn export_png(&self, e: &Entry) -> Result<PathBuf, String> {
        let dir = self.dir.join("glisser").join(e.id.to_string());
        let when = chrono::DateTime::from_timestamp(e.created, 0).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H.%M.%S").to_string()).unwrap_or_default();
        let p = dir.join(format!("{} {when}.png", if e.src == "Capture" { "Capture" } else { "Image" }));
        if p.exists() {
            return Ok(p);
        }
        let (rgba, w, h) = self.store.as_ref().ok_or("historique fermé")?.load_image(e.id)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        image::save_buffer(&p, &rgba, w, h, image::ColorType::Rgba8).map_err(|e| e.to_string())?;
        Ok(p)
    }

    /// Texte fusionné et fichiers (images exportées en PNG) des entrées données.
    fn gather(&self, ids: &[u64]) -> Result<(Option<String>, Vec<PathBuf>), String> {
        let Some(st) = self.store.as_ref() else { return Err("historique fermé".into()) };
        let (mut texts, mut files) = (Vec::new(), Vec::new());
        for id in ids {
            let Some(e) = st.get(*id) else { continue };
            match e.kind {
                Kind::Text => texts.push(e.text.clone()),
                Kind::Files => files.extend(e.text.lines().map(PathBuf::from)),
                Kind::Image => files.push(self.export_png(e)?),
            }
        }
        Ok(((!texts.is_empty()).then(|| texts.join("\n")), files))
    }

    /// Copie plusieurs entrées : textes fusionnés (une ligne entre chaque), ou fichiers si images/fichiers.
    fn copy_many(&mut self, ids: &[u64]) {
        if ids.len() == 1 {
            return self.copy_entry(ids[0], None);
        }
        let (text, files) = match self.gather(ids) {
            Ok(v) => v,
            Err(m) => return self.toast(m, true),
        };
        let data = match (text, files.is_empty()) {
            (Some(t), true) => Data::Text(t),
            (_, false) => Data::Files(files.iter().map(|p| p.to_string_lossy().into_owned()).collect()),
            (None, true) => return,
        };
        match clip::write(self.hwnd as _, &data) {
            Ok(()) => {
                if self.store.as_ref().is_some_and(|st| ids.iter().any(|i| st.get(*i).is_some_and(|e| e.secret))) && self.s.effacer_secrets_30s {
                    crate::watch::post(crate::watch::WM_CLEAR_LATER, clip::seq() as usize);
                }
                self.copied();
            }
            Err(m) => self.toast(m, true),
        }
    }

    fn start_drag(&mut self, ids: &[u64]) {
        match self.gather(ids) {
            Ok((text, files)) if text.is_some() || !files.is_empty() => self.out.drag = Some((text, files)),
            Ok(_) => {}
            Err(m) => self.toast(m, true),
        }
    }

    /// Fichier lié à l'entrée : capture enregistrée, premier fichier copié.
    fn file_of(e: &Entry) -> Option<PathBuf> {
        match e.kind {
            Kind::Image if !e.path.is_empty() => Some(PathBuf::from(&e.path)),
            Kind::Files => e.text.lines().next().map(PathBuf::from),
            _ => None,
        }
    }

    fn commands(&self) -> Vec<socle::ui::Command> {
        use socle::ui::Command;
        let e = self.selected();
        let text = e.is_some_and(|e| e.kind == Kind::Text);
        let mut v = vec![
            Command::new("copier", "Copier l'entrée").group("Entrée").keys("Entrée").icon(Icon::Copy).enabled(e.is_some()),
            Command::new("epingler", "Épingler / désépingler").group("Entrée").keys("Ctrl+P").icon(Icon::Pin).enabled(e.is_some()),
            Command::new("supprimer", "Supprimer l'entrée").group("Entrée").keys("Suppr").icon(Icon::Trash).enabled(e.is_some()),
            Command::new("reveler", "Révéler le secret").group("Entrée").keys("Ctrl+R").icon(Icon::Eye).enabled(e.is_some_and(|e| e.secret)),
            Command::new("ocr", "Copier le texte de l'image").group("Entrée").keys("Ctrl+Maj+C").icon(Icon::TextFile).enabled(e.is_some_and(|e| e.kind == Kind::Image && !e.text.trim().is_empty())),
            Command::new("explorer", "Afficher dans l'explorateur").group("Entrée").keys("Ctrl+E").icon(Icon::Folder).enabled(e.is_some_and(|e| Self::file_of(e).is_some())),
            Command::new("premier_plan", if self.on_top { "Ne plus garder au premier plan" } else { "Garder la fenêtre au premier plan" }).group("Fenêtre").keys("Ctrl+T").icon(Icon::Pin),
            Command::new("fermer_copie", if self.s.fermer_apres_copie { "Garder la fenêtre ouverte après une copie" } else { "Fermer la fenêtre après une copie" }).group("Mémoire").icon(Icon::Settings),
            Command::new("capture", "Nouvelle capture").group("Capture").keys(self.s.raccourci_capture.clone()).icon(Icon::Camera),
            Command::new("pause", if self.paused { "Reprendre l'enregistrement" } else { "Mettre l'enregistrement en pause" }).group("Mémoire").icon(Icon::Pause),
            Command::new("theme", "Basculer clair / sombre").group("Mémoire").icon(if self.pal.dark { Icon::Sun } else { Icon::Moon }),
            Command::new("reglages", "Ouvrir le fichier de réglages").group("Mémoire").icon(Icon::Settings),
            Command::new("demarrage", if self.s.demarrage_windows { "Ne plus démarrer avec Windows" } else { "Démarrer avec Windows" }).group("Mémoire").icon(Icon::Power),
            Command::new("quitter", "Quitter Mémoire").group("Mémoire").icon(Icon::Close),
        ];
        for t in transform::Transform::ALL {
            v.push(Command::new(format!("tf:{t:?}"), format!("Copier en : {}", t.label())).group("Coller transformé").icon(Icon::Convert).enabled(text));
        }
        v
    }

    fn run_command(&mut self, id: &str) {
        let sel = self.selected().map(|e| e.id);
        match id {
            "copier" => {
                let ids = self.targets();
                if !ids.is_empty() {
                    self.copy_many(&ids)
                }
            }
            "ocr" => {
                if let Some(e) = self.selected().filter(|e| e.kind == Kind::Image && !e.text.trim().is_empty()) {
                    let t = e.text.clone();
                    match clip::write(self.hwnd as _, &Data::Text(t)) {
                        Ok(()) => self.copied(),
                        Err(m) => self.toast(m, true),
                    }
                }
            }
            "explorer" => {
                if let Some(p) = self.selected().and_then(Self::file_of) {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
                }
            }
            "premier_plan" => {
                self.on_top = !self.on_top;
                self.out.topmost = Some(self.on_top);
                self.toast(if self.on_top { "Au premier plan : la fenêtre reste ouverte" } else { "Premier plan désactivé" }, false);
            }
            "fermer_copie" => {
                self.s.fermer_apres_copie = !self.s.fermer_apres_copie;
                self.saver.touch();
                self.toast(if self.s.fermer_apres_copie { "La fenêtre se fermera après une copie" } else { "La fenêtre reste ouverte après une copie" }, false);
            }
            "epingler" => {
                if let (Some(i), Some(st)) = (sel, self.store.as_mut()) {
                    st.update(i, false, |e| e.pinned = !e.pinned);
                }
            }
            "supprimer" => {
                let ids = self.targets();
                if let Some(st) = self.store.as_mut() {
                    for i in ids {
                        st.delete(i);
                    }
                }
                self.multi.clear();
            }
            "reveler" => {
                if let Some(i) = sel {
                    if !self.revealed.remove(&i) {
                        self.revealed.insert(i);
                    }
                }
            }
            "capture" => self.out.capture = true,
            "pause" => {
                self.paused = !self.paused;
                self.toast(if self.paused { "Enregistrement en pause" } else { "Enregistrement repris" }, false);
            }
            "theme" => {
                self.s.sombre = Some(!self.pal.dark);
                self.pal = theme::palette(!self.pal.dark);
                CTX.with(|c| c.borrow().as_ref().map(|ctx| theme::apply(ctx, &self.pal)));
                self.saver.touch();
            }
            "reglages" => {
                let _ = socle::settings::save("Memoire", &self.s);
                let p = socle::settings::path("Memoire");
                let _ = std::process::Command::new("explorer.exe").arg(p).spawn();
            }
            "demarrage" => {
                self.s.demarrage_windows = !self.s.demarrage_windows;
                crate::autostart(self.s.demarrage_windows);
                self.saver.touch();
            }
            "quitter" => self.out.quit = true,
            t if t.starts_with("tf:") => {
                let tf = transform::Transform::ALL.into_iter().find(|x| format!("tf:{x:?}") == t);
                if let (Some(i), Some(tf)) = (sel, tf) {
                    self.copy_entry(i, Some(tf));
                }
            }
            _ => {}
        }
    }

    // ---------- dessin ----------

    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        if let Some(d) = self.saver.tick("Memoire", &self.s) {
            ctx.request_repaint_after(d);
        }
        if let Some(e) = self.saver.take_error() {
            self.toast(e, true);
        }
        if self.editor.is_some() {
            self.draw_editor(ui);
        } else {
            self.draw_history(ui);
        }
    }

    fn draw_history(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        self.refresh();
        let cmds = self.commands();
        if self.palette.handle_shortcut(ctx) {
            self.focus_search = false;
        }
        if let Some(id) = self.palette.show(ctx, &self.pal, &cmds) {
            self.run_command(&id);
            self.focus_search = true;
        }
        socle::ui::help_window(ctx, &self.pal, &mut self.help, &cmds);
        let modal = self.palette.is_open() || self.help;
        if !modal {
            self.keys(ctx);
        }
        let pal = self.pal.clone();
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(pal.bg).inner_margin(10)).show(root, |ui| {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 400.0).max(200.0);
                let r = socle::ui::search_field(ui, &pal, egui::Id::unique("recherche"), &mut self.query, "Chercher dans l'historique (texte et images)…", w);
                if self.focus_search && !modal {
                    r.request_focus();
                    self.focus_search = false;
                }
                ui.add_space(8.0);
                socle::ui::segmented(ui, &pal, &mut self.filter, &FILTERS.map(|f| (None, f)));
            });
            ui.add_space(8.0);
            let avail = ui.available_size();
            let list_w = (avail.x * 0.45).clamp(260.0, 520.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(list_w, avail.y - 26.0), egui::Layout::top_down(egui::Align::Min), |ui| self.draw_list(ui));
                ui.add_space(10.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), avail.y - 26.0), egui::Layout::top_down(egui::Align::Min), |ui| self.draw_preview(ui));
            });
            ui.horizontal(|ui| {
                let n = self.store.as_ref().map_or(0, |s| s.entries.len());
                let sel = if self.multi.len() > 1 { format!("  ·  {} sélectionnées", self.multi.len()) } else { String::new() };
                let msg = format!("{} entrées{sel}{}  ·  Entrée copier  ·  glisser vers une app  ·  Ctrl+K commandes  ·  F1 aide", n, if self.paused { "  ·  EN PAUSE" } else { "" });
                ui.label(egui::RichText::new(msg).size(taille::ETAT).color(pal.muted));
                self.toasts.ui(ui, &pal);
            });
        });
    }

    fn keys(&mut self, ctx: &egui::Context) {
        let n = self.results.len();
        let (down, up, enter, esc, del, pgd, pgu) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::Delete),
                i.consume_key(Modifiers::NONE, Key::PageDown),
                i.consume_key(Modifiers::NONE, Key::PageUp),
            )
        });
        if down && self.sel + 1 < n {
            self.sel += 1;
            self.scroll_to_sel = true;
        }
        if up && self.sel > 0 {
            self.sel -= 1;
            self.scroll_to_sel = true;
        }
        if pgd {
            self.sel = (self.sel + 10).min(n.saturating_sub(1));
            self.scroll_to_sel = true;
        }
        if pgu {
            self.sel = self.sel.saturating_sub(10);
            self.scroll_to_sel = true;
        }
        if enter {
            self.run_command("copier");
        }
        if esc {
            if self.multi.is_empty() { self.out.hide = true } else { self.multi.clear() }
        }
        // Alt+1…9 : copier la n-ième entrée affichée
        let nums = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        if let Some(k) = nums.iter().position(|k| ctx.input_mut(|i| i.consume_key(Modifiers::ALT, *k))) {
            if let Some(&id) = self.results.get(k) {
                self.sel = k;
                self.multi.clear();
                self.copy_entry(id, None);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::C)) {
            self.run_command("ocr");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::E)) {
            self.run_command("explorer");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::T)) {
            self.run_command("premier_plan");
        }
        if del && self.query.is_empty() {
            self.run_command("supprimer");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::P)) {
            self.run_command("epingler");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::R)) {
            self.run_command("reveler");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.help = true;
        }
    }

    fn draw_list(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal.clone();
        if self.store.is_none() {
            return;
        }
        let row_h = 46.0;
        let mut click = None;
        let mut dbl = None;
        let mut drag = None;
        let mods = ui.input(|i| i.modifiers);
        self.rows.clear();
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("liste");
        if self.scroll_to_sel {
            let vis = ui.available_height();
            let y = self.sel as f32 * row_h;
            let cur = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::unique("liste_off"))).unwrap_or(0.0);
            let off = if y < cur { y } else if y + row_h > cur + vis { y + row_h - vis } else { cur };
            sa = sa.vertical_scroll_offset(off);
            self.scroll_to_sel = false;
        }
        let out = sa.show_rows(ui, row_h, self.results.len(), |ui, range| {
            for i in range {
                let Some(e) = self.store.as_ref().and_then(|st| st.get(self.results[i])).cloned() else { continue };
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                let p = ui.painter();
                if i == self.sel || self.multi.contains(&e.id) {
                    p.rect_filled(rect, 8, pal.sel_bg);
                } else if r.hovered() {
                    p.rect_filled(rect, 8, pal.hover_bg);
                }
                let icon = match e.kind {
                    _ if e.secret => Icon::Eye,
                    Kind::Text => Icon::TextFile,
                    Kind::Image => Icon::Image,
                    Kind::Files => Icon::Folder,
                };
                let slot = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(30.0, 30.0));
                let thumb = if e.kind == Kind::Image {
                    if !self.thumbs.contains_key(&e.id) {
                        self.thumbs.insert(e.id, None);
                        let _ = self.thumb_tx.send(e.id);
                    }
                    self.thumbs.get(&e.id).cloned().flatten()
                } else {
                    None
                };
                match &thumb {
                    Some(t) => {
                        let s = t.size_vec2();
                        let k = (slot.width() / s.x).min(slot.height() / s.y);
                        p.image(t.id(), Rect::from_center_size(slot.center(), s * k), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => icons::paint(p, slot.shrink(6.0), icon, pal.muted),
                }
                // aperçu agrandi au survol, instantané (miniature déjà chargée pour les lignes visibles)
                if let (Some(t), true) = (&thumb, r.hovered()) {
                    let s = t.size_vec2();
                    let k = (360.0 / s.x).min(360.0 / s.y).min(1.0);
                    let at = pos2(rect.right() + 8.0, rect.top());
                    egui::Area::new(egui::Id::unique(("survol", e.id))).order(egui::Order::Tooltip).fixed_pos(at).fade_in(false).interactable(false).show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).inner_margin(4).show(ui, |ui| {
                            ui.image((t.id(), s * k));
                        });
                    });
                }
                // texte long : aperçu au survol, instantané
                let long = e.kind != Kind::Image && (!e.secret || self.revealed.contains(&e.id)) && (e.text.len() > 70 || e.text.trim().contains('\n'));
                if long && r.hovered() && !r.dragged() {
                    let at = pos2(rect.right() + 8.0, rect.top());
                    let shown: String = e.text.chars().take(1500).collect();
                    egui::Area::new(egui::Id::unique(("survol_texte", e.id))).order(egui::Order::Tooltip).fixed_pos(at).fade_in(false).interactable(false).show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).inner_margin(8).show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.add(egui::Label::new(egui::RichText::new(shown).monospace().size(12.0)).wrap_mode(egui::TextWrapMode::Wrap));
                        });
                    });
                }
                if r.drag_started() {
                    drag = Some(e.id);
                }
                if i < 9 {
                    let n = Rect::from_min_size(pos2(rect.right() - 34.0, rect.top() + 4.0), vec2(14.0, 14.0));
                    socle::ui::paint_text_left(ui, n, n.left(), &format!("{}", i + 1), FontId::proportional(taille::META), pal.muted.gamma_multiply(0.7), 14.0);
                }
                self.rows.push(rect);
                let title = match e.kind {
                    _ if e.secret && !self.revealed.contains(&e.id) => "••••••••••••  (secret masqué)".to_string(),
                    Kind::Image if e.text.trim().is_empty() => format!("Image {} × {}", e.w, e.h),
                    Kind::Image => format!("Image : {}", e.text.split_whitespace().collect::<Vec<_>>().join(" ")),
                    _ => e.text.split_whitespace().collect::<Vec<_>>().join(" "),
                };
                let tw = rect.width() - 64.0;
                let font = FontId::proportional(taille::TEXTE);
                let g = if self.query.is_empty() || e.secret { socle::ui::trunc(ui, &title, font, pal.text, tw) } else { socle::ui::highlighted(ui, &title, &self.query, font, pal.text, pal.accent, tw) };
                ui.painter().galley(pos2(rect.left() + 36.0, rect.top() + 6.0), g, pal.text);
                let meta = format!("{}  ·  {}", socle::ui::ago((now() - e.t) as f32), if e.src.is_empty() { "?" } else { &e.src });
                socle::ui::paint_text_left(ui, Rect::from_min_max(pos2(rect.left(), rect.top() + 26.0), rect.max), rect.left() + 36.0, &meta, FontId::proportional(taille::META), pal.muted, tw);
                if e.pinned {
                    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(16.0, 16.0)), Icon::Pin, pal.accent);
                }
                if r.clicked() {
                    click = Some(i);
                }
                if r.double_clicked() {
                    dbl = Some(i);
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("liste_off"), out.state.clamped_offset().y));
        if let Some(i) = click {
            let id = self.results[i];
            if mods.command {
                if self.multi.is_empty() {
                    if let Some(&cur) = self.results.get(self.sel) {
                        self.multi.push(cur);
                    }
                }
                if let Some(k) = self.multi.iter().position(|x| *x == id) {
                    self.multi.remove(k);
                } else {
                    self.multi.push(id);
                }
            } else if mods.shift {
                let (a, b) = (self.sel.min(i), self.sel.max(i));
                self.multi = self.results[a..=b].to_vec();
            } else {
                self.multi.clear();
            }
            self.sel = i;
        }
        if let Some(id) = drag {
            let ids = if self.multi.contains(&id) { self.multi.clone() } else { vec![id] };
            self.start_drag(&ids);
        }
        if let Some(i) = dbl {
            self.sel = i;
            self.run_command("copier");
        }
        if self.results.is_empty() {
            ui.label(egui::RichText::new(if self.query.is_empty() { "Rien encore : copiez quelque chose." } else { "Aucun résultat." }).color(pal.muted));
        }
    }

    fn draw_preview(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal.clone();
        let Some(e) = self.selected().cloned() else { return };
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 12, pal.surface);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(12.0)).layout(egui::Layout::top_down(egui::Align::Min)));
        ui.horizontal(|ui| {
            let look = Look::tool(&pal);
            if icons::button(ui, Icon::Copy, 32.0, look, if self.multi.len() > 1 { "Copier la sélection (Entrée)" } else { "Copier (Entrée)" }).clicked() {
                self.run_command("copier");
            }
            if icons::button(ui, Icon::Pin, 32.0, if e.pinned { Look::active(&pal) } else { look }, "Épingler (Ctrl+P)").clicked() {
                self.run_command("epingler");
            }
            if e.secret && icons::button(ui, Icon::Eye, 32.0, look, "Révéler (Ctrl+R)").clicked() {
                self.run_command("reveler");
            }
            if e.kind == Kind::Image && !e.text.trim().is_empty() && icons::button(ui, Icon::TextFile, 32.0, look, "Copier le texte de l'image (Ctrl+Maj+C)").clicked() {
                self.run_command("ocr");
            }
            if Self::file_of(&e).is_some() && icons::button(ui, Icon::Folder, 32.0, look, "Afficher dans l'explorateur (Ctrl+E)").clicked() {
                self.run_command("explorer");
            }
            if icons::button(ui, Icon::Pin, 32.0, if self.on_top { Look::active(&pal) } else { look }, "Garder au premier plan (Ctrl+T)").clicked() {
                self.run_command("premier_plan");
            }
            if e.kind == Kind::Text && !e.secret {
                ui.menu_button("Copier en…", |ui| {
                    for t in transform::Transform::ALL {
                        if ui.button(t.label()).clicked() {
                            self.copy_entry(e.id, Some(t));
                            ui.close();
                        }
                    }
                });
            }
            if icons::button(ui, Icon::Trash, 32.0, look, "Supprimer (Suppr)").clicked() {
                self.run_command("supprimer");
            }
        });
        let when = chrono::DateTime::from_timestamp(e.created, 0).map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y %H:%M").to_string()).unwrap_or_default();
        ui.label(egui::RichText::new(format!("{when}  ·  {}  ·  {}", if e.src.is_empty() { "?" } else { &e.src }, human(e.size))).size(taille::META).color(pal.muted));
        ui.add_space(6.0);
        match e.kind {
            Kind::Image => {
                if self.preview.as_ref().is_none_or(|p| p.0 != e.id) && self.preview_wanted != e.id {
                    self.preview_wanted = e.id;
                    self.preview = None;
                    let (dir, wake, id) = (self.dir.clone(), self.wake.clone(), e.id);
                    std::thread::spawn(move || {
                        if let Ok(st) = crate::store::ImageReader::new(&dir) {
                            if let Ok((rgba, w, h)) = st.load(id) {
                                wake(Job::Preview(id, rgba, w, h));
                            }
                        }
                    });
                }
                let avail = ui.available_size() - vec2(0.0, if e.text.is_empty() { 0.0 } else { 110.0 });
                if let Some((_, tex)) = self.preview.as_ref().filter(|p| p.0 == e.id) {
                    let s = tex.size_vec2();
                    let k = (avail.x / s.x).min(avail.y / s.y).min(1.0);
                    ui.image((tex.id(), s * k));
                } else {
                    ui.allocate_space(vec2(avail.x, avail.y.min(200.0)));
                }
                if !e.text.is_empty() {
                    ui.label(egui::RichText::new("TEXTE RECONNU").size(taille::ENTETE).color(pal.muted));
                    egui::ScrollArea::vertical().id_salt("ocr").max_height(90.0).show(&mut ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&e.text).monospace()).selectable(true));
                    });
                } else if !e.ocr_done && self.s.ocr {
                    ui.label(egui::RichText::new("Lecture du texte en cours…").color(pal.muted));
                }
            }
            _ if e.secret && !self.revealed.contains(&e.id) => {
                ui.label(egui::RichText::new("•••••••••••••••• — secret masqué. Ctrl+R ou l'œil pour l'afficher.").color(pal.muted));
            }
            _ => {
                egui::ScrollArea::both().id_salt("texte").auto_shrink([false, false]).show(&mut ui, |ui| {
                    let shown: String = e.text.chars().take(100_000).collect();
                    ui.add(egui::Label::new(egui::RichText::new(shown).monospace().size(13.0)).selectable(true).wrap_mode(egui::TextWrapMode::Wrap));
                });
            }
        }
    }

    fn draw_editor(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        let pal = self.pal.clone();
        let (mut copy, mut save, mut close, mut undo) = ctx.input_mut(|i| {
            (i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::COMMAND, Key::C), i.consume_key(Modifiers::COMMAND, Key::S), i.consume_key(Modifiers::NONE, Key::Escape), i.consume_key(Modifiers::COMMAND, Key::Z))
        });
        let Some(ed) = self.editor.as_mut() else { return };
        ctx.input(|i| {
            if i.key_pressed(Key::A) {
                ed.tool = Tool::Arrow
            }
            if i.key_pressed(Key::R) {
                ed.tool = Tool::Rect
            }
            if i.key_pressed(Key::B) {
                ed.tool = Tool::Blur
            }
        });
        let mut changed = false;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(pal.bg).inner_margin(8)).show(root, |ui| {
            ui.horizontal(|ui| {
                for (t, ic, tip) in [(Tool::Arrow, Icon::ArrowRight, "Flèche (A)"), (Tool::Rect, Icon::Crop, "Cadre (R)"), (Tool::Blur, Icon::Filter, "Flou (B)")] {
                    let look = if ed.tool == t { Look::active(&pal) } else { Look::tool(&pal) };
                    if icons::button(ui, ic, 32.0, look, tip).clicked() {
                        ed.tool = t;
                    }
                }
                ui.add_space(8.0);
                undo |= icons::button(ui, Icon::Undo, 32.0, Look::tool(&pal), "Annuler (Ctrl+Z)").clicked();
                ui.add_space(8.0);
                copy |= icons::button(ui, Icon::Copy, 32.0, Look::tool(&pal), "Copier (Entrée)").clicked();
                save |= icons::button(ui, Icon::Save, 32.0, Look::tool(&pal), "Enregistrer en PNG et copier (Ctrl+S)").clicked();
                close |= icons::button(ui, Icon::Close, 32.0, Look::tool(&pal), "Fermer sans garder (Échap)").clicked();
                ui.label(egui::RichText::new(format!("{} × {}", ed.w, ed.h)).size(taille::META).color(pal.muted));
            });
            ui.add_space(6.0);
            let avail = ui.available_size();
            let (iw, ih) = (ed.w as f32, ed.h as f32);
            let k = (avail.x / iw).min(avail.y / ih).min(1.0 / ui.ctx().pixels_per_point());
            let (rect, r) = ui.allocate_exact_size(vec2(iw * k, ih * k), Sense::drag());
            ui.painter().image(ed.tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let to_img = |p: egui::Pos2| (((p.x - rect.left()) / k).clamp(0.0, iw), ((p.y - rect.top()) / k).clamp(0.0, ih));
            if r.drag_started() {
                ed.drag = r.interact_pointer_pos().map(to_img);
            }
            if let (Some(a), Some(p)) = (ed.drag, ui.ctx().pointer_latest_pos()) {
                let b = to_img(p);
                let sp = |q: (f32, f32)| pos2(rect.left() + q.0 * k, rect.top() + q.1 * k);
                let st = Stroke::new(3.0, Color32::from_rgb(229, 59, 59));
                match ed.tool {
                    Tool::Arrow => {
                        ui.painter().arrow(sp(a), sp(b) - sp(a), st);
                    }
                    Tool::Rect => {
                        ui.painter().rect_stroke(Rect::from_two_pos(sp(a), sp(b)), 0, st, egui::StrokeKind::Middle);
                    }
                    Tool::Blur => {
                        ui.painter().rect_filled(Rect::from_two_pos(sp(a), sp(b)), 0, Color32::from_gray(128).gamma_multiply(0.6));
                    }
                }
                if r.drag_stopped() {
                    if (a.0 - b.0).abs() + (a.1 - b.1).abs() > 3.0 {
                        ed.marks.push(Mark { tool: ed.tool, a, b });
                        changed = true;
                    }
                    ed.drag = None;
                }
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        });
        if undo && ed.marks.pop().is_some() {
            changed = true;
        }
        if changed {
            let img = annotate::render(&ed.base, ed.w, ed.h, &ed.marks);
            ed.tex.set(egui::ColorImage::from_rgba_unmultiplied([ed.w as usize, ed.h as usize], &img), egui::TextureOptions::LINEAR);
        }
        if close {
            self.editor = None;
            self.out.hide = true;
        } else if copy || save {
            self.finish_capture(save);
        }
    }

    /// Copie l'image annotée, la garde dans l'historique (OCR en fond) et l'enregistre si demandé.
    fn finish_capture(&mut self, save: bool) {
        let Some(ed) = self.editor.take() else { return };
        let img = annotate::render(&ed.base, ed.w, ed.h, &ed.marks);
        let data = Data::Image { rgba: img, w: ed.w, h: ed.h };
        if let Err(m) = clip::write(self.hwnd as _, &data) {
            self.toast(m, true);
        }
        let Data::Image { rgba, w, h } = data else { return };
        let mut saved = String::new();
        if save {
            let dir = self.s.dossier_captures.clone().map(PathBuf::from).unwrap_or_else(captures_dir);
            let name = format!("Capture {}.png", chrono::Local::now().format("%Y-%m-%d %H.%M.%S"));
            let p = dir.join(name);
            saved = p.to_string_lossy().into_owned();
            let (rgba2, wake) = (rgba.clone(), self.wake.clone());
            std::thread::spawn(move || {
                let _ = std::fs::create_dir_all(&dir);
                match image::save_buffer(&p, &rgba2, w, h, image::ColorType::Rgba8) {
                    Ok(()) => wake(Job::Info(format!("Enregistrée : {}", p.display()), false)),
                    Err(e) => wake(Job::Info(format!("Enregistrement impossible : {e}"), true)),
                }
            });
        }
        if let Some(st) = self.store.as_mut() {
            let t = now();
            let e = Entry { kind: Kind::Image, t, created: t, src: "Capture".into(), w, h, path: saved, hash: store::hash(&rgba) ^ 1, ..Default::default() };
            let (id, new) = st.add(e);
            if new {
                store::count(&self.dir, true);
                let _ = self.ocr_tx.send((id, Some((rgba, w, h))));
            }
        }
        self.out.hide = true;
    }
}

thread_local! {
    static CTX: std::cell::RefCell<Option<egui::Context>> = const { std::cell::RefCell::new(None) };
}

fn human(b: u64) -> String {
    match b {
        0 => String::new(),
        b if b < 1024 => format!("{b} o"),
        b if b < 1 << 20 => format!("{:.0} Ko", b as f64 / 1024.0),
        b => format!("{:.1} Mo", b as f64 / 1048576.0),
    }
}

fn captures_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    home.join("Pictures").join("Captures")
}

/// Thread de fond en priorité basse : enregistre les images (PNG chiffré) puis lit leur texte (OCR).
/// `None` en image = relire depuis le disque (OCR resté à faire).
fn spawn_worker(dir: &std::path::Path, wake: Wake, ocr: bool) -> std::sync::mpsc::Sender<(u64, Option<(Vec<u8>, u32, u32)>)> {
    let (tx, rx) = std::sync::mpsc::channel::<(u64, Option<(Vec<u8>, u32, u32)>)>();
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("ocr".into())
        .spawn(move || {
            crate::low_thread_priority();
            let Ok(rd) = store::ImageReader::new(&dir) else { return };
            while let Ok((id, img)) = rx.recv() {
                let img = match img {
                    Some((rgba, w, h)) => {
                        match rd.save(id, &rgba, w, h) {
                            Ok(size) => {
                                let _ = rd.thumb_from(id, &rgba, w, h);
                                wake(Job::Saved(id, size))
                            }
                            Err(e) => wake(Job::Info(format!("Image non gardée : {e}"), true)),
                        }
                        Some((rgba, w, h))
                    }
                    None => rd.load(id).ok(),
                };
                if let (true, Some((rgba, w, h))) = (ocr, img) {
                    let text = std::panic::catch_unwind(|| crate::ocr::recognize(&rgba, w, h)).ok().and_then(|r| r.ok()).unwrap_or_default();
                    wake(Job::Ocr(id, text));
                }
            }
        })
        .ok();
    tx
}

/// Miniatures (360 px, chiffrées sur le disque) : chargées à la demande pour les lignes visibles.
fn spawn_thumbs(dir: &std::path::Path, wake: Wake) -> std::sync::mpsc::Sender<u64> {
    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("miniatures".into())
        .spawn(move || {
            let Ok(rd) = store::ImageReader::new(&dir) else { return };
            while let Ok(id) = rx.recv() {
                if let Ok((rgba, w, h)) = rd.thumb(id) {
                    wake(Job::Thumb(id, rgba, w, h));
                }
            }
        })
        .ok();
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bug de la v0.1.0 : les lignes de l'historique se plaçaient côte à côte (mise en page horizontale
    /// héritée) : on ne voyait qu'une entrée, chaque copie semblait remplacer la précédente.
    #[test]
    fn historique_en_colonne() {
        let dir = store::tests::tmp("ui");
        let ctx = egui::Context::default();
        let wake: Wake = Arc::new(|_| {});
        let mut a = App::new(&ctx, Settings { ocr: false, ..Default::default() }, dir, wake);
        for s in ["un", "deux", "trois"] {
            a.store.as_mut().unwrap().add(Entry { text: s.into(), hash: store::hash(s.as_bytes()), t: now(), ..Default::default() });
        }
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(980.0, 620.0))), ..Default::default() };
            let _ = ctx.run_ui(raw, |ui| a.frame(ui));
        }
        assert_eq!(a.rows.len(), 3, "trois lignes visibles");
        assert!(a.rows.windows(2).all(|w| w[1].top() >= w[0].bottom() - 0.5 && (w[1].left() - w[0].left()).abs() < 0.5), "lignes empilées : {:?}", a.rows);
        assert!(a.rows.iter().all(|r| r.right() <= 980.0), "lignes dans la fenêtre");
    }
}

#[cfg(test)]
mod tests_v2 {
    use super::*;

    #[test]
    fn liens_reconnus() {
        assert!(is_link("https://exemple.fr/a?b=1"));
        assert!(is_link("  www.exemple.fr "));
        assert!(!is_link("voir https://exemple.fr"));
        assert!(!is_link("bonjour"));
    }

    #[test]
    fn dropfiles_format() {
        let b = crate::drag::dropfiles(&[PathBuf::from(r"C:\a.png"), PathBuf::from(r"C:\b c.png")]);
        assert_eq!(&b[..4], &20i32.to_le_bytes());
        assert_eq!(&b[16..20], &1i32.to_le_bytes(), "chemins en UTF-16");
        assert_eq!(b.len(), 20 + (8 + 1 + 10 + 1 + 1) * 2);
        assert!(b.ends_with(&[0, 0, 0, 0]));
    }

    /// Multi-sélection : textes fusionnés dans l'ordre du clic, images exportées en PNG puis effacées au redémarrage.
    #[test]
    fn selection_multiple() {
        let dir = store::tests::tmp("multi");
        let wake: Wake = Arc::new(|_| {});
        let ctx = egui::Context::default();
        let mut a = App::new(&ctx, Settings { ocr: false, ..Default::default() }, dir.clone(), wake.clone());
        let st = a.store.as_mut().unwrap();
        let (x, _) = st.add(Entry { text: "un".into(), hash: 1, t: now(), ..Default::default() });
        let (y, _) = st.add(Entry { text: "deux".into(), hash: 2, t: now(), ..Default::default() });
        let (text, files) = a.gather(&[y, x]).unwrap();
        assert_eq!(text.as_deref(), Some("deux\nun"));
        assert!(files.is_empty());
        // image : PNG lisible écrit sous glisser\
        let rd = store::ImageReader::new(&dir).unwrap();
        let st = a.store.as_mut().unwrap();
        let (im, _) = st.add(Entry { kind: Kind::Image, w: 2, h: 2, hash: 3, t: now(), ..Default::default() });
        rd.save(im, &[255u8; 16], 2, 2).unwrap();
        let (_, files) = a.gather(&[im]).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].starts_with(dir.join("glisser")) && image::open(&files[0]).is_ok());
        drop(a);
        let _a = App::new(&ctx, Settings { ocr: false, ..Default::default() }, dir.clone(), wake);
        assert!(!dir.join("glisser").exists(), "fichiers temporaires nettoyés");
    }
}
