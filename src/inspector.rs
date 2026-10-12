//! Tools ▸ Show Inspector: file information, metadata, annotations and security.

use egui::{vec2, Color32, FontId, RichText, Ui};
use egui_phosphor::regular as ph;

use crate::app::{App, InspectorTab};
use crate::document::{fmt_time, human_size, Kind};
use crate::model::MetaEdit;
use crate::theme::{bold, Palette};

fn row(ui: &mut Ui, k: &str, v: impl Into<String>) {
    let pal = Palette::of(ui.ctx());
    let v: String = v.into();
    ui.horizontal_top(|ui| {
        ui.add_sized(vec2(104.0, 18.0), egui::Label::new(RichText::new(k).color(pal.text_weak)).halign(egui::Align::RIGHT));
        ui.add(egui::Label::new(RichText::new(if v.is_empty() { "—".into() } else { v }).color(pal.text)).wrap().selectable(true));
    });
}

fn heading(ui: &mut Ui, t: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(t).font(FontId::new(14.0, bold())));
    ui.add_space(4.0);
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (t, icon, tip) in [
            (InspectorTab::Info, ph::INFO, "General Info"),
            (InspectorTab::Details, ph::LIST, "More Info"),
            (InspectorTab::Annotations, ph::PEN_NIB, "Annotations"),
            (InspectorTab::Security, ph::LOCK, "Encryption"),
        ] {
            if crate::layout::tool_button(ui, icon, tip, app.inspector_tab == t, true).clicked() {
                app.inspector_tab = t;
            }
        }
    });
    ui.add_space(8.0);
    let Some(doc) = app.doc() else { return };
    if !doc.ready() {
        ui.label(RichText::new("Loading…").color(pal.text_weak));
        return;
    }
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match app.inspector_tab {
        InspectorTab::Info => info(app, ui),
        InspectorTab::Details => details(app, ui),
        InspectorTab::Annotations => annotations(app, ui),
        InspectorTab::Security => security(app, ui),
    });
}

fn info(app: &mut App, ui: &mut Ui) {
    let doc = app.doc().unwrap();
    heading(ui, &doc.name);
    row(ui, "File name:", doc.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "Not saved".into()));
    row(ui, "File size:", if doc.file_size > 0 { human_size(doc.file_size) } else { "—".into() });
    row(ui, "Created:", fmt_time(doc.created));
    row(ui, "Modified:", fmt_time(doc.modified));
    match doc.kind {
        Kind::Pdf => {
            let Some(info) = doc.info.clone() else { return };
            row(ui, "Document type:", if info.meta.format.is_empty() { "PDF document".into() } else { info.meta.format.clone() });
            row(ui, "Page count:", doc.page_count().to_string());
            if let Some(s) = doc.model.slots.get(doc.current) {
                let sz = s.display_size();
                row(ui, "Page size:", format!("{:.1} × {:.1} cm ({:.0} × {:.0} pt)", sz.x / 72.0 * 2.54, sz.y / 72.0 * 2.54, sz.x, sz.y));
            }
            row(ui, "Title:", info.meta.title.clone());
            row(ui, "Author:", info.meta.author.clone());
            row(ui, "Subject:", info.meta.subject.clone());
            row(ui, "Content creator:", info.meta.creator.clone());
            row(ui, "PDF producer:", info.meta.producer.clone());
            row(ui, "Creation date:", info.meta.created.clone());
            row(ui, "Modification date:", info.meta.modified.clone());
            row(ui, "Form fields:", info.fields.len().to_string());
        }
        Kind::Image => {
            let Some(meta) = doc.image.clone() else { return };
            row(ui, "Document type:", meta.format.clone());
            if let Some(f) = doc.model.frames.get(doc.current) {
                row(ui, "Image size:", format!("{} × {} pixels", f.width(), f.height()));
                row(ui, "Image DPI:", format!("{:.0} pixels/inch", meta.dpi));
                row(ui, "Print size:", format!("{:.1} × {:.1} cm", f.width() as f32 / meta.dpi * 2.54, f.height() as f32 / meta.dpi * 2.54));
            }
            row(ui, "Colour model:", meta.color_model.clone());
            row(ui, "Alpha channel:", if meta.has_alpha { "Yes" } else { "No" });
            if doc.page_count() > 1 {
                row(ui, "Frames:", doc.page_count().to_string());
            }
            if let Some((lat, lon)) = meta.gps {
                ui.add_space(8.0);
                heading(ui, "Location");
                row(ui, "Latitude:", format!("{lat:.6}°"));
                row(ui, "Longitude:", format!("{lon:.6}°"));
                if ui.link(format!("{}  Show on OpenStreetMap", ph::MAP_PIN)).clicked() {
                    app.run(crate::app::Action::ShowLocation);
                }
            }
        }
    }
}

