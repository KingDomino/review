//! The document canvas: page layout, scrolling/zooming, drawing and every pointer tool.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::annots;
use crate::app::{App, ShapeTool, TextSel, Tool};
use crate::document::{Document, Fit, Kind, LoadState};
use crate::model::{Annot, MarkupKind, PageSrc, Shape, Style};
use crate::pdf::{FieldKind, LinkTarget};
use crate::render::{draw_slot, page_shadow, DrawCtx};
use crate::settings::PdfViewMode;
use crate::theme::Palette;

pub const GAP: f32 = 16.0;
pub const MARGIN: f32 = 28.0;

#[derive(Clone, Debug)]
pub enum Drag {
    None,
    Text { slot: usize, anchor: usize },
    Marquee { slot: usize, start: Pos2 },
    Ink { slot: usize, pts: Vec<Pos2> },
    Shape { slot: usize, tool: ShapeTool, start: Pos2, cur: Pos2 },
    Move { slot: usize, id: u64, last: Pos2, moved: bool },
    Resize { slot: usize, id: u64, handle: usize, orig: Rect },
    LineEnd { slot: usize, id: u64, end: usize },
    Pan,
    Alpha { slot: usize, seed: (u32, u32), start: Pos2 },
    Markup { slot: usize, kind: MarkupKind, anchor: usize },
    Redact { slot: usize, start: Pos2 },
    SelHandle { slot: usize, handle: usize, orig: Rect },
}

pub struct Slideshow {
    pub page: usize,
    pub last_move: Instant,
}

impl Slideshow {
    pub fn new(page: usize) -> Self {
        Self { page, last_move: Instant::now() }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PageBox {
    pub idx: usize,
    pub rect: Rect,
}

/// Which pages are laid out, and where (content coordinates at `zoom`).
pub fn layout(doc: &Document, zoom: f32, view: Vec2) -> (Vec<PageBox>, Vec2) {
    let n = doc.page_count();
    let sizes: Vec<Vec2> = doc.model.slots.iter().map(|s| s.display_size() * zoom).collect();
    let mut rows: Vec<Vec<usize>> = Vec::new();
    match doc.mode {
        _ if doc.kind == Kind::Image && n == 1 => rows.push(vec![0]),
        PdfViewMode::Continuous => rows.extend((0..n).map(|i| vec![i])),
        PdfViewMode::Two => {
            let mut i = 0;
            while i < n {
                rows.push((i..(i + 2).min(n)).collect());
                i += 2;
            }
        }
        PdfViewMode::Single => rows.push(vec![doc.current.min(n.saturating_sub(1))]),
    }
    if doc.mode == PdfViewMode::Single && doc.kind == Kind::Pdf && n > 0 {
        rows = vec![vec![doc.current.min(n - 1)]];
    }
    let row_w = |r: &Vec<usize>| r.iter().map(|i| sizes[*i].x).sum::<f32>() + GAP * (r.len().saturating_sub(1)) as f32;
    let row_h = |r: &Vec<usize>| r.iter().map(|i| sizes[*i].y).fold(0.0, f32::max);
    let max_w = rows.iter().map(row_w).fold(0.0, f32::max);
    let total_h: f32 = rows.iter().map(row_h).sum::<f32>() + GAP * rows.len().saturating_sub(1) as f32;
    let content = vec2((max_w + 2.0 * MARGIN).max(view.x), (total_h + 2.0 * MARGIN).max(view.y));
    let mut y = MARGIN + ((content.y - 2.0 * MARGIN - total_h) / 2.0).max(0.0);
    let mut out = Vec::with_capacity(n);
    for r in &rows {
        let w = row_w(r);
        let h = row_h(r);
        let mut x = (content.x - w) / 2.0;
        for i in r {
            let s = sizes[*i];
            out.push(PageBox { idx: *i, rect: Rect::from_min_size(pos2(x, y + (h - s.y) / 2.0), s) });
            x += s.x + GAP;
        }
        y += h + GAP;
    }
    (out, content)
}

pub fn fit_zoom(doc: &Document, view: Vec2, actual: f32) -> f32 {
    let n = doc.page_count();
    if n == 0 {
        return 1.0;
    }
    let avail = (view - Vec2::splat(2.0 * MARGIN)).max(vec2(40.0, 40.0));
    let cur = doc.model.slots[doc.current.min(n - 1)].display_size();
    let row = if doc.mode == PdfViewMode::Two && doc.kind == Kind::Pdf {
        let i = doc.current.min(n - 1) / 2 * 2;
        let a = doc.model.slots[i].display_size();
        let b = doc.model.slots.get(i + 1).map(|s| s.display_size()).unwrap_or(Vec2::ZERO);
        vec2(a.x + b.x + if b.x > 0.0 { GAP } else { 0.0 }, a.y.max(b.y))
    } else {
        cur
    };
    let widest = if doc.mode == PdfViewMode::Continuous {
        doc.model.slots.iter().map(|s| s.display_size().x).fold(0.0, f32::max)
    } else {
        row.x
    };
    match doc.fit {
        Fit::Width => avail.x / widest.max(1.0),
        Fit::Page => (avail.x / row.x.max(1.0)).min(avail.y / row.y.max(1.0)),
        Fit::Auto => (avail.x / row.x.max(1.0)).min(avail.y / row.y.max(1.0)).min(actual),
        Fit::None => doc.zoom,
    }
}

/// Scroll offset that keeps the document point at `local` (relative to the view) in place
/// when the zoom changes from `old_zoom` to `doc.zoom`. Anchoring on a page rather than
/// scaling the offset matters because margins and gaps between pages don't scale.
fn anchored_offset(doc: &Document, old_zoom: f32, view: Vec2, offset: Vec2, local: Vec2) -> Vec2 {
    let p = (offset + local).to_pos2();
    let (old, _) = layout(doc, old_zoom, view);
    let Some(b) = old.iter().min_by(|a, b| a.rect.distance_sq_to_pos(p).total_cmp(&b.rect.distance_sq_to_pos(p))) else {
        return offset;
    };
    let rel = (p - b.rect.min) / old_zoom.max(1e-4);
    let (new, _) = layout(doc, doc.zoom, view);
    match new.iter().find(|n| n.idx == b.idx) {
        Some(n) => (n.rect.min + rel * doc.zoom).to_vec2() - local,
        None => offset,
    }
}

/// Screen-space hit: which page box (by index into the layout) is under `p`.
fn page_at(boxes: &[PageBox], origin: Pos2, p: Pos2, slop: f32) -> Option<PageBox> {
    boxes.iter().find(|b| b.rect.translate(origin.to_vec2()).expand(slop).contains(p)).copied()
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let pal = Palette::of(&ctx);
    let Some(doc) = app.docs.get(app.active) else {
        empty_state(app, ui);
        return;
    };
    match doc.state.clone() {
        LoadState::Ready => {}
        LoadState::Loading | LoadState::Unloaded => {
            ui.centered_and_justified(|ui| {
                ui.add(egui::Spinner::new().size(28.0));
            });
            return;
        }
        LoadState::NeedsPassword { wrong } => {
            password_prompt(app, ui, wrong);
            return;
        }
        LoadState::Failed(e) => {
            let name = doc.name.clone();
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(egui::RichText::new(egui_phosphor::regular::WARNING_CIRCLE).size(40.0).color(pal.text_weak));
                    ui.label(egui::RichText::new(format!("“{name}” couldn’t be opened.")).font(FontId::new(16.0, crate::theme::bold())));
                    ui.label(egui::RichText::new(e).color(pal.text_weak));
                });
            });
            return;
        }
    }
    if app.docs[app.active].kind == Kind::Model {
        crate::view3d::show(app, ui, pal.canvas);
        return;
    }
    if app.contact_sheet {
        contact_sheet(app, ui);
        return;
    }

    let view = ui.available_size();
    let ppp = ctx.pixels_per_point();
    let px_scale = app.settings.image_pixel_scale;
    let doc_id = doc.id;
    let canvas_rect = ui.max_rect();
    // Must match how ScrollArea derives its id from `id_salt` below.
    let scroll_id = ui.make_persistent_id(egui::IdSalt::new(("canvas", doc_id)));
    let prev_offset = egui::scroll_area::State::load(&ctx, scroll_id).map(|s| s.offset).unwrap_or(Vec2::ZERO);

    // --- Zoom (fit modes, pinch, Ctrl+wheel, zoom-to-selection) ---------------------------
    let mut new_offset: Option<Vec2> = None;
    {
        let d = &mut app.docs[app.active];
        let actual = if d.kind == Kind::Image && !px_scale { 72.0 / d.image.as_ref().map(|i| i.dpi).unwrap_or(72.0) } else { 1.0 };
        let old_zoom = d.zoom;
        if d.fit != Fit::None {
            d.zoom = fit_zoom(d, view, actual);
        }
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let zd = ctx.input(|i| i.zoom_delta());
        let over = pointer.map(|p| canvas_rect.contains(p)).unwrap_or(false);
        if over && (zd - 1.0).abs() > 1e-4 && app.slideshow.is_none() {
            let p = pointer.unwrap();
            let f = ((d.zoom * zd).clamp(0.05, 64.0)) / d.zoom;
            d.fit = Fit::None;
            d.zoom *= f;
            new_offset = Some(anchored_offset(d, old_zoom, view, prev_offset, p - canvas_rect.min));
        } else if (d.zoom - old_zoom).abs() > 1e-4 && d.fit == Fit::None && d.pending_scroll.is_none() {
            // Toolbar/menu zoom: keep the view centre steady.
            new_offset = Some(anchored_offset(d, old_zoom, view, prev_offset, view / 2.0));
        }
        let zoom_to = ctx.data_mut(|m| {
            let v = m.get_temp::<(usize, Rect)>(egui::Id::new("zoom-to"));
            m.remove::<(usize, Rect)>(egui::Id::new("zoom-to"));
            v
        });
        if let Some((slot, r)) = zoom_to {
            if let Some(s) = d.model.slots.get(slot) {
                let sr = s.rect_to_screen(Rect::from_min_size(Pos2::ZERO, s.display_size()), r);
                let z = ((view.x - 80.0) / sr.width().max(1.0)).min((view.y - 80.0) / sr.height().max(1.0)).clamp(0.05, 64.0);
                d.fit = Fit::None;
                d.zoom = z;
                let (boxes, _) = layout(d, z, view);
                if let Some(b) = boxes.iter().find(|b| b.idx == slot) {
                    let c = b.rect.min + (sr.center().to_vec2()) * z;
                    new_offset = Some(c.to_vec2() - view / 2.0);
                }
            }
        }
    }

    let doc = &app.docs[app.active];
    let zoom = doc.zoom;
    let (boxes, content) = layout(doc, zoom, view);

    if let Some((page, y)) = app.docs[app.active].pending_scroll.take() {
        if let Some(b) = boxes.iter().find(|b| b.idx == page) {
            let slot = &app.docs[app.active].model.slots[page];
            let top = match y {
                Some(py) => {
                    let dst = Rect::from_min_size(Pos2::ZERO, b.rect.size());
                    slot.to_screen(dst, pos2(slot.visible().center().x, py)).y + b.rect.min.y - view.y * 0.3
                }
                None => b.rect.min.y - MARGIN * 0.5,
            };
            let x = if b.rect.width() > view.x { b.rect.min.x - MARGIN * 0.5 } else { 0.0 };
            new_offset = Some(vec2(x.max(0.0), top.max(0.0)));
        }
    }

    // --- Keyboard navigation ------------------------------------------------------------------
    let typing = ctx.egui_wants_keyboard_input();
    let mut key_scroll = Vec2::ZERO;
    let mut flip = 0i32;
    if !typing && app.dialog.is_none() {
        let doc = &app.docs[app.active];
        let single = doc.mode != PdfViewMode::Continuous || doc.kind == Kind::Image;
        let fits_w = content.x <= view.x + 1.0;
        let fits_h = content.y <= view.y + 1.0;
        ctx.input_mut(|i| {
            let line = 48.0;
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                if single && fits_h { flip += 1 } else { key_scroll.y += line }
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                if single && fits_h { flip -= 1 } else { key_scroll.y -= line }
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                if fits_w { flip += 1 } else { key_scroll.x += line }
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                if fits_w { flip -= 1 } else { key_scroll.x -= line }
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) || i.consume_key(Modifiers::SHIFT, Key::Space) {
                if single && fits_h { flip -= 1 } else { key_scroll.y -= view.y * 0.9 }
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) || i.consume_key(Modifiers::NONE, Key::Space) {
                if single && fits_h { flip += 1 } else { key_scroll.y += view.y * 0.9 }
            }
        });
    }
    if flip != 0 {
        let doc = &app.docs[app.active];
        if doc.kind == Kind::Image && doc.page_count() == 1 && app.docs.len() > 1 {
            // Browsing a folder of photos: arrows move between documents.
            let n = app.docs.len() as i32;
            let next = (app.active as i32 + flip).rem_euclid(n) as usize;
            app.activate(next);
        } else {
            app.go_page(flip);
        }
        return;
    }
    if key_scroll != Vec2::ZERO {
        new_offset = Some(new_offset.unwrap_or(prev_offset) + key_scroll);
    }

    // Space+drag / middle-drag panning.
    let panning = matches!(app.drag, Drag::Pan);
    let mut area = egui::ScrollArea::both().id_salt(("canvas", doc_id)).auto_shrink(false).animated(false);
    if let Some(o) = new_offset {
        let max = (content - view).max(Vec2::ZERO);
        area = area.scroll_offset(o.clamp(Vec2::ZERO, max));
    } else if panning {
        let d = ctx.input(|i| i.pointer.delta());
        area = area.scroll_offset(prev_offset - d);
    }

    let out = area.show_viewport(ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let (content_rect, resp) = ui.allocate_exact_size(content, Sense::click_and_drag());
        debug_assert!((content_rect.min - origin).length() < 1.0);
        let clip = ui.clip_rect();
        draw_pages(app, ui, &boxes, origin, clip, viewport, ppp);
        interact(app, ui, &resp, &boxes, origin, zoom);
        overlays(app, ui, &boxes, origin, zoom);
        // Current page = the one covering the middle of the viewport.
        let mid = viewport.center();
        let d = &mut app.docs[app.active];
        if d.mode == PdfViewMode::Continuous || d.mode == PdfViewMode::Two {
            if let Some(b) = boxes
                .iter()
                .filter(|b| b.rect.min.y <= mid.y + GAP)
                .min_by(|a, b| (a.rect.center().y - mid.y).abs().total_cmp(&(b.rect.center().y - mid.y).abs()))
                .or_else(|| boxes.first())
            {
                if d.current != b.idx && new_offset.is_none() {
                    d.current = b.idx;
                }
            }
        }
    });
    let user_scrolled = ctx.input(|i| i.smooth_scroll_delta.length() > 0.0 || i.pointer.is_decidedly_dragging()) || key_scroll != Vec2::ZERO;
    if new_offset.map(|_| key_scroll != Vec2::ZERO).unwrap_or(true) && user_scrolled && (out.state.offset - prev_offset).length() > 0.5 {
        app.page_badge_until = Some(Instant::now() + Duration::from_millis(1100));
    }

    page_badge(app, ui, canvas_rect);
    live_text_button(app, ui, canvas_rect);
    animation_controls(app, ui, canvas_rect);
    if app.magnifier {
        magnifier(app, ui, &boxes, out.inner_rect.min - out.state.offset, ppp);
    }
}

