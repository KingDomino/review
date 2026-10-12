//! MuPDF engine.
//!
//! Each opened PDF (or XPS/EPUB/CBZ/SVG) gets its own worker thread that owns
//! the MuPDF document, because MuPDF documents must stay on one thread. The
//! worker builds display lists, which *are* thread-safe, and hands them to a
//! shared pool that rasterises tiles in parallel. Nothing here ever blocks the
//! UI thread: results come back as [`Event`]s.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use egui::{pos2, Color32, Pos2, Rect};
use mupdf::pdf::{
    Encryption, LineEndingStyle, PdfAnnotation, PdfAnnotationType, PdfDocument, PdfPage, PdfWriteOptions, Permission,
};
use mupdf::{
    color::AnnotationColor, Colorspace, Device, DisplayList, Document, Matrix, MetadataName,
    Pixmap, Point, Quad, TextPageFlags,
};

use crate::model::{
    bubble_points, round_rect_points, star_points, Align, Annot, FieldEdit, MarkupKind, MetaEdit,
    Security, Shape, Style,
};

// ---------------------------------------------------------------------------
// Public protocol

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub source: u64,
    pub page: u32,
    /// Index into the zoom ladder (see `view::scale_for_level`).
    pub level: i32,
    pub tx: i32,
    pub ty: i32,
    pub dark: bool,
}

pub const TILE: i32 = 512;

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub format: String,
    pub encryption: String,
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
    pub created: String,
    pub modified: String,
}

#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub title: String,
    pub page: Option<u32>,
    pub uri: Option<String>,
    pub children: Vec<OutlineNode>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text { multiline: bool },
    Checkbox { on: String },
    Radio { on: String },
    Choice { options: Vec<String> },
    Other,
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub page: u32,
    pub xref: i32,
    pub name: String,
    pub value: String,
    pub rect: Rect,
    pub kind: FieldKind,
    pub readonly: bool,
}

#[derive(Debug)]
pub struct SourceInfo {
    pub pages: Vec<(Rect, String)>,
    pub outline: Vec<OutlineNode>,
    pub meta: Meta,
    pub is_pdf: bool,
    pub encrypted: bool,
    pub permissions: Option<u32>,
    pub annots: Vec<(u32, Annot)>,
    pub fields: Vec<FieldInfo>,
}

#[derive(Clone, Debug)]
pub struct TChar {
    pub c: char,
    pub rect: Rect,
    pub line: u32,
}

#[derive(Debug, Default)]
pub struct PageText {
    pub chars: Vec<TChar>,
    pub lines: Vec<Rect>,
}

impl PageText {
    /// Index of the character nearest to `p` (page space), preferring the line under it.
    pub fn nearest(&self, p: Pos2) -> Option<usize> {
        if self.chars.is_empty() {
            return None;
        }
        let line = self
            .lines
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| dist_rect(**a, p).total_cmp(&dist_rect(**b, p)))
            .map(|(i, _)| i as u32)?;
        self.chars
            .iter()
            .enumerate()
            .filter(|(_, c)| c.line == line)
            .min_by(|(_, a), (_, b)| {
                let da = (a.rect.center().x - p.x).abs();
                let db = (b.rect.center().x - p.x).abs();
                da.total_cmp(&db)
            })
            .map(|(i, c)| if p.x > c.rect.center().x { i + 1 } else { i })
    }

    pub fn word_at(&self, i: usize) -> (usize, usize) {
        let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '-';
        let i = i.min(self.chars.len().saturating_sub(1));
        let mut a = i;
        let mut b = i;
        while a > 0 && is_word(self.chars[a - 1].c) && self.chars[a - 1].line == self.chars[i].line {
            a -= 1;
        }
        while b < self.chars.len() && is_word(self.chars[b].c) && self.chars[b].line == self.chars[i].line {
            b += 1;
        }
        if a == b {
            b = (a + 1).min(self.chars.len());
        }
        (a, b)
    }

    pub fn line_at(&self, i: usize) -> (usize, usize) {
        let i = i.min(self.chars.len().saturating_sub(1));
        let line = self.chars[i].line;
        let a = self.chars.iter().position(|c| c.line == line).unwrap_or(i);
        let b = self.chars.iter().rposition(|c| c.line == line).map(|x| x + 1).unwrap_or(i + 1);
        (a, b)
    }

    pub fn text(&self, a: usize, b: usize) -> String {
        let mut s = String::new();
        let mut last_line = None;
        for c in &self.chars[a.min(self.chars.len())..b.min(self.chars.len())] {
            if let Some(l) = last_line {
                if l != c.line && !s.ends_with(['-', ' ', '\n']) {
                    s.push('\n');
                }
            }
            last_line = Some(c.line);
            s.push(c.c);
        }
        s
    }

    /// One rectangle per line covering the characters `a..b`.
    pub fn rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let mut out: Vec<(u32, Rect)> = Vec::new();
        for c in &self.chars[a.min(self.chars.len())..b.min(self.chars.len())] {
            match out.last_mut() {
                Some((l, r)) if *l == c.line => *r = r.union(c.rect),
                _ => out.push((c.line, c.rect)),
            }
        }
        out.into_iter().map(|(_, r)| r).collect()
    }
}

fn dist_rect(r: Rect, p: Pos2) -> f32 {
    let dx = (r.min.x - p.x).max(0.0).max(p.x - r.max.x);
    let dy = (r.min.y - p.y).max(0.0).max(p.y - r.max.y);
    dy * 4.0 + dx
}

#[derive(Clone, Debug)]
pub enum LinkTarget {
    Page(u32, Option<f32>),
    Uri(String),
}

#[derive(Clone, Debug)]
pub struct LinkInfo {
    pub rect: Rect,
    pub target: LinkTarget,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub page: u32,
    pub rects: Vec<Rect>,
    pub context: String,
}

pub enum PlanSrc {
    Own(u32),
    External { path: PathBuf, password: Option<String>, index: u32 },
    Blank { w: f32, h: f32 },
}

pub struct PagePlan {
    pub src: PlanSrc,
    pub rot: i32,
    pub crop: Option<Rect>,
    /// Annotations to (re)create on this page.
    pub annots: Vec<Annot>,
    /// Imported annotations replaced by edited versions.
    pub replaced: Vec<i32>,
}

pub struct SavePlan {
    pub dest: PathBuf,
    pub pages: Vec<PagePlan>,
    pub fields: Vec<FieldEdit>,
    pub deleted: Vec<i32>,
    pub meta: Option<MetaEdit>,
    pub security: Option<Security>,
    pub author: String,
}

#[allow(dead_code)]
pub enum Event {
    Opened { source: u64, result: Result<SourceInfo, OpenError> },
    Tile { key: TileKey, image: egui::ColorImage },
    Text { source: u64, page: u32, text: Arc<PageText> },
    Links { source: u64, page: u32, links: Vec<LinkInfo> },
    SearchHits { source: u64, generation: u64, hits: Vec<SearchHit> },
    SearchDone { source: u64, generation: u64 },
    Saved { source: u64, result: Result<PathBuf, String> },
    PageImage { request: u64, result: Result<image::RgbaImage, String> },
}

#[derive(Debug, Clone)]
pub enum OpenError {
    NeedsPassword,
    WrongPassword,
    Failed(String),
}

