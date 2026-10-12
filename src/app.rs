//! The application: open documents, commands, event plumbing and the window layout.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{Key, KeyboardShortcut, Modifiers, Pos2, Rect};

use crate::document::{Document, Fit, Kind, LoadState};
use crate::imaging::{self, ExportFormat};
use crate::model::{Annot, MarkupKind, PageSrc, Slot, Style};
use crate::pdf::{Engine, Event, LinkInfo, PageText, SearchHit, Source, SourceInfo};
use crate::render::{ImageTex, Tiles};
use crate::settings::{PdfViewMode, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeTool {
    Line,
    Arrow,
    Rect,
    RoundRect,
    Oval,
    Star,
    Bubble,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Text,
    Rect,
    Sketch,
    Shape(ShapeTool),
    Polygon,
    TextBox,
    Note,
    Markup(MarkupKind),
    Signature(usize),
    Redact,
    InstantAlpha,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    Thumbnails,
    Contents,
    Notes,
    Bookmarks,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectorTab {
    Info,
    Details,
    Annotations,
    Security,
}

#[derive(Clone, Debug)]
pub struct TextSel {
    pub doc: u64,
    pub slot: usize,
    pub a: usize,
    pub b: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub text: Option<TextSel>,
    /// Rectangular selection: (doc, slot, page-space rect)
    pub rect: Option<(u64, usize, Rect)>,
    /// Selected annotation: (doc, slot, annotation id)
    pub annot: Option<(u64, usize, u64)>,
    /// Text box / note being edited.
    pub editing: Option<u64>,
    /// Form field being edited: (doc, xref)
    pub field: Option<(u64, i32)>,
}

pub struct AlphaSel {
    pub doc: u64,
    pub slot: usize,
    pub mask: Arc<Vec<bool>>,
    pub tex: egui::TextureHandle,
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub generation: u64,
    pub doc: u64,
    pub hits: Vec<SearchHit>,
    pub current: Option<usize>,
    pub running: bool,
    pub focus: bool,
}

pub enum Msg {
    ImageLoaded { doc: u64, result: Result<imaging::Loaded, String> },
    Opened(Vec<PathBuf>),
    Chosen { purpose: Choose, paths: Vec<PathBuf> },
    ImageSaved { doc: u64, path: PathBuf, result: Result<(), String>, reload: bool },
    Info(String),
    Error(String),
    NewImage(image::RgbaImage, String),
    Ocr(crate::ocr::OcrResult),
    Scanned { insert: Option<(u64, usize)>, dpi: u32, result: Result<image::RgbaImage, String> },
    ModelLoaded { doc: u64, result: Result<crate::model3d::Scene, String> },
}

#[derive(Clone, Debug)]
pub enum Choose {
    Open,
    InsertPages { doc: u64, at: usize },
}

pub struct ExportOpts {
    pub format: ExportFormat,
    pub quality: u8,
    pub alpha: bool,
    pub security: Option<crate::model::Security>,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// What to do with a page image the engine renders for us.
pub enum PageUse {
    /// Copy (part of) a rendered page: crop rect in pixels of the rendered image.
    CopyImage(Option<Rect>),
    /// Live Text on a scanned page rendered at `scale`.
    Ocr { source: u64, page: u32, scale: f32 },
}

pub struct App {
    /// GPU access for the 3D viewer.
    pub wgpu: Option<eframe::egui_wgpu::RenderState>,
    pub viewers: HashMap<u64, crate::view3d::Viewer>,
    image_jobs: Sender<(u64, PathBuf)>,
    pub engine: Engine,
    events: Receiver<Event>,
    pub msg_tx: Sender<Msg>,
    msg_rx: Receiver<Msg>,
    pub sources: HashMap<u64, Source>,
    pub source_info: HashMap<u64, Arc<SourceInfo>>,
    pub docs: Vec<Document>,
    pub active: usize,
    pub settings: Settings,
    pub tiles: Tiles,
    pub images: HashMap<(u64, usize), ImageTex>,
    pub texts: HashMap<(u64, u32), Arc<PageText>>,
    text_req: HashSet<(u64, u32)>,
    pub links: HashMap<(u64, u32), Vec<LinkInfo>>,
    link_req: HashSet<(u64, u32)>,
    pub tool: Tool,
    pub shape_style: Style,
    pub text_style: Style,
    pub markup_color: egui::Color32,
    pub sel: Selection,
    pub alpha: Option<AlphaSel>,
    pub drag: crate::canvas::Drag,
    pub sidebar: SidebarMode,
    pub show_inspector: bool,
    pub inspector_tab: InspectorTab,
    pub show_markup: bool,
    pub show_toolbar: bool,
    pub highlight_fields: bool,
    pub search: SearchState,
    pub dialog: Option<crate::dialogs::Dialog>,
    pub toasts: Vec<Toast>,
    pub fullscreen: bool,
    pub slideshow: Option<crate::canvas::Slideshow>,
    pub magnifier: bool,
    pub contact_sheet: bool,
    pub annot_clipboard: Option<Annot>,
    pub page_requests: HashMap<u64, PageUse>,
    next_request: u64,
    pub ctx: egui::Context,
    pub page_badge_until: Option<Instant>,
    pub polygon: Vec<Pos2>,
    pub drag_pages: Option<(u64, Vec<usize>)>,
    quit_after_confirm: bool,
    allow_close: bool,
    pub renaming: Option<(u64, String)>,
    pub thumb_scroll_to: Option<usize>,
    /// Screen rect of each visible page in the last frame (slot index, rect).
    pub page_rects: Vec<(usize, Rect)>,
    /// Screen rect of each visible thumbnail in the last frame (doc id, slot index, rect).
    pub thumb_rects: Vec<(u64, usize, Rect)>,
    ocr_tx: Sender<crate::ocr::OcrJob>,
    ocr_req: HashSet<crate::ocr::OcrKey>,
    /// Recognised text of image frames: (doc, frame) -> (pixels pointer, text).
    pub image_texts: HashMap<(u64, usize), (usize, Arc<PageText>)>,
    /// Scanned PDF pages whose text came from Live Text.
    pub ocr_pages: HashSet<(u64, u32)>,
    /// The Live Text button is on: outline all recognised text.
    pub live_text_highlight: bool,
    /// View ▸ Soft Proof with Profile.
    pub proof: Option<Arc<crate::color::Proof>>,
    proof_gen: u64,
    started: Instant,
    shot_requested: bool,
}

pub fn ctrl() -> Modifiers {
    Modifiers::COMMAND
}

pub fn ctrl_shift() -> Modifiers {
    Modifiers::COMMAND | Modifiers::SHIFT
}

pub fn ctrl_alt() -> Modifiers {
    Modifiers::COMMAND | Modifiers::ALT
}

pub fn alt_shift() -> Modifiers {
    Modifiers::ALT | Modifiers::SHIFT
}

/// Every command in the menus, toolbar and keyboard map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Open,
    NewFromClipboard,
    Close,
    Save,
    ExportAs,
    ExportPdf,
    Print,
    Revert,
    Rename,
    Duplicate,
    MoveToTrash,
    ShowInFolder,
    EditPermissions,
    Screenshot(u8),
    ImportScanner,
    InsertFromScanner,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    InvertSelection,
    InsertBlank,
    InsertFromFile,
    Find,
    FindNext,
    FindPrev,
    UseSelForFind,
    JumpToSel,
    Speak,
    StopSpeaking,
    HideSidebar,
    Sidebar(SidebarMode),
    ContactSheet,
    ViewMode(PdfViewMode),
    DarkPdf,
    ActualSize,
    ZoomFit,
    ZoomIn,
    ZoomOut,
    ZoomSel,
    ToggleMarkup,
    ToggleToolbar,
    Slideshow,
    Fullscreen,
    PrevDoc,
    NextDoc,
    PrevPage,
    NextPage,
    FirstPage,
    LastPage,
    GoToPage,
    Back,
    Forward,
    Inspector,
    Magnifier,
    AdjustColor,
    AdjustSize,
    Tool(Tool),
    NewSignature,
    AddBookmark,
    RotateLeft,
    RotateRight,
    FlipH,
    FlipV,
    Crop,
    RemoveBackground,
    ShowLocation,
    HighlightFields,
    Preferences,
    About,
    Quit,
    Escape,
}

pub fn shortcuts() -> Vec<(KeyboardShortcut, Action)> {
    use Action::*;
    let k = KeyboardShortcut::new;
    vec![
        (k(ctrl(), Key::O), Open),
        (k(ctrl(), Key::N), NewFromClipboard),
        (k(ctrl(), Key::W), Close),
        (k(ctrl(), Key::S), Save),
        (k(ctrl_shift(), Key::S), ExportAs),
        (k(ctrl(), Key::P), Print),
        (k(ctrl(), Key::Delete), MoveToTrash),
        (k(ctrl_shift(), Key::Z), Redo),
        (k(ctrl(), Key::Y), Redo),
        (k(ctrl(), Key::Z), Undo),
        (k(ctrl(), Key::X), Cut),
        (k(ctrl(), Key::C), Copy),
        (k(ctrl(), Key::V), Paste),
        (k(ctrl(), Key::A), SelectAll),
        (k(ctrl_shift(), Key::I), InvertSelection),
        (k(ctrl(), Key::F), Find),
        (k(ctrl_shift(), Key::G), FindPrev),
        (k(ctrl(), Key::G), FindNext),
        (k(ctrl(), Key::E), UseSelForFind),
        (k(ctrl(), Key::J), JumpToSel),
        (k(ctrl_alt(), Key::Num1), HideSidebar),
        (k(ctrl_alt(), Key::Num2), Sidebar(SidebarMode::Thumbnails)),
        (k(ctrl_alt(), Key::Num3), Sidebar(SidebarMode::Contents)),
        (k(ctrl_alt(), Key::Num4), Sidebar(SidebarMode::Notes)),
        (k(ctrl_alt(), Key::Num5), Sidebar(SidebarMode::Bookmarks)),
        (k(ctrl_alt(), Key::Num6), ContactSheet),
        (k(ctrl(), Key::Num1), ViewMode(PdfViewMode::Continuous)),
        (k(ctrl(), Key::Num2), ViewMode(PdfViewMode::Single)),
        (k(ctrl(), Key::Num3), ViewMode(PdfViewMode::Two)),
        (k(ctrl(), Key::Num0), ActualSize),
        (k(ctrl(), Key::Num9), ZoomFit),
        (k(ctrl(), Key::Plus), ZoomIn),
        (k(ctrl(), Key::Equals), ZoomIn),
        (k(ctrl(), Key::Minus), ZoomOut),
        (k(ctrl_shift(), Key::Num8), ZoomSel),
        (k(ctrl_shift(), Key::A), ToggleMarkup),
        (k(ctrl_shift(), Key::F), Slideshow),
        (k(Modifiers::NONE, Key::F11), Fullscreen),
        (k(Modifiers::ALT, Key::PageUp), PrevDoc),
        (k(Modifiers::ALT, Key::PageDown), NextDoc),
        (k(Modifiers::ALT, Key::ArrowUp), PrevPage),
        (k(Modifiers::ALT, Key::ArrowDown), NextPage),
        (k(ctrl_alt(), Key::G), GoToPage),
        (k(ctrl(), Key::OpenBracket), Back),
        (k(ctrl(), Key::CloseBracket), Forward),
        (k(ctrl(), Key::I), Inspector),
        (k(Modifiers::NONE, Key::Backtick), Magnifier),
        (k(ctrl_alt(), Key::C), AdjustColor),
        (k(ctrl(), Key::D), AddBookmark),
        (k(ctrl(), Key::L), RotateLeft),
        (k(ctrl(), Key::R), RotateRight),
        (k(ctrl(), Key::K), Crop),
        (k(ctrl_shift(), Key::K), RemoveBackground),
        (k(alt_shift(), Key::H), Tool(crate::app::Tool::Markup(MarkupKind::Highlight))),
        (k(alt_shift(), Key::U), Tool(crate::app::Tool::Markup(MarkupKind::Underline))),
        (k(alt_shift(), Key::S), Tool(crate::app::Tool::Markup(MarkupKind::Strike))),
        (k(alt_shift(), Key::R), Tool(crate::app::Tool::Shape(ShapeTool::Rect))),
        (k(alt_shift(), Key::O), Tool(crate::app::Tool::Shape(ShapeTool::Oval))),
        (k(alt_shift(), Key::I), Tool(crate::app::Tool::Shape(ShapeTool::Line))),
        (k(alt_shift(), Key::A), Tool(crate::app::Tool::Shape(ShapeTool::Arrow))),
        (k(alt_shift(), Key::T), Tool(crate::app::Tool::TextBox)),
        (k(alt_shift(), Key::N), Tool(crate::app::Tool::Note)),
        (k(ctrl(), Key::Comma), Preferences),
        (k(ctrl(), Key::Q), Quit),
    ]
}

pub fn shortcut_text(a: Action) -> String {
    for (s, act) in shortcuts() {
        if act == a {
            let mut out = String::new();
            if s.modifiers.command || s.modifiers.ctrl {
                out += "Ctrl+";
            }
            if s.modifiers.alt {
                out += "Alt+";
            }
            if s.modifiers.shift {
                out += "Shift+";
            }
            let key = match s.logical_key {
                Key::Plus | Key::Equals => "+".to_string(),
                Key::Minus => "−".to_string(),
                Key::Backtick => "`".to_string(),
                Key::OpenBracket => "[".to_string(),
                Key::CloseBracket => "]".to_string(),
                Key::PageUp => "PgUp".to_string(),
                Key::PageDown => "PgDn".to_string(),
                Key::ArrowUp => "↑".to_string(),
                Key::ArrowDown => "↓".to_string(),
                Key::Delete => "Del".to_string(),
                Key::Comma => ",".to_string(),
                Key::Num8 if s.modifiers.shift => return "Ctrl+*".to_string(),
                k => k.name().trim_start_matches("Num").to_string(),
            };
            return out + &key;
        }
    }
    String::new()
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, ipc: Option<Receiver<Vec<PathBuf>>>) -> Self {
        let ctx = cc.egui_ctx.clone();
        crate::theme::install(&ctx);
        let rp = ctx.clone();
        let (engine, events) = Engine::new(Arc::new(move || rp.request_repaint()));
        let (msg_tx, msg_rx) = unbounded();
        if let Some(ipc) = ipc {
            let tx = msg_tx.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                for paths in ipc {
                    let _ = tx.send(Msg::Opened(paths));
                    c.request_repaint();
                }
            });
        }
        let settings = Settings::load();
        // A small pool decodes images so opening a folder of photos can't spawn hundreds of threads.
        let (image_jobs, jobs_rx) = unbounded::<(u64, PathBuf)>();
        for i in 0..3 {
            let rx = jobs_rx.clone();
            let tx = msg_tx.clone();
            let c = ctx.clone();
            std::thread::Builder::new()
                .name(format!("review-decode-{i}"))
                .spawn(move || {
                    for (doc, path) in rx {
                        let result = imaging::load(&path);
                        let _ = tx.send(Msg::ImageLoaded { doc, result });
                        c.request_repaint();
                    }
                })
                .expect("spawn decoder");
        }
        let ocr_tx = {
            let tx = msg_tx.clone();
            let c = ctx.clone();
            crate::ocr::spawn(move |r| {
                let _ = tx.send(Msg::Ocr(r));
                c.request_repaint();
            })
        };
        let mut app = Self {
            wgpu: cc.wgpu_render_state.clone(),
            viewers: HashMap::new(),
            ocr_tx,
            ocr_req: HashSet::new(),
            image_texts: HashMap::new(),
            ocr_pages: HashSet::new(),
            live_text_highlight: false,
            image_jobs,
            engine,
            events,
            msg_tx,
            msg_rx,
            sources: HashMap::new(),
            source_info: HashMap::new(),
            docs: Vec::new(),
            active: 0,
            tiles: Tiles::default(),
            images: HashMap::new(),
            texts: HashMap::new(),
            text_req: HashSet::new(),
            links: HashMap::new(),
            link_req: HashSet::new(),
            tool: Tool::Text,
            shape_style: Style::default(),
            text_style: Style { stroke: egui::Color32::BLACK, width: 0.0, font_size: 14.0, ..Style::default() },
            markup_color: egui::Color32::from_rgb(0xFF, 0xD6, 0x0A),
            sel: Selection::default(),
            alpha: None,
            drag: crate::canvas::Drag::None,
            sidebar: SidebarMode::Thumbnails,
            show_inspector: false,
            inspector_tab: InspectorTab::Info,
            show_markup: false,
            show_toolbar: true,
            highlight_fields: false,
            search: SearchState::default(),
            dialog: None,
            toasts: Vec::new(),
            fullscreen: false,
            slideshow: None,
            magnifier: false,
            contact_sheet: false,
            annot_clipboard: None,
            page_requests: HashMap::new(),
            next_request: 1,
            ctx: ctx.clone(),
            page_badge_until: None,
            polygon: Vec::new(),
            drag_pages: None,
            quit_after_confirm: false,
            allow_close: false,
            renaming: None,
            thumb_scroll_to: None,
            page_rects: Vec::new(),
            thumb_rects: Vec::new(),
            proof: None,
            proof_gen: 0,
            started: Instant::now(),
            shot_requested: false,
            settings,
        };
        app.open_paths(files);
        app
    }

    pub fn doc(&self) -> Option<&Document> {
        self.docs.get(self.active)
    }

    pub fn doc_mut(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.active)
    }

    pub fn doc_index(&self, id: u64) -> Option<usize> {
        self.docs.iter().position(|d| d.id == id)
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, until: Instant::now() + Duration::from_secs(3) });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, until: Instant::now() + Duration::from_secs(6) });
    }

    // ------------------------------------------------------------------
    // Opening

    pub fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut first_new = None;
        for p in expand_paths(paths) {
            let p = std::fs::canonicalize(&p).unwrap_or(p);
            if let Some(i) = self.docs.iter().position(|d| d.path.as_deref() == Some(p.as_path())) {
                first_new.get_or_insert(i);
                continue;
            }
            self.settings.add_recent(p.clone());
            let idx = self.open_one(p);
            first_new.get_or_insert(idx);
        }
        if let Some(i) = first_new {
            self.activate(i);
        }
        self.settings.save();
    }

    fn load_model(&self, doc: u64, p: PathBuf) {
        let tx = self.msg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = crate::model3d::load(&p);
            let _ = tx.send(Msg::ModelLoaded { doc, result });
            ctx.request_repaint();
        });
    }

    fn open_one(&mut self, p: PathBuf) -> usize {
        if crate::model3d::is_model_path(&p) {
            let doc = Document::new(Some(p.clone()), Kind::Model);
            self.load_model(doc.id, p);
            self.docs.push(doc);
        } else if imaging::is_image_path(&p) {
            let mut doc = Document::new(Some(p.clone()), Kind::Image);
            doc.fit = Fit::Auto;
            let _ = self.image_jobs.send((doc.id, p));
            self.docs.push(doc);
        } else {
            let mut doc = Document::new(Some(p.clone()), Kind::Pdf);
            doc.mode = self.settings.pdf_view;
            let sid = crate::model::next_id();
            self.sources.insert(sid, self.engine.open(sid, &p, None));
            doc.source = Some(sid);
            if self.settings.resume_last_page {
                if let Some(pg) = self.settings.last_page.get(&p.to_string_lossy().to_string()) {
                    doc.current = *pg as usize;
                    doc.pending_scroll = Some((*pg as usize, None));
                }
            }
            self.docs.push(doc);
        }
        self.docs.len() - 1
    }

    pub fn retry_password(&mut self, idx: usize, password: String) {
        let Some(doc) = self.docs.get_mut(idx) else { return };
        let Some(path) = doc.path.clone() else { return };
        if let Some(old) = doc.source.take() {
            self.sources.remove(&old);
        }
        let sid = crate::model::next_id();
        self.sources.insert(sid, self.engine.open(sid, &path, Some(password.clone())));
        doc.source = Some(sid);
        doc.password = Some(password);
        doc.state = LoadState::Loading;
    }

    /// Re-open a document from disk (after saving or Revert).
    pub fn reload(&mut self, idx: usize) {
        let Some(doc) = self.docs.get_mut(idx) else { return };
        let Some(path) = doc.path.clone() else { return };
        match doc.kind {
            Kind::Pdf => {
                if let Some(old) = doc.source.take() {
                    self.sources.remove(&old);
                    self.tiles.drop_source(old);
                }
                let sid = crate::model::next_id();
                self.sources.insert(sid, self.engine.open(sid, &path, doc.password.clone()));
                doc.source = Some(sid);
                doc.pending_scroll = Some((doc.current, None));
            }
            Kind::Image => {
                let _ = self.image_jobs.send((doc.id, path));
            }
            Kind::Model => {
                let id = doc.id;
                self.viewers.remove(&id);
                let (tx, ctx) = (self.msg_tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let result = crate::model3d::load(&path);
                    let _ = tx.send(Msg::ModelLoaded { doc: id, result });
                    ctx.request_repaint();
                });
            }
        }
        let doc = &mut self.docs[idx];
        doc.state = LoadState::Loading;
        let m = std::fs::metadata(doc.path.as_ref().unwrap()).ok();
        doc.file_size = m.as_ref().map(|m| m.len()).unwrap_or(doc.file_size);
        doc.modified = m.and_then(|m| m.modified().ok());
    }

    pub fn activate(&mut self, i: usize) {
        if let Some(d) = self.docs.get_mut(i) {
            d.last_active = Instant::now();
            if d.state == LoadState::Unloaded {
                if let Some(p) = d.path.clone() {
                    let _ = self.image_jobs.send((d.id, p));
                }
            }
        }
        if i < self.docs.len() && i != self.active {
            self.remember_page();
            self.active = i;
            self.sel = Selection::default();
            self.alpha = None;
            if self.search.doc != self.docs[i].id && !self.search.query.is_empty() {
                self.start_search();
            }
            if !self.docs[i].is_pdf() && self.sidebar == SidebarMode::Search {
                self.sidebar = SidebarMode::Thumbnails;
            }
            self.update_title();
        }
        self.update_title();
    }

    pub fn update_title(&self) {
        let title = match self.doc() {
            Some(d) => format!("{}{}", d.name, if d.dirty() { " — Edited" } else { "" }),
            None => "Review".into(),
        };
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn remember_page(&mut self) {
        if let Some(d) = self.docs.get(self.active) {
            if d.kind == Kind::Pdf {
                if let Some(p) = &d.path {
                    self.settings.last_page.insert(p.to_string_lossy().to_string(), d.current as u32);
                    if self.settings.last_page.len() > 500 {
                        let k = self.settings.last_page.keys().next().cloned();
                        if let Some(k) = k {
                            self.settings.last_page.remove(&k);
                        }
                    }
                }
            }
        }
    }

    pub fn close_doc(&mut self, i: usize, force: bool) {
        if i >= self.docs.len() {
            return;
        }
        if !force && self.docs[i].dirty() {
            self.dialog = Some(crate::dialogs::Dialog::ConfirmClose { docs: vec![self.docs[i].id], quit: false });
            return;
        }
        let prev = self.active;
        self.active = i;
        self.remember_page();
        self.active = prev;
        let doc = self.docs.remove(i);
        if let Some(s) = doc.source {
            // Keep the source alive if another document still shows its pages.
            let used = self.docs.iter().any(|d| d.model.slots.iter().any(|sl| matches!(sl.src, PageSrc::Pdf { source, .. } if source == s)));
            if !used {
                self.sources.remove(&s);
                self.tiles.drop_source(s);
            }
        }
        self.images.retain(|(d, _), _| *d != doc.id);
        self.viewers.remove(&doc.id);
        if self.active >= self.docs.len() {
            self.active = self.docs.len().saturating_sub(1);
        } else if self.active > i {
            self.active -= 1;
        }
        self.sel = Selection::default();
        self.settings.save();
        self.update_title();
    }

    // ------------------------------------------------------------------
    // Engine events

    fn process_events(&mut self) {
        while let Ok(e) = self.events.try_recv() {
            match e {
                Event::Opened { source, result } => {
                    let Some(i) = self.docs.iter().position(|d| d.source == Some(source)) else {
                        // A source opened for inserted pages.
                        if let Ok(info) = result {
                            self.source_info.insert(source, Arc::new(info));
                            self.finish_pending_insert(source);
                        }
                        continue;
                    };
                    let doc = &mut self.docs[i];
                    doc.pdf_opened(result, false);
                    if let Some(info) = &doc.info {
                        self.source_info.insert(source, info.clone());
                    }
                    if doc.ready() {
                        let cur = doc.current;
                        doc.pending_scroll = Some((cur, None));
                    }
                    if i == self.active {
                        self.update_title();
                    }
                }
                Event::Tile { key, image } => self.tiles.insert(&self.ctx, key, image),
                Event::Text { source, page, text } => {
                    let scanned = text.chars.is_empty();
                    self.texts.insert((source, page), text);
                    if scanned {
                        self.request_pdf_ocr(source, page);
                    }
                }
                Event::Links { source, page, links } => {
                    self.links.insert((source, page), links);
                }
                Event::SearchHits { generation, hits, .. } => {
                    if generation == self.search.generation {
                        self.search.hits.extend(hits);
                        if self.search.current.is_none() && !self.search.hits.is_empty() {
                            self.search.current = Some(0);
                            self.reveal_hit(0);
                        }
                    }
                }
                Event::SearchDone { generation, .. } => {
                    if generation == self.search.generation {
                        self.search.running = false;
                    }
                }
                Event::Saved { source, result } => {
                    let Some(i) = self.docs.iter().position(|d| d.source == Some(source)) else { continue };
                    self.docs[i].saving = false;
                    match result {
                        Ok(path) => {
                            let reload = self.docs[i].reload_after_save.take().is_some();
                            if reload {
                                self.docs[i].mark_saved(path.clone());
                                self.reload(i);
                            }
                            self.toast(format!("Saved “{}”", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
                            self.after_save_continue();
                        }
                        Err(e) => {
                            self.docs[i].reload_after_save = None;
                            self.error(format!("Couldn’t save: {e}"));
                            self.quit_after_confirm = false;
                        }
                    }
                    self.update_title();
                }
                Event::PageImage { request, result } => {
                    let Some(usage) = self.page_requests.remove(&request) else { continue };
                    match (usage, result) {
                        (PageUse::Ocr { source, page, scale }, Ok(img)) => {
                            let origin = self.source_info.get(&source).and_then(|i| i.pages.get(page as usize)).map(|p| p.0.min).unwrap_or(Pos2::ZERO);
                            let key = crate::ocr::OcrKey::Pdf { source, page };
                            let _ = self.ocr_tx.send(crate::ocr::OcrJob { key, image: Arc::new(img), scale, origin });
                        }
                        (PageUse::CopyImage(crop), Ok(img)) => {
                            let img = match crop {
                                Some(r) => imaging::crop(&img, r),
                                None => img,
                            };
                            copy_image_to_clipboard(&img, &self.msg_tx)
                        }
                        (_, Err(e)) => self.error(e),
                    }
                }
            }
        }
        while let Ok(m) = self.msg_rx.try_recv() {
            match m {
                Msg::ImageLoaded { doc, result } => {
                    if let Some(i) = self.doc_index(doc) {
                        // A reload may finish after the user edited again; never clobber edits.
                        if self.docs[i].dirty() && self.docs[i].ready() {
                            continue;
                        }
                        self.images.retain(|(d, _), _| *d != doc);
                        self.docs[i].image_loaded(result);
                        self.update_title();
                        self.enforce_memory_budget();
                    }
                }
                Msg::Opened(paths) => {
                    self.open_paths(paths);
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Msg::Chosen { purpose, paths } => self.file_chosen(purpose, paths),
                Msg::ImageSaved { doc, path, result, reload } => {
                    if let Some(i) = self.doc_index(doc) {
                        self.docs[i].saving = false;
                        match result {
                            Ok(()) => {
                                if reload {
                                    self.docs[i].mark_saved(path.clone());
                                }
                                self.toast(format!("Saved “{}”", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
                                self.after_save_continue();
                            }
                            Err(e) => {
                                self.error(format!("Couldn’t save: {e}"));
                                self.quit_after_confirm = false;
                            }
                        }
                        self.update_title();
                    }
                }
                Msg::Info(s) => self.toast(s),
                Msg::NewImage(img, name) => self.new_image_doc(img, &name),
                Msg::ModelLoaded { doc, result } => {
                    if let Some(i) = self.doc_index(doc) {
                        let d = &mut self.docs[i];
                        match result {
                            Ok(scene) => {
                                d.scene = Some(Arc::new(scene));
                                d.state = LoadState::Ready;
                            }
                            Err(e) => d.state = LoadState::Failed(e),
                        }
                        self.viewers.remove(&doc);
                        self.update_title();
                    }
                }
                Msg::Scanned { insert, dpi, result } => match (insert, result) {
                    (_, Err(e)) => self.error(format!("Scanning failed: {e}")),
                    (None, Ok(img)) => {
                        self.new_image_doc(img, "Scan");
                        if let Some(m) = self.doc_mut().and_then(|d| d.image.as_mut()) {
                            m.dpi = dpi as f32;
                        }
                    }
                    (Some((doc, at)), Ok(img)) => {
                        let tmp = std::env::temp_dir().join(format!("review-scan-{}.pdf", crate::model::next_id()));
                        match crate::pdf::images_to_pdf(&[&img], dpi as f32, &tmp) {
                            Ok(()) => self.queue_insert(doc, at, tmp),
                            Err(e) => self.error(e),
                        }
                    }
                },
                Msg::Ocr(r) => {
                    if let Ok(t) = r.text {
                        match r.key {
                            crate::ocr::OcrKey::Pdf { source, page } => {
                                if !t.chars.is_empty() {
                                    self.texts.insert((source, page), t);
                                    self.ocr_pages.insert((source, page));
                                }
                            }
                            crate::ocr::OcrKey::Image { doc, frame, ptr } => {
                                self.image_texts.insert((doc, frame), (ptr, t));
                            }
                        }
                        // Fold new text into an active search.
                        if !self.search.query.is_empty() && !self.search.running {
                            self.start_search();
                        }
                    }
                }
                Msg::Error(s) => self.error(s),
            }
        }
    }

    /// Turn soft proofing on (with a profile) or off.
    pub fn set_proof(&mut self, profile: Option<&crate::color::IccProfile>) {
        let proof = match profile {
            Some(p) => {
                self.proof_gen += 1;
                match crate::color::Proof::new(p, self.proof_gen) {
                    Ok(pr) => Some(pr),
                    Err(e) => {
                        self.error(format!("Can't use “{}” for soft proofing: {e}", p.name));
                        return;
                    }
                }
            }
            None => None,
        };
        self.proof = proof.clone();
        self.tiles.set_proof(proof);
    }

    /// Tools ▸ Assign Profile: reinterpret the image's colours in the given profile.
    pub fn assign_profile(&mut self, profile: &crate::color::IccProfile) {
        let Some(d) = self.doc_mut() else { return };
        let path = profile.path.clone();
        let name = profile.name.clone();
        let mut err = None;
        for idx in d.target_pages() {
            let Some(f) = d.model.frames.get(idx).cloned() else { continue };
            match crate::color::assign(&f, &path) {
                Ok(img) => d.edit_frame(idx, "Assign Profile", |_| img, |p| p),
                Err(e) => err = Some(e),
            }
        }
        if let Some(m) = d.image.as_mut() {
            m.icc_name = Some(name);
        }
        if let Some(e) = err {
            self.error(e);
        }
    }

    /// Keep decoded image pixels under a budget by unloading clean, inactive images.
    pub fn enforce_memory_budget(&mut self) {
        const BUDGET: usize = 1536 << 20;
        loop {
            let total: usize = self.docs.iter().filter(|d| d.kind == Kind::Image && d.ready()).map(|d| d.pixel_bytes()).sum();
            if total <= BUDGET {
                break;
            }
            let active = self.active;
            let victim = self
                .docs
                .iter()
                .enumerate()
                .filter(|(i, d)| *i != active && d.kind == Kind::Image && d.ready() && !d.dirty() && d.path.is_some() && d.page_count() > 0)
                .min_by_key(|(_, d)| d.last_active)
                .map(|(i, _)| i);
            let Some(v) = victim else { break };
            let d = &mut self.docs[v];
            d.model.frames.clear();
            d.state = LoadState::Unloaded;
            let id = d.id;
            self.images.retain(|(doc, _), _| *doc != id);
        }
    }

    fn after_save_continue(&mut self) {
        if self.quit_after_confirm && !self.docs.iter().any(|d| d.dirty() || d.saving) {
            self.allow_close = true;
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Ask the engine for text / links of a page once.
    pub fn page_text(&mut self, source: u64, page: u32) -> Option<Arc<PageText>> {
        if let Some(t) = self.texts.get(&(source, page)) {
            return Some(t.clone());
        }
        if self.text_req.insert((source, page)) {
            if let Some(s) = self.sources.get(&source) {
                s.request_text(page);
            }
        }
        None
    }

    pub fn page_links(&mut self, source: u64, page: u32) -> Option<&Vec<LinkInfo>> {
        if !self.links.contains_key(&(source, page)) && self.link_req.insert((source, page)) {
            if let Some(s) = self.sources.get(&source) {
                s.request_links(page);
            }
        }
        self.links.get(&(source, page))
    }

    // ------------------------------------------------------------------
    // Search

    pub fn start_search(&mut self) {
        self.search.generation += 1;
        self.search.hits.clear();
        self.search.current = None;
        let Some(doc) = self.docs.get(self.active) else { return };
        self.search.doc = doc.id;
        if self.search.query.trim().is_empty() {
            self.search.running = false;
            if self.sidebar == SidebarMode::Search {
                self.sidebar = SidebarMode::Thumbnails;
            }
            return;
        }
        // Live Text (photos and scanned pages) is searched here; PDF text by MuPDF.
        let doc_id = doc.id;
        let q = self.search.query.clone();
        let mut local = Vec::new();
        for (i, s) in doc.model.slots.iter().enumerate() {
            let (page, text) = match s.src {
                PageSrc::Image { frame } => match self.image_texts.get(&(doc_id, frame as usize)) {
                    Some((_, t)) => (frame, t.clone()),
                    None => continue,
                },
                PageSrc::Pdf { source, index } if self.ocr_pages.contains(&(source, index)) => match self.texts.get(&(source, index)) {
                    Some(t) => (index, t.clone()),
                    None => continue,
                },
                _ => continue,
            };
            let _ = i;
            for (a, b) in crate::ocr::find(&text, &q) {
                let (la, lb) = text.line_at(a);
                local.push(SearchHit { page, rects: text.rects(a, b), context: text.text(la, lb).replace('\n', " ") });
            }
        }
        self.search.hits.extend(local);
        if !self.search.hits.is_empty() && self.search.current.is_none() {
            self.search.current = Some(0);
        }
        self.sidebar = SidebarMode::Search;
        self.settings.show_sidebar = true;
        if !doc.is_pdf() {
            self.search.running = false;
            if let Some(0) = self.search.current {
                self.reveal_hit(0);
            }
            return;
        }
        // Search every source this document shows pages from.
        let mut srcs: Vec<u64> = doc
            .model
            .slots
            .iter()
            .filter_map(|s| if let PageSrc::Pdf { source, .. } = s.src { Some(source) } else { None })
            .collect();
        srcs.sort_unstable();
        srcs.dedup();
        for s in srcs {
            if let Some(src) = self.sources.get(&s) {
                src.search(self.search.query.clone(), self.search.generation);
            }
        }
        self.search.running = true;
        self.sidebar = SidebarMode::Search;
        self.settings.show_sidebar = true;
    }

    /// Slot index showing the hit's page.
    pub fn hit_slot(&self, hit: &SearchHit) -> Option<usize> {
        let doc = self.doc()?;
        doc.model.slots.iter().position(|s| match s.src {
            PageSrc::Pdf { index, .. } => index == hit.page,
            PageSrc::Image { frame } => frame == hit.page,
            PageSrc::Blank => false,
        })
    }

    pub fn reveal_hit(&mut self, i: usize) {
        let Some(hit) = self.search.hits.get(i).cloned() else { return };
        let Some(slot) = self.hit_slot(&hit) else { return };
        let y = hit.rects.first().map(|r| r.min.y);
        if let Some(d) = self.doc_mut() {
            d.pending_scroll = Some((slot, y));
            d.current = slot;
        }
        self.search.current = Some(i);
    }

    pub fn step_hit(&mut self, dir: i32) {
        let n = self.search.hits.len();
        if n == 0 {
            return;
        }
        let cur = self.search.current.map(|c| c as i32).unwrap_or(-1);
        let next = (cur + dir).rem_euclid(n as i32) as usize;
        self.reveal_hit(next);
    }

    // ------------------------------------------------------------------
    // Commands

    pub fn enabled(&self, a: Action) -> bool {
        use Action::*;
        let doc = self.doc();
        let ready = doc.map(|d| d.ready()).unwrap_or(false);
        let is_pdf = doc.map(|d| d.is_pdf()).unwrap_or(false);
        let is_img = doc.map(|d| d.kind == Kind::Image && d.ready()).unwrap_or(false);
        // 3D models are view-only: allow navigation, zoom, export and file commands.
        if doc.map(|d| d.kind == Kind::Model).unwrap_or(false) {
            return matches!(
                a,
                Open | NewFromClipboard | Screenshot(_) | ImportScanner | Preferences | About | Quit | HideSidebar | Sidebar(_) | Escape
                    | Close | ExportAs | Inspector | ZoomIn | ZoomOut | ZoomFit | ActualSize | Fullscreen | PrevDoc | NextDoc
                    | MoveToTrash | ShowInFolder | Rename | Revert
            ) && (ready || matches!(a, Open | Close | Quit | Preferences | About | Escape)) && !(matches!(a, PrevDoc | NextDoc) && self.docs.len() < 2) && a != Revert;
        }
        match a {
            Open | NewFromClipboard | Screenshot(_) | ImportScanner | Preferences | About | Quit | HideSidebar | Sidebar(_) | Escape => true,
            Undo => doc.map(|d| d.undo_name().is_some()).unwrap_or(false),
            Redo => doc.map(|d| d.redo_name().is_some()).unwrap_or(false),
            Save => ready && doc.map(|d| d.dirty() && !d.saving).unwrap_or(false),
            Revert => ready && doc.map(|d| d.dirty() && d.path.is_some()).unwrap_or(false),
            EditPermissions | InsertBlank | InsertFromFile | InsertFromScanner | ViewMode(_) | DarkPdf | HighlightFields | AddBookmark => is_pdf,
            AdjustColor | AdjustSize | FlipH | FlipV | RemoveBackground | Tool(crate::app::Tool::InstantAlpha) => is_img,
            ShowLocation => doc.and_then(|d| d.image.as_ref()).map(|i| i.gps.is_some()).unwrap_or(false),
            Tool(crate::app::Tool::Markup(_)) | Tool(crate::app::Tool::Redact) => is_pdf,
            Crop => ready && self.sel.rect.is_some(),
            ZoomSel => ready && (self.sel.rect.is_some() || self.sel.annot.is_some()),
            InvertSelection => is_img && self.sel.rect.is_some(),
            FindNext | FindPrev => !self.search.hits.is_empty(),
            Find | UseSelForFind | JumpToSel => is_pdf,
            Delete | Cut => ready && (self.sel.annot.is_some() || self.alpha.is_some() || (is_img && self.sel.rect.is_some()) || !doc.map(|d| d.selected_pages.is_empty()).unwrap_or(true)),
            Copy => ready,
            Paste => ready && (self.annot_clipboard.is_some()),
            PrevDoc | NextDoc => self.docs.len() > 1,
            _ => ready,
        }
    }

    pub fn run(&mut self, a: Action) {
        use Action::*;
        if !self.enabled(a) {
            return;
        }
        if matches!(a, ZoomIn | ZoomOut | ZoomFit | ActualSize) && self.doc().map(|d| d.kind == Kind::Model).unwrap_or(false) {
            let id = self.doc().map(|d| d.id).unwrap_or(0);
            if let Some(v) = self.viewers.get_mut(&id) {
                match a {
                    ZoomIn => v.cam.dist /= 1.25,
                    ZoomOut => v.cam.dist *= 1.25,
                    _ => v.cam = v.home,
                }
            }
            return;
        }
        match a {
            Open => self.choose(Choose::Open),
            NewFromClipboard => self.new_from_clipboard(),
            Close => {
                if self.slideshow.is_some() {
                    self.slideshow = None;
                } else if !self.docs.is_empty() {
                    self.close_doc(self.active, false);
                } else {
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Save => self.save(self.active),
            ExportAs | ExportPdf => {
                if let Some(d) = self.doc() {
                    self.dialog = Some(crate::dialogs::Dialog::export(d, a == ExportPdf, &self.settings));
                }
            }
            Print => self.dialog = Some(crate::dialogs::Dialog::print()),
            Revert => {
                if let Some(d) = self.doc_mut() {
                    d.edits = 0;
                }
                self.reload(self.active);
                self.update_title();
            }
            Rename => {
                if let Some(d) = self.doc() {
                    self.renaming = Some((d.id, d.name.clone()));
                }
            }
            Duplicate => self.duplicate(),
            MoveToTrash => self.move_to_trash(),
            ShowInFolder => {
                if let Some(p) = self.doc().and_then(|d| d.path.clone()) {
                    if let Some(dir) = p.parent() {
                        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                    }
                }
            }
            EditPermissions => {
                let sec = self.doc().and_then(|d| d.model.security.clone()).unwrap_or(crate::model::Security {
                    allow_print: true,
                    allow_copy: true,
                    allow_modify: true,
                    allow_annotate: true,
                    allow_forms: true,
                    allow_assemble: true,
                    ..Default::default()
                });
                self.dialog = Some(crate::dialogs::Dialog::Permissions(sec, String::new()));
            }
            Screenshot(kind) => crate::platform::screenshot(kind, self.msg_tx.clone(), self.ctx.clone()),
            ImportScanner => self.dialog = Some(crate::dialogs::Dialog::scan(None, &self.ctx)),
            InsertFromScanner => {
                if let Some(d) = self.doc() {
                    let at = (d.current + 1).min(d.page_count());
                    self.dialog = Some(crate::dialogs::Dialog::scan(Some((d.id, at)), &self.ctx));
                }
            }
            Undo => {
                if let Some(d) = self.doc_mut() {
                    d.undo();
                }
                self.sel = Selection::default();
                self.update_title();
            }
            Redo => {
                if let Some(d) = self.doc_mut() {
                    d.redo();
                }
                self.sel = Selection::default();
                self.update_title();
            }
            Cut => {
                self.run(Copy);
                self.run(Delete);
            }
            Copy => self.copy(),
            Paste => self.paste(),
            Delete => self.delete_selection(),
            SelectAll => self.select_all(),
            InvertSelection => {
                if let (Some(d), Some((_, slot, r))) = (self.doc(), self.sel.rect) {
                    if let Some(f) = d.model.frames.get(slot) {
                        let (w, h) = (f.width() as usize, f.height() as usize);
                        let mut mask = vec![true; w * h];
                        let r = r.intersect(Rect::from_min_max(Pos2::ZERO, egui::pos2(w as f32, h as f32)));
                        for y in r.min.y as usize..r.max.y as usize {
                            for x in r.min.x as usize..r.max.x as usize {
                                mask[y * w + x] = false;
                            }
                        }
                        let id = d.id;
                        self.set_alpha_mask(id, slot, mask);
                        self.sel.rect = None;
                    }
                }
            }
            InsertBlank => self.insert_blank(),
            InsertFromFile => {
                let at = self.doc().map(|d| d.current + 1).unwrap_or(0);
                if let Some(id) = self.doc().map(|d| d.id) {
                    self.choose(Choose::InsertPages { doc: id, at });
                }
            }
            Find => {
                self.search.focus = true;
                self.show_toolbar = true;
            }
            FindNext => self.step_hit(1),
            FindPrev => self.step_hit(-1),
            UseSelForFind => {
                if let Some(t) = self.selected_text() {
                    self.search.query = t.lines().next().unwrap_or("").trim().to_string();
                    self.start_search();
                }
            }
            JumpToSel => {
                if let Some(ts) = self.sel.text.clone() {
                    if let Some(d) = self.doc_mut() {
                        d.pending_scroll = Some((ts.slot, None));
                    }
                }
            }
            Speak => {
                let text = self.selected_text().or_else(|| self.current_page_text());
                if let Some(t) = text {
                    crate::platform::speak(&t);
                }
            }
            StopSpeaking => crate::platform::stop_speaking(),
            HideSidebar => self.settings.show_sidebar = !self.settings.show_sidebar,
            Sidebar(m) => {
                self.settings.show_sidebar = true;
                self.sidebar = m;
                self.contact_sheet = false;
            }
            ContactSheet => self.contact_sheet = !self.contact_sheet,
            ViewMode(m) => {
                if let Some(d) = self.doc_mut() {
                    d.mode = m;
                    let c = d.current;
                    d.pending_scroll = Some((c, None));
                }
                self.settings.pdf_view = m;
                self.contact_sheet = false;
            }
            DarkPdf => self.settings.dark_pdf = !self.settings.dark_pdf,
            ActualSize => {
                let px = self.settings.image_pixel_scale;
                if let Some(d) = self.doc_mut() {
                    d.fit = Fit::None;
                    d.zoom = if d.kind == Kind::Image && !px { 72.0 / d.image.as_ref().map(|i| i.dpi).unwrap_or(72.0) } else { 1.0 };
                    let c = d.current;
                    d.pending_scroll = Some((c, None));
                }
            }
            ZoomFit => {
                if let Some(d) = self.doc_mut() {
                    d.fit = if d.kind == Kind::Image { Fit::Page } else if d.fit == Fit::Width { Fit::Page } else { Fit::Width };
                    let c = d.current;
                    d.pending_scroll = Some((c, None));
                }
            }
            ZoomIn => self.zoom_by(1.25),
            ZoomOut => self.zoom_by(0.8),
            ZoomSel => self.zoom_to_selection(),
            ToggleMarkup => self.show_markup = !self.show_markup,
            ToggleToolbar => self.show_toolbar = !self.show_toolbar,
            Slideshow => {
                let start = self.doc().map(|d| d.current).unwrap_or(0);
                self.slideshow = Some(crate::canvas::Slideshow::new(start));
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            Fullscreen => {
                self.fullscreen = !self.fullscreen;
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            PrevDoc => {
                let n = self.docs.len();
                self.activate((self.active + n - 1) % n);
            }
            NextDoc => {
                let n = self.docs.len();
                self.activate((self.active + 1) % n);
            }
            PrevPage => self.go_page(-1),
            NextPage => self.go_page(1),
            FirstPage => self.go_to(0),
            LastPage => {
                let n = self.doc().map(|d| d.page_count()).unwrap_or(1);
                self.go_to(n.saturating_sub(1));
            }
            GoToPage => self.dialog = Some(crate::dialogs::Dialog::GoToPage(String::new())),
            Back => self.history_step(-1),
            Forward => self.history_step(1),
            Inspector => self.show_inspector = !self.show_inspector,
            Magnifier => self.magnifier = !self.magnifier,
            AdjustColor => {
                if let Some(d) = self.doc() {
                    self.dialog = Some(crate::dialogs::Dialog::adjust_color(d));
                }
            }
            AdjustSize => {
                if let Some(d) = self.doc() {
                    self.dialog = Some(crate::dialogs::Dialog::adjust_size(d));
                }
            }
            Tool(t) => self.set_tool(t),
            NewSignature => self.dialog = Some(crate::dialogs::Dialog::Signature(Vec::new())),
            AddBookmark => {
                if let Some(d) = self.doc_mut() {
                    let uid = d.model.slots.get(d.current).map(|s| s.uid);
                    if let Some(uid) = uid {
                        d.checkpoint("Add Bookmark");
                        if let Some(pos) = d.model.bookmarks.iter().position(|b| *b == uid) {
                            d.model.bookmarks.remove(pos);
                        } else {
                            d.model.bookmarks.push(uid);
                        }
                    }
                }
            }
            RotateLeft => self.rotate(-90),
            RotateRight => self.rotate(90),
            FlipH => {
                if let Some(d) = self.doc_mut() {
                    d.flip_image(true);
                }
            }
            FlipV => {
                if let Some(d) = self.doc_mut() {
                    d.flip_image(false);
                }
            }
            Crop => self.crop(),
            RemoveBackground => {
                if let Some(d) = self.doc_mut() {
                    for idx in d.target_pages() {
                        d.edit_frame(idx, "Remove Background", imaging::remove_background, |p| p);
                    }
                }
            }
            ShowLocation => {
                if let Some((lat, lon)) = self.doc().and_then(|d| d.image.as_ref()).and_then(|i| i.gps) {
                    self.ctx.open_url(egui::OpenUrl::new_tab(format!("https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map=16/{lat:.6}/{lon:.6}")));
                }
            }
            HighlightFields => self.highlight_fields = !self.highlight_fields,
            Preferences => self.dialog = Some(crate::dialogs::Dialog::Preferences),
            About => self.dialog = Some(crate::dialogs::Dialog::About),
            Quit => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Escape => {
                if self.slideshow.is_some() {
                    self.slideshow = None;
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                } else if self.dialog.is_some() {
                    self.dialog = None;
                } else if self.sel.annot.is_some() || self.sel.text.is_some() || self.sel.rect.is_some() || self.alpha.is_some() {
                    self.sel = Selection::default();
                    self.alpha = None;
                } else if !self.polygon.is_empty() {
                    self.polygon.clear();
                } else if self.tool != crate::app::Tool::Text {
                    self.tool = crate::app::Tool::Text;
                } else if self.fullscreen {
                    self.fullscreen = false;
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
        }
        self.update_title();
    }

    pub fn set_tool(&mut self, t: Tool) {
        // Highlight/underline/strike apply directly to an existing text selection.
        if let Tool::Markup(kind) = t {
            if self.apply_markup_to_selection(kind) {
                return;
            }
        }
        if matches!(t, Tool::Signature(_)) && self.settings.signatures.is_empty() {
            self.dialog = Some(crate::dialogs::Dialog::Signature(Vec::new()));
            return;
        }
        self.tool = if self.tool == t { Tool::Text } else { t };
        if self.tool != Tool::Text && self.tool != Tool::Rect {
            self.show_markup = true;
        }
        self.polygon.clear();
    }

    pub fn apply_markup_to_selection(&mut self, kind: MarkupKind) -> bool {
        let Some(ts) = self.sel.text.clone() else { return false };
        let color = match kind {
            MarkupKind::Highlight => self.markup_color,
            _ => egui::Color32::from_rgb(0xE5, 0x32, 0x2D),
        };
        let Some(text) = self.slot_text(ts.slot) else { return false };
        let (a, b) = (ts.a.min(ts.b), ts.a.max(ts.b));
        let quads: Vec<[Pos2; 4]> = text.rects(a, b).iter().map(|r| [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]).collect();
        if quads.is_empty() {
            return false;
        }
        let author = self.author();
        let Some(d) = self.doc_mut() else { return false };
        d.checkpoint("Highlight");
        let mut an = Annot::new(crate::model::Shape::Markup { kind, quads }, Style { stroke: color, ..Style::default() });
        an.author = author;
        if let Some(s) = d.model.slots.get_mut(ts.slot) {
            s.annots.push(an);
        }
        self.sel.text = None;
        true
    }

    pub fn author(&self) -> String {
        if self.settings.add_name_to_annotations {
            self.settings.author.clone()
        } else {
            String::new()
        }
    }

    pub fn slot_text(&mut self, slot: usize) -> Option<Arc<PageText>> {
        let src = self.doc()?.model.slots.get(slot)?.src.clone();
        match src {
            PageSrc::Pdf { source, index } => self.page_text(source, index),
            PageSrc::Image { frame } => self.image_text(self.active, frame as usize),
            PageSrc::Blank => None,
        }
    }

    /// Live Text for an image frame; starts recognition the first time it's asked for.
    pub fn image_text(&mut self, doc_idx: usize, frame: usize) -> Option<Arc<PageText>> {
        let d = self.docs.get(doc_idx)?;
        let img = d.model.frames.get(frame)?.clone();
        let ptr = Arc::as_ptr(&img) as usize;
        if let Some((p, t)) = self.image_texts.get(&(d.id, frame)) {
            if *p == ptr {
                return Some(t.clone());
            }
        }
        let key = crate::ocr::OcrKey::Image { doc: d.id, frame, ptr };
        if self.settings.live_text && crate::ocr::available() && self.ocr_req.insert(key) {
            let _ = self.ocr_tx.send(crate::ocr::OcrJob { key, image: img, scale: 1.0, origin: Pos2::ZERO });
        }
        None
    }

    /// Render a text-less PDF page at ~300 dpi and run Live Text on it.
    fn request_pdf_ocr(&mut self, source: u64, page: u32) {
        let key = crate::ocr::OcrKey::Pdf { source, page };
        if !self.settings.live_text || !crate::ocr::available() || !self.ocr_req.insert(key) {
            return;
        }
        let Some(src) = self.sources.get(&source) else { return };
        let size = self.source_info.get(&source).and_then(|i| i.pages.get(page as usize)).map(|p| p.0.size()).unwrap_or(egui::vec2(612.0, 792.0));
        let scale = (300.0 / 72.0f32).min(5000.0 / size.x.max(size.y).max(1.0));
        let req = self.next_request;
        self.next_request += 1;
        self.page_requests.insert(req, PageUse::Ocr { source, page, scale });
        src.render_page(page, scale, req, false);
    }

    /// The current page's text if it came from Live Text (drives the Live Text button).
    pub fn live_text_for_current(&mut self) -> Option<(usize, Arc<PageText>)> {
        let d = self.doc()?;
        let cur = d.current;
        let src = d.model.slots.get(cur)?.src.clone();
        let t = match src {
            PageSrc::Image { frame } => self.image_text(self.active, frame as usize)?,
            PageSrc::Pdf { source, index } if self.ocr_pages.contains(&(source, index)) => self.texts.get(&(source, index))?.clone(),
            _ => return None,
        };
        (!t.chars.is_empty()).then_some((cur, t))
    }

    pub fn selected_text(&mut self) -> Option<String> {
        let ts = self.sel.text.clone()?;
        let t = self.slot_text(ts.slot)?;
        let s = t.text(ts.a.min(ts.b), ts.a.max(ts.b));
        (!s.trim().is_empty()).then_some(s)
    }

    fn current_page_text(&mut self) -> Option<String> {
        let cur = self.doc()?.current;
        let t = self.slot_text(cur)?;
        Some(t.text(0, t.chars.len()))
    }

    fn zoom_by(&mut self, f: f32) {
        if let Some(d) = self.doc_mut() {
            d.fit = Fit::None;
            d.zoom = (d.zoom * f).clamp(0.05, 64.0);
        }
    }

    fn zoom_to_selection(&mut self) {
        let target = self.sel.rect.map(|(_, s, r)| (s, r)).or_else(|| {
            let (_, s, id) = self.sel.annot?;
            let a = self.doc()?.model.slots.get(s)?.annots.iter().find(|a| a.id == id)?;
            Some((s, a.bounds()))
        });
        if let Some((slot, r)) = target {
            self.ctx.data_mut(|d| d.insert_temp(egui::Id::new("zoom-to"), (slot, r)));
        }
    }

    pub fn go_page(&mut self, d: i32) {
        let Some(doc) = self.doc() else { return };
        let step = if doc.mode == PdfViewMode::Two { 2 } else { 1 };
        let n = doc.page_count() as i32;
        let next = (doc.current as i32 + d * step).clamp(0, (n - 1).max(0)) as usize;
        self.go_to(next);
    }

    pub fn go_to(&mut self, page: usize) {
        if let Some(d) = self.doc_mut() {
            d.playing = None;
            let page = page.min(d.page_count().saturating_sub(1));
            if d.history.get(d.history_pos) != Some(&page) {
                d.history.truncate(d.history_pos + 1);
                d.history.push(page);
                d.history_pos = d.history.len() - 1;
            }
            d.current = page;
            d.pending_scroll = Some((page, None));
        }
        self.thumb_scroll_to = Some(page);
    }

    fn history_step(&mut self, dir: i32) {
        if let Some(d) = self.doc_mut() {
            let np = d.history_pos as i32 + dir;
            if np >= 0 && (np as usize) < d.history.len() {
                d.history_pos = np as usize;
                let p = d.history[d.history_pos];
                d.current = p;
                d.pending_scroll = Some((p, None));
            }
        }
    }

    fn rotate(&mut self, deg: i32) {
        let Some(d) = self.doc_mut() else { return };
        if d.kind == Kind::Image {
            d.rotate_image(deg);
        } else {
            let pages = d.target_pages();
            d.checkpoint(if deg > 0 { "Rotate Right" } else { "Rotate Left" });
            for p in pages {
                if let Some(s) = d.model.slots.get_mut(p) {
                    s.rot = (s.rot + deg).rem_euclid(360);
                }
            }
            let c = d.current;
            d.pending_scroll = Some((c, None));
        }
    }

    fn crop(&mut self) {
        let Some((doc_id, slot, r)) = self.sel.rect else { return };
        let Some(i) = self.doc_index(doc_id) else { return };
        let d = &mut self.docs[i];
        if d.kind == Kind::Image {
            let r = Rect::from_min_max(r.min.round(), r.max.round());
            let off = r.min.to_vec2();
            d.edit_frame(slot, "Crop", |img| imaging::crop(img, r), move |p| p - off);
        } else {
            d.checkpoint("Crop");
            let pages = if d.selected_pages.len() > 1 { d.target_pages() } else { vec![slot] };
            for p in pages {
                if let Some(s) = d.model.slots.get_mut(p) {
                    let r = r.intersect(s.bounds);
                    if r.width() > 4.0 && r.height() > 4.0 {
                        s.crop = Some(r);
                    }
                }
            }
        }
        self.sel.rect = None;
    }

    fn insert_blank(&mut self) {
        let Some(d) = self.doc_mut() else { return };
        let at = (d.current + 1).min(d.page_count());
        let size = d.model.slots.get(d.current).map(|s| s.bounds).unwrap_or(Rect::from_min_max(Pos2::ZERO, egui::pos2(612.0, 792.0)));
        d.checkpoint("Insert Blank Page");
        let b = Rect::from_min_size(Pos2::ZERO, size.size());
        d.model.slots.insert(at, Slot::new(PageSrc::Blank, b));
        d.current = at;
        d.pending_scroll = Some((at, None));
    }

    fn select_all(&mut self) {
        let Some(d) = self.doc() else { return };
        let (cur, id, is_image) = (d.current, d.id, d.kind == Kind::Image);
        let bounds = d.model.slots.get(cur).map(|s| s.bounds);
        // Text first (PDF text, or Live Text in a photo with the text tool), else the whole image.
        let text = if !is_image || self.tool == Tool::Text { self.slot_text(cur).filter(|t| !t.chars.is_empty()) } else { None };
        match (text, bounds) {
            (Some(t), _) => self.sel.text = Some(TextSel { doc: id, slot: cur, a: 0, b: t.chars.len() }),
            (None, Some(b)) if is_image => self.sel.rect = Some((id, cur, b)),
            _ => {}
        }
    }

    fn copy(&mut self) {
        if let Some(t) = self.selected_text() {
            self.ctx.copy_text(t);
            return;
        }
        let Some(d) = self.doc() else { return };
        if let Some((_, slot, id)) = self.sel.annot {
            self.annot_clipboard = d.model.slots.get(slot).and_then(|s| s.annots.iter().find(|a| a.id == id)).cloned();
            return;
        }
        let cur = d.current;
        match d.kind {
            Kind::Model => {}
            Kind::Image => {
                let Some(mut img) = d.flattened_frame(cur) else { return };
                if let Some((_, s, r)) = self.sel.rect {
                    if s == cur {
                        img = imaging::crop(&img, r);
                    }
                }
                copy_image_to_clipboard(&img, &self.msg_tx);
            }
            Kind::Pdf => {
                // Copy the rectangular selection as an image (rendered at 2x).
                let Some((_, slot, r)) = self.sel.rect else { return };
                let Some(s) = d.model.slots.get(slot) else { return };
                if let PageSrc::Pdf { source, index } = s.src {
                    if let Some(src) = self.sources.get(&source) {
                        let scale = 2.0;
                        let px = Rect::from_min_max(((r.min - s.bounds.min) * scale).to_pos2(), ((r.max - s.bounds.min) * scale).to_pos2());
                        let req = self.next_request;
                        self.next_request += 1;
                        self.page_requests.insert(req, PageUse::CopyImage(Some(px)));
                        src.render_page(index, scale, req, true);
                    }
                }
            }
        }
    }

    fn paste(&mut self) {
        let Some(mut a) = self.annot_clipboard.clone() else { return };
        let Some(d) = self.doc_mut() else { return };
        let cur = d.current;
        d.checkpoint("Paste");
        a.id = crate::model::next_id();
        a.origin = None;
        a.dirty = true;
        a.translate(egui::vec2(12.0, 12.0));
        let id = a.id;
        let did = d.id;
        if let Some(s) = d.model.slots.get_mut(cur) {
            s.annots.push(a);
            self.sel.annot = Some((did, cur, id));
        }
    }

    pub fn delete_selection(&mut self) {
        if let Some(alpha) = self.alpha.take() {
            if let Some(i) = self.doc_index(alpha.doc) {
                let mask = alpha.mask.clone();
                self.docs[i].edit_frame(alpha.slot, "Delete", move |img| imaging::apply_mask(img, &mask, true), |p| p);
            }
            return;
        }
        if let Some((doc_id, slot, id)) = self.sel.annot.take() {
            if let Some(i) = self.doc_index(doc_id) {
                let d = &mut self.docs[i];
                let src = d.model.slots.get(slot).map(|s| s.src.clone());
                d.checkpoint("Delete");
                if let Some(s) = d.model.slots.get_mut(slot) {
                    if let Some(pos) = s.annots.iter().position(|a| a.id == id) {
                        let a = s.annots.remove(pos);
                        if let (Some(x), Some(PageSrc::Pdf { source, .. })) = (a.origin, src) {
                            d.model.deleted_origins.push((source, x));
                        }
                    }
                }
            }
            return;
        }
        if let (Some(d), Some((_, slot, r))) = (self.docs.get_mut(self.active), self.sel.rect) {
            if d.kind == Kind::Image {
                if let Some(f) = d.model.frames.get(slot) {
                    let (w, h) = (f.width() as usize, f.height() as usize);
                    let mut mask = vec![false; w * h];
                    let r = r.intersect(Rect::from_min_max(Pos2::ZERO, egui::pos2(w as f32, h as f32)));
                    for y in r.min.y as usize..r.max.y as usize {
                        for x in r.min.x as usize..r.max.x as usize {
                            mask[y * w + x] = true;
                        }
                    }
                    d.edit_frame(slot, "Delete", move |img| imaging::apply_mask(img, &mask, false), |p| p);
                }
                self.sel.rect = None;
                return;
            }
        }
        // Delete selected pages.
        if let Some(d) = self.doc_mut() {
            let pages = d.target_pages();
            if d.selected_pages.is_empty() || pages.len() >= d.page_count() {
                return;
            }
            d.checkpoint("Delete");
            for p in pages.into_iter().rev() {
                d.model.slots.remove(p);
            }
            d.selected_pages.clear();
            d.after_model_change();
        }
    }

    pub fn set_alpha_mask(&mut self, doc: u64, slot: usize, mask: Vec<bool>) {
        let Some(d) = self.docs.iter().find(|d| d.id == doc) else { return };
        let Some(f) = d.model.frames.get(slot) else { return };
        let (w, h) = (f.width() as usize, f.height() as usize);
        let accent = crate::theme::Palette::of(&self.ctx).accent;
        let mut px = Vec::with_capacity(w * h);
        for m in &mask {
            px.push(if *m { egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 120) } else { egui::Color32::TRANSPARENT });
        }
        let img = egui::ColorImage { size: [w, h], pixels: px, source_size: egui::vec2(w as f32, h as f32) };
        let tex = self.ctx.load_texture("alpha-mask", img, egui::TextureOptions::LINEAR);
        self.alpha = Some(AlphaSel { doc, slot, mask: Arc::new(mask), tex });
    }

    fn new_from_clipboard(&mut self) {
        let img = arboard::Clipboard::new().and_then(|mut c| c.get_image());
        match img {
            Ok(img) => {
                let Some(rgba) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else {
                    self.error("The clipboard image could not be read.");
                    return;
                };
                self.new_image_doc(rgba, "Untitled");
            }
            Err(_) => self.error("There is no image on the clipboard."),
        }
    }

    pub fn new_image_doc(&mut self, rgba: image::RgbaImage, name: &str) {
        let mut doc = Document::new(None, Kind::Image);
        doc.name = name.to_string();
        doc.fit = Fit::Auto;
        doc.image_loaded(Ok(imaging::Loaded {
            frames: vec![Arc::new(rgba)],
            delays: vec![],
            format: "PNG image".into(),
            exif: vec![],
            gps: None,
            dpi: 72.0,
            has_alpha: true,
            color_model: "RGB + Alpha".into(),
            thumb: None,
            icc_name: None,
        }));
        doc.edits = 1;
        self.docs.push(doc);
        self.activate(self.docs.len() - 1);
    }

    fn duplicate(&mut self) {
        let Some(d) = self.doc() else { return };
        if d.kind == Kind::Image {
            if let Some(img) = d.flattened_frame(d.current) {
                let name = format!("{} copy", d.name.rsplit_once('.').map(|x| x.0).unwrap_or(&d.name));
                self.new_image_doc(img, &name);
            }
        } else if let Some(p) = d.path.clone() {
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let ext = p.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let mut n = 1;
            let dest = loop {
                let c = p.with_file_name(if n == 1 { format!("{stem} copy.{ext}") } else { format!("{stem} copy {n}.{ext}") });
                if !c.exists() {
                    break c;
                }
                n += 1;
            };
            if let Err(e) = std::fs::copy(&p, &dest) {
                self.error(format!("Couldn’t duplicate: {e}"));
            } else {
                self.open_paths(vec![dest]);
            }
        }
    }

    fn move_to_trash(&mut self) {
        let Some(d) = self.doc() else { return };
        let Some(p) = d.path.clone() else { return };
        match trash::delete(&p) {
            Ok(()) => {
                let i = self.active;
                self.close_doc(i, true);
                self.toast(format!("Moved “{}” to the Trash", p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
            }
            Err(e) => self.error(format!("Couldn’t move to Trash: {e}")),
        }
    }

    // ------------------------------------------------------------------
    // Saving

    pub fn save(&mut self, idx: usize) {
        let Some(d) = self.docs.get(idx) else { return };
        if !d.can_save_in_place() {
            self.dialog = Some(crate::dialogs::Dialog::export(d, false, &self.settings));
            return;
        }
        let path = d.path.clone().unwrap();
        self.save_to(idx, path, None, true);
    }

    /// Save document `idx` to `dest`. Without `opts` the format follows the file extension.
    pub fn save_to(&mut self, idx: usize, dest: PathBuf, opts: Option<ExportOpts>, adopt: bool) {
        let author = self.author();
        let sources: HashMap<u64, (PathBuf, Option<String>)> = self
            .sources
            .iter()
            .map(|(k, s)| (*k, (s.path.clone(), s.password.clone())))
            .collect();
        let Some(d) = self.docs.get_mut(idx) else { return };
        let fmt = opts.as_ref().map(|o| o.format).or_else(|| ExportFormat::from_path(&dest)).unwrap_or(ExportFormat::Pdf);
        let security = opts.as_ref().and_then(|o| o.security.clone());
        match d.kind {
            Kind::Model => {
                // Export a snapshot of the current 3D view.
                let id = d.id;
                let (Some(rs), Some(v)) = (self.wgpu.clone(), self.viewers.get_mut(&id)) else { return };
                let bg = crate::theme::Palette::of(&self.ctx).canvas;
                let snap = v.snapshot(&rs, [2400, 1800], bg);
                let (q, alpha) = opts.as_ref().map(|o| (o.quality, o.alpha)).unwrap_or((90, false));
                let fmt = if fmt == ExportFormat::Pdf { ExportFormat::Png } else { fmt };
                let result = snap.and_then(|img| imaging::export(&img, fmt, q, alpha, 144.0, &dest));
                let _ = self.msg_tx.send(Msg::ImageSaved { doc: id, path: dest, result, reload: false });
            }
            Kind::Pdf if fmt == ExportFormat::Pdf => {
                if !d.is_pdf() {
                    // EPUB, XPS, CBZ…: let MuPDF convert it.
                    let Some(src) = d.source.and_then(|s| self.sources.get(&s)).map(|s| (s.path.clone(), s.password.clone())) else { return };
                    let tx = self.msg_tx.clone();
                    let ctx = self.ctx.clone();
                    let doc = d.id;
                    std::thread::spawn(move || {
                        let result = crate::pdf::convert_to_pdf(&src.0, src.1.as_deref(), &dest);
                        let _ = tx.send(Msg::ImageSaved { doc, path: dest, result, reload: false });
                        ctx.request_repaint();
                    });
                    return;
                }
                let mut plan = d.save_plan(&dest, &sources, &author);
                if security.is_some() {
                    plan.security = security;
                }
                if let Some(src) = d.source.and_then(|s| self.sources.get(&s)) {
                    d.saving = true;
                    if adopt {
                        d.reload_after_save = Some(dest.clone());
                    }
                    src.save(plan);
                }
            }
            Kind::Pdf => {
                // Export the current page as an image.
                let cur = d.current;
                let Some(PageSrc::Pdf { source, index }) = d.model.slots.get(cur).map(|s| s.src.clone()) else { return };
                let Some(src_path) = self.sources.get(&source).map(|s| (s.path.clone(), s.password.clone())) else { return };
                let (q, alpha) = opts.as_ref().map(|o| (o.quality, o.alpha)).unwrap_or((90, false));
                let tx = self.msg_tx.clone();
                let ctx = self.ctx.clone();
                let doc = d.id;
                std::thread::spawn(move || {
                    let result = crate::platform::render_page_file(&src_path.0, src_path.1.as_deref(), index, 150.0)
                        .and_then(|img| imaging::export(&img, fmt, q, alpha, 150.0, &dest));
                    let _ = tx.send(Msg::ImageSaved { doc, path: dest, result, reload: false });
                    ctx.request_repaint();
                });
            }
            Kind::Image => {
                let frames: Vec<image::RgbaImage> = (0..d.page_count()).filter_map(|i| d.flattened_frame(i)).collect();
                let dpi = d.image.as_ref().map(|i| i.dpi).unwrap_or(72.0);
                let (q, alpha) = opts.as_ref().map(|o| (o.quality, o.alpha)).unwrap_or((self.settings.export_quality.max(80), true));
                let tx = self.msg_tx.clone();
                let ctx = self.ctx.clone();
                let doc = d.id;
                d.saving = true;
                std::thread::spawn(move || {
                    let result = if fmt == ExportFormat::Pdf {
                        let refs: Vec<&image::RgbaImage> = frames.iter().collect();
                        crate::pdf::images_to_pdf(&refs, dpi, &dest)
                    } else {
                        match frames.first() {
                            Some(f) => imaging::export(f, fmt, q, alpha, dpi, &dest),
                            None => Err("Nothing to save".into()),
                        }
                    };
                    let _ = tx.send(Msg::ImageSaved { doc, path: dest, result, reload: adopt && fmt != ExportFormat::Pdf });
                    ctx.request_repaint();
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // File chooser (runs on a thread so the window stays responsive)

    pub fn choose(&mut self, purpose: Choose) {
        let tx = self.msg_tx.clone();
        let ctx = self.ctx.clone();
        let start_dir = self.doc().and_then(|d| d.path.as_ref()).and_then(|p| p.parent().map(Path::to_path_buf)).or_else(dirs::home_dir);
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut dlg = rfd::FileDialog::new();
            if let Some(d) = start_dir.clone() {
                dlg = dlg.set_directory(d);
            }
            let paths = match &purpose {
                Choose::Open => dlg
                    .set_title("Open")
                    .add_filter("Documents and images", &supported_exts())
                    .add_filter("All files", &["*"])
                    .pick_files()
                    .unwrap_or_default(),
                Choose::InsertPages { .. } => dlg
                    .set_title("Insert Pages from File")
                    .add_filter("PDF documents", &["pdf"])
                    .add_filter("Images", imaging::IMAGE_EXTS)
                    .pick_files()
                    .unwrap_or_default(),
            };
            // No answer within a moment usually means no desktop portal: fall back to kdialog/zenity.
            let paths = if paths.is_empty() && started.elapsed() < Duration::from_millis(300) {
                crate::platform::pick_files_fallback(start_dir.as_deref())
            } else {
                paths
            };
            let _ = tx.send(Msg::Chosen { purpose, paths });
            ctx.request_repaint();
        });
    }

    fn file_chosen(&mut self, purpose: Choose, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        match purpose {
            Choose::Open => self.open_paths(paths),
            Choose::InsertPages { doc, at } => self.insert_files(doc, at, paths),
        }
    }

    /// Insert pages from files into document `doc` at `at`.
    pub fn insert_files(&mut self, doc: u64, at: usize, paths: Vec<PathBuf>) {
        let Some(i) = self.doc_index(doc) else { return };
        if self.docs[i].kind != Kind::Pdf {
            self.error("Pages can only be inserted into PDF documents.");
            return;
        }
        for p in paths.into_iter().rev() {
            if imaging::is_image_path(&p) {
                // Convert to a one-page PDF first, then insert that.
                match imaging::load(&p) {
                    Ok(l) => {
                        let tmp = std::env::temp_dir().join(format!("review-insert-{}.pdf", crate::model::next_id()));
                        let refs: Vec<&image::RgbaImage> = l.frames.iter().map(|f| f.as_ref()).collect();
                        if let Err(e) = crate::pdf::images_to_pdf(&refs, l.dpi.max(72.0), &tmp) {
                            self.error(e);
                            continue;
                        }
                        self.queue_insert(doc, at, tmp);
                    }
                    Err(e) => self.error(e),
                }
            } else {
                self.queue_insert(doc, at, p);
            }
        }
    }

    pub fn queue_insert(&mut self, doc: u64, at: usize, path: PathBuf) {
        let sid = crate::model::next_id();
        self.sources.insert(sid, self.engine.open(sid, &path, None));
        self.ctx.data_mut(|d| {
            let v: &mut Vec<(u64, u64, usize)> = d.get_temp_mut_or_default(egui::Id::new("pending-inserts"));
            v.push((sid, doc, at));
        });
    }

    fn finish_pending_insert(&mut self, source: u64) {
        let pending: Vec<(u64, u64, usize)> = self.ctx.data_mut(|d| d.get_temp(egui::Id::new("pending-inserts")).unwrap_or_default());
        let Some(&(_, doc, at)) = pending.iter().find(|(s, _, _)| *s == source) else { return };
        self.ctx.data_mut(|d| {
            let v: &mut Vec<(u64, u64, usize)> = d.get_temp_mut_or_default(egui::Id::new("pending-inserts"));
            v.retain(|(s, _, _)| *s != source);
        });
        let Some(info) = self.source_info.get(&source).cloned() else { return };
        let Some(i) = self.doc_index(doc) else { return };
        let d = &mut self.docs[i];
        d.checkpoint("Insert");
        let at = at.min(d.page_count());
        for (k, (b, label)) in info.pages.iter().enumerate().rev() {
            let mut s = Slot::new(PageSrc::Pdf { source, index: k as u32 }, *b);
            s.label = label.clone();
            for (p, a) in &info.annots {
                if *p as usize == k {
                    s.annots.push(a.clone());
                }
            }
            d.model.slots.insert(at, s);
        }
        d.current = at;
        d.pending_scroll = Some((at, None));
    }

    /// Move pages (by slot index) from one document to another position.
    pub fn move_pages(&mut self, from_doc: u64, pages: Vec<usize>, to_doc: u64, mut at: usize, copy: bool) {
        let (Some(fi), Some(ti)) = (self.doc_index(from_doc), self.doc_index(to_doc)) else { return };
        if self.docs[ti].kind != Kind::Pdf || self.docs[fi].kind != Kind::Pdf {
            return;
        }
        let mut pages = pages;
        pages.sort_unstable();
        pages.dedup();
        let moved: Vec<Slot> = pages.iter().filter_map(|p| self.docs[fi].model.slots.get(*p).cloned()).collect();
        if moved.is_empty() {
            return;
        }
        if fi == ti {
            let d = &mut self.docs[fi];
            d.checkpoint("Move");
            let before = pages.iter().filter(|p| **p < at).count();
            for p in pages.iter().rev() {
                d.model.slots.remove(*p);
            }
            at = at.saturating_sub(before).min(d.model.slots.len());
            for (k, s) in moved.into_iter().enumerate() {
                d.model.slots.insert(at + k, s);
            }
            d.selected_pages = (at..at + pages.len()).collect();
            d.current = at;
        } else {
            if !copy && self.docs[fi].page_count() > pages.len() {
                let d = &mut self.docs[fi];
                d.checkpoint("Move");
                for p in pages.iter().rev() {
                    d.model.slots.remove(*p);
                }
                d.selected_pages.clear();
                d.after_model_change();
            }
            let d = &mut self.docs[ti];
            d.checkpoint("Insert");
            let at = at.min(d.model.slots.len());
            let n = moved.len();
            for (k, mut s) in moved.into_iter().enumerate() {
                s.uid = crate::model::next_id();
                for a in &mut s.annots {
                    a.dirty = true;
                }
                d.model.slots.insert(at + k, s);
            }
            d.selected_pages = (at..at + n).collect();
        }
    }

    // ------------------------------------------------------------------
    // Frame

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let mut fired = Vec::new();
        // egui ignores extra Shift/Alt when matching, so try the most specific shortcuts first
        // (otherwise Ctrl+Shift+S would trigger Save).
        let mut list = shortcuts();
        list.sort_by_key(|(s, _)| std::cmp::Reverse(s.modifiers.shift as u8 + s.modifiers.alt as u8));
        ctx.input_mut(|i| {
            for (sc, a) in list {
                // While typing, only let through shortcuts that don't edit text.
                if typing && matches!(a, Action::Copy | Action::Cut | Action::Paste | Action::SelectAll | Action::Undo | Action::Redo | Action::Magnifier | Action::Delete) {
                    continue;
                }
                if i.consume_shortcut(&sc) {
                    fired.push(a);
                }
            }
            if !typing {
                if i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace) {
                    fired.push(Action::Delete);
                }
                if i.consume_key(Modifiers::NONE, Key::Home) {
                    fired.push(Action::FirstPage);
                }
                if i.consume_key(Modifiers::NONE, Key::End) {
                    fired.push(Action::LastPage);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                fired.push(Action::Escape);
            }
        });
        for a in fired {
            if a == Action::Delete && self.enabled(Action::Delete) {
                self.delete_selection();
            } else {
                self.run(a);
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        // Dropping onto the thumbnail sidebar inserts pages; anywhere else opens.
        let target: Option<(u64, usize)> = ctx.data(|d| d.get_temp(egui::Id::new("drop-target")));
        match target {
            Some((doc, at)) => self.insert_files(doc, at, dropped),
            None => self.open_paths(dropped),
        }
    }
}

fn copy_image_to_clipboard(img: &image::RgbaImage, tx: &Sender<Msg>) {
    let data = arboard::ImageData { width: img.width() as usize, height: img.height() as usize, bytes: std::borrow::Cow::Owned(img.as_raw().clone()) };
    let tx = tx.clone();
    // arboard on Wayland must keep serving the clipboard; do it off the UI thread.
    std::thread::spawn(move || match arboard::Clipboard::new() {
        Ok(mut c) => {
            #[cfg(target_os = "linux")]
            {
                use arboard::SetExtLinux;
                let _ = c.set().wait_until(std::time::Instant::now() + Duration::from_secs(60)).image(data);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = c.set_image(data);
            }
            let _ = tx.send(Msg::Info("Copied".into()));
        }
        Err(e) => {
            let _ = tx.send(Msg::Error(format!("Clipboard unavailable: {e}")));
        }
    });
}

pub fn supported_exts() -> Vec<&'static str> {
    let mut v = vec!["pdf", "xps", "oxps", "epub", "cbz", "fb2", "mobi", "svg"];
    v.extend_from_slice(imaging::IMAGE_EXTS);
    v.extend_from_slice(crate::model3d::MODEL_EXTS);
    v
}

fn expand_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let exts = supported_exts();
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            entries.sort();
            for e in entries {
                let ok = e.extension().map(|x| exts.contains(&x.to_string_lossy().to_lowercase().as_str())).unwrap_or(false);
                if ok && e.is_file() {
                    out.push(e);
                }
            }
        } else {
            out.push(p);
        }
    }
    out
}

impl eframe::App for App {
    /// Opaque, matching the canvas. eframe's default is semi-transparent, which lets the
    /// compositor treat the window as see-through and causes flicker on some drivers.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        let c = if visuals.dark_mode { egui::Color32::from_rgb(0x1C, 0x1C, 0x1E) } else { egui::Color32::from_rgb(0xE9, 0xE9, 0xEB) };
        c.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.process_events();
        self.handle_drops(&ctx);
        self.handle_shortcuts(&ctx);

        // Confirm before closing the window with unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let dirty: Vec<u64> = self.docs.iter().filter(|d| d.dirty()).map(|d| d.id).collect();
            if !dirty.is_empty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(crate::dialogs::Dialog::ConfirmClose { docs: dirty, quit: true });
            } else {
                self.remember_page();
                self.settings.save();
            }
        }

        self.self_test_screenshot(&ctx);
        crate::layout::show(self, ui);
        crate::dialogs::show(self, &ctx);
        crate::layout::toasts(self, &ctx);
        self.tiles.end_frame();
        self.toasts.retain(|t| t.until > Instant::now());
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self) {
        self.remember_page();
        self.settings.save();
    }
}

impl App {
    /// `REVIEW_SCREENSHOT=out.png review file.pdf` renders, saves a screenshot of the window and quits.
    /// Handy for checking the GPU path on a new machine.
    fn self_test_screenshot(&mut self, ctx: &egui::Context) {
        let Some(out) = std::env::var_os("REVIEW_SCREENSHOT") else { return };
        let loaded = self.docs.iter().all(|d| !matches!(d.state, LoadState::Loading));
        if !self.shot_requested && loaded && self.started.elapsed() > Duration::from_millis(1500) {
            self.shot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                let _ = buf.save(PathBuf::from(&out));
                eprintln!("review: screenshot saved to {}", PathBuf::from(&out).display());
            }
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    pub fn confirm_close_result(&mut self, docs: Vec<u64>, quit: bool, save: Option<bool>) {
        match save {
            None => {
                self.quit_after_confirm = false;
            }
            Some(false) => {
                for id in &docs {
                    if let Some(i) = self.doc_index(*id) {
                        self.docs[i].edits = 0;
                    }
                }
                if quit {
                    self.allow_close = true;
                    self.remember_page();
                    self.settings.save();
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    for id in docs {
                        if let Some(i) = self.doc_index(id) {
                            self.close_doc(i, true);
                        }
                    }
                }
            }
            Some(true) => {
                self.quit_after_confirm = quit;
                for id in docs.clone() {
                    if let Some(i) = self.doc_index(id) {
                        if self.docs[i].can_save_in_place() {
                            self.save(i);
                            if !quit {
                                // Closing waits for the save to land; close right away is safe since
                                // the worker keeps running until it reports back.
                            }
                        } else {
                            self.activate(i);
                            self.save(i);
                            return;
                        }
                    }
                }
                if !quit {
                    for id in docs {
                        if let Some(i) = self.doc_index(id) {
                            if !self.docs[i].saving {
                                self.close_doc(i, true);
                            }
                        }
                    }
                }
            }
        }
    }
}
