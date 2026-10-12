//! The sidebar: thumbnails (with drag & drop), table of contents, notes, bookmarks, search results.

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_phosphor::regular as ph;

use crate::annots;
use crate::app::{Action, App, SidebarMode};
use crate::document::Kind;
use crate::model::PageSrc;
use crate::pdf::OutlineNode;
use crate::render::{draw_slot, page_shadow, DrawCtx};
use crate::theme::{bold, Palette};

pub fn show(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    // Mode switcher
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.x = 2.0;
        let modes = [
            (SidebarMode::Thumbnails, ph::SQUARES_FOUR, "Thumbnails (Ctrl+Alt+2)"),
            (SidebarMode::Contents, ph::LIST_BULLETS, "Table of Contents (Ctrl+Alt+3)"),
            (SidebarMode::Notes, ph::HIGHLIGHTER, "Highlights and Notes (Ctrl+Alt+4)"),
            (SidebarMode::Bookmarks, ph::BOOKMARK_SIMPLE, "Bookmarks (Ctrl+Alt+5)"),
        ];
        for (m, icon, tip) in modes {
            if crate::layout::tool_button(ui, icon, tip, app.sidebar == m, true).clicked() {
                app.sidebar = m;
            }
        }
        if !app.search.query.is_empty() && crate::layout::tool_button(ui, ph::MAGNIFYING_GLASS, "Search Results", app.sidebar == SidebarMode::Search, true).clicked() {
            app.sidebar = SidebarMode::Search;
        }
    });
    ui.add_space(4.0);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment([pos2(r.min.x, r.min.y), pos2(r.max.x, r.min.y)], Stroke::new(1.0, pal.separator));
    match app.sidebar {
        SidebarMode::Thumbnails => thumbnails(app, ui),
        SidebarMode::Contents => contents(app, ui),
        SidebarMode::Notes => notes(app, ui),
        SidebarMode::Bookmarks => bookmarks(app, ui),
        SidebarMode::Search => search_results(app, ui),
    }
}