fn draw_pages(app: &mut App, ui: &mut Ui, boxes: &[PageBox], origin: Pos2, clip: Rect, viewport: Rect, ppp: f32) {
    let pal = Palette::of(ui.ctx());
    let painter = ui.painter().clone();
    let dark = app.settings.dark_pdf;
    app.page_rects.clear();
    let sel_text = app.sel.text.clone().and_then(|ts| app.slot_text(ts.slot).map(|t| (ts, t)));
    let live = if app.live_text_highlight { app.live_text_for_current() } else { None };
    let App { docs, active, tiles, sources, images, search, sel, alpha, page_rects, proof, .. } = app;
    let doc = &docs[*active];
    let prefetch = viewport.expand2(vec2(0.0, viewport.height() * 1.5));
    let zoom = doc.zoom;
    let mut dc = DrawCtx { tiles, sources, images, frames: &doc.model.frames, thumb: doc.thumb.as_ref(), doc_id: doc.id, dark: dark && doc.kind == Kind::Pdf, ppp, proof: proof.as_deref() };
    for b in boxes {
        if !b.rect.intersects(prefetch) {
            continue;
        }
        let r = b.rect.translate(origin.to_vec2());
        let slot = &doc.model.slots[b.idx];
        let visible = b.rect.intersects(viewport);
        if visible {
            page_shadow(&painter, r, pal.page_shadow);
        }
        // Off-screen neighbours only get their cheap preview tile (prefetch).
        draw_slot(&mut dc, &painter, slot, r, if visible { clip } else { r }, !visible);
        if !visible {
            continue;
        }
        page_rects.push((b.idx, r));
        let pclip = painter.with_clip_rect(r.intersect(clip));
        // Search hits
        if search.doc == doc.id {
            let page_id = match slot.src {
                PageSrc::Pdf { index, .. } => Some(index),
                PageSrc::Image { frame } => Some(frame),
                PageSrc::Blank => None,
            };
            if let Some(index) = page_id {
                for (hi, hit) in search.hits.iter().enumerate().filter(|(_, h)| h.page == index) {
                    let current = search.current == Some(hi);
                    for hr in &hit.rects {
                        let sr = slot.rect_to_screen(r, *hr).expand(1.5);
                        if current {
                            pclip.rect(sr, 3.0, Color32::from_rgba_unmultiplied(255, 200, 0, 150), Stroke::new(1.5, Color32::from_rgb(230, 140, 0)), StrokeKind::Outside);
                        } else {
                            pclip.rect_filled(sr, 3.0, Color32::from_rgba_unmultiplied(255, 220, 0, 90));
                        }
                    }
                }
            }
        }
        // Text selection
        if let Some((ts, t)) = &sel_text {
            if ts.doc == doc.id && ts.slot == b.idx {
                for tr in t.rects(ts.a.min(ts.b), ts.a.max(ts.b)) {
                    pclip.rect_filled(slot.rect_to_screen(r, tr).expand(1.0), 1.5, pal.accent.gamma_multiply(0.35));
                }
            }
        }
        // Live Text: outline every recognised line.
        if let Some((slot_idx, t)) = &live {
            if *slot_idx == b.idx {
                for lr in &t.lines {
                    let sr = slot.rect_to_screen(r, *lr).expand(3.0);
                    pclip.rect(sr, 4.0, Color32::from_white_alpha(60), Stroke::new(1.5, pal.accent), StrokeKind::Outside);
                }
            }
        }
        // Annotations
        for a in &slot.annots {
            if sel.editing == Some(a.id) {
                continue;
            }
            annots::paint(&pclip, slot, r, zoom, a);
        }
        // Instant Alpha mask
        if let Some(al) = alpha.as_ref() {
            if al.doc == doc.id && al.slot == b.idx {
                let full = slot.rect_to_screen(r, slot.bounds);
                pclip.image(al.tex.id(), full, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        // Crop marker for cropped pages is implicit (page is smaller).
        if doc.selected_pages.contains(&b.idx) && doc.page_count() > 1 && doc.kind == Kind::Pdf {
            // subtle hint that page commands will act on this page
        }
    }
}

fn interact(app: &mut App, ui: &mut Ui, resp: &egui::Response, boxes: &[PageBox], origin: Pos2, zoom: f32) {
    let ctx = ui.ctx().clone();
    let hover = resp.hover_pos();
    let space = ctx.input(|i| i.key_down(Key::Space)) && !ctx.egui_wants_keyboard_input();
    let middle = ctx.input(|i| i.pointer.button_down(PointerButton::Middle));
    let doc_id = app.docs[app.active].id;
    let tol = 6.0 / zoom.max(0.01);

    let page_of = |p: Pos2| page_at(boxes, origin, p, 0.0);
    let to_page = |app: &App, b: &PageBox, p: Pos2| {
        let slot = &app.docs[app.active].model.slots[b.idx];
        slot.to_page(b.rect.translate(origin.to_vec2()), p)
    };

    // ----- Cursor feedback ------------------------------------------------------------------
    if let Some(h) = hover {
        let cursor = if space || matches!(app.drag, Drag::Pan) {
            if ctx.input(|i| i.pointer.any_down()) { CursorIcon::Grabbing } else { CursorIcon::Grab }
        } else if let Some(b) = page_of(h) {
            let pp = to_page(app, &b, h);
            let on_annot = app.docs[app.active].model.slots[b.idx].annots.iter().rev().any(|a| annots::hit(a, pp, tol));
            match app.tool {
                Tool::Text | Tool::Rect if on_annot => CursorIcon::Move,
                Tool::Text => {
                    let slot_src = app.docs[app.active].model.slots[b.idx].src.clone();
                    let mut c = CursorIcon::Default;
                    if let PageSrc::Pdf { source, index } = slot_src {
                        let on_link = app.page_links(source, index).map(|l| l.iter().any(|l| l.rect.contains(pp))).unwrap_or(false);
                        let on_field = field_at(app, b.idx, pp).is_some();
                        if on_link || on_field {
                            c = CursorIcon::PointingHand;
                        } else if let Some(t) = app.page_text(source, index) {
                            if t.chars.iter().any(|ch| ch.rect.expand(2.0).contains(pp)) {
                                c = CursorIcon::Text;
                            }
                        }
                    } else if app.docs[app.active].kind == Kind::Image {
                        let on_text = app.slot_text(b.idx).map(|t| t.chars.iter().any(|ch| ch.rect.expand(2.0).contains(pp))).unwrap_or(false);
                        c = if on_text { CursorIcon::Text } else { CursorIcon::Crosshair };
                    }
                    c
                }
                Tool::Markup(_) => CursorIcon::Text,
                Tool::TextBox | Tool::Note | Tool::Signature(_) => CursorIcon::Copy,
                _ => CursorIcon::Crosshair,
            }
        } else {
            CursorIcon::Default
        };
        ctx.set_cursor_icon(cursor);
    }

    // ----- Drag start -----------------------------------------------------------------------
    if resp.drag_started() {
        // Where the button went down, not where the drag threshold was crossed.
        let start = ctx.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).unwrap_or_default();
        if space || middle || ctx.input(|i| i.pointer.button_down(PointerButton::Middle)) {
            app.drag = Drag::Pan;
        } else if let Some(b) = page_of(start).or_else(|| page_at(boxes, origin, start, 40.0)) {
            let pp = to_page(app, &b, start);
            app.drag = begin_drag(app, b, pp, start, origin, zoom);
        }
    }

    // ----- Drag update ----------------------------------------------------------------------
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            update_drag(app, boxes, origin, p, zoom, ctx.input(|i| i.modifiers.shift));
        }
    }
    if matches!(app.drag, Drag::Pan) && !ctx.input(|i| i.pointer.any_down()) {
        app.drag = Drag::None;
    }

    // ----- Drag end ---------------------------------------------------------------------------
    if resp.drag_stopped() {
        end_drag(app, boxes, origin, zoom);
    }

    // ----- Clicks -----------------------------------------------------------------------------
    if resp.clicked() || resp.double_clicked() || resp.triple_clicked() {
        let Some(p) = resp.interact_pointer_pos() else { return };
        let Some(b) = page_of(p) else {
            app.sel.annot = None;
            app.sel.text = None;
            app.sel.rect = None;
            app.sel.field = None;
            return;
        };
        let pp = to_page(app, &b, p);
        click(app, b.idx, pp, resp.double_clicked(), resp.triple_clicked(), doc_id, zoom);
    }

    // Right-click: context menu with the most useful commands.
    resp.context_menu(|ui| {
        use crate::app::Action;
        let has_text = app.sel.text.is_some();
        if has_text {
            if ui.button("Copy").clicked() {
                app.run(Action::Copy);
                ui.close();
            }
            if ui.button("Highlight").clicked() {
                app.apply_markup_to_selection(MarkupKind::Highlight);
                ui.close();
            }
            if ui.button("Underline").clicked() {
                app.apply_markup_to_selection(MarkupKind::Underline);
                ui.close();
            }
            if ui.button("Strike Through").clicked() {
                app.apply_markup_to_selection(MarkupKind::Strike);
                ui.close();
            }
            if ui.button("Search for Selection").clicked() {
                app.run(Action::UseSelForFind);
                ui.close();
            }
            ui.separator();
        }
        if app.sel.annot.is_some() {
            if ui.button("Delete Annotation").clicked() {
                app.delete_selection();
                ui.close();
            }
            ui.separator();
        }
        for (label, a) in [
            ("Rotate Left", Action::RotateLeft),
            ("Rotate Right", Action::RotateRight),
            ("Zoom In", Action::ZoomIn),
            ("Zoom Out", Action::ZoomOut),
            ("Zoom to Fit", Action::ZoomFit),
            ("Actual Size", Action::ActualSize),
        ] {
            if ui.add_enabled(app.enabled(a), egui::Button::new(label).shortcut_text(crate::app::shortcut_text(a))).clicked() {
                app.run(a);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Show Inspector").clicked() {
            app.run(Action::Inspector);
            ui.close();
        }
    });
}

fn field_at(app: &App, slot: usize, pp: Pos2) -> Option<crate::pdf::FieldInfo> {
    let d = &app.docs[app.active];
    let s = d.model.slots.get(slot)?;
    let PageSrc::Pdf { source, index } = s.src else { return None };
    if Some(source) != d.source {
        return None;
    }
    let info = d.info.as_ref()?;
    info.fields.iter().find(|f| f.page == index && f.rect.contains(pp) && !f.readonly && f.kind != FieldKind::Other).cloned()
}

fn begin_drag(app: &mut App, b: PageBox, pp: Pos2, screen: Pos2, origin: Pos2, zoom: f32) -> Drag {
    let tol = 6.0 / zoom.max(0.01);
    let doc = &app.docs[app.active];
    let slot = &doc.model.slots[b.idx];
    let dst = b.rect.translate(origin.to_vec2());

    // Handles of the selected annotation take priority.
    if let Some((d, s, id)) = app.sel.annot {
        if d == doc.id && s == b.idx {
            if let Some(a) = slot.annots.iter().find(|a| a.id == id) {
                if let Shape::Line { a: p0, b: p1, .. } = &a.shape {
                    for (end, p) in [*p0, *p1].iter().enumerate() {
                        if (slot.to_screen(dst, *p) - screen).length() < 9.0 {
                            app.docs[app.active].checkpoint("Change of Annotation Properties");
                            return Drag::LineEnd { slot: b.idx, id, end };
                        }
                    }
                } else if a.resizable() {
                    let sr = slot.rect_to_screen(dst, a.bounds()).expand(2.0);
                    for (h, hp) in annots::handle_points(sr).iter().enumerate() {
                        if (*hp - screen).length() < 9.0 {
                            let orig = sr;
                            app.docs[app.active].checkpoint("Resize");
                            return Drag::Resize { slot: b.idx, id, handle: h, orig };
                        }
                    }
                }
            }
        }
    }
    // Handles of the rectangular selection.
    if let Some((d, s, r)) = app.sel.rect {
        if d == doc.id && s == b.idx {
            let sr = slot.rect_to_screen(dst, r);
            for (h, hp) in annots::handle_points(sr).iter().enumerate() {
                if (*hp - screen).length() < 9.0 {
                    return Drag::SelHandle { slot: b.idx, handle: h, orig: sr };
                }
            }
        }
    }
    // Grabbing an annotation moves it (in the selection tools).
    if matches!(app.tool, Tool::Text | Tool::Rect) {
        if let Some(a) = slot.annots.iter().rev().find(|a| annots::hit(a, pp, tol)) {
            let id = a.id;
            app.sel.annot = Some((doc.id, b.idx, id));
            app.sel.text = None;
            app.docs[app.active].checkpoint("Move");
            return Drag::Move { slot: b.idx, id, last: pp, moved: false };
        }
    }
    app.sel.annot = None;
    app.sel.editing = None;
    app.sel.field = None;
    let idx = b.idx;
    match app.tool {
        Tool::Text => {
            let is_image = matches!(slot.src, PageSrc::Image { .. });
            if let Some(t) = app.slot_text(idx) {
                // On images, only start a text selection right on recognised text.
                let reach = if is_image { vec2(4.0, 2.0) } else { vec2(40.0, 6.0) };
                if !t.chars.is_empty() && t.lines.iter().any(|l| l.expand2(reach).contains(pp)) {
                    let i = t.nearest(pp).unwrap_or(0);
                    let id = app.docs[app.active].id;
                    app.sel.text = Some(TextSel { doc: id, slot: idx, a: i, b: i });
                    app.sel.rect = None;
                    return Drag::Text { slot: idx, anchor: i };
                }
            }
            app.sel.text = None;
            Drag::Marquee { slot: idx, start: pp }
        }
        Tool::Rect => {
            app.sel.text = None;
            Drag::Marquee { slot: idx, start: pp }
        }
        Tool::Sketch => Drag::Ink { slot: idx, pts: vec![pp] },
        Tool::Shape(t) => Drag::Shape { slot: idx, tool: t, start: pp, cur: pp },
        Tool::Markup(kind) => {
            if let Some(t) = app.slot_text(idx) {
                let i = t.nearest(pp).unwrap_or(0);
                let id = app.docs[app.active].id;
                app.sel.text = Some(TextSel { doc: id, slot: idx, a: i, b: i });
                return Drag::Markup { slot: idx, kind, anchor: i };
            }
            Drag::None
        }
        Tool::Redact => Drag::Redact { slot: idx, start: pp },
        Tool::InstantAlpha => {
            let f = doc.model.frames.get(idx);
            match f {
                Some(f) if pp.x >= 0.0 && pp.y >= 0.0 && pp.x < f.width() as f32 && pp.y < f.height() as f32 => {
                    Drag::Alpha { slot: idx, seed: (pp.x as u32, pp.y as u32), start: screen }
                }
                _ => Drag::None,
            }
        }
        _ => Drag::None,
    }
}

fn constrain(start: Pos2, cur: Pos2, shift: bool, line: bool) -> Pos2 {
    if !shift {
        return cur;
    }
    let d = cur - start;
    if line {
        let ang = d.y.atan2(d.x);
        let snap = (ang / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
        let len = d.length();
        start + vec2(snap.cos(), snap.sin()) * len
    } else {
        let m = d.x.abs().max(d.y.abs());
        start + vec2(m * d.x.signum(), m * d.y.signum())
    }
}

fn update_drag(app: &mut App, boxes: &[PageBox], origin: Pos2, p: Pos2, zoom: f32, shift: bool) {
    let slot_idx = match &app.drag {
        Drag::Text { slot, .. }
        | Drag::Marquee { slot, .. }
        | Drag::Ink { slot, .. }
        | Drag::Shape { slot, .. }
        | Drag::Move { slot, .. }
        | Drag::Resize { slot, .. }
        | Drag::LineEnd { slot, .. }
        | Drag::Alpha { slot, .. }
        | Drag::Markup { slot, .. }
        | Drag::Redact { slot, .. }
        | Drag::SelHandle { slot, .. } => *slot,
        Drag::Pan | Drag::None => return,
    };
    let Some(b) = boxes.iter().find(|b| b.idx == slot_idx) else { return };
    let dst = b.rect.translate(origin.to_vec2());
    let doc_id = app.docs[app.active].id;
    let slot = app.docs[app.active].model.slots[slot_idx].clone();
    let pp = slot.to_page(dst, p);
    let clamped = pos2(pp.x.clamp(slot.bounds.min.x, slot.bounds.max.x), pp.y.clamp(slot.bounds.min.y, slot.bounds.max.y));
    match &mut app.drag {
        Drag::Text { anchor, .. } | Drag::Markup { anchor, .. } => {
            let a = *anchor;
            if let Some(t) = app.slot_text(slot_idx) {
                let i = t.nearest(pp).unwrap_or(a);
                app.sel.text = Some(TextSel { doc: doc_id, slot: slot_idx, a, b: i });
            }
        }
        Drag::Marquee { start, .. } | Drag::Redact { start, .. } => {
            let r = Rect::from_two_pos(*start, clamped);
            if matches!(app.drag, Drag::Marquee { .. }) {
                app.sel.rect = Some((doc_id, slot_idx, r));
            } else {
                app.ctx.data_mut(|d| d.insert_temp(egui::Id::new("redact-preview"), (slot_idx, r)));
            }
        }
        Drag::Ink { pts, .. } => {
            if pts.last().map(|l| (*l - pp).length() * zoom > 1.5).unwrap_or(true) {
                pts.push(pp);
            }
        }
        Drag::Shape { start, cur, tool, .. } => {
            let line = matches!(tool, ShapeTool::Line | ShapeTool::Arrow);
            *cur = constrain(*start, pp, shift, line);
        }
        Drag::Move { id, last, moved, .. } => {
            let d = pp - *last;
            *last = pp;
            *moved = true;
            let id = *id;
            if let Some(a) = app.docs[app.active].model.slots[slot_idx].annots.iter_mut().find(|a| a.id == id) {
                a.translate(d);
            }
        }
        Drag::Resize { id, handle, orig, .. } => {
            let nr = annots::drag_handle(*orig, *handle, p);
            let id = *id;
            let target = slot.rect_to_page(dst, nr.shrink(2.0));
            if target.width() > 2.0 && target.height() > 2.0 {
                if let Some(a) = app.docs[app.active].model.slots[slot_idx].annots.iter_mut().find(|a| a.id == id) {
                    let cur = a.bounds();
                    a.set_bounds(cur, target);
                }
            }
        }
        Drag::LineEnd { id, end, .. } => {
            let (id, end) = (*id, *end);
            if let Some(a) = app.docs[app.active].model.slots[slot_idx].annots.iter_mut().find(|a| a.id == id) {
                if let Shape::Line { a: p0, b: p1, .. } = &mut a.shape {
                    let other = if end == 0 { *p1 } else { *p0 };
                    let np = constrain(other, pp, shift, true);
                    if end == 0 {
                        *p0 = np;
                    } else {
                        *p1 = np;
                    }
                    a.dirty = true;
                }
            }
        }
        Drag::SelHandle { handle, orig, .. } => {
            let nr = annots::drag_handle(*orig, *handle, p);
            app.sel.rect = Some((doc_id, slot_idx, slot.rect_to_page(dst, nr).intersect(slot.bounds)));
        }
        Drag::Alpha { seed, start, .. } => {
            let tol = ((p - *start).length() / 2.0).clamp(4.0, 120.0);
            let seed = *seed;
            let last: Option<f32> = app.ctx.data(|d| d.get_temp(egui::Id::new("alpha-tol")));
            if last.map(|l| (l - tol).abs() >= 3.0).unwrap_or(true) {
                app.ctx.data_mut(|d| d.insert_temp(egui::Id::new("alpha-tol"), tol));
                if let Some(f) = app.docs[app.active].model.frames.get(slot_idx).cloned() {
                    let mask = crate::imaging::flood_mask(&f, &[seed], tol);
                    app.set_alpha_mask(doc_id, slot_idx, mask);
                }
            }
        }
        Drag::Pan | Drag::None => {}
    }
}

fn end_drag(app: &mut App, _boxes: &[PageBox], _origin: Pos2, zoom: f32) {
    let drag = std::mem::replace(&mut app.drag, Drag::None);
    let doc_id = app.docs[app.active].id;
    let author = app.author();
    let add = |app: &mut App, slot: usize, mut a: Annot, what: &'static str| {
        a.author = author.clone();
        let id = a.id;
        let d = &mut app.docs[app.active];
        d.checkpoint(what);
        d.model.slots[slot].annots.push(a);
        app.sel.annot = Some((doc_id, slot, id));
    };
    match drag {
        Drag::Ink { slot, pts } => {
            if pts.len() > 1 || zoom > 0.0 {
                let pts = simplify(&pts, 0.6 / zoom.max(0.01));
                add(app, slot, Annot::new(Shape::Ink(vec![pts]), app.shape_style.clone()), "Add Sketch");
                app.sel.annot = None;
            }
        }
        Drag::Shape { slot, tool, start, cur } => {
            let min = 4.0 / zoom.max(0.01);
            let (start, cur) = if (cur - start).length() < min {
                // A click places a default-sized shape.
                (start - vec2(40.0, 30.0), start + vec2(40.0, 30.0))
            } else {
                (start, cur)
            };
            let r = Rect::from_two_pos(start, cur);
            let mut style = app.shape_style.clone();
            let shape = match tool {
                ShapeTool::Line => Shape::Line { a: start, b: cur, arrow: false },
                ShapeTool::Arrow => Shape::Line { a: start, b: cur, arrow: true },
                ShapeTool::Rect => Shape::Rect(r),
                ShapeTool::RoundRect => Shape::RoundRect(r),
                ShapeTool::Oval => Shape::Oval(r),
                ShapeTool::Star => Shape::Star(r),
                ShapeTool::Bubble => {
                    if style.fill.is_none() {
                        style.fill = Some(Color32::WHITE);
                    }
                    Shape::Bubble(r)
                }
            };
            add(app, slot, Annot::new(shape, style), "Add Shape");
            app.tool = Tool::Text;
        }
        Drag::Markup { slot, kind, .. } => {
            let applied = app.apply_markup_to_selection(kind);
            let _ = (slot, applied);
        }
        Drag::Redact { slot, start } => {
            let r: Option<(usize, Rect)> = app.ctx.data_mut(|d| {
                let v = d.get_temp(egui::Id::new("redact-preview"));
                d.remove::<(usize, Rect)>(egui::Id::new("redact-preview"));
                v
            });
            if let Some((_, r)) = r {
                if r.width() > 2.0 && r.height() > 2.0 {
                    add(app, slot, Annot::new(Shape::Redact(r), Style::default()), "Redact");
                }
            }
            let _ = start;
        }
        Drag::Marquee { slot, .. } => {
            if let Some((_, _, r)) = app.sel.rect {
                if r.width() < 2.0 || r.height() < 2.0 {
                    app.sel.rect = None;
                } else if app.docs[app.active].kind == Kind::Pdf && app.tool == Tool::Text {
                    // Rect dragged on a text-less area of a PDF: keep it (enables Crop / Zoom to Selection).
                }
            }
            let _ = slot;
        }
        Drag::Alpha { .. } => {
            app.ctx.data_mut(|d| d.remove_temp::<f32>(egui::Id::new("alpha-tol")));
        }
        Drag::Move { moved, .. } => {
            if !moved {
                app.docs[app.active].cancel_checkpoint();
            }
        }
        _ => {}
    }
}

/// Drop points closer than `eps` to keep ink light.
fn simplify(pts: &[Pos2], eps: f32) -> Vec<Pos2> {
    let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().map(|l| (*l - *p).length() >= eps).unwrap_or(true) {
            out.push(*p);
        }
    }
    if out.len() == 1 && pts.len() > 1 {
        out.push(*pts.last().unwrap());
    }
    out
}

