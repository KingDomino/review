//! Texture caches and drawing page content (PDF tiles or image pixels) to the screen.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{pos2, Color32, Mesh, Painter, Rect, TextureHandle, TextureOptions};
use image::RgbaImage;

use crate::model::{PageSrc, Slot};
use crate::pdf::{Source, TileKey, TILE};

pub fn scale_for_level(l: i32) -> f32 {
    2f32.powf(l as f32 / 4.0)
}

/// Smallest ladder level that is at least as sharp as `scale`.
pub fn level_for_scale(s: f32) -> i32 {
    (s.max(1e-3).log2() * 4.0).ceil() as i32
}

/// Level at which the whole page fits in a single tile.
pub fn preview_level(bounds: Rect) -> i32 {
    let m = bounds.width().max(bounds.height()).max(1.0);
    ((TILE as f32 / m).log2() * 4.0).floor() as i32
}

/// Largest level that keeps a page under ~16k pixels per side.
pub fn max_level(bounds: Rect) -> i32 {
    let m = bounds.width().max(bounds.height()).max(1.0);
    ((16000.0 / m).log2() * 4.0).floor() as i32
}

struct TileEntry {
    tex: TextureHandle,
    used: u64,
}

/// Rendered PDF tiles, shared by all documents.
#[derive(Default)]
pub struct Tiles {
    map: HashMap<TileKey, TileEntry>,
    inflight: HashMap<TileKey, Arc<AtomicBool>>,
    wanted: HashSet<TileKey>,
    frame: u64,
}

impl Tiles {
    pub fn insert(&mut self, ctx: &egui::Context, key: TileKey, image: egui::ColorImage) {
        self.inflight.remove(&key);
        let tex = ctx.load_texture(format!("tile-{key:?}"), image, TextureOptions::LINEAR);
        self.map.insert(key, TileEntry { tex, used: self.frame });
    }

    /// Returns the texture if rendered; otherwise asks the source to render it.
    pub fn get(&mut self, key: TileKey, source: &Source) -> Option<&TextureHandle> {
        self.get_with(key, source, false)
    }

    /// Like [`Self::get`]; `urgent` requests are rendered before everything else.
    pub fn get_with(&mut self, key: TileKey, source: &Source, urgent: bool) -> Option<&TextureHandle> {
        self.wanted.insert(key);
        if self.map.contains_key(&key) {
            let e = self.map.get_mut(&key).unwrap();
            e.used = self.frame;
            return Some(&e.tex);
        }
        if !self.inflight.contains_key(&key) {
            let cancel = Arc::new(AtomicBool::new(false));
            source.render(key, scale_for_level(key.level), cancel.clone(), urgent);
            self.inflight.insert(key, cancel);
        }
        None
    }

    /// A cached tile without requesting it (fallback drawing must not queue work).
    pub fn peek(&mut self, key: &TileKey) -> Option<&TextureHandle> {
        let frame = self.frame;
        self.map.get_mut(key).map(|e| {
            e.used = frame;
            &e.tex
        })
    }

    /// Cancel work nobody looked at this frame and keep memory bounded.
    pub fn end_frame(&mut self) {
        let wanted = std::mem::take(&mut self.wanted);
        self.inflight.retain(|k, c| {
            let keep = wanted.contains(k);
            if !keep {
                c.store(true, Ordering::Relaxed);
            }
            keep
        });
        const BUDGET: usize = 360; // ~360 MB of 512² RGBA tiles
        if self.map.len() > BUDGET {
            let mut by_age: Vec<(u64, TileKey)> = self.map.iter().map(|(k, e)| (e.used, *k)).collect();
            by_age.sort_unstable_by_key(|(u, _)| *u);
            let excess = self.map.len() - BUDGET * 3 / 4;
            for (used, k) in by_age.into_iter().take(excess) {
                if used + 2 < self.frame {
                    self.map.remove(&k);
                }
            }
        }
        self.frame += 1;
    }