/// Thumbnails for every open document, grouped like Preview's multi-document sidebar.
fn thumbnails(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let pal = Palette::of(&ctx);
    let ppp = ctx.pixels_per_point();
    let width = ui.available_width();
    let thumb_w = (width - 56.0).clamp(60.0, 360.0);
    let multi = app.docs.len() > 1;
    let pointer = ctx.pointer_hover_pos();
    let dragging = app.drag_pages.clone();
    let mut drop_at: Option<(u64, usize, f32)> = None;
    let mut clicked: Option<(usize, usize, egui::Modifiers)> = None;
    let mut activate_doc: Option<usize> = None;
    let mut close_doc: Option<usize> = None;
    let mut start_drag: Option<(u64, usize)> = None;
    let mut ctx_menu_action: Option<(usize, usize, Action)> = None;
    let scroll_to = app.thumb_scroll_to.take();
    let file_hover = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let mut drop_target_for_files: Option<(u64, usize)> = None;

    app.thumb_rects.clear();
    egui::ScrollArea::vertical().id_salt("thumbs").auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let origin = ui.max_rect().min;
        let mut y = 8.0;
        for di in 0..app.docs.len() {
            let doc_id = app.docs[di].id;
            let active = di == app.active;
            if multi {
                // Document header
                let hr = Rect::from_min_size(origin + vec2(8.0, y), vec2(width - 16.0, 26.0));
                let resp = ui.interact(hr, egui::Id::new(("dochdr", doc_id)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(hr, 6.0, pal.hover);
                }
                let name = app.docs[di].name.clone();
                let dirty = app.docs[di].dirty();
                let icon = match app.docs[di].kind {
                    Kind::Image => ph::IMAGE,
                    Kind::Model => ph::CUBE,
                    Kind::Pdf => ph::FILE_PDF,
                };
                ui.painter().text(hr.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, icon, FontId::proportional(14.0), if active { pal.accent } else { pal.text_weak });
                let galley = ui.painter().layout(format!("{name}{}", if dirty { " •" } else { "" }), FontId::new(12.0, bold()), if active { pal.text } else { pal.text_weak }, hr.width() - 52.0);
                ui.painter().galley(hr.left_center() + vec2(28.0, -galley.size().y / 2.0), galley, pal.text);
                if resp.hovered() {
                    let xr = Rect::from_center_size(hr.right_center() - vec2(14.0, 0.0), vec2(20.0, 20.0));
                    let x = ui.interact(xr, egui::Id::new(("docx", doc_id)), Sense::click());
                    ui.painter().text(xr.center(), Align2::CENTER_CENTER, ph::X, FontId::proportional(12.0), if x.hovered() { pal.text } else { pal.text_weak });
                    if x.clicked() {
                        close_doc = Some(di);
                    }
                }
                if resp.clicked() {
                    activate_doc = Some(di);
                }
                resp.context_menu(|ui| {
                    if ui.button("Close").clicked() {
                        close_doc = Some(di);
                        ui.close();
                    }
                    if ui.button("Show in Folder").clicked() {
                        activate_doc = Some(di);
                        ctx_menu_action = Some((di, 0, Action::ShowInFolder));
                        ui.close();
                    }
                    if ui.button("Move to Trash").clicked() {
                        activate_doc = Some(di);
                        ctx_menu_action = Some((di, 0, Action::MoveToTrash));
                        ui.close();
                    }
                });
                y += 30.0;
            }
            if !app.docs[di].ready() && app.docs[di].state != crate::document::LoadState::Unloaded {
                let r = Rect::from_min_size(origin + vec2(0.0, y), vec2(width, 40.0));
                if r.intersects(viewport.translate(origin.to_vec2())) {
                    ui.put(Rect::from_center_size(r.center(), vec2(18.0, 18.0)), egui::Spinner::new());
                }
                y += 44.0;
                continue;
            }
            if app.docs[di].kind == Kind::Model {
                // One cell showing the live 3D view.
                let size = vec2(thumb_w, thumb_w * 0.75);
                let cell = Rect::from_min_size(origin + vec2(0.0, y), vec2(width, size.y + 30.0));
                let pr = Rect::from_min_size(origin + vec2((width - size.x) / 2.0, y + 6.0), size);
                let resp = ui.interact(cell, egui::Id::new(("thumb3d", doc_id)), Sense::click());
                page_shadow(ui.painter(), pr, pal.page_shadow);
                ui.painter().rect_filled(pr, 4.0, pal.canvas);
                // Render a first view for models that haven't been shown yet.
                if !app.viewers.contains_key(&doc_id) {
                    if let (Some(rs), Some(scene)) = (app.wgpu.clone(), app.docs[di].scene.clone()) {
                        let mut v = crate::view3d::Viewer::new(&rs, &scene);
                        let px = (size * ppp).round();
                        v.render(&rs, [px.x as u32, px.y as u32], pal.canvas);
                        app.viewers.insert(doc_id, v);
                    }
                }
                if let Some(tex) = app.viewers.get(&doc_id).and_then(|v| v.thumbnail_id()) {
                    ui.painter().image(tex, pr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    ui.painter().text(pr.center(), Align2::CENTER_CENTER, ph::CUBE, FontId::proportional(32.0), pal.text_weak);
                }
                if active {
                    ui.painter().rect_stroke(pr.expand(2.0), 3.0, Stroke::new(2.0, pal.accent), StrokeKind::Outside);
                }
                if resp.clicked() {
                    activate_doc = Some(di);
                }
                y += size.y + 36.0;
                continue;
            }
            let n = app.docs[di].page_count();
            for i in 0..n {
                let ds = app.docs[di].model.slots[i].display_size();
                let s = (thumb_w / ds.x).min(thumb_w * 1.5 / ds.y);
                let size = ds * s;
                let cell_h = size.y + 30.0;
                let cell = Rect::from_min_size(origin + vec2(0.0, y), vec2(width, cell_h));
                let pr = Rect::from_min_size(origin + vec2((width - size.x) / 2.0, y + 6.0), size);
                if scroll_to == Some(i) && active {
                    ui.scroll_to_rect(cell, Some(egui::Align::Center));
                }
                // Drop indicator position while dragging pages / files.
                if let Some(p) = pointer {
                    let over_sidebar = ui.clip_rect().contains(p);
                    if (dragging.is_some() || file_hover) && over_sidebar && p.y >= cell.min.y && p.y < cell.max.y {
                        let after = p.y > pr.center().y;
                        let at = if after { i + 1 } else { i };
                        let line_y = if after { cell.max.y - 6.0 } else { cell.min.y + 2.0 };
                        drop_at = Some((doc_id, at, line_y));
                    }
                }
                if cell.intersects(viewport.translate(origin.to_vec2()).expand(200.0)) {
                    app.thumb_rects.push((doc_id, i, pr));
                    let resp = ui.interact(cell, egui::Id::new(("thumb", doc_id, i)), Sense::click_and_drag());
                    let selected = app.docs[di].selected_pages.contains(&i);
                    let current = active && app.docs[di].current == i;
                    if selected || (current && app.docs[di].selected_pages.is_empty()) {
                        ui.painter().rect_filled(pr.expand(5.0), 6.0, if selected || current { pal.selected } else { Color32::TRANSPARENT });
                    }
                    page_shadow(ui.painter(), pr, pal.page_shadow);
                    {
                        let App { docs, tiles, sources, images, settings, proof, .. } = &mut *app;
                        let doc = &docs[di];
                        let slot = &doc.model.slots[i];
                        let mut dc = DrawCtx { tiles, sources, images, frames: &doc.model.frames, thumb: doc.thumb.as_ref(), doc_id: doc.id, dark: settings.dark_pdf && doc.kind == Kind::Pdf, ppp, proof: proof.as_deref() };
                        draw_slot(&mut dc, ui.painter(), slot, pr, ui.clip_rect(), true);
                        let p = ui.painter().with_clip_rect(pr.intersect(ui.clip_rect()));
                        for a in &slot.annots {
                            annots::paint(&p, slot, pr, s, a);
                        }
                        if doc.model.bookmarks.contains(&slot.uid) {
                            p.text(pr.right_top() + vec2(-8.0, 2.0), Align2::CENTER_TOP, ph::BOOKMARK_SIMPLE, FontId::proportional(14.0), Color32::from_rgb(0xFF, 0x3B, 0x30));
                        }
                    }
                    if current {
                        ui.painter().rect_stroke(pr.expand(2.0), 3.0, Stroke::new(2.0, pal.accent), StrokeKind::Outside);
                    }
                    let label = {
                        let slot = &app.docs[di].model.slots[i];
                        if slot.label.is_empty() || !app.settings.logical_page_numbers { (i + 1).to_string() } else { slot.label.clone() }
                    };
                    ui.painter().text(pos2(cell.center().x, pr.max.y + 13.0), Align2::CENTER_CENTER, label, FontId::proportional(11.5), if current { pal.text } else { pal.text_weak });
                    if resp.clicked() {
                        clicked = Some((di, i, ui.input(|inp| inp.modifiers)));
                    }
                    if resp.drag_started() && app.docs[di].kind == Kind::Pdf {
                        start_drag = Some((doc_id, i));
                    }
                    resp.context_menu(|ui| {
                        for (label, a) in [
                            ("Rotate Left", Action::RotateLeft),
                            ("Rotate Right", Action::RotateRight),
                            ("Insert Blank Page After", Action::InsertBlank),
                            ("Insert Pages from File…", Action::InsertFromFile),
                            ("Add Bookmark", Action::AddBookmark),
                            ("Delete", Action::Delete),
                        ] {
                            if ui.button(label).clicked() {
                                ctx_menu_action = Some((di, i, a));
                                ui.close();
                            }
                        }
                    });
                }
                y += cell_h + 4.0;
            }
            y += 6.0;
        }
        ui.allocate_rect(Rect::from_min_size(origin, vec2(width, y + 20.0)), Sense::hover());
        if let Some((_, _, ly)) = drop_at {
            let lr = Rect::from_min_size(pos2(origin.x + 12.0, ly), vec2(width - 24.0, 3.0));
            ui.painter().rect_filled(lr, 1.5, pal.accent);
        }
    });

    // --- Apply interactions --------------------------------------------------------------------
    if let Some((doc, i)) = start_drag {
        let di = app.doc_index(doc).unwrap_or(0);
        let mut pages = app.docs[di].selected_pages.clone();
        if !pages.contains(&i) {
            pages = vec![i];
        }
        app.drag_pages = Some((doc, pages));
    }
    if let Some((from_doc, pages)) = app.drag_pages.clone() {
        if let Some(p) = pointer {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("dragpages")));
            let r = Rect::from_min_size(p + vec2(10.0, 6.0), vec2(26.0, 20.0));
            painter.rect_filled(r, 10.0, pal.accent);
            painter.text(r.center(), Align2::CENTER_CENTER, pages.len().to_string(), FontId::proportional(11.0), Color32::WHITE);
        }
        if ctx.input(|i| i.pointer.any_released()) {
            app.drag_pages = None;
            if let Some((to_doc, at, _)) = drop_at {
                let copy = ctx.input(|i| i.modifiers.ctrl);
                app.move_pages(from_doc, pages, to_doc, at, copy);
                if let Some(ti) = app.doc_index(to_doc) {
                    app.activate(ti);
                }
            }
        }
    }
    if file_hover {
        if let Some((doc, at, _)) = drop_at {
            drop_target_for_files = Some((doc, at));
        }
    }
    let dropping_now = ctx.input(|i| !i.raw.dropped_files.is_empty());
    ctx.data_mut(|d| match drop_target_for_files {
        Some(t) => {
            d.insert_temp(egui::Id::new("drop-target"), t);
        }
        None => {
            if !dropping_now {
                d.remove::<(u64, usize)>(egui::Id::new("drop-target"));
            }
        }
    });
    if let Some(di) = activate_doc {
        app.activate(di);
    }
    if let Some((di, i, mods)) = clicked {
        app.activate(di);
        let d = &mut app.docs[di];
        if mods.command {
            if let Some(pos) = d.selected_pages.iter().position(|p| *p == i) {
                d.selected_pages.remove(pos);
            } else {
                if d.selected_pages.is_empty() {
                    d.selected_pages.push(d.current);
                }
                d.selected_pages.push(i);
            }
        } else if mods.shift {
            let a = d.current.min(i);
            let b = d.current.max(i);
            d.selected_pages = (a..=b).collect();
        } else {
            d.selected_pages.clear();
        }
        app.go_to(i);
    }
    if let Some((di, i, a)) = ctx_menu_action {
        app.activate(di);
        let d = &mut app.docs[di];
        if !d.selected_pages.contains(&i) {
            d.selected_pages = vec![i];
        }
        d.current = i;
        if a == Action::Delete {
            if d.selected_pages.len() < d.page_count() {
                app.delete_selection();
            }
        } else {
            app.run(a);
        }
    }
    if let Some(di) = close_doc {
        app.close_doc(di, false);
    }
}