enum Cmd {
    Render { key: TileKey, scale: f32, cancel: Arc<AtomicBool>, urgent: bool },
    Text { page: u32 },
    Links { page: u32 },
    Search { query: String, generation: u64 },
    Save(Box<SavePlan>),
    RenderPage { page: u32, scale: f32, request: u64, annots: bool },
}

/// Handle to one document worker. Dropping it stops the worker.
#[allow(dead_code)]
pub struct Source {
    pub id: u64,
    pub path: PathBuf,
    pub password: Option<String>,
    tx: Sender<Cmd>,
}

impl Source {
    /// `urgent` jobs (cheap whole-page previews) jump ahead of sharp tiles, so a page
    /// scrolling into view is never shown blank.
    pub fn render(&self, key: TileKey, scale: f32, cancel: Arc<AtomicBool>, urgent: bool) {
        let _ = self.tx.send(Cmd::Render { key, scale, cancel, urgent });
    }
    pub fn request_text(&self, page: u32) {
        let _ = self.tx.send(Cmd::Text { page });
    }
    pub fn request_links(&self, page: u32) {
        let _ = self.tx.send(Cmd::Links { page });
    }
    pub fn search(&self, query: String, generation: u64) {
        let _ = self.tx.send(Cmd::Search { query, generation });
    }
    pub fn save(&self, plan: SavePlan) {
        let _ = self.tx.send(Cmd::Save(Box::new(plan)));
    }
    pub fn render_page(&self, page: u32, scale: f32, request: u64, annots: bool) {
        let _ = self.tx.send(Cmd::RenderPage { page, scale, request, annots });
    }
}

/// Shared state: event channel back to the UI and the raster pool.
#[derive(Clone)]
pub struct Engine {
    events: Sender<Event>,
    pool: Arc<Pool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Engine {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> (Self, Receiver<Event>) {
        let (tx, rx) = unbounded();
        let pool = Pool::new(tx.clone(), repaint.clone());
        (Self { events: tx, pool, repaint }, rx)
    }

    pub fn open(&self, id: u64, path: &Path, password: Option<String>) -> Source {
        let (tx, rx) = unbounded();
        let engine = self.clone();
        let p = path.to_path_buf();
        let pw = password.clone();
        thread::Builder::new()
            .name(format!("review-doc-{id}"))
            .spawn(move || worker(engine, id, p, pw, rx))
            .expect("spawn document worker");
        Source { id, path: path.to_path_buf(), password, tx }
    }

    fn send(&self, e: Event) {
        let _ = self.events.send(e);
        (self.repaint)();
    }
}

// ---------------------------------------------------------------------------
// Raster pool: previews first, then LIFO so the most recently requested tiles
// (the ones on screen) come next.

struct Job {
    urgent: bool,
    key: TileKey,
    list: Arc<DisplayList>,
    origin: Pos2,
    scale: f32,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Job>,
    normal: VecDeque<Job>,
}

struct Pool {
    queue: Mutex<Queues>,
    cv: Condvar,
}

impl Pool {
    fn new(events: Sender<Event>, repaint: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        let pool = Arc::new(Self { queue: Mutex::new(Queues::default()), cv: Condvar::new() });
        let n = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8);
        for i in 0..n {
            let pool = pool.clone();
            let events = events.clone();
            let repaint = repaint.clone();
            thread::Builder::new()
                .name(format!("review-raster-{i}"))
                .spawn(move || loop {
                    let job = {
                        let mut q = pool.queue.lock().unwrap();
                        loop {
                            if let Some(j) = q.urgent.pop_back().or_else(|| q.normal.pop_back()) {
                                break j;
                            }
                            q = pool.cv.wait(q).unwrap();
                        }
                    };
                    if job.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Some(image) = raster_tile(&job) {
                        let _ = events.send(Event::Tile { key: job.key, image });
                        repaint();
                    }
                })
                .expect("spawn raster thread");
        }
        pool
    }

    fn push(&self, job: Job) {
        let mut q = self.queue.lock().unwrap();
        // Drop cancelled work so the queue cannot grow without bound while scrolling.
        q.urgent.retain(|j| !j.cancel.load(Ordering::Relaxed));
        q.normal.retain(|j| !j.cancel.load(Ordering::Relaxed));
        if job.urgent {
            q.urgent.push_back(job);
        } else {
            q.normal.push_back(job);
        }
        self.cv.notify_one();
    }
}

fn raster_tile(job: &Job) -> Option<egui::ColorImage> {
    let s = job.scale;
    let bounds = job.list.bounds();
    let full_w = ((bounds.x1 - bounds.x0) * s).ceil() as i32;
    let full_h = ((bounds.y1 - bounds.y0) * s).ceil() as i32;
    let x = job.key.tx * TILE;
    let y = job.key.ty * TILE;
    let w = TILE.min(full_w - x);
    let h = TILE.min(full_h - y);
    if w <= 0 || h <= 0 {
        return None;
    }
    let cs = Colorspace::device_rgb();
    let mut pix = Pixmap::new(&cs, x, y, w, h, false).ok()?;
    pix.clear_with(255).ok()?;
    {
        let dev = Device::from_pixmap(&pix).ok()?;
        let ctm = Matrix::new(s, 0.0, 0.0, s, -job.origin.x * s, -job.origin.y * s);
        let area = mupdf::Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32);
        job.list.run(&dev, &ctm, area).ok()?;
    }
    Some(pixmap_to_color_image(&pix, job.key.dark))
}

