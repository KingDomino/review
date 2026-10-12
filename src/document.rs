//! An open document: its model, undo history, load state and save logic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{pos2, Rect};
use image::RgbaImage;

use crate::imaging::{self, ExportFormat};
use crate::model::{next_id, Annot, Model, PageSrc, Slot};
use crate::pdf::{OpenError, PagePlan, PlanSrc, SavePlan, SourceInfo};
use crate::settings::PdfViewMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Image,
    Model,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadState {
    Loading,
    Ready,
    NeedsPassword { wrong: bool },
    Failed(String),
    /// Image pixels dropped to save memory; reloaded when shown again.
    Unloaded,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    None,
    Width,
    Page,
    /// Fit the window, but never enlarge past actual size (images).
    Auto,
}

#[derive(Clone, Debug)]
pub struct ImageMeta {
    pub format: String,
    pub exif: Vec<(String, String)>,
    pub gps: Option<(f64, f64)>,
    pub dpi: f32,
    /// Per-frame delays (ms) of animated images.
    pub delays: Vec<u32>,
    pub has_alpha: bool,
    pub color_model: String,
    pub icc_name: Option<String>,
}

pub struct Document {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub name: String,
    pub kind: Kind,
    pub state: LoadState,
    /// MuPDF source holding this document's own pages.
    pub source: Option<u64>,
    pub info: Option<Arc<SourceInfo>>,
    pub image: Option<ImageMeta>,
    pub model: Model,
    undo: Vec<(Model, &'static str)>,
    redo: Vec<(Model, &'static str)>,
    /// Number of edits since the last save (0 = clean).
    pub edits: u64,
    pub file_size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub created: Option<std::time::SystemTime>,
    pub password: Option<String>,
    // View state
    pub zoom: f32,
    pub fit: Fit,
    pub mode: PdfViewMode,
    pub current: usize,
    pub selected_pages: Vec<usize>,
    pub pending_scroll: Option<(usize, Option<f32>)>,
    pub history: Vec<usize>,
    pub history_pos: usize,
    pub saving: bool,
    /// After saving, the path to reload from.
    pub reload_after_save: Option<PathBuf>,
    /// Small preview of the first frame (images), kept while unloaded.
    pub thumb: Option<Arc<RgbaImage>>,
    pub last_active: std::time::Instant,
    /// Animated GIF playback: next frame time while playing.
    pub playing: Option<std::time::Instant>,
    /// 3D documents: the loaded scene.
    pub scene: Option<Arc<crate::model3d::Scene>>,
}

impl Document {
    pub fn new(path: Option<PathBuf>, kind: Kind) -> Self {
        let name = path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into());
        let meta = path.as_ref().and_then(|p| std::fs::metadata(p).ok());
        Self {
            id: next_id(),
            path,
            name,
            kind,
            state: LoadState::Loading,
            source: None,
            info: None,
            image: None,
            model: Model::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            edits: 0,
            file_size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.as_ref().and_then(|m| m.modified().ok()),
            created: meta.as_ref().and_then(|m| m.created().ok()),
            password: None,
            zoom: 1.0,
            fit: Fit::Width,
            mode: PdfViewMode::Continuous,
            current: 0,
            selected_pages: Vec::new(),
            pending_scroll: None,
            history: vec![0],
            history_pos: 0,
            saving: false,
            reload_after_save: None,
            thumb: None,
            last_active: std::time::Instant::now(),
            playing: None,
            scene: None,
        }
    }

    pub fn ready(&self) -> bool {
        self.state == LoadState::Ready
    }

    pub fn is_animated(&self) -> bool {
        self.image.as_ref().map(|m| m.delays.len() > 1).unwrap_or(false) && self.page_count() > 1
    }

    /// Bytes of decoded pixels held by this document.
    pub fn pixel_bytes(&self) -> usize {
        self.model.frames.iter().map(|f| f.as_raw().len()).sum()
    }

    pub fn dirty(&self) -> bool {
        self.edits > 0
    }

    pub fn page_count(&self) -> usize {
        self.model.slots.len()
    }

    pub fn is_pdf(&self) -> bool {
        self.kind == Kind::Pdf && self.info.as_ref().map(|i| i.is_pdf).unwrap_or(false)
    }

    /// Display name with page label, e.g. "report.pdf — Page 3 of 12".
    pub fn subtitle(&self, logical: bool) -> String {
        match self.kind {
            Kind::Model => match &self.scene {
                Some(s) => format!("{} · {} triangles", s.format, group_digits(s.triangles())),
                None => String::new(),
            },
            Kind::Image => {
                let Some(f) = self.model.frames.first() else { return String::new() };
                if self.page_count() > 1 {
                    format!("{} × {} · {} of {}", f.width(), f.height(), self.current + 1, self.page_count())
                } else {
                    format!("{} × {} pixels", f.width(), f.height())
                }
            }
            Kind::Pdf => {
                if self.page_count() == 0 {
                    return String::new();
                }
                let slot = &self.model.slots[self.current.min(self.page_count() - 1)];
                if logical && !slot.label.is_empty() {
                    format!("Page {} ({} of {})", slot.label, self.current + 1, self.page_count())
                } else {
                    format!("Page {} of {}", self.current + 1, self.page_count())
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Loading

    pub fn pdf_opened(&mut self, result: Result<SourceInfo, OpenError>, author_filter: bool) {
        let _ = author_filter;
        match result {
            Ok(info) => {
                let src = self.source.unwrap_or(0);
                let mut slots: Vec<Slot> = info
                    .pages
                    .iter()
                    .enumerate()
                    .map(|(i, (bounds, label))| {
                        let mut s = Slot::new(PageSrc::Pdf { source: src, index: i as u32 }, *bounds);
                        s.label = label.clone();
                        s
                    })
                    .collect();
                for (page, a) in &info.annots {
                    if let Some(s) = slots.get_mut(*page as usize) {
                        s.annots.push(a.clone());
                    }
                }
                self.model = Model { slots, ..Model::default() };
                self.undo.clear();
                self.redo.clear();
                self.edits = 0;
                self.info = Some(Arc::new(info));
                self.state = if self.model.slots.is_empty() {
                    LoadState::Failed("The document has no pages.".into())
                } else {
                    LoadState::Ready
                };
                self.current = self.current.min(self.page_count().saturating_sub(1));
            }
            Err(OpenError::NeedsPassword) => self.state = LoadState::NeedsPassword { wrong: false },
            Err(OpenError::WrongPassword) => self.state = LoadState::NeedsPassword { wrong: true },
            Err(OpenError::Failed(e)) => self.state = LoadState::Failed(e),
        }
    }

    pub fn image_loaded(&mut self, result: Result<imaging::Loaded, String>) {
        match result {
            Ok(l) => {
                let keep_view = self.state == LoadState::Unloaded;
                self.thumb = l.thumb.clone().or_else(|| l.frames.first().map(|f| Arc::new(imaging::make_thumb(f))));
                self.model = Model::default();
                for (i, f) in l.frames.iter().enumerate() {
                    let b = Rect::from_min_max(pos2(0.0, 0.0), pos2(f.width() as f32, f.height() as f32));
                    self.model.slots.push(Slot::new(PageSrc::Image { frame: i as u32 }, b));
                }
                self.model.frames = l.frames;
                self.image = Some(ImageMeta {
                    format: l.format,
                    exif: l.exif,
                    gps: l.gps,
                    dpi: l.dpi,
                    delays: l.delays,
                    has_alpha: l.has_alpha,
                    color_model: l.color_model,
                    icc_name: l.icc_name,
                });
                self.undo.clear();
                self.redo.clear();
                self.edits = 0;
                self.state = LoadState::Ready;
                if !keep_view {
                    self.fit = Fit::Auto;
                }
                // Animated GIFs play in single-frame view; frames stay browsable in the sidebar.
                if self.is_animated() && !keep_view {
                    self.mode = PdfViewMode::Single;
                    self.playing = Some(std::time::Instant::now());
                }
            }
            Err(e) => self.state = LoadState::Failed(e),
        }
    }

    // ------------------------------------------------------------------
    // Undo

    /// Record the current model before a change.
    pub fn checkpoint(&mut self, what: &'static str) {
        self.undo.push((self.model.clone(), what));
        // Image snapshots are large: keep fewer of them.
        let limit = if self.kind == Kind::Image { 24 } else { 200 };
        if self.undo.len() > limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.edits += 1;
    }

    /// Forget the last checkpoint when the change it guarded didn't happen.
    pub fn cancel_checkpoint(&mut self) {
        if self.undo.pop().is_some() {
            self.edits = self.edits.saturating_sub(1);
        }
    }

    pub fn undo_name(&self) -> Option<&'static str> {
        self.undo.last().map(|(_, w)| *w)
    }

    pub fn redo_name(&self) -> Option<&'static str> {
        self.redo.last().map(|(_, w)| *w)
    }

    pub fn undo(&mut self) -> bool {
        let Some((m, w)) = self.undo.pop() else { return false };
        let cur = std::mem::replace(&mut self.model, m);
        self.redo.push((cur, w));
        self.edits = self.edits.saturating_sub(1);
        self.after_model_change();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((m, w)) = self.redo.pop() else { return false };
        let cur = std::mem::replace(&mut self.model, m);
        self.undo.push((cur, w));
        self.edits += 1;
        self.after_model_change();
        true
    }

    pub fn after_model_change(&mut self) {
        let n = self.page_count();
        self.current = self.current.min(n.saturating_sub(1));
        self.selected_pages.retain(|p| *p < n);
        if self.kind == Kind::Image {
            // Frame sizes may have changed (rotate/crop/resize).
            for (i, s) in self.model.slots.iter_mut().enumerate() {
                if let Some(f) = self.model.frames.get(i) {
                    s.bounds = Rect::from_min_max(pos2(0.0, 0.0), pos2(f.width() as f32, f.height() as f32));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Image edits (applied to pixels, annotation coordinates follow)

    pub fn edit_frame(&mut self, idx: usize, what: &'static str, f: impl FnOnce(&RgbaImage) -> RgbaImage, map: impl Fn(egui::Pos2) -> egui::Pos2) {
        let Some(frame) = self.model.frames.get(idx).cloned() else { return };
        self.checkpoint(what);
        let new = f(&frame);
        self.model.frames[idx] = Arc::new(new);
        if let Some(slot) = self.model.slots.get_mut(idx) {
            for a in &mut slot.annots {
                a.map_points(&map);
            }
            slot.crop = None;
        }
        self.after_model_change();
    }

    pub fn rotate_image(&mut self, deg: i32) {
        let idxs = self.target_pages();
        for idx in idxs {
            let Some(f) = self.model.frames.get(idx) else { continue };
            let (w, h) = (f.width() as f32, f.height() as f32);
            let d = deg.rem_euclid(360);
            self.edit_frame(
                idx,
                if d == 90 { "Rotate Right" } else { "Rotate Left" },
                |img| imaging::rotate(img, d),
                move |p| match d {
                    90 => pos2(h - p.y, p.x),
                    180 => pos2(w - p.x, h - p.y),
                    270 => pos2(p.y, w - p.x),
                    _ => p,
                },
            );
        }
    }

    pub fn flip_image(&mut self, horizontal: bool) {
        for idx in self.target_pages() {
            let Some(f) = self.model.frames.get(idx) else { continue };
            let (w, h) = (f.width() as f32, f.height() as f32);
            self.edit_frame(
                idx,
                if horizontal { "Flip Horizontal" } else { "Flip Vertical" },
                |img| if horizontal { image::imageops::flip_horizontal(img) } else { image::imageops::flip_vertical(img) },
                move |p| if horizontal { pos2(w - p.x, p.y) } else { pos2(p.x, h - p.y) },
            );
        }
    }

    /// Pages that page commands act on: the sidebar selection, else the current page.
    pub fn target_pages(&self) -> Vec<usize> {
        if self.selected_pages.is_empty() {
            if self.page_count() > 0 {
                vec![self.current.min(self.page_count() - 1)]
            } else {
                vec![]
            }
        } else {
            let mut v = self.selected_pages.clone();
            v.sort_unstable();
            v.dedup();
            v
        }
    }

    // ------------------------------------------------------------------
    // Saving

    pub fn can_save_in_place(&self) -> bool {
        match (&self.path, self.kind) {
            (Some(_), Kind::Pdf) => self.is_pdf(),
            (Some(p), Kind::Image) => ExportFormat::from_path(p).map(|f| f != ExportFormat::Pdf).unwrap_or(false) && self.page_count() == 1,
            _ => false,
        }
    }

    pub fn save_plan(&self, dest: &Path, sources: &HashMap<u64, (PathBuf, Option<String>)>, author: &str) -> SavePlan {
        let own = self.source.unwrap_or(u64::MAX);
        let pages = self
            .model
            .slots
            .iter()
            .map(|s| {
                let (src, fresh) = match &s.src {
                    PageSrc::Pdf { source, index } if *source == own => (PlanSrc::Own(*index), false),
                    PageSrc::Pdf { source, index } => {
                        let (path, password) = sources.get(source).cloned().unwrap_or_default();
                        (PlanSrc::External { path, password, index: *index }, true)
                    }
                    _ => (PlanSrc::Blank { w: s.bounds.width(), h: s.bounds.height() }, true),
                };
                let annots: Vec<Annot> =
                    s.annots.iter().filter(|a| fresh || a.origin.is_none() || a.dirty).cloned().collect();
                let replaced = if fresh { vec![] } else { s.annots.iter().filter(|a| a.dirty).filter_map(|a| a.origin).collect() };
                PagePlan { src, rot: s.rot, crop: s.crop, annots, replaced }
            })
            .collect();
        SavePlan {
            dest: dest.to_path_buf(),
            pages,
            fields: self.model.fields.iter().filter(|f| f.source == own).cloned().collect(),
            deleted: self.model.deleted_origins.iter().filter(|(s, _)| *s == own).map(|(_, x)| *x).collect(),
            meta: self.model.meta.clone(),
            security: self.model.security.clone(),
            author: author.to_string(),
        }
    }

    /// Image with markup burned in, crop and slot rotation applied (for saving/exporting).
    pub fn flattened_frame(&self, idx: usize) -> Option<RgbaImage> {
        let frame = self.model.frames.get(idx)?;
        let slot = self.model.slots.get(idx)?;
        let mut img = if slot.annots.is_empty() {
            (**frame).clone()
        } else {
            imaging::burn_annotations(frame, &slot.annots, crate::theme::INTER)
        };
        if let Some(c) = slot.crop {
            img = imaging::crop(&img, c);
        }
        if slot.rot != 0 {
            img = imaging::rotate(&img, slot.rot);
        }
        Some(img)
    }

    pub fn mark_saved(&mut self, path: PathBuf) {
        self.name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.path = Some(path);
        self.edits = 0;
        // Keep history usable, but saving is a new baseline.
        for (m, _) in self.undo.iter_mut() {
            let _ = m;
        }
        if let Some(p) = &self.path {
            if let Ok(m) = std::fs::metadata(p) {
                self.file_size = m.len();
                self.modified = m.modified().ok();
            }
        }
    }
}

pub fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Human friendly byte size.
pub fn human_size(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.2} GB", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1} MB", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0} KB", n / 1e3)
    } else {
        format!("{n} bytes")
    }
}

pub fn fmt_time(t: Option<std::time::SystemTime>) -> String {
    match t {
        Some(t) => chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string(),
        None => "—".into(),
    }
}
