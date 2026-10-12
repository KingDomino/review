//! Drawing annotations on screen and hit-testing them.

use egui::{pos2, vec2, Align2, Color32, FontId, Mesh, Painter, Pos2, Rect, Shape as EShape, Stroke, StrokeKind};

use crate::imaging::arrow_head;
use crate::model::{bubble_points, round_rect_points, star_points, Align, Annot, MarkupKind, Shape, Slot};

fn fill_fan(painter: &Painter, pts: &[Pos2], color: Color32) {
    if pts.len() < 3 || color.a() == 0 {
        return;
    }
    let c = pts.iter().fold(Pos2::ZERO, |a, p| a + p.to_vec2()) / pts.len() as f32;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(c, color);
    for p in pts {
        mesh.colored_vertex(*p, color);
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(mesh);
}

/// Paint `a` on page `slot` drawn at `dst`. `zoom` = screen points per page unit.
pub fn paint(painter: &Painter, slot: &Slot, dst: Rect, zoom: f32, a: &Annot) {
    let st = &a.style;
    let map = |p: Pos2| slot.to_screen(dst, p);
    let stroke = Stroke::new((st.width * zoom).max(0.75), st.stroke);
    let closed = |pts: Vec<Pos2>| {
        let pts: Vec<Pos2> = pts.into_iter().map(map).collect();
        if let Some(f) = st.fill {
            fill_fan(painter, &pts, f);
        }
        if st.width > 0.0 {
            painter.add(EShape::closed_line(pts, stroke));
        }
    };
    match &a.shape {
        Shape::Rect(r) => {
            let r = slot.rect_to_screen(dst, *r);
            painter.rect(r, 0.0, st.fill.unwrap_or(Color32::TRANSPARENT), stroke, StrokeKind::Middle);
        }
        Shape::Redact(r) => {
            let r = slot.rect_to_screen(dst, *r);
            painter.rect_filled(r, 0.0, Color32::from_black_alpha(235));
            // Diagonal hatching shows it is pending until saved.
            let p = painter.with_clip_rect(r.intersect(painter.clip_rect()));
            let step = 8.0;
            let mut x = r.min.x - r.height();
            while x < r.max.x {
                p.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], Stroke::new(1.0, Color32::from_gray(60)));
                x += step;
            }
        }
        Shape::Oval(r) => {
            let r = slot.rect_to_screen(dst, *r);
            if let Some(f) = st.fill {
                painter.add(EShape::ellipse_filled(r.center(), r.size() / 2.0, f));
            }
            painter.add(EShape::ellipse_stroke(r.center(), r.size() / 2.0, stroke));
        }
        Shape::RoundRect(r) => closed(round_rect_points(*r)),
        Shape::Star(r) => closed(star_points(*r)),
        Shape::Bubble(r) => closed(bubble_points(*r)),
        Shape::Polygon(p) => closed(p.clone()),
        Shape::Line { a: p0, b: p1, arrow } => {
            let (s0, s1) = (map(*p0), map(*p1));
            painter.line_segment([s0, s1], stroke);
            if *arrow {
                let head = arrow_head(*p0, *p1, st.width);
                painter.add(EShape::line(head.into_iter().map(map).collect(), stroke));
            }
        }
        Shape::Ink(strokes) | Shape::Signature { strokes, .. } => {
            for s in strokes {
                if s.len() == 1 {
                    painter.circle_filled(map(s[0]), stroke.width / 2.0, st.stroke);
                } else {
                    painter.add(EShape::line(s.iter().map(|p| map(*p)).collect(), stroke));
                }
            }
        }
        Shape::Markup { kind, quads } => {
            for q in quads {
                let r = slot.rect_to_screen(dst, crate::model::points_bounds(q.iter()));
                match kind {
                    MarkupKind::Highlight => {
                        painter.rect_filled(r, 2.0, st.stroke.gamma_multiply(0.42));
                    }
                    MarkupKind::Underline => {
                        let (a0, a1) = (map(q[3]), map(q[2]));
                        painter.line_segment([a0, a1], Stroke::new((1.4 * zoom).max(1.0), st.stroke));
                    }
                    MarkupKind::Strike => {
                        let m0 = map(pos2(q[0].x, (q[0].y + q[3].y) / 2.0));
                        let m1 = map(pos2(q[1].x, (q[1].y + q[2].y) / 2.0));
                        painter.line_segment([m0, m1], Stroke::new((1.4 * zoom).max(1.0), st.stroke));
                    }
                }
            }
        }
        Shape::Text { rect, text } => {
            let r = slot.rect_to_screen(dst, *rect);
            if let Some(f) = st.fill {
                painter.rect_filled(r, 0.0, f);
            }
            if st.width > 0.0 {
                painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
            }
            paint_text(painter, r, text, st.font_size * zoom, st.text_color, st.align);
        }
        Shape::Note { at, .. } => paint_note_icon(painter, map(*at), (20.0 * zoom).clamp(14.0, 30.0), st.stroke),
    }
}