    pub fn drop_source(&mut self, source: u64) {
        self.map.retain(|k, _| k.source != source);
        self.inflight.retain(|k, c| {
            if k.source == source {
                c.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }

}

/// GPU textures for one image frame, split into tiles that fit any GPU.
pub struct ImageTex {
    image: Arc<RgbaImage>,
    tiles: Vec<(Rect, TextureHandle)>,
    thumb: TextureHandle,
}

const IMG_TILE: u32 = 2048;

impl ImageTex {
    pub fn new(ctx: &egui::Context, image: Arc<RgbaImage>) -> Self {
        let opts = TextureOptions {
            magnification: egui::TextureFilter::Linear,
            minification: egui::TextureFilter::Linear,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            mipmap_mode: Some(egui::TextureFilter::Linear),
        };
        let (w, h) = image.dimensions();
        let mut tiles = Vec::new();
        let mut y = 0;
        while y < h {
            let mut x = 0;
            let th = IMG_TILE.min(h - y);
            while x < w {
                let tw = IMG_TILE.min(w - x);
                let sub = image::imageops::crop_imm(&*image, x, y, tw, th).to_image();
                let ci = egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], sub.as_raw());
                let tex = ctx.load_texture(format!("img-{x}-{y}"), ci, opts);
                tiles.push((Rect::from_min_size(pos2(x as f32, y as f32), egui::vec2(tw as f32, th as f32)), tex));
                x += IMG_TILE;
            }
            y += IMG_TILE;
        }
        let small = image::imageops::thumbnail(&*image, 320.min(w).max(1), ((320.min(w) as f32 / w as f32) * h as f32).max(1.0) as u32);
        let ci = egui::ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw());
        let thumb = ctx.load_texture("img-thumb", ci, TextureOptions::LINEAR);
        Self { image, tiles, thumb }
    }

    pub fn is_for(&self, image: &Arc<RgbaImage>) -> bool {
        Arc::ptr_eq(&self.image, image)
    }
}

/// Page-space rect covered by tile (`tx`, `ty`) at scale `s`. Edge tiles are smaller than
/// `TILE`, so the rect must come from the texture's real pixel size, or they get stretched.
fn tile_rect(b: Rect, tx: i32, ty: i32, s: f32, tex: &TextureHandle) -> Rect {
    let t = TILE as f32;
    let [w, h] = tex.size();
    Rect::from_min_size(pos2(b.min.x + tx as f32 * t / s, b.min.y + ty as f32 * t / s), egui::vec2(w as f32 / s, h as f32 / s))
}

/// Draw a texture covering page-space rect `src` of `slot` into page display rect `dst`.
fn draw_quad(painter: &Painter, tex: egui::TextureId, slot: &Slot, dst: Rect, src: Rect, tint: Color32) {
    let corners = [src.left_top(), src.right_top(), src.right_bottom(), src.left_bottom()];
    let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
    let mut mesh = Mesh::with_texture(tex);
    for (c, uv) in corners.iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex { pos: slot.to_screen(dst, *c), uv, color: tint });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

pub struct DrawCtx<'a> {
    pub tiles: &'a mut Tiles,
    pub sources: &'a HashMap<u64, Source>,
    pub images: &'a mut HashMap<(u64, usize), ImageTex>,
    pub frames: &'a [Arc<RgbaImage>],
    /// Preview used while an image's full pixels are unloaded.
    pub thumb: Option<&'a Arc<RgbaImage>>,
    pub doc_id: u64,
    pub dark: bool,
    pub ppp: f32,
}