fn empty(ui: &mut Ui, icon: &str, text: &str) {
    let pal = Palette::of(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(RichText::new(icon).size(30.0).color(pal.text_weak.gamma_multiply(0.7)));
        ui.add_space(4.0);
        ui.label(RichText::new(text).color(pal.text_weak));
    });
}

fn contents(app: &mut App, ui: &mut Ui) {
    let Some(doc) = app.doc() else { return };
    let outline = doc.info.as_ref().map(|i| i.outline.clone()).unwrap_or_default();
    let own = doc.source;
    if outline.is_empty() {
        empty(ui, ph::LIST_BULLETS, "No table of contents");
        return;
    }
    let mut go = None;
    let mut url = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.add_space(6.0);
        fn rec(ui: &mut Ui, nodes: &[OutlineNode], depth: usize, go: &mut Option<u32>, url: &mut Option<String>) {
            let pal = Palette::of(ui.ctx());
            for (k, n) in nodes.iter().enumerate() {
                let id = ui.make_persistent_id((depth, k, &n.title));
                let row = |ui: &mut Ui, go: &mut Option<u32>, url: &mut Option<String>| {
                    let text = RichText::new(&n.title).color(pal.text);
                    let resp = ui.add(egui::Button::new(text).frame(false).wrap_mode(egui::TextWrapMode::Truncate));
                    if let Some(p) = n.page {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new((p + 1).to_string()).small().color(pal.text_weak));
                        });
                    }
                    if resp.clicked() {
                        if let Some(p) = n.page {
                            *go = Some(p);
                        } else if let Some(u) = &n.uri {
                            *url = Some(u.clone());
                        }
                    }
                };
                if n.children.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0 + depth as f32 * 12.0 + 16.0);
                        row(ui, go, url);
                    });
                } else {
                    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth == 0 && nodes.len() < 12)
                        .show_header(ui, |ui| {
                            ui.add_space(depth as f32 * 12.0);
                            row(ui, go, url);
                        })
                        .body(|ui| rec(ui, &n.children, depth + 1, go, url));
                }
            }
        }
        rec(ui, &outline, 0, &mut go, &mut url);
    });
    if let Some(p) = go {
        let slot = app.doc().and_then(|d| d.model.slots.iter().position(|s| matches!(s.src, PageSrc::Pdf { source, index } if Some(source) == own && index == p)));
        if let Some(s) = slot {
            app.go_to(s);
        }
    }
    if let Some(u) = url {
        app.ctx.open_url(egui::OpenUrl::new_tab(u));
    }
}