fn click(app: &mut App, slot_idx: usize, pp: Pos2, double: bool, triple: bool, doc_id: u64, zoom: f32) {
    let tol = 6.0 / zoom.max(0.01);
    let author = app.author();
    let slot = app.docs[app.active].model.slots[slot_idx].clone();
    match app.tool {
        Tool::TextBox => {
            let mut style = app.text_style.clone();
            style.width = 0.0;
            let w = 200.0 / zoom.max(0.3);
            let h = (style.font_size * 2.2).max(24.0);
            let mut a = Annot::new(Shape::Text { rect: Rect::from_min_size(pp - vec2(4.0, h / 2.0), vec2(w, h)), text: String::new() }, style);
            a.author = author;
            let id = a.id;
            let d = &mut app.docs[app.active];
            d.checkpoint("Add Text");
            d.model.slots[slot_idx].annots.push(a);
            app.sel.annot = Some((doc_id, slot_idx, id));
            app.sel.editing = Some(id);
            app.tool = Tool::Text;
            return;
        }
        Tool::Note => {
            let mut a = Annot::new(Shape::Note { at: pp - vec2(10.0, 10.0), text: String::new() }, Style { stroke: Color32::from_rgb(0xFF, 0xD6, 0x0A), ..Style::default() });
            a.author = author;
            let id = a.id;
            let d = &mut app.docs[app.active];
            d.checkpoint("Add Note");
            d.model.slots[slot_idx].annots.push(a);
            app.sel.annot = Some((doc_id, slot_idx, id));
            app.sel.editing = Some(id);
            app.tool = Tool::Text;
            return;
        }
        Tool::Signature(i) => {
            if let Some(sig) = app.settings.signatures.get(i).cloned() {
                let w = 150.0f32.min(slot.bounds.width() * 0.6);
                let h = w * sig.aspect;
                let r = Rect::from_center_size(pp, vec2(w, h));
                let strokes = sig.strokes.iter().map(|s| s.iter().map(|p| pos2(r.min.x + p[0] * w, r.min.y + p[1] * w)).collect()).collect();
                let style = Style { stroke: Color32::from_rgb(0x10, 0x10, 0x30), width: 1.6, ..Style::default() };
                let mut a = Annot::new(Shape::Signature { rect: r, strokes }, style);
                a.author = author;
                let id = a.id;
                let d = &mut app.docs[app.active];
                d.checkpoint("Add Signature");
                d.model.slots[slot_idx].annots.push(a);
                app.sel.annot = Some((doc_id, slot_idx, id));
                app.tool = Tool::Text;
            }
            return;
        }
        Tool::Polygon => {
            app.polygon.push(pp);
            if double && app.polygon.len() >= 3 {
                let mut pts = std::mem::take(&mut app.polygon);
                pts.dedup_by(|a, b| (*a - *b).length() < tol);
                let mut a = Annot::new(Shape::Polygon(pts), app.shape_style.clone());
                a.author = author;
                let id = a.id;
                let d = &mut app.docs[app.active];
                d.checkpoint("Add Shape");
                d.model.slots[slot_idx].annots.push(a);
                app.sel.annot = Some((doc_id, slot_idx, id));
                app.tool = Tool::Text;
            }
            return;
        }
        _ => {}
    }

    // Annotation under the pointer?
    if let Some(a) = slot.annots.iter().rev().find(|a| annots::hit(a, pp, tol)) {
        let id = a.id;
        app.sel.annot = Some((doc_id, slot_idx, id));
        app.sel.text = None;
        app.sel.rect = None;
        let editable = matches!(a.shape, Shape::Text { .. }) && double || matches!(a.shape, Shape::Note { .. });
        if editable {
            app.docs[app.active].checkpoint("Text");
            app.sel.editing = Some(id);
        }
        return;
    }
    app.sel.annot = None;
    app.sel.editing = None;

    // Form fields.
    if app.tool == Tool::Text {
        if let Some(f) = field_at(app, slot_idx, pp) {
            let src = app.docs[app.active].source.unwrap_or(0);
            match &f.kind {
                FieldKind::Checkbox { on } | FieldKind::Radio { on } => {
                    let cur = field_value(&app.docs[app.active], src, &f);
                    let is_radio = matches!(f.kind, FieldKind::Radio { .. });
                    let newv = if cur == *on && !is_radio { "Off".to_string() } else { on.clone() };
                    set_field(&mut app.docs[app.active], src, &f, newv);
                }
                _ => {
                    app.sel.field = Some((doc_id, f.xref));
                }
            }
            return;
        }
        app.sel.field = None;
    }

    // Links (single click, no text selection in progress).
    if app.tool == Tool::Text && !double && !triple {
        if let PageSrc::Pdf { source, index } = slot.src {
            if let Some(link) = app.page_links(source, index).and_then(|l| l.iter().find(|l| l.rect.contains(pp)).cloned()) {
                match link.target {
                    LinkTarget::Uri(u) => app.ctx.open_url(egui::OpenUrl::new_tab(u)),
                    LinkTarget::Page(p, y) => {
                        let target = app.docs[app.active].model.slots.iter().position(|s| s.src == PageSrc::Pdf { source, index: p });
                        if let Some(t) = target {
                            app.go_to(t);
                            if let Some(d) = app.doc_mut() {
                                d.pending_scroll = Some((t, y));
                            }
                        }
                    }
                }
                app.sel.text = None;
                return;
            }
        }
    }

    // Word / line selection.
    if (double || triple) && matches!(app.tool, Tool::Text | Tool::Markup(_)) {
        if let Some(t) = app.slot_text(slot_idx) {
            if let Some(i) = t.chars.iter().position(|c| c.rect.expand(1.0).contains(pp)) {
                let (a, b) = if triple { t.line_at(i) } else { t.word_at(i) };
                app.sel.text = Some(TextSel { doc: doc_id, slot: slot_idx, a, b });
                if let Tool::Markup(k) = app.tool {
                    app.apply_markup_to_selection(k);
                }
                return;
            }
        }
    }
    if !double && !triple {
        app.sel.text = None;
        if app.tool == Tool::Text || app.tool == Tool::Rect {
            app.sel.rect = None;
        }
        if app.tool != Tool::InstantAlpha {
            app.alpha = None;
        }
    }
    app.docs[app.active].current = slot_idx;
}