fn details(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let Some(doc) = app.docs.get_mut(app.active) else { return };
    match doc.kind {
        Kind::Image => {
            let exif = doc.image.as_ref().map(|i| i.exif.clone()).unwrap_or_default();
            heading(ui, "EXIF");
            if exif.is_empty() {
                ui.label(RichText::new("No camera information").color(pal.text_weak));
            }
            for (k, v) in exif {
                row(ui, &format!("{k}:"), v);
            }
        }
        Kind::Pdf => {
            if !doc.is_pdf() {
                ui.label(RichText::new("Metadata can be edited for PDF documents only.").color(pal.text_weak));
                return;
            }
            heading(ui, "Document Properties");
            let info = doc.info.clone().unwrap();
            let mut m = doc.model.meta.clone().unwrap_or(MetaEdit {
                title: info.meta.title.clone(),
                author: info.meta.author.clone(),
                subject: info.meta.subject.clone(),
                keywords: info.meta.keywords.clone(),
            });
            let before = m.clone();
            egui::Grid::new("meta").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                for (label, v) in [("Title", &mut m.title), ("Author", &mut m.author), ("Subject", &mut m.subject)] {
                    ui.label(RichText::new(label).color(pal.text_weak));
                    ui.add(egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
                    ui.end_row();
                }
            });
            heading(ui, "Keywords");
            let mut kws: Vec<String> = m.keywords.split([',', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for (i, k) in kws.iter().enumerate() {
                    egui::Frame::new().fill(pal.selected).corner_radius(10).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(k);
                            if ui.add(egui::Button::new(RichText::new(ph::X).size(10.0)).frame(false)).clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
            });
            if let Some(i) = remove {
                kws.remove(i);
            }
            let key = egui::Id::new("new-kw");
            let mut new_kw: String = ui.ctx().data_mut(|d| d.get_temp(key)).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut new_kw).hint_text("Add keyword").desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !new_kw.trim().is_empty() {
                kws.push(new_kw.trim().to_string());
                new_kw.clear();
            }
            ui.ctx().data_mut(|d| d.insert_temp(key, new_kw));
            m.keywords = kws.join(", ");
            if m != before {
                // Coalesce typing into one undo step per field focus.
                let typing = ui.ctx().memory(|mem| mem.focused().is_some());
                if !typing || doc.undo_name() != Some("Change of Document Properties") {
                    doc.checkpoint("Change of Document Properties");
                }
                doc.model.meta = Some(m);
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Changes are written when you save.").small().color(pal.text_weak));
        }
    }
}

fn annotations(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let Some(doc) = app.doc() else { return };
    let doc_id = doc.id;
    let list: Vec<(usize, u64, &'static str, String, Color32)> = doc
        .model
        .slots
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.annots.iter().map(move |a| (i, a.id, a.label(), a.author.clone(), a.style.stroke)))
        .collect();
    heading(ui, &format!("{} annotation{}", list.len(), if list.len() == 1 { "" } else { "s" }));
    let mut go = None;
    egui::Grid::new("annots").striped(true).num_columns(3).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        ui.label(RichText::new("Page").small().color(pal.text_weak));
        ui.label(RichText::new("Type").small().color(pal.text_weak));
        ui.label(RichText::new("Author").small().color(pal.text_weak));
        ui.end_row();
        for (p, id, label, author, color) in list {
            ui.label((p + 1).to_string());
            let r = ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 4.5, color);
                ui.add(egui::Button::new(label).frame(false))
            });
            if r.inner.clicked() {
                go = Some((p, id));
            }
            ui.label(author);
            ui.end_row();
        }
    });
    if let Some((p, id)) = go {
        app.sel.annot = Some((doc_id, p, id));
        app.go_to(p);
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Your name:").color(pal.text_weak));
        if ui.text_edit_singleline(&mut app.settings.author).lost_focus() {
            app.settings.save();
        }
    });
}

fn security(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let Some(doc) = app.doc() else { return };
    heading(ui, "Encryption");
    match doc.kind {
        Kind::Image => {
            ui.label(RichText::new("Images aren’t encrypted.").color(pal.text_weak));
        }
        Kind::Pdf => {
            let Some(info) = doc.info.clone() else { return };
            row(ui, "Encrypted:", if info.encrypted { info.meta.encryption.clone() } else { "No".into() });
            row(ui, "Opened with password:", if doc.password.is_some() { "Yes" } else { "No" });
            if let Some(p) = info.permissions {
                use mupdf::pdf::Permission as P;
                let p = P::from_bits_truncate(p);
                let yn = |b: bool| if b { "Allowed" } else { "Not allowed" };
                row(ui, "Printing:", yn(p.contains(P::PRINT)));
                row(ui, "Copying:", yn(p.contains(P::COPY)));
                row(ui, "Changing:", yn(p.contains(P::MODIFY)));
                row(ui, "Annotating:", yn(p.contains(P::ANNOTATE)));
                row(ui, "Form filling:", yn(p.contains(P::FORM)));
                row(ui, "Page assembly:", yn(p.contains(P::ASSEMBLE)));
            }
            if doc.model.security.is_some() {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{} New permissions will be applied when you save.", ph::INFO)).color(pal.accent));
            }
            ui.add_space(8.0);
            if ui.button("Edit Permissions…").clicked() {
                app.run(crate::app::Action::EditPermissions);
            }
        }
    }
}