pub fn paint_text(painter: &Painter, r: Rect, text: &str, size: f32, color: Color32, align: Align) {
    if size < 1.5 {
        return;
    }
    let pad = 4.0 * size / 14.0;
    let mut job = egui::text::LayoutJob::simple(text.to_string(), FontId::proportional(size), color, (r.width() - 2.0 * pad).max(1.0));
    job.halign = match align {
        Align::Left => egui::Align::LEFT,
        Align::Center => egui::Align::Center,
        Align::Right => egui::Align::RIGHT,
    };
    let galley = painter.layout_job(job);
    let x = match align {
        Align::Left => r.min.x + pad,
        Align::Center => r.center().x,
        Align::Right => r.max.x - pad,
    };
    painter.with_clip_rect(r.expand(2.0).intersect(painter.clip_rect())).galley(pos2(x, r.min.y + pad), galley, color);
}

pub fn paint_note_icon(painter: &Painter, at: Pos2, size: f32, color: Color32) {
    let r = Rect::from_min_size(at, vec2(size, size));
    painter.rect(r, size * 0.15, color, Stroke::new(1.0, color.gamma_multiply(0.6)), StrokeKind::Inside);
    for i in 0..3 {
        let y = r.min.y + size * (0.32 + i as f32 * 0.18);
        painter.line_segment([pos2(r.min.x + size * 0.22, y), pos2(r.max.x - size * 0.22, y)], Stroke::new((size / 16.0).max(1.0), Color32::from_black_alpha(110)));
    }
}

/// Selection chrome: dashed bounds plus resize handles.
pub fn paint_selection(painter: &Painter, slot: &Slot, dst: Rect, a: &Annot, accent: Color32) -> Vec<(usize, Rect)> {
    let mut handles = Vec::new();
    if let Shape::Line { a: p0, b: p1, .. } = &a.shape {
        for (i, p) in [*p0, *p1].iter().enumerate() {
            let c = slot.to_screen(dst, *p);
            painter.circle(c, 5.0, Color32::WHITE, Stroke::new(1.5, accent));
            handles.push((i, Rect::from_center_size(c, vec2(14.0, 14.0))));
        }
        return handles;
    }
    let r = slot.rect_to_screen(dst, a.bounds()).expand(2.0);
    let dashes = EShape::dashed_line(&[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()], Stroke::new(1.0, accent), 4.0, 3.0);
    painter.extend(dashes);
    if a.resizable() {
        for (i, c) in handle_points(r).iter().enumerate() {
            painter.circle(*c, 4.5, Color32::WHITE, Stroke::new(1.5, accent));
            handles.push((i, Rect::from_center_size(*c, vec2(14.0, 14.0))));
        }
    }
    handles
}

/// Corner and edge midpoints, clockwise from top-left.
pub fn handle_points(r: Rect) -> [Pos2; 8] {
    [
        r.left_top(),
        r.center_top(),
        r.right_top(),
        r.right_center(),
        r.right_bottom(),
        r.center_bottom(),
        r.left_bottom(),
        r.left_center(),
    ]
}

/// Resize a screen rect by dragging handle `h` to `p`.
pub fn drag_handle(r: Rect, h: usize, p: Pos2) -> Rect {
    let mut r = r;
    match h {
        0 => r.min = p,
        1 => r.min.y = p.y,
        2 => {
            r.max.x = p.x;
            r.min.y = p.y;
        }
        3 => r.max.x = p.x,
        4 => r.max = p,
        5 => r.max.y = p.y,
        6 => {
            r.min.x = p.x;
            r.max.y = p.y;
        }
        _ => r.min.x = p.x,
    }
    Rect::from_two_pos(r.min, r.max)
}

fn dist_seg(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// Does page point `p` hit annotation `a`? `tol` is in page units.
pub fn hit(a: &Annot, p: Pos2, tol: f32) -> bool {
    let w = a.style.width / 2.0 + tol;
    let near_poly = |pts: &[Pos2], closed: bool| {
        let n = pts.len();
        (0..n.saturating_sub(if closed { 0 } else { 1 })).any(|i| dist_seg(p, pts[i], pts[(i + 1) % n]) <= w)
    };
    match &a.shape {
        Shape::Rect(r) | Shape::RoundRect(r) | Shape::Star(r) | Shape::Oval(r) | Shape::Bubble(r) => {
            if a.style.fill.is_some() {
                r.expand(w).contains(p)
            } else {
                r.expand(w).contains(p) && !r.shrink(w.max(4.0)).contains(p)
            }
        }
        Shape::Redact(r) | Shape::Text { rect: r, .. } | Shape::Signature { rect: r, .. } => r.expand(tol).contains(p),
        Shape::Line { a: p0, b: p1, .. } => dist_seg(p, *p0, *p1) <= w.max(4.0),
        Shape::Ink(s) => s.iter().any(|s| if s.len() == 1 { (s[0] - p).length() <= w } else { near_poly(s, false) }),
        Shape::Polygon(pts) => {
            if a.style.fill.is_some() {
                crate::model::points_bounds(pts.iter()).contains(p)
            } else {
                near_poly(pts, true)
            }
        }
        Shape::Note { at, .. } => Rect::from_min_size(*at, vec2(20.0, 20.0)).expand(tol).contains(p),
        Shape::Markup { quads, .. } => quads.iter().any(|q| crate::model::points_bounds(q.iter()).expand(tol * 0.5).contains(p)),
    }
}

pub fn label_badge(painter: &Painter, at: Pos2, text: &str, accent: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(11.0), Color32::WHITE);
    let r = Align2::LEFT_BOTTOM.anchor_size(at, galley.size() + vec2(10.0, 4.0));
    painter.rect_filled(r, 4.0, accent);
    painter.galley(r.min + vec2(5.0, 2.0), galley, Color32::WHITE);
}