pub fn field_value(doc: &Document, source: u64, f: &crate::pdf::FieldInfo) -> String {
    doc.model
        .fields
        .iter()
        .find(|e| e.source == source && e.xref == f.xref)
        .map(|e| e.value.clone())
        .unwrap_or_else(|| f.value.clone())
}

pub fn set_field(doc: &mut Document, source: u64, f: &crate::pdf::FieldInfo, value: String) {
    doc.checkpoint("Change of Form Field");
    // Radio groups: one button on means its siblings go off.
    if let FieldKind::Radio { .. } = f.kind {
        if let Some(info) = doc.info.clone() {
            for other in info.fields.iter().filter(|o| o.name == f.name && o.xref != f.xref) {
                upsert_field(doc, source, other.page, other.xref, "Off".into());
            }
        }
    }
    upsert_field(doc, source, f.page, f.xref, value);
}

fn upsert_field(doc: &mut Document, source: u64, page: u32, xref: i32, value: String) {
    match doc.model.fields.iter_mut().find(|e| e.source == source && e.xref == xref) {
        Some(e) => e.value = value,
        None => doc.model.fields.push(crate::model::FieldEdit { source, page, xref, value }),
    }
}

/// Widgets drawn above the pages: in-progress shapes, selection chrome, editors.
fn overlays(app: &mut App, ui: &mut Ui, boxes: &[PageBox], origin: Pos2, zoom: f32) {
    let pal = Palette::of(ui.ctx());
    let painter = ui.painter().clone();
    let doc_id = app.docs[app.active].id;
    let ctx = ui.ctx().clone();

    for b in boxes {
        let dst = b.rect.translate(origin.to_vec2());
        if !ui.clip_rect().intersects(dst) {
            continue;
        }
        let slot = app.docs[app.active].model.slots[b.idx].clone();
        let pclip = painter.with_clip_rect(dst.expand(12.0).intersect(ui.clip_rect()));

        // In-progress drags
        match &app.drag {
            Drag::Ink { slot: s, pts } if *s == b.idx => {
                let a = Annot::new(Shape::Ink(vec![pts.clone()]), app.shape_style.clone());
                annots::paint(&pclip, &slot, dst, zoom, &a);
            }
            Drag::Shape { slot: s, tool, start, cur } if *s == b.idx => {
                let r = Rect::from_two_pos(*start, *cur);
                let shape = match tool {
                    ShapeTool::Line => Shape::Line { a: *start, b: *cur, arrow: false },
                    ShapeTool::Arrow => Shape::Line { a: *start, b: *cur, arrow: true },
                    ShapeTool::Rect => Shape::Rect(r),
                    ShapeTool::RoundRect => Shape::RoundRect(r),
                    ShapeTool::Oval => Shape::Oval(r),
                    ShapeTool::Star => Shape::Star(r),
                    ShapeTool::Bubble => Shape::Bubble(r),
                };
                annots::paint(&pclip, &slot, dst, zoom, &Annot::new(shape, app.shape_style.clone()));
            }
            _ => {}
        }
        if let Some((s, r)) = ctx.data(|d| d.get_temp::<(usize, Rect)>(egui::Id::new("redact-preview"))) {
            if s == b.idx && matches!(app.drag, Drag::Redact { .. }) {
                pclip.rect_filled(slot.rect_to_screen(dst, r), 0.0, Color32::from_black_alpha(200));
            }
        }
        if app.tool == Tool::Polygon && !app.polygon.is_empty() && app.docs[app.active].current == b.idx {
            let mut pts: Vec<Pos2> = app.polygon.iter().map(|p| slot.to_screen(dst, *p)).collect();
            if let Some(h) = ui.ctx().pointer_hover_pos() {
                pts.push(h);
            }
            pclip.add(egui::Shape::line(pts, Stroke::new(app.shape_style.width * zoom, app.shape_style.stroke)));
        }

        // Rectangular selection
        if let Some((d, s, r)) = app.sel.rect {
            if d == doc_id && s == b.idx {
                let sr = slot.rect_to_screen(dst, r);
                // Dim outside the selection, like Preview.
                let outside = Color32::from_black_alpha(70);
                pclip.rect_filled(Rect::from_min_max(dst.min, pos2(dst.max.x, sr.min.y)), 0.0, outside);
                pclip.rect_filled(Rect::from_min_max(pos2(dst.min.x, sr.max.y), dst.max), 0.0, outside);
                pclip.rect_filled(Rect::from_min_max(pos2(dst.min.x, sr.min.y), pos2(sr.min.x, sr.max.y)), 0.0, outside);
                pclip.rect_filled(Rect::from_min_max(pos2(sr.max.x, sr.min.y), pos2(dst.max.x, sr.max.y)), 0.0, outside);
                pclip.extend(egui::Shape::dashed_line(&[sr.left_top(), sr.right_top(), sr.right_bottom(), sr.left_bottom(), sr.left_top()], Stroke::new(1.0, Color32::WHITE), 5.0, 4.0));
                for hp in annots::handle_points(sr) {
                    pclip.circle(hp, 4.0, Color32::WHITE, Stroke::new(1.2, pal.accent));
                }
                let unit = if app.docs[app.active].kind == Kind::Image { "px" } else { "pt" };
                annots::label_badge(&pclip, sr.left_top() - vec2(0.0, 4.0), &format!("{:.0} × {:.0} {unit}", r.width(), r.height()), pal.accent);
            }
        }

        // Selected annotation
        if let Some((d, s, id)) = app.sel.annot {
            if d == doc_id && s == b.idx && app.sel.editing != Some(id) {
                if let Some(a) = slot.annots.iter().find(|a| a.id == id) {
                    annots::paint_selection(&pclip, &slot, dst, a, pal.accent);
                }
            }
        }

        // Form fields: highlight, edited values and the active editor.
        if let (Some(info), PageSrc::Pdf { source, index }) = (app.docs[app.active].info.clone(), slot.src.clone()) {
            if Some(source) == app.docs[app.active].source {
                for f in info.fields.iter().filter(|f| f.page == index) {
                    let fr = slot.rect_to_screen(dst, f.rect);
                    if app.highlight_fields && !f.readonly {
                        pclip.rect_filled(fr, 2.0, pal.accent.gamma_multiply(0.12));
                        pclip.rect_stroke(fr, 2.0, Stroke::new(1.0, pal.accent.gamma_multiply(0.5)), StrokeKind::Inside);
                    }
                    let edited = app.docs[app.active].model.fields.iter().find(|e| e.source == source && e.xref == f.xref).map(|e| e.value.clone());
                    let active = app.sel.field == Some((doc_id, f.xref));
                    if active {
                        field_editor(app, ui, f, fr, source, zoom);
                    } else if let Some(v) = edited {
                        paint_field_value(&pclip, f, fr, &v, zoom);
                    }
                }
            }
        }

        // Text box / note editors
        if let Some(id) = app.sel.editing {
            let found = slot.annots.iter().find(|a| a.id == id).cloned();
            if let Some(a) = found {
                match &a.shape {
                    Shape::Text { rect, .. } => {
                        let sr = slot.rect_to_screen(dst, *rect);
                        let size = a.style.font_size * zoom;
                        if let Some(fill) = a.style.fill {
                            painter.rect_filled(sr, 0.0, fill);
                        }
                        painter.rect_stroke(sr, 0.0, Stroke::new(1.0, pal.accent), StrokeKind::Outside);
                        let d = &mut app.docs[app.active];
                        if let Some(Shape::Text { text, .. }) = d.model.slots[b.idx].annots.iter_mut().find(|x| x.id == id).map(|x| &mut x.shape) {
                            let te = egui::TextEdit::multiline(text)
                                .font(FontId::proportional(size.max(6.0)))
                                .text_color(a.style.text_color)
                                .frame(egui::Frame::NONE)
                                .background_color(Color32::TRANSPARENT)
                                .margin(egui::Margin::same((4.0 * zoom).clamp(1.0, 20.0) as i8))
                                .horizontal_align(match a.style.align {
                                    crate::model::Align::Left => egui::Align::LEFT,
                                    crate::model::Align::Center => egui::Align::Center,
                                    crate::model::Align::Right => egui::Align::RIGHT,
                                })
                                .hint_text("Text")
                                .desired_width(sr.width());
                            let r = ui.put(sr, te);
                            if !r.has_focus() && !r.lost_focus() {
                                r.request_focus();
                            }
                            // Grow the box to fit its text.
                            let need = r.rect.height().max(sr.height());
                            if need > sr.height() + 1.0 {
                                if let Some(x) = d.model.slots[b.idx].annots.iter_mut().find(|x| x.id == id) {
                                    if let Shape::Text { rect, .. } = &mut x.shape {
                                        rect.max.y = rect.min.y + need / zoom;
                                    }
                                }
                            }
                            if r.changed() {
                                if let Some(x) = d.model.slots[b.idx].annots.iter_mut().find(|x| x.id == id) {
                                    x.dirty = true;
                                }
                            }
                            if r.lost_focus() {
                                app.sel.editing = None;
                                finish_text_edit(app, b.idx, id);
                            }
                        }
                    }
                    Shape::Note { at, .. } => {
                        let anchor = slot.to_screen(dst, *at) + vec2((20.0 * zoom).clamp(14.0, 30.0) + 6.0, 0.0);
                        note_popup(app, ui, b.idx, id, anchor, a.style.stroke);
                    }
                    _ => app.sel.editing = None,
                }
            }
        }
    }
}