fn pixmap_to_color_image(pix: &Pixmap, dark: bool) -> egui::ColorImage {
    let (w, h) = (pix.width() as usize, pix.height() as usize);
    let n = pix.n() as usize;
    let stride = pix.stride() as usize;
    let samples = pix.samples();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let line = &samples[row * stride..row * stride + w * n];
        for px in line.chunks_exact(n) {
            let (r, g, b) = if n >= 3 { (px[0], px[1], px[2]) } else { (px[0], px[0], px[0]) };
            let (r, g, b) = if dark { dark_pixel(r, g, b) } else { (r, g, b) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

/// "Use Dark Appearance for PDF": invert lightness while keeping hue, and
/// map paper white to a soft dark grey instead of pure black.
fn dark_pixel(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f32, g as f32, b as f32);
    let y = 0.299 * rf + 0.587 * gf + 0.114 * bf;
    let y2 = 30.0 + (255.0 - y) * 0.86;
    let f = |c: f32| (c - y + y2).clamp(0.0, 255.0) as u8;
    (f(rf), f(gf), f(bf))
}

pub fn pixmap_to_rgba(pix: &Pixmap) -> image::RgbaImage {
    let img = pixmap_to_color_image(pix, false);
    let [w, h] = img.size;
    image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()).expect("pixmap size")
}

// ---------------------------------------------------------------------------
// Document worker

enum Doc {
    Pdf(PdfDocument),
    Other(Document),
}

impl Doc {
    fn g(&self) -> &Document {
        match self {
            Doc::Pdf(p) => p,
            Doc::Other(d) => d,
        }
    }
    fn pdf(&self) -> Option<&PdfDocument> {
        match self {
            Doc::Pdf(p) => Some(p),
            Doc::Other(_) => None,
        }
    }
    fn page(&self, i: u32) -> Result<mupdf::Page, mupdf::Error> {
        self.g().load_page(i as i32)
    }
}

fn open_doc(path: &Path, password: Option<&str>) -> Result<Doc, OpenError> {
    let p = path.to_string_lossy().to_string();
    let mut doc = Document::open(p.as_str()).map_err(|e| OpenError::Failed(e.to_string()))?;
    if doc.needs_password().unwrap_or(false) {
        match password {
            None => return Err(OpenError::NeedsPassword),
            Some(pw) => {
                if !doc.authenticate(pw).unwrap_or(false) {
                    return Err(OpenError::WrongPassword);
                }
            }
        }
    }
    if doc.is_reflowable().unwrap_or(false) {
        let _ = doc.layout(420.0, 595.0, 11.0);
    }
    if doc.is_pdf() {
        let pdf = PdfDocument::try_from(doc).map_err(|e| OpenError::Failed(e.to_string()))?;
        return Ok(Doc::Pdf(pdf));
    }
    Ok(Doc::Other(doc))
}

fn worker(engine: Engine, id: u64, path: PathBuf, password: Option<String>, rx: Receiver<Cmd>) {
    let doc = match open_doc(&path, password.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            engine.send(Event::Opened { source: id, result: Err(e) });
            return;
        }
    };
    let info = match load_info(&doc) {
        Ok(i) => i,
        Err(e) => {
            engine.send(Event::Opened { source: id, result: Err(OpenError::Failed(e.to_string())) });
            return;
        }
    };
    engine.send(Event::Opened { source: id, result: Ok(info) });

    let mut lists: HashMap<u32, Arc<DisplayList>> = HashMap::new();
    let mut list_order: VecDeque<u32> = VecDeque::new();
    let mut origins: HashMap<u32, Pos2> = HashMap::new();
    let mut texts: HashMap<u32, Arc<PageText>> = HashMap::new();
    let page_count = doc.g().page_count().unwrap_or(0).max(0) as u32;

    // Active search: (query, generation, next page)
    let mut search: Option<(String, u64, u32)> = None;

    loop {
        let cmd = if search.is_some() {
            match rx.try_recv() {
                Ok(c) => Some(c),
                Err(crossbeam_channel::TryRecvError::Empty) => None,
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(std::time::Duration::from_secs(3600)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(_) => return,
            }
        };

        match cmd {
            None => {}
            Some(Cmd::Render { key, scale, cancel, urgent }) => {
                if cancel.load(Ordering::Relaxed) || key.page >= page_count {
                    continue;
                }
                let list = match lists.get(&key.page) {
                    Some(l) => l.clone(),
                    None => {
                        let Ok(page) = doc.page(key.page) else { continue };
                        let Ok(bounds) = page.bounds() else { continue };
                        let Ok(list) = page.to_display_list(true) else { continue };
                        let list = Arc::new(list);
                        origins.insert(key.page, pos2(bounds.x0, bounds.y0));
                        lists.insert(key.page, list.clone());
                        list_order.push_back(key.page);
                        while list_order.len() > 48 {
                            if let Some(old) = list_order.pop_front() {
                                lists.remove(&old);
                            }
                        }
                        list
                    }
                };
                let origin = origins.get(&key.page).copied().unwrap_or(Pos2::ZERO);
                engine.pool.push(Job { urgent, key, list, origin, scale, cancel });
            }
            Some(Cmd::Text { page }) => {
                let text = page_text(&doc, page, &mut texts);
                engine.send(Event::Text { source: id, page, text });
            }
            Some(Cmd::Links { page }) => {
                let links = page_links(&doc, page);
                engine.send(Event::Links { source: id, page, links });
            }
            Some(Cmd::Search { query, generation }) => {
                search = if query.trim().is_empty() { None } else { Some((query, generation, 0)) };
                if search.is_none() {
                    engine.send(Event::SearchDone { source: id, generation });
                }
            }
            Some(Cmd::Save(plan)) => {
                let result = save(&path, password.as_deref(), &plan).map(|_| plan.dest.clone());
                engine.send(Event::Saved { source: id, result });
            }
            Some(Cmd::RenderPage { page, scale, request, annots }) => {
                let result = (|| -> Result<image::RgbaImage, mupdf::Error> {
                    let p = doc.page(page)?;
                    let pix = if annots {
                        p.to_pixmap(&Matrix::new_scale(scale, scale), &Colorspace::device_rgb(), false, true)?
                    } else {
                        let b = p.bounds()?;
                        let list = p.to_display_list(false)?;
                        let w = ((b.x1 - b.x0) * scale).ceil() as i32;
                        let h = ((b.y1 - b.y0) * scale).ceil() as i32;
                        let mut pix = Pixmap::new(&Colorspace::device_rgb(), 0, 0, w, h, false)?;
                        pix.clear_with(255)?;
                        {
                            let dev = Device::from_pixmap(&pix)?;
                            let ctm = Matrix::new(scale, 0.0, 0.0, scale, -b.x0 * scale, -b.y0 * scale);
                            list.run(&dev, &ctm, mupdf::Rect::new(0.0, 0.0, w as f32, h as f32))?;
                        }
                        pix
                    };
                    Ok(pixmap_to_rgba(&pix))
                })()
                .map_err(|e| e.to_string());
                engine.send(Event::PageImage { request, result });
            }
        }

        // Advance an active search by one page per loop so other commands stay responsive.
        if let Some((query, generation, next)) = search.as_mut() {
            if *next >= page_count {
                engine.send(Event::SearchDone { source: id, generation: *generation });
                search = None;
            } else {
                let page = *next;
                *next += 1;
                let text = page_text(&doc, page, &mut texts);
                let hits = search_page(&doc, page, query, &text);
                if !hits.is_empty() {
                    engine.send(Event::SearchHits { source: id, generation: *generation, hits });
                }
            }
        }
    }
}

fn load_info(doc: &Doc) -> Result<SourceInfo, mupdf::Error> {
    let n = doc.g().page_count()?.max(0) as u32;
    let mut pages = Vec::with_capacity(n as usize);
    let mut annots = Vec::new();
    let mut fields = Vec::new();
    for i in 0..n {
        let label = doc.pdf().and_then(|p| p.page_label(i as usize).ok()).unwrap_or_default();
        let label = if label.is_empty() || label == (i + 1).to_string() { String::new() } else { label };
        match doc.pdf() {
            Some(pdf) => {
                let mut page = pdf.load_pdf_page(i as i32)?;
                let b = page.bounds()?;
                pages.push((Rect::from_min_max(pos2(b.x0, b.y0), pos2(b.x1, b.y1)), label));
                import_annots(&mut page, i, &mut annots);
                import_fields(&page, i, &mut fields);
            }
            None => {
                let page = doc.g().load_page(i as i32)?;
                let b = page.bounds()?;
                pages.push((Rect::from_min_max(pos2(b.x0, b.y0), pos2(b.x1, b.y1)), label));
            }
        }
    }
    let md = |k| doc.g().metadata(k).unwrap_or_default();
    let meta = Meta {
        format: md(MetadataName::Format),
        encryption: md(MetadataName::Encryption),
        title: md(MetadataName::Title),
        author: md(MetadataName::Author),
        subject: md(MetadataName::Subject),
        keywords: md(MetadataName::Keywords),
        creator: md(MetadataName::Creator),
        producer: md(MetadataName::Producer),
        created: pretty_pdf_date(&md(MetadataName::CreationDate)),
        modified: pretty_pdf_date(&md(MetadataName::ModDate)),
    };
    let outline = doc.g().outlines().map(|o| convert_outline(&o)).unwrap_or_default();
    let encrypted = !meta.encryption.is_empty() && meta.encryption != "None";
    let permissions = doc.pdf().map(|p| p.permissions().bits());
    Ok(SourceInfo { pages, outline, meta, is_pdf: doc.pdf().is_some(), encrypted, permissions, annots, fields })
}

fn convert_outline(items: &[mupdf::Outline]) -> Vec<OutlineNode> {
    items
        .iter()
        .map(|o| OutlineNode {
            title: o.title.trim().to_string(),
            page: o.dest.map(|d| d.loc.page_number),
            uri: o.uri.clone().filter(|u| !u.starts_with('#')),
            children: convert_outline(&o.down),
        })
        .collect()
}

/// "D:20240131120000+01'00'" -> "2024-01-31 12:00"
pub fn pretty_pdf_date(s: &str) -> String {
    let d = s.trim_start_matches("D:");
    if d.len() >= 12 && d[..12].chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{} {}:{}", &d[0..4], &d[4..6], &d[6..8], &d[8..10], &d[10..12])
    } else if d.len() >= 8 && d[..8].chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
    } else {
        s.to_string()
    }
}

