//! Document model shared by the UI, the PDF engine and the image pipeline.
//!
//! Everything here is plain data so a whole [`Model`] can be cloned for undo.
//! Coordinates are in *page space*: PDF points (y down, origin at the top-left
//! of the page as MuPDF reports it) for PDF pages, pixels for images.

use egui::{pos2, Color32, Pos2, Rect};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Where a page's pixels come from.
#[derive(Clone, Debug, PartialEq)]
pub enum PageSrc {
    /// Page `index` of the MuPDF source `source`.
    Pdf { source: u64, index: u32 },
    /// A blank page inserted by the user.
    Blank,
    /// Frame `frame` of an image document.
    Image { frame: u32 },
}

/// One page as the user sees it: a source page plus non-destructive edits.
#[derive(Clone, Debug)]
pub struct Slot {
    pub uid: u64,
    pub src: PageSrc,
    /// Full page bounds in page space (before crop).
    pub bounds: Rect,
    /// Extra clockwise rotation applied by the user, in degrees (0/90/180/270).
    pub rot: i32,
    /// Visible area in page space; `None` = whole page.
    pub crop: Option<Rect>,
    pub annots: Vec<Annot>,
    /// Page label ("iv", "12") when the PDF defines one.
    pub label: String,
}

impl Slot {
    pub fn new(src: PageSrc, bounds: Rect) -> Self {
        Self { uid: next_id(), src, bounds, rot: 0, crop: None, annots: Vec::new(), label: String::new() }
    }

    pub fn visible(&self) -> Rect {
        self.crop.unwrap_or(self.bounds)
    }

    /// Size on screen at zoom 1, after crop and rotation.
    pub fn display_size(&self) -> egui::Vec2 {
        let v = self.visible();
        if self.rot % 180 == 0 {
            v.size()
        } else {
            egui::vec2(v.height(), v.width())
        }
    }

    /// Map a page-space point into a display rect (`dst` = where the page is drawn).
    pub fn to_screen(&self, dst: Rect, p: Pos2) -> Pos2 {
        let v = self.visible();
        let (x, y) = ((p.x - v.min.x) / v.width(), (p.y - v.min.y) / v.height());
        let (u, w) = match self.rot.rem_euclid(360) {
            90 => (1.0 - y, x),
            180 => (1.0 - x, 1.0 - y),
            270 => (y, 1.0 - x),
            _ => (x, y),
        };
        pos2(dst.min.x + u * dst.width(), dst.min.y + w * dst.height())
    }

    /// Inverse of [`Self::to_screen`].
    pub fn to_page(&self, dst: Rect, s: Pos2) -> Pos2 {
        let v = self.visible();
        let (u, w) = ((s.x - dst.min.x) / dst.width(), (s.y - dst.min.y) / dst.height());
        let (x, y) = match self.rot.rem_euclid(360) {
            90 => (w, 1.0 - u),
            180 => (1.0 - u, 1.0 - w),
            270 => (1.0 - w, u),
            _ => (u, w),
        };
        pos2(v.min.x + x * v.width(), v.min.y + y * v.height())
    }

    pub fn rect_to_screen(&self, dst: Rect, r: Rect) -> Rect {
        Rect::from_two_pos(self.to_screen(dst, r.min), self.to_screen(dst, r.max))
    }