fn finish_text_edit(app: &mut App, slot: usize, id: u64) {
    let d = &mut app.docs[app.active];
    let empty = d.model.slots[slot].annots.iter().find(|a| a.id == id).and_then(|a| a.text().map(|t| t.trim().is_empty())).unwrap_or(false);
    if empty {
        if let Some(pos) = d.model.slots[slot].annots.iter().position(|a| a.id == id) {
            let a = d.model.slots[slot].annots.remove(pos);
            if let (Some(x), PageSrc::Pdf { source, .. }) = (a.origin, d.model.slots[slot].src.clone()) {
                d.model.deleted_origins.push((source, x));
            }
        }
        app.sel.annot = None;
    }
}

fn note_popup(app: &mut App, ui: &mut Ui, slot: usize, id: u64, anchor: Pos2, color: Color32) {
    let ctx = ui.ctx().clone();
    let pal = Palette::of(&ctx);
    let mut close = false;
    let mut delete = false;
    let author = app.docs[app.active].model.slots[slot].annots.iter().find(|a| a.id == id).map(|a| a.author.clone()).unwrap_or_default();
    let resp = egui::Area::new(egui::Id::new(("note", id)))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(&ctx, |ui| {
            let fill = if pal.dark { Color32::from_rgb(0x4A, 0x43, 0x20) } else { Color32::from_rgb(0xFF, 0xF7, 0xC2) };
            egui::Frame::new()
                .fill(fill)
                .stroke(Stroke::new(1.0, color.gamma_multiply(0.8)))
                .corner_radius(8)
                .inner_margin(10)
                .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(50) })
                .show(ui, |ui| {
                    ui.set_width(240.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(if author.is_empty() { "Note".to_string() } else { author.clone() }).small().strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(egui_phosphor::regular::X).on_hover_text("Close").clicked() {
                                close = true;
                            }
                            if ui.small_button(egui_phosphor::regular::TRASH).on_hover_text("Delete note").clicked() {
                                delete = true;
                            }
                        });
                    });
                    let d = &mut app.docs[app.active];
                    if let Some(x) = d.model.slots[slot].annots.iter_mut().find(|a| a.id == id) {
                        if let Shape::Note { text, .. } = &mut x.shape {
                            let r = ui.add(egui::TextEdit::multiline(text).desired_rows(5).desired_width(f32::INFINITY).frame(egui::Frame::NONE).background_color(Color32::TRANSPARENT).hint_text("Type a note…"));
                            if r.changed() {
                                x.dirty = true;
                            }
                            if !r.has_focus() && ui.ctx().memory(|m| m.focused().is_none()) {
                                r.request_focus();
                            }
                        }
                    }
                });
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed()) && !resp.response.rect.contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or_default())) && !resp.response.hovered();
    if delete {
        app.sel.editing = None;
        app.sel.annot = Some((app.docs[app.active].id, slot, id));
        app.delete_selection();
    } else if close || clicked_outside {
        app.sel.editing = None;
    }
}