fn page_text(doc: &Doc, page: u32, cache: &mut HashMap<u32, Arc<PageText>>) -> Arc<PageText> {
    if let Some(t) = cache.get(&page) {
        return t.clone();
    }
    let mut out = PageText::default();
    if let Ok(p) = doc.page(page) {
        if let Ok(tp) = p.to_text_page(TextPageFlags::empty()) {
            let mut line_no = 0u32;
            for block in tp.blocks() {
                for line in block.lines() {
                    let mut lr = Rect::NOTHING;
                    let mut any = false;
                    for ch in line.chars() {
                        let Some(c) = ch.char() else { continue };
                        let q = ch.quad();
                        let mut r = Rect::NOTHING;
                        for pt in [q.ul, q.ur, q.ll, q.lr] {
                            r.extend_with(pos2(pt.x, pt.y));
                        }
                        if r.is_negative() {
                            continue;
                        }
                        lr = lr.union(r);
                        any = true;
                        out.chars.push(TChar { c, rect: r, line: line_no });
                    }
                    if any {
                        out.lines.push(lr);
                        line_no += 1;
                    }
                }
            }
        }
    }
    let t = Arc::new(out);
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(page, t.clone());
    t
}

fn page_links(doc: &Doc, page: u32) -> Vec<LinkInfo> {
    let Ok(p) = doc.page(page) else { return Vec::new() };
    let Ok(links) = p.links() else { return Vec::new() };
    links
        .filter_map(|l| {
            let rect = Rect::from_min_max(pos2(l.bounds.x0, l.bounds.y0), pos2(l.bounds.x1, l.bounds.y1));
            let target = match l.dest {
                Some(d) => {
                    let y = match d.kind {
                        mupdf::DestinationKind::XYZ { top, .. } => top,
                        mupdf::DestinationKind::FitH { top } | mupdf::DestinationKind::FitBH { top } => top,
                        _ => None,
                    };
                    LinkTarget::Page(d.loc.page_number, y)
                }
                None if !l.uri.is_empty() => LinkTarget::Uri(l.uri.clone()),
                None => return None,
            };
            Some(LinkInfo { rect, target })
        })
        .collect()
}