fn notes(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let Some(doc) = app.doc() else { return };
    let doc_id = doc.id;
    let items: Vec<(usize, u64, &'static str, String, Color32, String)> = doc
        .model
        .slots
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            s.annots.iter().map(move |a| {
                let snippet = a.text().map(|t| t.to_string()).unwrap_or_default();
                (i, a.id, a.label(), snippet, a.style.stroke, a.author.clone())
            })
        })
        .collect();
    if items.is_empty() {
        empty(ui, ph::HIGHLIGHTER, "No highlights or notes");
        return;
    }
    // Text under highlights, from page text.
    let mut sel = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.add_space(6.0);
        let mut last_page = usize::MAX;
        for (page, id, label, snippet, color, author) in items {
            if page != last_page {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(format!("Page {}", page + 1)).small().strong().color(pal.text_weak));
                });
                last_page = page;
            }
            let selected = app.sel.annot == Some((doc_id, page, id));
            let resp = egui::Frame::new()
                .fill(if selected { pal.selected } else { Color32::TRANSPARENT })
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .outer_margin(egui::Margin::symmetric(6, 1))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(4.0, 30.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, color);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(label).strong());
                            let body = if snippet.is_empty() { highlighted_text(app, page, id) } else { snippet.clone() };
                            if !body.is_empty() {
                                ui.add(egui::Label::new(RichText::new(body).color(pal.text_weak)).wrap());
                            }
                            if !author.is_empty() {
                                ui.label(RichText::new(author).small().color(pal.text_weak));
                            }
                        });
                    });
                })
                .response
                .interact(Sense::click());
            if resp.clicked() {
                sel = Some((page, id));
            }
        }
    });
    if let Some((page, id)) = sel {
        app.sel.annot = Some((doc_id, page, id));
        app.go_to(page);
    }
}