fn paint_field_value(painter: &egui::Painter, f: &crate::pdf::FieldInfo, r: Rect, v: &str, zoom: f32) {
    match &f.kind {
        FieldKind::Checkbox { on } | FieldKind::Radio { on } => {
            painter.rect_filled(r.shrink(1.0), 1.0, Color32::WHITE);
            if v == on {
                let c = r.center();
                let s = r.width().min(r.height()) * 0.32;
                painter.line(vec![c + vec2(-s, 0.0), c + vec2(-s * 0.3, s * 0.7), c + vec2(s, -s * 0.8)], Stroke::new((1.6 * zoom).max(1.2), Color32::BLACK));
            }
        }
        _ => {
            painter.rect_filled(r.shrink(0.5), 1.0, Color32::from_rgb(0xF7, 0xF9, 0xFF));
            let size = (r.height() * 0.62).min(12.0 * zoom).max(6.0);
            let p = painter.with_clip_rect(r.intersect(painter.clip_rect()));
            p.text(pos2(r.min.x + 3.0 * zoom, r.center().y), Align2::LEFT_CENTER, v, FontId::proportional(size), Color32::BLACK);
        }
    }
}

fn field_editor(app: &mut App, ui: &mut Ui, f: &crate::pdf::FieldInfo, r: Rect, source: u64, zoom: f32) {
    let current = field_value(&app.docs[app.active], source, f);
    match &f.kind {
        FieldKind::Choice { options } => {
            let mut chosen = None;
            let resp = ui.put(r, |ui: &mut Ui| {
                egui::ComboBox::from_id_salt(("field", f.xref))
                    .selected_text(current.clone())
                    .width(r.width())
                    .show_ui(ui, |ui| {
                        for o in options {
                            if ui.selectable_label(*o == current, o).clicked() {
                                chosen = Some(o.clone());
                            }
                        }
                    })
                    .response
            });
            let _ = resp;
            if let Some(c) = chosen {
                set_field(&mut app.docs[app.active], source, f, c);
                app.sel.field = None;
            }
        }
        FieldKind::Text { multiline } => {
            let key = egui::Id::new(("field-edit", f.xref));
            let mut text: String = ui.ctx().data_mut(|d| d.get_temp(key)).unwrap_or(current.clone());
            let size = (r.height() * 0.62).min(12.0 * zoom).max(6.0);
            let te = if *multiline { egui::TextEdit::multiline(&mut text) } else { egui::TextEdit::singleline(&mut text) };
            let resp = ui.put(r, te.font(FontId::proportional(size)).text_color(Color32::BLACK).background_color(Color32::from_rgb(0xF7, 0xF9, 0xFF)).margin(egui::Margin::symmetric(3, 1)));
            if !resp.has_focus() && !resp.lost_focus() {
                resp.request_focus();
            }
            ui.ctx().data_mut(|d| d.insert_temp(key, text.clone()));
            let tab = ui.input(|i| i.key_pressed(Key::Tab));
            if resp.lost_focus() {
                ui.ctx().data_mut(|d| d.remove_temp::<String>(key));
                if text != current {
                    set_field(&mut app.docs[app.active], source, f, text);
                }
                app.sel.field = None;
                if tab {
                    // Tab moves to the next field.
                    if let Some(info) = app.docs[app.active].info.clone() {
                        let editable: Vec<&crate::pdf::FieldInfo> = info.fields.iter().filter(|x| !x.readonly && matches!(x.kind, FieldKind::Text { .. })).collect();
                        if let Some(pos) = editable.iter().position(|x| x.xref == f.xref) {
                            if let Some(next) = editable.get(pos + 1) {
                                app.sel.field = Some((app.docs[app.active].id, next.xref));
                            }
                        }
                    }
                }
            }
        }
        _ => app.sel.field = None,
    }
}