    pub fn rect_to_page(&self, dst: Rect, r: Rect) -> Rect {
        Rect::from_two_pos(self.to_page(dst, r.min), self.to_page(dst, r.max))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkupKind {
    Highlight,
    Underline,
    Strike,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub stroke: Color32,
    pub fill: Option<Color32>,
    pub width: f32,
    pub font_size: f32,
    pub text_color: Color32,
    pub align: Align,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            stroke: Color32::from_rgb(0xE5, 0x32, 0x2D),
            fill: None,
            width: 2.0,
            font_size: 14.0,
            text_color: Color32::BLACK,
            align: Align::Left,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect(Rect),
    RoundRect(Rect),
    Oval(Rect),
    Line { a: Pos2, b: Pos2, arrow: bool },
    Ink(Vec<Vec<Pos2>>),
    Polygon(Vec<Pos2>),
    Star(Rect),
    Bubble(Rect),
    Text { rect: Rect, text: String },
    Note { at: Pos2, text: String },
    Markup { kind: MarkupKind, quads: Vec<[Pos2; 4]> },
    Signature { rect: Rect, strokes: Vec<Vec<Pos2>> },
    Redact(Rect),
}

#[derive(Clone, Debug)]
pub struct Annot {
    pub id: u64,
    pub shape: Shape,
    pub style: Style,
    /// Object number of the PDF annotation this was imported from.
    pub origin: Option<i32>,
    /// Changed since import (or never saved).
    pub dirty: bool,
    pub author: String,
}

impl Annot {
    pub fn new(shape: Shape, style: Style) -> Self {
        Self { id: next_id(), shape, style, origin: None, dirty: true, author: String::new() }
    }

    pub fn bounds(&self) -> Rect {
        let pad = self.style.width.max(1.0);
        match &self.shape {
            Shape::Rect(r) | Shape::RoundRect(r) | Shape::Oval(r) | Shape::Star(r) | Shape::Redact(r) => {
                r.expand(pad)
            }
            Shape::Bubble(r) => Rect::from_min_max(r.min, pos2(r.max.x, r.max.y + r.height() * 0.35)).expand(pad),
            Shape::Text { rect, .. } => *rect,
            Shape::Signature { rect, .. } => *rect,
            Shape::Line { a, b, .. } => Rect::from_two_pos(*a, *b).expand(pad * 4.0),
            Shape::Ink(strokes) => points_bounds(strokes.iter().flatten()).expand(pad),
            Shape::Polygon(p) => points_bounds(p.iter()).expand(pad),
            Shape::Note { at, .. } => Rect::from_min_size(*at, egui::vec2(20.0, 20.0)),
            Shape::Markup { quads, .. } => points_bounds(quads.iter().flatten()),
        }
    }

    /// Move by `d` page units.
    pub fn translate(&mut self, d: egui::Vec2) {
        match &mut self.shape {
            Shape::Rect(r) | Shape::RoundRect(r) | Shape::Oval(r) | Shape::Star(r) | Shape::Bubble(r)
            | Shape::Redact(r) => *r = r.translate(d),
            Shape::Text { rect, .. } => *rect = rect.translate(d),
            Shape::Signature { rect, strokes } => {
                *rect = rect.translate(d);
                strokes.iter_mut().flatten().for_each(|p| *p += d);
            }
            Shape::Line { a, b, .. } => {
                *a += d;
                *b += d;
            }
            Shape::Ink(s) => s.iter_mut().flatten().for_each(|p| *p += d),
            Shape::Polygon(p) => p.iter_mut().for_each(|p| *p += d),
            Shape::Note { at, .. } => *at += d,
            Shape::Markup { quads, .. } => quads.iter_mut().flatten().for_each(|p| *p += d),
        }
        self.dirty = true;
    }

    /// Fit the shape into a new bounding box (resizing).
    pub fn set_bounds(&mut self, from: Rect, to: Rect) {
        let map = |p: Pos2| -> Pos2 {
            let fx = if from.width().abs() > 1e-3 { (p.x - from.min.x) / from.width() } else { 0.0 };
            let fy = if from.height().abs() > 1e-3 { (p.y - from.min.y) / from.height() } else { 0.0 };
            pos2(to.min.x + fx * to.width(), to.min.y + fy * to.height())
        };
        match &mut self.shape {
            Shape::Rect(r) | Shape::RoundRect(r) | Shape::Oval(r) | Shape::Star(r) | Shape::Bubble(r)
            | Shape::Redact(r) => *r = Rect::from_two_pos(map(r.min), map(r.max)),
            Shape::Text { rect, .. } => *rect = Rect::from_two_pos(map(rect.min), map(rect.max)),
            Shape::Signature { rect, strokes } => {
                *rect = Rect::from_two_pos(map(rect.min), map(rect.max));
                strokes.iter_mut().flatten().for_each(|p| *p = map(*p));
            }
            Shape::Line { a, b, .. } => {
                *a = map(*a);
                *b = map(*b);
            }
            Shape::Ink(s) => s.iter_mut().flatten().for_each(|p| *p = map(*p)),
            Shape::Polygon(pts) => pts.iter_mut().for_each(|p| *p = map(*p)),
            Shape::Note { .. } | Shape::Markup { .. } => {}
        }
        self.dirty = true;
    }

    /// Apply a point transform to every coordinate (used when an image is rotated or cropped).
    pub fn map_points(&mut self, f: impl Fn(Pos2) -> Pos2) {
        let fr = |r: &mut Rect| *r = Rect::from_two_pos(f(r.min), f(r.max));
        match &mut self.shape {
            Shape::Rect(r) | Shape::RoundRect(r) | Shape::Oval(r) | Shape::Star(r) | Shape::Bubble(r)
            | Shape::Redact(r) => fr(r),
            Shape::Text { rect, .. } => fr(rect),
            Shape::Signature { rect, strokes } => {
                fr(rect);
                strokes.iter_mut().flatten().for_each(|p| *p = f(*p));
            }
            Shape::Line { a, b, .. } => {
                *a = f(*a);
                *b = f(*b);
            }
            Shape::Ink(s) => s.iter_mut().flatten().for_each(|p| *p = f(*p)),
            Shape::Polygon(p) => p.iter_mut().for_each(|p| *p = f(*p)),
            Shape::Note { at, .. } => *at = f(*at),
            Shape::Markup { quads, .. } => quads.iter_mut().flatten().for_each(|p| *p = f(*p)),
        }
        self.dirty = true;
    }

    pub fn resizable(&self) -> bool {
        !matches!(self.shape, Shape::Note { .. } | Shape::Markup { .. } | Shape::Line { .. })
    }

    pub fn label(&self) -> &'static str {
        match &self.shape {
            Shape::Rect(_) => "Rectangle",
            Shape::RoundRect(_) => "Rounded Rectangle",
            Shape::Oval(_) => "Oval",
            Shape::Line { arrow: true, .. } => "Arrow",
            Shape::Line { .. } => "Line",
            Shape::Ink(_) => "Sketch",
            Shape::Polygon(_) => "Polygon",
            Shape::Star(_) => "Star",
            Shape::Bubble(_) => "Speech Bubble",
            Shape::Text { .. } => "Text",
            Shape::Note { .. } => "Note",
            Shape::Markup { kind: MarkupKind::Highlight, .. } => "Highlight",
            Shape::Markup { kind: MarkupKind::Underline, .. } => "Underline",
            Shape::Markup { kind: MarkupKind::Strike, .. } => "Strikethrough",
            Shape::Signature { .. } => "Signature",
            Shape::Redact(_) => "Redaction",
        }
    }

    /// Text shown in the Highlights and Notes sidebar.
    pub fn text(&self) -> Option<&str> {
        match &self.shape {
            Shape::Text { text, .. } | Shape::Note { text, .. } => Some(text),
            _ => None,
        }
    }
}

pub fn points_bounds<'a>(pts: impl Iterator<Item = &'a Pos2>) -> Rect {
    let mut r = Rect::NOTHING;
    for p in pts {
        r.extend_with(*p);
    }
    if r.is_negative() {
        Rect::from_min_size(Pos2::ZERO, egui::Vec2::ZERO)
    } else {
        r
    }
}