/// Paint the pixels of `slot` into `dst` (screen rect of the visible page), clipped to `clip`.
/// `thumb` uses the cheap single-tile preview only.
pub fn draw_slot(dc: &mut DrawCtx, painter: &Painter, slot: &Slot, dst: Rect, clip: Rect, thumb: bool) {
    let painter = painter.with_clip_rect(clip.intersect(dst).intersect(painter.clip_rect()));
    let paper = if dc.dark { Color32::from_rgb(0x1E, 0x1E, 0x1E) } else { Color32::WHITE };
    match &slot.src {
        PageSrc::Blank => {
            painter.rect_filled(dst, 0.0, paper);
        }
        PageSrc::Image { frame } => {
            let (img, key) = match dc.frames.get(*frame as usize) {
                Some(img) => (img, (dc.doc_id, *frame as usize)),
                None => match dc.thumb {
                    Some(t) if *frame == 0 => (t, (dc.doc_id, usize::MAX)),
                    _ => return,
                },
            };
            let thumb = thumb || key.1 == usize::MAX;
            let stale = dc.images.get(&key).map(|t| !t.is_for(img)).unwrap_or(true);
            if stale {
                dc.images.insert(key, ImageTex::new(painter.ctx(), img.clone()));
            }
            let tex = &dc.images[&key];
            let zoom = dst.width() / slot.display_size().x.max(1.0);
            if thumb || zoom * dc.ppp < 320.0 / img.width().max(1) as f32 {
                draw_quad(&painter, tex.thumb.id(), slot, dst, slot.bounds, Color32::WHITE);
            } else {
                let visible_page = slot.rect_to_page(dst, painter.clip_rect());
                for (r, t) in &tex.tiles {
                    if r.intersects(visible_page) {
                        draw_quad(&painter, t.id(), slot, dst, *r, Color32::WHITE);
                    }
                }
            }
        }
        PageSrc::Pdf { source, index } => {
            painter.rect_filled(dst, 0.0, paper);
            let Some(src) = dc.sources.get(source) else { return };
            let b = slot.bounds;
            let pl = preview_level(b);
            let pkey = TileKey { source: *source, page: *index, level: pl, tx: 0, ty: 0, dark: dc.dark };
            if let Some(t) = dc.tiles.get_with(pkey, src, true) {
                let full = tile_rect(b, 0, 0, scale_for_level(pl), t);
                draw_quad(&painter, t.id(), slot, dst, full, Color32::WHITE);
            }
            if thumb {
                return;
            }
            let zoom = dst.width() / slot.display_size().x.max(1.0);
            let level = level_for_scale(zoom * dc.ppp).min(max_level(b));
            if level <= pl {
                return;
            }
            let s = scale_for_level(level);
            let vis = slot.rect_to_page(dst, painter.clip_rect()).intersect(b);
            if vis.is_negative() {
                return;
            }
            let t = TILE as f32;
            let x0 = ((vis.min.x - b.min.x) * s / t).floor().max(0.0) as i32;
            let y0 = ((vis.min.y - b.min.y) * s / t).floor().max(0.0) as i32;
            let x1 = ((vis.max.x - b.min.x) * s / t).ceil() as i32;
            let y1 = ((vis.max.y - b.min.y) * s / t).ceil() as i32;
            for ty in y0..y1.min(y0 + 64) {
                for tx in x0..x1.min(x0 + 64) {
                    let key = TileKey { source: *source, page: *index, level, tx, ty, dark: dc.dark };
                    let r = Rect::from_min_size(
                        pos2(b.min.x + tx as f32 * t / s, b.min.y + ty as f32 * t / s),
                        egui::vec2(t / s, t / s),
                    );
                    match dc.tiles.get(key, src) {
                        Some(tex) => {
                            let tr = tile_rect(b, tx, ty, s, tex);
                            draw_quad(&painter, tex.id(), slot, dst, tr, Color32::WHITE)
                        }
                        None => {
                            // Not rendered yet: cover the gap with whatever we already have at nearby
                            // levels (sharper ones while zooming out, coarser ones while zooming in),
                            // so the page never flashes blank or drops to the blurry preview.
                            let cell = painter.with_clip_rect(slot.rect_to_screen(dst, r).intersect(painter.clip_rect()));
                            for d in [1, -1, 2, -2, 3, -3, 4, -4, 6, -6] {
                                let lv = level + d;
                                if lv <= pl || lv > max_level(b) {
                                    continue;
                                }
                                let sl = scale_for_level(lv);
                                let ax = ((r.min.x - b.min.x) * sl / t).floor() as i32;
                                let ay = ((r.min.y - b.min.y) * sl / t).floor() as i32;
                                let bx = (((r.max.x - b.min.x) * sl / t).ceil() as i32).max(ax + 1);
                                let by = (((r.max.y - b.min.y) * sl / t).ceil() as i32).max(ay + 1);
                                if (bx - ax) * (by - ay) > 9 {
                                    continue;
                                }
                                let mut found = false;
                                for fy in ay..by {
                                    for fx in ax..bx {
                                        let fk = TileKey { level: lv, tx: fx, ty: fy, ..key };
                                        if let Some(tex) = dc.tiles.peek(&fk) {
                                            let lr = tile_rect(b, fx, fy, sl, tex);
                                            draw_quad(&cell, tex.id(), slot, dst, lr, Color32::WHITE);
                                            found = true;
                                        }
                                    }
                                }
                                if found {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Drop shadow under a page.
pub fn page_shadow(painter: &Painter, r: Rect, color: Color32) {
    let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 14, spread: 0, color };
    painter.add(shadow.as_shape(r, 2.0));
}