/// Play/pause for animated GIFs, advancing frames on their own delays.
fn animation_controls(app: &mut App, ui: &mut Ui, canvas: Rect) {
    let Some(d) = app.docs.get_mut(app.active) else { return };
    if !d.is_animated() || !d.ready() {
        return;
    }
    let n = d.page_count();
    if let Some(due) = d.playing {
        let now = Instant::now();
        if now >= due {
            d.current = (d.current + 1) % n;
            let delay = d.image.as_ref().and_then(|m| m.delays.get(d.current).copied()).unwrap_or(100);
            d.playing = Some(now + Duration::from_millis(delay as u64));
        }
        let wait = d.playing.unwrap().saturating_duration_since(now);
        ui.ctx().request_repaint_after(wait);
    }
    let pal = Palette::of(ui.ctx());
    let r = Rect::from_center_size(pos2(canvas.center().x, canvas.max.y - 34.0), vec2(150.0, 34.0));
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("gif-controls")));
    painter.rect_filled(r, 17.0, Color32::from_black_alpha(150));
    let icon = if d.playing.is_some() { egui_phosphor::regular::PAUSE } else { egui_phosphor::regular::PLAY };
    let br = Rect::from_center_size(r.left_center() + vec2(22.0, 0.0), vec2(28.0, 28.0));
    painter.text(br.center(), Align2::CENTER_CENTER, icon, FontId::proportional(16.0), Color32::WHITE);
    painter.text(r.center() + vec2(14.0, 0.0), Align2::CENTER_CENTER, format!("{} / {n}", d.current + 1), FontId::proportional(12.5), Color32::WHITE);
    let _ = pal;
    let resp = ui.interact(r, egui::Id::new("gif-play"), Sense::click());
    if resp.clicked() {
        d.playing = if d.playing.is_some() { None } else { Some(Instant::now()) };
    }
}

/// Preview's Live Text button: appears when text was recognised on the current page and
/// outlines everything that can be selected, copied or searched.
fn live_text_button(app: &mut App, ui: &mut Ui, canvas: Rect) {
    // Asking for the current page's text also starts recognition when it is needed.
    if let Some(cur) = app.doc().map(|d| d.current) {
        let _ = app.slot_text(cur);
    }
    if app.live_text_for_current().is_none() {
        return;
    }
    let pal = Palette::of(ui.ctx());
    let on = app.live_text_highlight;
    let r = Rect::from_min_size(canvas.max - vec2(132.0, 50.0), vec2(116.0, 34.0));
    let resp = ui.interact(r, egui::Id::new("live-text"), Sense::click());
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("live-text-btn")));
    painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(50) }.as_shape(r, 17.0));
    let fill = if on { pal.accent } else if pal.dark { Color32::from_rgb(0x3A, 0x3A, 0x3E) } else { Color32::WHITE };
    painter.rect(r, 17.0, fill, Stroke::new(1.0, pal.separator), StrokeKind::Inside);
    let fg = if on { Color32::WHITE } else { pal.text };
    painter.text(r.left_center() + vec2(18.0, 0.0), Align2::CENTER_CENTER, egui_phosphor::regular::SCAN, FontId::proportional(16.0), fg);
    painter.text(r.left_center() + vec2(34.0, 0.0), Align2::LEFT_CENTER, "Live Text", FontId::proportional(13.0), fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.on_hover_text("Show all recognised text — select, copy or search it").clicked() {
        app.live_text_highlight = !app.live_text_highlight;
    }
}

fn page_badge(app: &App, ui: &Ui, canvas: Rect) {
    let Some(until) = app.page_badge_until else { return };
    let now = Instant::now();
    if now > until {
        return;
    }
    let Some(d) = app.doc() else { return };
    if d.page_count() < 2 {
        return;
    }
    let t = (until - now).as_secs_f32();
    let alpha = (t / 0.3).min(1.0);
    let text = d.subtitle(app.settings.logical_page_numbers);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("badge")));
    let galley = painter.layout_no_wrap(text, FontId::proportional(12.5), Color32::WHITE.gamma_multiply(alpha));
    let size = galley.size() + vec2(22.0, 10.0);
    let r = Rect::from_center_size(pos2(canvas.center().x, canvas.max.y - 30.0), size);
    painter.rect_filled(r, size.y / 2.0, Color32::from_black_alpha((150.0 * alpha) as u8));
    painter.galley(r.min + vec2(11.0, 5.0), galley, Color32::WHITE);
    ui.ctx().request_repaint_after(Duration::from_millis(50));
}

fn magnifier(app: &mut App, ui: &mut Ui, boxes: &[PageBox], origin: Pos2, ppp: f32) {
    let ctx = ui.ctx().clone();
    let Some(p) = ctx.pointer_hover_pos() else { return };
    let Some(b) = page_at(boxes, origin, p, 0.0) else { return };
    let factor = 2.5;
    let lens = Rect::from_center_size(p, vec2(230.0, 230.0));
    let dst = b.rect.translate(origin.to_vec2());
    let zdst = Rect::from_min_max(p + (dst.min - p) * factor, p + (dst.max - p) * factor);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("loupe")));
    let pal = Palette::of(&ctx);
    painter.add(egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(90) }.as_shape(lens, 18.0));
    painter.rect_filled(lens, 18.0, pal.canvas);
    let App { docs, active, tiles, sources, images, proof, .. } = app;
    let doc = &docs[*active];
    let slot = &doc.model.slots[b.idx];
    let mut dc = DrawCtx { tiles, sources, images, frames: &doc.model.frames, thumb: doc.thumb.as_ref(), doc_id: doc.id, dark: app.settings.dark_pdf && doc.kind == Kind::Pdf, ppp, proof: proof.as_deref() };
    let inner = lens.shrink(3.0);
    draw_slot(&mut dc, &painter, slot, zdst, inner, false);
    let pclip = painter.with_clip_rect(inner);
    for a in &slot.annots {
        annots::paint(&pclip, slot, zdst, doc.zoom * factor, a);
    }
    painter.rect_stroke(lens, 18.0, Stroke::new(3.0, Color32::from_white_alpha(230)), StrokeKind::Inside);
    painter.rect_stroke(lens, 18.0, Stroke::new(1.0, Color32::from_black_alpha(60)), StrokeKind::Outside);
    ctx.set_cursor_icon(CursorIcon::None);
}