fn search_page(doc: &Doc, page: u32, query: &str, text: &PageText) -> Vec<SearchHit> {
    let Ok(p) = doc.page(page) else { return Vec::new() };
    let Ok(tp) = p.to_text_page(TextPageFlags::empty()) else { return Vec::new() };
    let mut groups: Vec<Vec<Quad>> = Vec::new();
    let _ = tp.search_cb(query, &mut groups, |g, quads: &[Quad]| {
        g.push(quads.to_vec());
        mupdf::text_page::SearchHitResponse::ContinueSearch
    });
    groups
        .into_iter()
        .take(2000)
        .map(|quads| {
            let rects: Vec<Rect> = quads
                .iter()
                .map(|q| {
                    let mut r = Rect::NOTHING;
                    for pt in [q.ul, q.ur, q.ll, q.lr] {
                        r.extend_with(pos2(pt.x, pt.y));
                    }
                    r
                })
                .collect();
            let context = rects
                .first()
                .and_then(|r| {
                    let c = r.center();
                    text.chars.iter().position(|ch| ch.rect.expand(0.5).contains(c))
                })
                .map(|i| {
                    let (a, b) = text.line_at(i);
                    let start = a.max(i.saturating_sub(40));
                    let end = b.min(i + 60);
                    text.text(start, end).replace('\n', " ")
                })
                .unwrap_or_default();
            SearchHit { page, rects, context }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Annotation import / export

fn to_color(c: Option<AnnotationColor>) -> Option<Color32> {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match c? {
        AnnotationColor::Gray(g) => Some(Color32::from_gray(f(g))),
        AnnotationColor::Rgb { red, green, blue } => Some(Color32::from_rgb(f(red), f(green), f(blue))),
        AnnotationColor::Cmyk { cyan, magenta, yellow, key } => Some(Color32::from_rgb(
            f((1.0 - cyan) * (1.0 - key)),
            f((1.0 - magenta) * (1.0 - key)),
            f((1.0 - yellow) * (1.0 - key)),
        )),
    }
}

fn from_color(c: Color32) -> AnnotationColor {
    AnnotationColor::Rgb { red: c.r() as f32 / 255.0, green: c.g() as f32 / 255.0, blue: c.b() as f32 / 255.0 }
}

fn mrect(r: mupdf::Rect) -> Rect {
    Rect::from_min_max(pos2(r.x0, r.y0), pos2(r.x1, r.y1))
}

fn rect_m(r: Rect) -> mupdf::Rect {
    mupdf::Rect::new(r.min.x, r.min.y, r.max.x, r.max.y)
}

fn pt(p: Pos2) -> Point {
    Point { x: p.x, y: p.y }
}

fn import_one(a: &PdfAnnotation) -> Option<Annot> {
    let kind = a.r#type().ok()?;
    let rect = mrect(a.rect().ok()?);
    let color = to_color(a.color().ok().flatten());
    let fill = to_color(a.interior_color().ok().flatten());
    let width = a.border_width().unwrap_or(1.0).max(0.5);
    let mut style = Style { stroke: color.unwrap_or(Color32::BLACK), fill, width, ..Style::default() };
    let p = |q: Point| pos2(q.x, q.y);
    let shape = match kind {
        PdfAnnotationType::Square => Shape::Rect(rect.shrink(width / 2.0)),
        PdfAnnotationType::Circle => Shape::Oval(rect.shrink(width / 2.0)),
        PdfAnnotationType::Line => {
            let (a1, b1) = a.line().ok()?;
            let (s, e) = a.line_ending_styles().unwrap_or((LineEndingStyle::None, LineEndingStyle::None));
            let arrowish = |l: LineEndingStyle| {
                matches!(l, LineEndingStyle::OpenArrow | LineEndingStyle::ClosedArrow)
            };
            if arrowish(s) && !arrowish(e) {
                Shape::Line { a: p(b1), b: p(a1), arrow: true }
            } else {
                Shape::Line { a: p(a1), b: p(b1), arrow: arrowish(e) }
            }
        }
        PdfAnnotationType::Ink => {
            let strokes: Vec<Vec<Pos2>> =
                a.ink_list().ok()?.into_iter().map(|s| s.into_iter().map(p).collect()).collect();
            if strokes.is_empty() {
                return None;
            }
            Shape::Ink(strokes)
        }
        PdfAnnotationType::Polygon => Shape::Polygon(a.vertices().ok()?.into_iter().map(p).collect()),
        PdfAnnotationType::PolyLine => Shape::Ink(vec![a.vertices().ok()?.into_iter().map(p).collect()]),
        PdfAnnotationType::Highlight
        | PdfAnnotationType::Underline
        | PdfAnnotationType::StrikeOut
        | PdfAnnotationType::Squiggly => {
            let kind = match kind {
                PdfAnnotationType::Highlight => MarkupKind::Highlight,
                PdfAnnotationType::StrikeOut => MarkupKind::Strike,
                _ => MarkupKind::Underline,
            };
            let quads = a.quad_points().ok()?;
            if quads.is_empty() {
                return None;
            }
            Shape::Markup { kind, quads: quads.into_iter().map(|q| [p(q.ul), p(q.ur), p(q.lr), p(q.ll)]).collect() }
        }
        PdfAnnotationType::FreeText => {
            if let Ok(Some(da)) = a.default_appearance() {
                style.font_size = da.size.max(4.0);
                style.text_color = to_color(da.color).unwrap_or(Color32::BLACK);
            }
            style.fill = color;
            style.align = match a.quadding() {
                Ok(mupdf::pdf::AnnotationTextAlign::Center) => Align::Center,
                Ok(mupdf::pdf::AnnotationTextAlign::Right) => Align::Right,
                _ => Align::Left,
            };
            style.width = 0.0;
            Shape::Text { rect, text: a.contents().ok().flatten().unwrap_or("").to_string() }
        }
        PdfAnnotationType::Text => {
            style.stroke = color.unwrap_or(Color32::from_rgb(0xFF, 0xD6, 0x0A));
            Shape::Note { at: rect.min, text: a.contents().ok().flatten().unwrap_or("").to_string() }
        }
        _ => return None,
    };
    let mut annot = Annot::new(shape, style);
    annot.author = a.author().ok().flatten().unwrap_or("").to_string();
    annot.origin = a.xref().ok();
    annot.dirty = false;
    Some(annot)
}

fn import_annots(page: &mut PdfPage, index: u32, out: &mut Vec<(u32, Annot)>) {
    let mut hide = Vec::new();
    for a in page.annotations() {
        if let Some(x) = import_one(&a) {
            out.push((index, x));
            hide.push(a);
        }
    }
    // Hide imported annotations in this in-memory copy: Review draws them itself so they can
    // be edited. Saving always starts from a fresh copy of the file.
    for mut a in hide {
        if let Ok(f) = a.flags() {
            let _ = a.set_flags(f | mupdf::pdf::AnnotationFlags::IS_HIDDEN);
        }
    }
}

fn import_fields(page: &PdfPage, index: u32, out: &mut Vec<FieldInfo>) {
    use mupdf::pdf::WidgetType;
    for w in page.widgets() {
        let Ok(t) = w.r#type() else { continue };
        let Ok(rect) = w.annotation().rect() else { continue };
        let obj = w.annotation().object();
        let on_state = || -> String {
            (|| -> Option<String> {
                let ap = obj.get_dict("AP").ok()??;
                let n = ap.get_dict("N").ok()??;
                for i in 0..n.dict_len().ok()? {
                    let k = n.get_dict_key(i as i32).ok()??;
                    let name = String::from_utf8_lossy(&k.as_name().ok()?).to_string();
                    if name != "Off" {
                        return Some(name);
                    }
                }
                None
            })()
            .unwrap_or_else(|| "Yes".into())
        };
        let kind = match t {
            WidgetType::Text => FieldKind::Text {
                multiline: w.field_flags().map(|f| f.bits() & (1 << 12) != 0).unwrap_or(false),
            },
            WidgetType::Checkbox => FieldKind::Checkbox { on: on_state() },
            WidgetType::RadioButton => FieldKind::Radio { on: on_state() },
            WidgetType::Combobox | WidgetType::Listbox => {
                let mut options = Vec::new();
                if let Ok(Some(opt)) = obj.get_dict_inheritable("Opt") {
                    for i in 0..opt.len().unwrap_or(0) {
                        if let Ok(Some(o)) = opt.get_array(i as i32) {
                            let s = if o.is_array().unwrap_or(false) {
                                o.get_array(1).ok().flatten().and_then(|x| x.as_string().ok())
                            } else {
                                o.as_string().ok()
                            };
                            if let Some(s) = s {
                                options.push(s);
                            }
                        }
                    }
                }
                FieldKind::Choice { options }
            }
            _ => FieldKind::Other,
        };
        out.push(FieldInfo {
            page: index,
            xref: w.xref().unwrap_or(0),
            name: w.name().ok().flatten().unwrap_or_default(),
            value: w.value().ok().flatten().unwrap_or_default(),
            rect: mrect(rect),
            kind,
            readonly: w.is_readonly().unwrap_or(false),
        });
    }
}

fn write_annot(page: &mut PdfPage, a: &Annot, author: &str) -> Result<(), mupdf::Error> {
    let st = &a.style;
    let poly = |page: &mut PdfPage, pts: Vec<Pos2>| -> Result<PdfAnnotation, mupdf::Error> {
        let mut an = page.create_annotation(PdfAnnotationType::Polygon)?;
        an.set_vertices(pts.into_iter().map(pt))?;
        Ok(an)
    };
    let mut annot = match &a.shape {
        Shape::Rect(r) => page.add_square_annotation(rect_m(r.expand(st.width / 2.0)))?,
        Shape::Oval(r) => page.add_circle_annotation(rect_m(r.expand(st.width / 2.0)))?,
        Shape::RoundRect(r) => poly(page, round_rect_points(*r))?,
        Shape::Star(r) => poly(page, star_points(*r))?,
        Shape::Bubble(r) => poly(page, bubble_points(*r))?,
        Shape::Polygon(p) => poly(page, p.clone())?,
        Shape::Line { a: p0, b: p1, arrow } => {
            let mut an = page.create_annotation(PdfAnnotationType::Line)?;
            an.set_line(pt(*p0), pt(*p1))?;
            if *arrow {
                an.set_line_ending_styles(LineEndingStyle::None, LineEndingStyle::OpenArrow)?;
            }
            an
        }
        Shape::Ink(strokes) | Shape::Signature { strokes, .. } => {
            let s: Vec<Vec<Point>> =
                strokes.iter().filter(|s| !s.is_empty()).map(|s| s.iter().map(|p| pt(*p)).collect()).collect();
            if s.is_empty() {
                return Ok(());
            }
            page.add_ink_annotation(s)?
        }
        Shape::Text { rect, text } => {
            let mut an = page.add_free_text_annotation(rect_m(*rect), text)?;
            an.set_default_appearance("Helv", st.font_size, Some(from_color(st.text_color)))?;
            an.set_quadding(match st.align {
                Align::Left => mupdf::pdf::AnnotationTextAlign::Left,
                Align::Center => mupdf::pdf::AnnotationTextAlign::Center,
                Align::Right => mupdf::pdf::AnnotationTextAlign::Right,
            })?;
            an.set_border_width(0.0)?;
            if let Some(f) = st.fill {
                an.set_color(from_color(f))?;
            }
            if !author.is_empty() {
                an.set_author(author)?;
            }
            an.update()?;
            return Ok(());
        }
        Shape::Note { at, text } => {
            let mut an = page.add_text_annotation(mupdf::Rect::new(at.x, at.y, at.x + 20.0, at.y + 20.0), text)?;
            an.set_color(from_color(st.stroke))?;
            an.set_icon_name("Note")?;
            if !author.is_empty() {
                an.set_author(author)?;
            }
            an.update()?;
            return Ok(());
        }
        Shape::Markup { kind, quads } => {
            let q: Vec<Quad> = quads
                .iter()
                .map(|q| Quad::new(pt(q[0]), pt(q[1]), pt(q[3]), pt(q[2])))
                .collect();
            if q.is_empty() {
                return Ok(());
            }
            match kind {
                MarkupKind::Highlight => page.add_highlight_annotation(q)?,
                MarkupKind::Underline => page.add_underline_annotation(q)?,
                MarkupKind::Strike => page.add_strikeout_annotation(q)?,
            }
        }
        Shape::Redact(r) => page.add_redact_annotation(rect_m(*r))?,
    };
    match &a.shape {
        Shape::Markup { .. } => {
            annot.set_color(from_color(st.stroke))?;
        }
        Shape::Redact(_) => {}
        _ => {
            annot.set_color(from_color(st.stroke))?;
            annot.set_border_width(st.width)?;
            if let Some(f) = st.fill {
                if !matches!(a.shape, Shape::Ink(_) | Shape::Line { .. } | Shape::Signature { .. }) {
                    annot.set_interior_color(from_color(f))?;
                }
            }
        }
    }
    if !author.is_empty() && !matches!(a.shape, Shape::Redact(_)) {
        annot.set_author(if a.author.is_empty() { author } else { &a.author })?;
    }
    annot.update()?;
    Ok(())
}

fn find_annot_by_xref(page: &PdfPage, xref: i32) -> Option<PdfAnnotation> {
    page.annotations().find(|a| a.xref().ok() == Some(xref))
}

fn importable(t: PdfAnnotationType) -> bool {
    use PdfAnnotationType::*;
    matches!(t, Square | Circle | Line | Ink | Polygon | PolyLine | Highlight | Underline | StrikeOut | Squiggly | FreeText | Text)
}

// ---------------------------------------------------------------------------
// Saving

pub fn save(path: &Path, password: Option<&str>, plan: &SavePlan) -> Result<(), String> {
    let e = |e: mupdf::Error| e.to_string();
    let Doc::Pdf(mut pdf) = open_doc(path, password).map_err(|e| format!("{e:?}"))? else {
        return Err("Only PDF documents can be saved as PDF.".into());
    };
    let original_count = pdf.page_count().map_err(e)? as usize;

    // 1. Bring every needed page into the document, remembering its index.
    let mut externals: HashMap<PathBuf, PdfDocument> = HashMap::new();
    let mut index_of: Vec<usize> = Vec::with_capacity(plan.pages.len());
    let mut own_used = vec![false; original_count];
    let mut fresh_pages: Vec<bool> = Vec::new(); // true for pages that did not come from this file
    for pp in &plan.pages {
        let count = pdf.page_count().map_err(e)? as usize;
        match &pp.src {
            PlanSrc::Own(i) if (*i as usize) < original_count && !own_used[*i as usize] => {
                own_used[*i as usize] = true;
                index_of.push(*i as usize);
                fresh_pages.push(false);
            }
            PlanSrc::Own(i) => {
                // Same page used twice: append a copy.
                pdf.copy_page(*i as usize, mupdf::pdf::InsertPosition::Append).map_err(e)?;
                index_of.push(count);
                fresh_pages.push(true);
            }
            PlanSrc::External { path, password, index } => {
                if !externals.contains_key(path) {
                    let Doc::Pdf(ext) = open_doc(path, password.as_deref()).map_err(|e| format!("{e:?}"))? else {
                        return Err("Inserted pages must come from a PDF".into());
                    };
                    externals.insert(path.clone(), ext);
                }
                let src = &externals[path];
                let opts = mupdf::pdf::InsertPdfOptions {
                    source_pages: mupdf::pdf::PageSelection::Pages(vec![*index as usize]),
                    ..Default::default()
                };
                pdf.insert_pdf(src, opts).map_err(e)?;
                index_of.push(count);
                fresh_pages.push(true);
            }
            PlanSrc::Blank { w, h } => {
                pdf.new_page(mupdf::Size { width: *w, height: *h }).map_err(e)?;
                index_of.push(count);
                fresh_pages.push(true);
            }
        }
    }

    // 2. Per-page edits while indices are still stable.
    let mut has_redactions = Vec::new();
    for (k, pp) in plan.pages.iter().enumerate() {
        let idx = index_of[k];
        let mut page = pdf.load_pdf_page(idx as i32).map_err(e)?;
        if fresh_pages[k] {
            // Pages from elsewhere carry their own copies of annotations Review imported;
            // the slot's annotation list is authoritative.
            let doomed: Vec<PdfAnnotation> =
                page.annotations().filter(|a| a.r#type().map(importable).unwrap_or(false)).collect();
            for a in doomed {
                page.delete_annotation(a).map_err(e)?;
            }
        } else {
            for x in pp.replaced.iter().chain(plan.deleted.iter()) {
                if let Some(a) = find_annot_by_xref(&page, *x) {
                    page.delete_annotation(a).map_err(e)?;
                }
            }
        }
        let mut redact = false;
        for a in &pp.annots {
            redact |= matches!(a.shape, Shape::Redact(_));
            write_annot(&mut page, a, &plan.author).map_err(e)?;
        }
        has_redactions.push(redact);
        if !fresh_pages[k] {
            for f in plan.fields.iter().filter(|f| f.page as usize == idx) {
                for mut w in page.widgets() {
                    if w.xref().ok() == Some(f.xref) {
                        let _ = w.set_value(&mut pdf, &f.value, true);
                        let _ = w.update();
                    }
                }
            }
        }
        if let Some(c) = pp.crop {
            let ctm = page.ctm().map_err(e)?;
            if let Some(inv) = ctm.invert() {
                let r = rect_m(c).transform(&inv);
                let mut arr = pdf.new_array().map_err(e)?;
                for v in [r.x0, r.y0, r.x1, r.y1] {
                    arr.array_push(mupdf::pdf::PdfObject::new_real(v).map_err(e)?).map_err(e)?;
                }
                page.object().dict_put("CropBox", arr).map_err(e)?;
            }
        }
        if pp.rot % 360 != 0 {
            let r = page.rotation().unwrap_or(0);
            page.set_rotation((r + pp.rot).rem_euclid(360)).map_err(e)?;
        }
    }
    for (k, redact) in has_redactions.iter().enumerate() {
        if *redact {
            let mut page = pdf.load_pdf_page(index_of[k] as i32).map_err(e)?;
            page.apply_redactions().map_err(e)?;
        }
    }

    // 3. Drop pages that are no longer used, then put the rest in order.
    let keep: std::collections::HashSet<usize> = index_of.iter().copied().collect();
    let total = pdf.page_count().map_err(e)? as usize;
    let mut current: Vec<usize> = (0..total).collect();
    for i in (0..total).rev() {
        if !keep.contains(&i) {
            pdf.delete_page(i as i32).map_err(e)?;
            current.remove(i);
        }
    }
    for (target, want) in index_of.iter().enumerate() {
        let pos = current.iter().position(|x| x == want).ok_or("page bookkeeping failed")?;
        if pos != target {
            pdf.move_page(pos, target).map_err(e)?;
            let v = current.remove(pos);
            current.insert(target, v);
        }
    }

    // 4. Metadata.
    if let Some(m) = &plan.meta {
        let mut trailer = pdf.trailer().map_err(e)?;
        let mut info = match trailer.get_dict("Info").map_err(e)? {
            Some(i) => i,
            None => {
                let d = pdf.new_dict().map_err(e)?;
                let d = pdf.add_object(&d).map_err(e)?;
                trailer.dict_put("Info", d.try_clone().map_err(e)?).map_err(e)?;
                d
            }
        };
        for (k, v) in [("Title", &m.title), ("Author", &m.author), ("Subject", &m.subject), ("Keywords", &m.keywords)] {
            if v.is_empty() {
                let _ = info.dict_delete(k);
            } else {
                info.dict_put(k, mupdf::pdf::PdfObject::new_string(v).map_err(e)?).map_err(e)?;
            }
        }
    }

    // 5. Write next to the destination, then atomically replace it.
    let mut opts = PdfWriteOptions::default();
    opts.set_garbage_level(1).set_compress(true);
    match &plan.security {
        Some(s) if !s.user_password.is_empty() || !s.owner_password.is_empty() => {
            let mut perm = Permission::ACCESSIBILITY;
            if s.allow_print {
                perm |= Permission::PRINT | Permission::PRINT_HQ;
            }
            if s.allow_copy {
                perm |= Permission::COPY;
            }
            if s.allow_modify {
                perm |= Permission::MODIFY;
            }
            if s.allow_annotate {
                perm |= Permission::ANNOTATE;
            }
            if s.allow_forms {
                perm |= Permission::FORM;
            }
            if s.allow_assemble {
                perm |= Permission::ASSEMBLE;
            }
            let owner = if s.owner_password.is_empty() { &s.user_password } else { &s.owner_password };
            opts.set_encryption(Encryption::Aes256)
                .set_permissions(perm)
                .set_owner_password(owner)
                .set_user_password(&s.user_password);
        }
        Some(_) => {
            opts.set_encryption(Encryption::None);
        }
        None => {
            opts.set_encryption(if password.is_some() { Encryption::Keep } else { Encryption::None });
        }
    }
    let tmp = tmp_path(&plan.dest);
    let tmp_s = tmp.to_string_lossy().to_string();
    if let Err(err) = pdf.save_with_options(&tmp_s, opts) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.to_string());
    }
    std::fs::rename(&tmp, &plan.dest).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        err.to_string()
    })
}