/// Vertices of a five-pointed star inscribed in `r`.
pub fn star_points(r: Rect) -> Vec<Pos2> {
    let c = r.center();
    let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
    (0..10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let k = if i % 2 == 0 { 1.0 } else { 0.4 };
            pos2(c.x + rx * k * a.cos(), c.y + ry * k * a.sin())
        })
        .collect()
}

/// Outline of a rounded rectangle as a polygon.
pub fn round_rect_points(r: Rect) -> Vec<Pos2> {
    let rad = (r.width().min(r.height()) * 0.18).max(1.0);
    let mut pts = Vec::new();
    let corners = [
        (pos2(r.max.x - rad, r.min.y + rad), -90.0f32),
        (pos2(r.max.x - rad, r.max.y - rad), 0.0),
        (pos2(r.min.x + rad, r.max.y - rad), 90.0),
        (pos2(r.min.x + rad, r.min.y + rad), 180.0),
    ];
    for (c, start) in corners {
        for i in 0..=6 {
            let a = (start + i as f32 * 15.0).to_radians();
            pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    pts
}

/// Outline of a speech bubble (rounded body plus a tail) occupying `r`;
/// the tail extends below `r`.
pub fn bubble_points(r: Rect) -> Vec<Pos2> {
    let mut body = Vec::new();
    let (c, rx, ry) = (r.center(), r.width() / 2.0, r.height() / 2.0);
    // Ellipse body with a gap for the tail at the bottom-left.
    let tail_a = 110.0f32;
    let tail_b = 130.0f32;
    let mut deg = tail_b;
    while deg <= tail_a + 360.0 {
        let a = deg.to_radians();
        body.push(pos2(c.x + rx * a.cos(), c.y + ry * a.sin()));
        deg += 6.0;
    }
    let tip = pos2(r.min.x + r.width() * 0.18, r.max.y + r.height() * 0.35);
    body.push(tip);
    body
}

/// Everything the user can undo.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub slots: Vec<Slot>,
    /// Form field values typed by the user, keyed by (source, page, field object number).
    pub fields: Vec<FieldEdit>,
    /// Imported PDF annotations that were deleted, by (source, object number).
    pub deleted_origins: Vec<(u64, i32)>,
    pub bookmarks: Vec<u64>,
    /// Image documents: current pixels per frame.
    pub frames: Vec<std::sync::Arc<image::RgbaImage>>,
    /// PDF document properties edited in the Inspector.
    pub meta: Option<MetaEdit>,
    /// New encryption settings chosen in Edit Permissions.
    pub security: Option<Security>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldEdit {
    pub source: u64,
    pub page: u32,
    pub xref: i32,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetaEdit {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Security {
    pub user_password: String,
    pub owner_password: String,
    pub allow_print: bool,
    pub allow_copy: bool,
    pub allow_modify: bool,
    pub allow_annotate: bool,
    pub allow_forms: bool,
    pub allow_assemble: bool,
}