fn empty_state(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let r = ui.max_rect();
    let resp = ui.allocate_rect(r, Sense::click());
    ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(r.height() * 0.30);
            ui.label(egui::RichText::new(egui_phosphor::regular::FILE_TEXT).size(56.0).color(pal.text_weak.gamma_multiply(0.7)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Open a PDF or image").font(FontId::new(18.0, crate::theme::bold())).color(pal.text));
            ui.label(egui::RichText::new("Drop files here, or press Ctrl+O").color(pal.text_weak));
            ui.add_space(14.0);
            if ui.add(egui::Button::new(egui::RichText::new("Open…").size(14.0).color(Color32::WHITE)).min_size(vec2(110.0, 30.0)).fill(pal.accent).corner_radius(8)).clicked() {
                app.run(crate::app::Action::Open);
            }
            let recent: Vec<_> = app.settings.recent.iter().filter(|p| p.exists()).take(6).cloned().collect();
            if !recent.is_empty() {
                ui.add_space(22.0);
                ui.label(egui::RichText::new("Recent").small().color(pal.text_weak));
                for p in recent {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    if ui.add(egui::Button::new(egui::RichText::new(name).color(pal.accent)).frame(false)).on_hover_text(p.to_string_lossy()).clicked() {
                        app.open_paths(vec![p]);
                    }
                }
            }
        });
    });
    if resp.double_clicked() {
        app.run(crate::app::Action::Open);
    }
}

fn password_prompt(app: &mut App, ui: &mut Ui, wrong: bool) {
    let pal = Palette::of(ui.ctx());
    let key = egui::Id::new(("pw", app.active));
    let mut pw: String = ui.ctx().data_mut(|d| d.get_temp(key)).unwrap_or_default();
    let name = app.docs[app.active].name.clone();
    let mut submit = false;
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.28);
        ui.label(egui::RichText::new(egui_phosphor::regular::LOCK).size(44.0).color(pal.text_weak));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("“{name}” is password protected")).font(FontId::new(16.0, crate::theme::bold())));
        ui.label(egui::RichText::new("Enter the password to open it.").color(pal.text_weak));
        ui.add_space(10.0);
        let r = ui.add(egui::TextEdit::singleline(&mut pw).password(true).desired_width(240.0).hint_text("Password"));
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
        }
        if wrong {
            ui.label(egui::RichText::new("Incorrect password — try again.").color(ui.visuals().error_fg_color));
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            submit = true;
        }
        ui.add_space(6.0);
        if ui.add(egui::Button::new("Open").fill(pal.accent).min_size(vec2(90.0, 28.0))).clicked() {
            submit = true;
        }
    });
    if submit {
        ui.ctx().data_mut(|d| d.remove_temp::<String>(key));
        app.retry_password(app.active, pw);
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(key, pw));
    }
}

/// View ▸ Contact Sheet: a grid of every page in the main area.
fn contact_sheet(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let ppp = ui.ctx().pixels_per_point();
    let cell = 190.0;
    let cols = ((ui.available_width() - 40.0) / (cell + 20.0)).floor().max(1.0) as usize;
    let n = app.docs[app.active].page_count();
    let rows = n.div_ceil(cols);
    let mut go = None;
    ui.add_space(18.0);
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, cell + 34.0, rows, |ui, range| {
        for row in range {
            ui.horizontal(|ui| {
                ui.set_min_height(cell + 34.0);
                ui.add_space(20.0);
                for col in 0..cols {
                    let i = row * cols + col;
                    if i >= n {
                        break;
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(cell, cell + 28.0), Sense::click());
                    let App { docs, active, tiles, sources, images, settings, proof, .. } = app;
                    let doc = &docs[*active];
                    let slot = &doc.model.slots[i];
                    let ds = slot.display_size();
                    let s = (cell / ds.x).min(cell / ds.y);
                    let pr = Rect::from_center_size(pos2(r.center().x, r.min.y + cell / 2.0 + 4.0), ds * s * 0.94);
                    page_shadow(ui.painter(), pr, pal.page_shadow);
                    let mut dc = DrawCtx { tiles, sources, images, frames: &doc.model.frames, thumb: doc.thumb.as_ref(), doc_id: doc.id, dark: settings.dark_pdf && doc.kind == Kind::Pdf, ppp, proof: proof.as_deref() };
                    draw_slot(&mut dc, ui.painter(), slot, pr, ui.clip_rect(), true);
                    if doc.current == i {
                        ui.painter().rect_stroke(pr.expand(3.0), 4.0, Stroke::new(2.5, pal.accent), StrokeKind::Outside);
                    }
                    let label = if slot.label.is_empty() { (i + 1).to_string() } else { slot.label.clone() };
                    ui.painter().text(pos2(r.center().x, r.max.y - 10.0), Align2::CENTER_CENTER, label, FontId::proportional(12.0), pal.text_weak);
                    if resp.double_clicked() || resp.clicked() {
                        go = Some((i, resp.double_clicked()));
                    }
                    ui.add_space(20.0 - ui.spacing().item_spacing.x);
                }
            });
        }
    });
    if let Some((i, open)) = go {
        app.go_to(i);
        if open {
            app.contact_sheet = false;
        }
    }
}

/// Full-screen slideshow (View ▸ Slideshow).
pub fn slideshow(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 0.0, Color32::BLACK);
    let Some(ss) = app.slideshow.as_mut() else { return };
    let n = app.docs.get(app.active).map(|d| d.page_count()).unwrap_or(0);
    if n == 0 {
        app.slideshow = None;
        return;
    }
    let mut step = 0i32;
    ctx.input_mut(|i| {
        for k in [Key::ArrowRight, Key::ArrowDown, Key::PageDown, Key::Space, Key::Enter] {
            if i.consume_key(Modifiers::NONE, k) {
                step += 1;
            }
        }
        for k in [Key::ArrowLeft, Key::ArrowUp, Key::PageUp, Key::Backspace] {
            if i.consume_key(Modifiers::NONE, k) {
                step -= 1;
            }
        }
    });
    let resp = ui.allocate_rect(r, Sense::click());
    if resp.clicked() {
        step += 1;
    }
    if resp.secondary_clicked() {
        step -= 1;
    }
    if ctx.input(|i| i.pointer.delta().length() > 0.0) {
        ss.last_move = Instant::now();
    }
    ss.page = (ss.page as i32 + step).clamp(0, n as i32 - 1) as usize;
    let page = ss.page;
    let show_controls = ss.last_move.elapsed() < Duration::from_secs(2);
    let ppp = ctx.pixels_per_point();
    {
        let App { docs, active, tiles, sources, images, settings, proof, .. } = app;
        let doc = &docs[*active];
        let slot = &doc.model.slots[page];
        let ds = slot.display_size();
        let s = (r.width() / ds.x).min(r.height() / ds.y);
        let pr = Rect::from_center_size(r.center(), ds * s);
        let mut dc = DrawCtx { tiles, sources, images, frames: &doc.model.frames, thumb: doc.thumb.as_ref(), doc_id: doc.id, dark: settings.dark_pdf && doc.kind == Kind::Pdf, ppp, proof: proof.as_deref() };
        draw_slot(&mut dc, ui.painter(), slot, pr, r, false);
        for a in &slot.annots {
            annots::paint(ui.painter(), slot, pr, s, a);
        }
        // Prefetch the next page.
        if let Some(next) = doc.model.slots.get(page + 1) {
            let nds = next.display_size();
            let ns = (r.width() / nds.x).min(r.height() / nds.y);
            let off = Rect::from_center_size(r.center() + vec2(r.width() * 2.0, 0.0), nds * ns);
            draw_slot(&mut dc, ui.painter(), next, off, Rect::from_min_size(off.min, off.size()), false);
        }
    }
    if show_controls {
        let bar = Rect::from_center_size(pos2(r.center().x, r.max.y - 50.0), vec2(220.0, 44.0));
        ui.painter().rect_filled(bar, 22.0, Color32::from_black_alpha(170));
        let text = format!("{} / {}", page + 1, n);
        ui.painter().text(bar.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), Color32::WHITE);
        let left = Rect::from_center_size(bar.left_center() + vec2(26.0, 0.0), vec2(36.0, 36.0));
        let right = Rect::from_center_size(bar.right_center() - vec2(26.0, 0.0), vec2(36.0, 36.0));
        if ui.put(left, egui::Button::new(egui::RichText::new(egui_phosphor::regular::CARET_LEFT).size(18.0).color(Color32::WHITE)).frame(false)).clicked() {
            if let Some(ss) = app.slideshow.as_mut() {
                ss.page = ss.page.saturating_sub(1);
            }
        }
        if ui.put(right, egui::Button::new(egui::RichText::new(egui_phosphor::regular::CARET_RIGHT).size(18.0).color(Color32::WHITE)).frame(false)).clicked() {
            if let Some(ss) = app.slideshow.as_mut() {
                ss.page = (ss.page + 1).min(n - 1);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    } else {
        ctx.set_cursor_icon(CursorIcon::None);
    }
    if let Some(d) = app.docs.get_mut(app.active) {
        d.current = page;
    }
}