pub fn tmp_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    dest.with_file_name(format!(".{name}.review-{}.tmp", std::process::id()))
}

/// Build a PDF from images (one page per image, sized at `dpi`).
pub fn images_to_pdf(images: &[&image::RgbaImage], dpi: f32, dest: &Path) -> Result<(), String> {
    let e = |e: mupdf::Error| e.to_string();
    let tmp = tmp_path(dest);
    {
        let mut w = mupdf::DocumentWriter::new(tmp.to_string_lossy().as_ref(), "pdf", "compress").map_err(e)?;
        for img in images {
            let (iw, ih) = (img.width() as i32, img.height() as i32);
            let mut pix = Pixmap::new(&Colorspace::device_rgb(), 0, 0, iw, ih, true).map_err(e)?;
            {
                let samples = pix.samples_mut();
                samples.copy_from_slice(img.as_raw());
            }
            let image = mupdf::Image::from_pixmap(&pix).map_err(e)?;
            let (pw, ph) = (iw as f32 * 72.0 / dpi, ih as f32 * 72.0 / dpi);
            let dev = w.begin_page(mupdf::Rect::new(0.0, 0.0, pw, ph)).map_err(e)?;
            dev.fill_image(&image, &Matrix::new(pw, 0.0, 0.0, ph, 0.0, 0.0), 1.0, Default::default())
                .map_err(e)?;
            w.end_page(dev).map_err(e)?;
        }
    }
    std::fs::rename(&tmp, dest).map_err(|err| err.to_string())
}