fn highlighted_text(app: &mut App, page: usize, id: u64) -> String {
    let Some(doc) = app.doc() else { return String::new() };
    let Some(slot) = doc.model.slots.get(page) else { return String::new() };
    let Some(a) = slot.annots.iter().find(|a| a.id == id) else { return String::new() };
    let crate::model::Shape::Markup { quads, .. } = &a.shape else { return String::new() };
    let quads = quads.clone();
    let PageSrc::Pdf { source, index } = slot.src else { return String::new() };
    let Some(t) = app.page_text(source, index) else { return String::new() };
    let mut s = String::new();
    for c in &t.chars {
        let cc = c.rect.center();
        if quads.iter().any(|q| crate::model::points_bounds(q.iter()).expand(1.0).contains(cc)) {
            s.push(c.c);
        }
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 140 {
        format!("{}…", s.chars().take(140).collect::<String>())
    } else {
        s
    }
}

fn bookmarks(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let Some(doc) = app.doc() else { return };
    let marks: Vec<(usize, String)> = doc
        .model
        .bookmarks
        .iter()
        .filter_map(|uid| doc.model.slots.iter().position(|s| s.uid == *uid))
        .map(|i| (i, doc.model.slots[i].label.clone()))
        .collect();
    if marks.is_empty() {
        ui.vertical_centered(|ui| {
            empty(ui, ph::BOOKMARK_SIMPLE, "No bookmarks");
            ui.label(RichText::new("Press Ctrl+D to bookmark a page").small().color(pal.text_weak));
        });
        return;
    }
    let mut go = None;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.add_space(6.0);
        for (i, label) in marks {
            let text = if label.is_empty() { format!("Page {}", i + 1) } else { format!("Page {label}") };
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(ph::BOOKMARK_SIMPLE).color(Color32::from_rgb(0xFF, 0x3B, 0x30)));
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    go = Some(i);
                }
            });
        }
    });
    if let Some(i) = go {
        app.go_to(i);
    }
}