/// Convert any MuPDF-readable document (EPUB, XPS, CBZ, SVG…) to a PDF file.
pub fn convert_to_pdf(path: &Path, password: Option<&str>, dest: &Path) -> Result<(), String> {
    let doc = open_doc(path, password).map_err(|e| format!("{e:?}"))?;
    let n = doc.g().page_count().map_err(|e| e.to_string())?;
    let pdf = doc.g().convert_to_pdf(0, n - 1, 0).map_err(|e| e.to_string())?;
    let tmp = tmp_path(dest);
    pdf.save(tmp.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// Render one page to pixels on the calling thread (used by background export jobs).
pub fn render_page_image(path: &Path, password: Option<&str>, index: u32, scale: f32) -> Result<image::RgbaImage, String> {
    let doc = open_doc(path, password).map_err(|e| format!("{e:?}"))?;
    let page = doc.page(index).map_err(|e| e.to_string())?;
    let pix = page
        .to_pixmap(&Matrix::new_scale(scale, scale), &Colorspace::device_rgb(), false, true)
        .map_err(|e| e.to_string())?;
    Ok(pixmap_to_rgba(&pix))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::model::{Annot, Shape, Style};

    /// A small hand-written 3-page PDF; MuPDF repairs the missing xref table.
    pub fn sample_pdf(dir: &Path) -> PathBuf {
        let mut s = String::from("%PDF-1.4\n");
        s += "1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n";
        s += "2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >> endobj\n";
        for (i, word) in ["Alpha", "Bravo", "Charlie"].iter().enumerate() {
            let page = 3 + i;
            let content = 6 + i;
            s += &format!("{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 9 0 R >> >> /Contents {content} 0 R >> endobj\n");
            let text = format!("BT /F1 24 Tf 72 700 Td (Page {} {word} needle) Tj ET", i + 1);
            s += &format!("{content} 0 obj << /Length {} >> stream\n{text}\nendstream endobj\n", text.len());
        }
        s += "9 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n";
        s += "trailer << /Root 1 0 R /Size 10 >>\n%%EOF\n";
        let p = dir.join("sample.pdf");
        std::fs::write(&p, s).unwrap();
        p
    }

    /// An `n`-page text PDF for performance checks.
    pub fn big_pdf(dir: &Path, n: usize) -> PathBuf {
        let mut s = String::from("%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n");
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 10 + i * 2)).collect();
        s += &format!("2 0 obj << /Type /Pages /Kids [{}] /Count {n} >> endobj\n", kids.join(" "));
        s += "3 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n";
        for i in 0..n {
            let mut text = String::from("BT /F1 11 Tf 72 740 Td 14 TL ");
            for l in 0..45 {
                text += &format!("(Page {} line {l}: the quick brown fox jumps over the lazy dog) '", i + 1);
            }
            text += " ET";
            s += &format!("{} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >> endobj\n", 10 + i * 2, 11 + i * 2);
            s += &format!("{} 0 obj << /Length {} >> stream\n{text}\nendstream endobj\n", 11 + i * 2, text.len());
        }
        s += &format!("trailer << /Root 1 0 R /Size {} >>\n%%EOF\n", 11 + n * 2);
        let p = dir.join("big.pdf");
        std::fs::write(&p, s).unwrap();
        p
    }

    #[test]
    #[ignore = "performance numbers; run with --ignored --nocapture"]
    fn perf_big_pdf() {
        let dir = tmpdir("perf");
        let path = big_pdf(&dir, 2000);
        // Re-save through MuPDF so the file has a proper xref, like real-world PDFs.
        let fixed = dir.join("big-fixed.pdf");
        PdfDocument::open(path.to_string_lossy().as_ref()).unwrap().save(fixed.to_string_lossy().as_ref()).unwrap();
        let (eng, rx) = engine();
        let t0 = std::time::Instant::now();
        let src = eng.open(1, &fixed, None);
        let info = wait(&rx, |e| match e {
            Event::Opened { result, .. } => Some(result.unwrap()),
            _ => None,
        });
        let opened = t0.elapsed();
        src.render(TileKey { source: 1, page: 0, level: 0, tx: 0, ty: 0, dark: false }, 1.0, Arc::new(AtomicBool::new(false)), false);
        wait(&rx, |e| matches!(e, Event::Tile { .. }).then_some(()));
        let first_tile = t0.elapsed();
        let t1 = std::time::Instant::now();
        src.search("lazy dog".into(), 1);
        let mut hits = 0;
        wait(&rx, |e| match e {
            Event::SearchHits { hits: h, .. } => {
                hits += h.len();
                None
            }
            Event::SearchDone { .. } => Some(()),
            _ => None,
        });
        eprintln!("pages={} open={opened:?} first_tile={first_tile:?} search={:?} hits={hits}", info.pages.len(), t1.elapsed());
    }

    fn engine() -> (Engine, Receiver<Event>) {
        Engine::new(Arc::new(|| {}))
    }

    fn wait<T>(rx: &Receiver<Event>, mut f: impl FnMut(Event) -> Option<T>) -> T {
        loop {
            let e = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("engine event");
            if let Some(t) = f(e) {
                return t;
            }
        }
    }

    pub fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("review-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn open_render_text_search_save() {
        let dir = tmpdir("engine");
        let path = sample_pdf(&dir);
        let (eng, rx) = engine();
        let src = eng.open(1, &path, None);
        let info = wait(&rx, |e| match e {
            Event::Opened { result, .. } => Some(result.expect("opens")),
            _ => None,
        });
        assert_eq!(info.pages.len(), 3);
        assert!(info.is_pdf);
        assert!((info.pages[0].0.width() - 612.0).abs() < 0.1);

        let key = TileKey { source: 1, page: 0, level: 0, tx: 0, ty: 0, dark: false };
        src.render(key, 1.0, Arc::new(AtomicBool::new(false)), false);
        let img = wait(&rx, |e| match e {
            Event::Tile { image, .. } => Some(image),
            _ => None,
        });
        assert_eq!(img.size, [512, 512]);
        assert!(img.pixels.iter().any(|p| p.r() < 100), "text should be drawn");

        src.request_text(1);
        let text = wait(&rx, |e| match e {
            Event::Text { text, .. } => Some(text),
            _ => None,
        });
        let all = text.text(0, text.chars.len());
        assert!(all.contains("Bravo"), "got {all:?}");

        src.search("needle".into(), 7);
        let mut hits = 0;
        wait(&rx, |e| match e {
            Event::SearchHits { hits: h, generation: 7, .. } => {
                hits += h.len();
                assert!(h[0].context.contains("needle"));
                None
            }
            Event::SearchDone { generation: 7, .. } => Some(()),
            _ => None,
        });
        assert_eq!(hits, 3);

        // Save: drop page 2, put page 3 first, rotate it, add a blank page and annotations.
        let mut rect = Annot::new(Shape::Rect(Rect::from_min_size(pos2(100.0, 100.0), egui::vec2(50.0, 40.0))), Style::default());
        rect.author = "Tester".into();
        let note = Annot::new(Shape::Note { at: pos2(300.0, 300.0), text: "hello note".into() }, Style::default());
        let hl = Annot::new(
            Shape::Markup { kind: MarkupKind::Highlight, quads: vec![[pos2(72.0, 70.0), pos2(200.0, 70.0), pos2(200.0, 95.0), pos2(72.0, 95.0)]] },
            Style::default(),
        );
        let out = dir.join("out.pdf");
        let plan = SavePlan {
            dest: out.clone(),
            pages: vec![
                PagePlan { src: PlanSrc::Own(2), rot: 90, crop: None, annots: vec![rect], replaced: vec![] },
                PagePlan { src: PlanSrc::Own(0), rot: 0, crop: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(306.0, 396.0))), annots: vec![note, hl], replaced: vec![] },
                PagePlan { src: PlanSrc::Blank { w: 300.0, h: 300.0 }, rot: 0, crop: None, annots: vec![], replaced: vec![] },
            ],
            fields: vec![],
            deleted: vec![],
            meta: Some(MetaEdit { title: "Saved Title".into(), author: "Me".into(), ..Default::default() }),
            security: None,
            author: "Review".into(),
        };
        src.save(plan);
        wait(&rx, |e| match e {
            Event::Saved { result, .. } => Some(result.expect("saves")),
            _ => None,
        });

        let src2 = eng.open(2, &out, None);
        let info2 = wait(&rx, |e| match e {
            Event::Opened { source: 2, result } => Some(result.expect("reopens")),
            _ => None,
        });
        assert_eq!(info2.pages.len(), 3);
        // Rotated page is landscape now.
        assert!((info2.pages[0].0.width() - 792.0).abs() < 0.5, "{:?}", info2.pages[0]);
        // Cropped page.
        assert!((info2.pages[1].0.width() - 306.0).abs() < 0.5, "{:?}", info2.pages[1]);
        assert!((info2.pages[2].0.width() - 300.0).abs() < 0.5);
        assert_eq!(info2.meta.title, "Saved Title");
        let kinds: Vec<(u32, &'static str)> = info2.annots.iter().map(|(p, a)| (*p, a.label())).collect();
        assert!(kinds.contains(&(0, "Rectangle")), "{kinds:?}");
        assert!(kinds.contains(&(1, "Note")), "{kinds:?}");
        assert!(kinds.contains(&(1, "Highlight")), "{kinds:?}");
        src2.request_text(0);
        let t = wait(&rx, |e| match e {
            Event::Text { source: 2, text, .. } => Some(text),
            _ => None,
        });
        assert!(t.text(0, t.chars.len()).contains("Charlie"));
    }

    #[test]
    fn encrypt_roundtrip() {
        let dir = tmpdir("crypt");
        let path = sample_pdf(&dir);
        let (eng, rx) = engine();
        let src = eng.open(1, &path, None);
        wait(&rx, |e| matches!(e, Event::Opened { .. }).then_some(()));
        let out = dir.join("locked.pdf");
        src.save(SavePlan {
            dest: out.clone(),
            pages: (0..3).map(|i| PagePlan { src: PlanSrc::Own(i), rot: 0, crop: None, annots: vec![], replaced: vec![] }).collect(),
            fields: vec![],
            deleted: vec![],
            meta: None,
            security: Some(Security { user_password: "pw".into(), allow_print: true, ..Default::default() }),
            author: String::new(),
        });
        wait(&rx, |e| match e {
            Event::Saved { result, .. } => Some(result.expect("saves")),
            _ => None,
        });
        let _a = eng.open(2, &out, None);
        let r = wait(&rx, |e| match e {
            Event::Opened { source: 2, result } => Some(result),
            _ => None,
        });
        assert!(matches!(r, Err(OpenError::NeedsPassword)));
        let _b = eng.open(3, &out, Some("pw".into()));
        let r = wait(&rx, |e| match e {
            Event::Opened { source: 3, result } => Some(result),
            _ => None,
        });
        assert_eq!(r.expect("opens with password").pages.len(), 3);
    }
}