fn search_results(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let n = app.search.hits.len();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let status = if app.search.running {
            format!("Searching… {n} found")
        } else if n == 1 {
            "1 match".to_string()
        } else {
            format!("{n} matches")
        };
        ui.label(RichText::new(status).small().strong().color(pal.text_weak));
        if app.search.running {
            ui.add(egui::Spinner::new().size(12.0));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if ui.small_button("Done").clicked() {
                app.search.query.clear();
                app.start_search();
            }
        });
    });
    let mut go = None;
    let q = app.search.query.to_lowercase();
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, 46.0, n, |ui, range| {
        for i in range {
            let hit = &app.search.hits[i];
            let current = app.search.current == Some(i);
            let r = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
            let rect = r.0.shrink2(vec2(6.0, 1.0));
            if current {
                ui.painter().rect_filled(rect, 6.0, pal.selected);
            } else if r.1.hovered() {
                ui.painter().rect_filled(rect, 6.0, pal.hover);
            }
            ui.painter().text(rect.left_top() + vec2(8.0, 5.0), Align2::LEFT_TOP, format!("Page {}", hit.page + 1), FontId::new(11.0, bold()), pal.text_weak);
            // Snippet with the match emphasised.
            let mut job = egui::text::LayoutJob::default();
            let ctx_text = hit.context.trim();
            let lower = ctx_text.to_lowercase();
            let (a, b) = match lower.find(&q) {
                Some(p) if !q.is_empty() && ctx_text.is_char_boundary(p) && ctx_text.is_char_boundary(p + q.len()) => (p, p + q.len()),
                _ => (0, 0),
            };
            let f = egui::TextFormat { font_id: FontId::proportional(12.0), color: pal.text, ..Default::default() };
            let hl = egui::TextFormat { font_id: FontId::new(12.0, bold()), color: pal.text, background: Color32::from_rgba_unmultiplied(255, 214, 10, 110), ..Default::default() };
            job.append(&ctx_text[..a], 0.0, f.clone());
            job.append(&ctx_text[a..b], 0.0, hl);
            job.append(&ctx_text[b..], 0.0, f);
            job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(rect.left_top() + vec2(8.0, 21.0), galley, pal.text);
            if r.1.clicked() {
                go = Some(i);
            }
        }
    });
    if let Some(i) = go {
        app.reveal_hit(i);
    }
}
