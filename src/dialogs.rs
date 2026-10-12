//! Panels and dialogs (fields recovered from Preview's nibs: PVAdjustColor, PVDocumentSizing,
//! PVPreferences, the export and permission sheets).

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{vec2, Color32, FontId, Pos2, RichText, Sense, Stroke, Ui};
use image::RgbaImage;

use crate::app::{shortcut_text, shortcuts, App, Msg};
use crate::document::{human_size, Document, Kind};
use crate::imaging::{self, Adjust, ExportFormat};
use crate::model::Security;
use crate::settings::{Background, OpenMode, PdfViewMode, Settings, Signature};
use crate::theme::{bold, Palette};

pub struct AdjustState {
    pub doc: u64,
    pub slot: usize,
    pub original: Arc<RgbaImage>,
    pub adj: Adjust,
    pub applied: Adjust,
    pub hist: [[u64; 256]; 4],
    pub busy: bool,
    pub tx: Sender<(Adjust, RgbaImage)>,
    pub rx: Receiver<(Adjust, RgbaImage)>,
}

pub struct SizeState {
    pub doc: u64,
    pub slot: usize,
    pub orig: (u32, u32),
    pub dpi: f32,
    pub w: f32,
    pub h: f32,
    pub unit: usize,
    pub keep: bool,
    pub resample: bool,
}

pub struct ExportState {
    pub doc: u64,
    pub dir: PathBuf,
    pub name: String,
    pub format: ExportFormat,
    pub quality: u8,
    pub alpha: bool,
    pub encrypt: bool,
    pub password: String,
    pub password2: String,
    pub pdf_only: bool,
}

pub struct PrintState {
    pub printers: Vec<String>,
    pub printer: usize,
    pub copies: u32,
    pub range: String,
    pub fit: bool,
    pub two_sided: bool,
    pub busy: bool,
}

pub struct ScanState {
    pub devices: Option<Result<Vec<crate::scanner::Device>, String>>,
    rx: Receiver<Result<Vec<crate::scanner::Device>, String>>,
    pub device: usize,
    pub dpi: u32,
    pub mode: usize,
    /// Insert into this document at this page instead of opening a new image.
    pub insert: Option<(u64, usize)>,
}

pub enum Dialog {
    ConfirmClose { docs: Vec<u64>, quit: bool },
    GoToPage(String),
    Export(ExportState),
    Permissions(Security, String),
    Preferences,
    Signature(Vec<Vec<Pos2>>),
    Print(PrintState),
    AdjustColor(Box<AdjustState>),
    AdjustSize(SizeState),
    About,
    Shortcuts,
    Scan(ScanState),
}

impl Dialog {
    pub fn export(d: &Document, pdf_only: bool, s: &Settings) -> Dialog {
        let path = d.path.clone();
        let dir = path.as_ref().and_then(|p| p.parent().map(|p| p.to_path_buf())).or_else(dirs::picture_dir).or_else(dirs::home_dir).unwrap_or_default();
        let stem = path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_else(|| d.name.clone());
        let format = if pdf_only || d.kind == Kind::Pdf {
            ExportFormat::Pdf
        } else {
            path.as_ref().and_then(|p| ExportFormat::from_path(p)).filter(|f| *f != ExportFormat::Pdf).unwrap_or(ExportFormat::Png)
        };
        Dialog::Export(ExportState {
            doc: d.id,
            dir,
            name: format!("{stem}.{}", format.ext()),
            format,
            quality: s.export_quality,
            alpha: true,
            encrypt: false,
            password: String::new(),
            password2: String::new(),
            pdf_only,
        })
    }

    pub fn scan(insert: Option<(u64, usize)>, ctx: &egui::Context) -> Dialog {
        let (tx, rx) = unbounded();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::scanner::devices());
            c.request_repaint();
        });
        Dialog::Scan(ScanState { devices: None, rx, device: 0, dpi: 300, mode: 0, insert })
    }

    pub fn print() -> Dialog {
        let printers = crate::platform::printers();
        Dialog::Print(PrintState { printers, printer: 0, copies: 1, range: String::new(), fit: true, two_sided: false, busy: false })
    }

    pub fn adjust_color(d: &Document) -> Dialog {
        let slot = d.current.min(d.page_count().saturating_sub(1));
        let original = d.model.frames[slot].clone();
        let (tx, rx) = unbounded();
        Dialog::AdjustColor(Box::new(AdjustState {
            doc: d.id,
            slot,
            hist: imaging::histogram(&original),
            original,
            adj: Adjust::default(),
            applied: Adjust::default(),
            busy: false,
            tx,
            rx,
        }))
    }

    pub fn adjust_size(d: &Document) -> Dialog {
        let slot = d.current.min(d.page_count().saturating_sub(1));
        let f = &d.model.frames[slot];
        let dpi = d.image.as_ref().map(|i| i.dpi).unwrap_or(72.0);
        Dialog::AdjustSize(SizeState { doc: d.id, slot, orig: (f.width(), f.height()), dpi, w: f.width() as f32, h: f.height() as f32, unit: 0, keep: true, resample: true })
    }
}

fn window<'a>(title: &'a str) -> egui::Window<'a> {
    egui::Window::new(RichText::new(title).font(FontId::new(14.0, bold())))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -40.0))
}

fn primary(ui: &mut Ui, text: &str) -> egui::Response {
    let pal = Palette::of(ui.ctx());
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(pal.accent).min_size(vec2(84.0, 28.0)).corner_radius(7))
}

fn secondary(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(text).min_size(vec2(84.0, 28.0)).corner_radius(7))
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut dialog) = app.dialog.take() else { return };
    let mut keep = true;
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    match &mut dialog {
        Dialog::ConfirmClose { docs, quit } => {
            let names: Vec<String> = docs.iter().filter_map(|id| app.docs.iter().find(|d| d.id == *id)).map(|d| d.name.clone()).collect();
            let mut result: Option<Option<bool>> = None;
            egui::Modal::new(egui::Id::new("confirm-close")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new(egui_phosphor::regular::WARNING).size(30.0).color(Color32::from_rgb(0xFF, 0x95, 0x00)));
                let title = if names.len() == 1 { format!("Save changes to “{}”?", names[0]) } else { format!("You have {} documents with unsaved changes.", names.len()) };
                ui.label(RichText::new(title).font(FontId::new(15.0, bold())));
                ui.label("Your changes will be lost if you don’t save them.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if secondary(ui, "Don’t Save").clicked() {
                        result = Some(Some(false));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary(ui, "Save").clicked() || enter {
                            result = Some(Some(true));
                        }
                        if secondary(ui, "Cancel").clicked() {
                            result = Some(None);
                        }
                    });
                });
            });
            if let Some(r) = result {
                keep = false;
                let (docs, quit) = (docs.clone(), *quit);
                app.dialog = None;
                app.confirm_close_result(docs, quit, r);
                if app.dialog.is_some() {
                    return;
                }
            }
        }
        Dialog::GoToPage(text) => {
            let n = app.doc().map(|d| d.page_count()).unwrap_or(0);
            let mut go = None;
            egui::Modal::new(egui::Id::new("goto")).show(ctx, |ui| {
                ui.set_width(260.0);
                ui.label(RichText::new("Go to Page").font(FontId::new(14.0, bold())));
                ui.add_space(6.0);
                let r = ui.add(egui::TextEdit::singleline(text).hint_text(format!("1 – {n}, or a page label")).desired_width(f32::INFINITY));
                r.request_focus();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary(ui, "Go").clicked() || enter {
                            go = Some(text.clone());
                        }
                        if secondary(ui, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                });
            });
            if let Some(t) = go {
                keep = false;
                let t = t.trim().to_string();
                let by_label = app.doc().and_then(|d| d.model.slots.iter().position(|s| !s.label.is_empty() && s.label.eq_ignore_ascii_case(&t)));
                match by_label.or_else(|| t.parse::<usize>().ok().map(|x| x.saturating_sub(1))) {
                    Some(p) if p < n => app.go_to(p),
                    _ => app.error(format!("There’s no page “{t}”.")),
                }
            }
        }
        Dialog::Export(st) => keep = export_dialog(app, ctx, st),
        Dialog::Permissions(sec, confirm) => {
            let mut done = None;
            window("Edit Permissions").show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label("Require a password to open the document:");
                ui.add(egui::TextEdit::singleline(&mut sec.user_password).password(true).hint_text("Open password (optional)").desired_width(f32::INFINITY));
                ui.add_space(8.0);
                ui.label("Permissions — protected by the owner password:");
                ui.checkbox(&mut sec.allow_print, "Printing");
                ui.checkbox(&mut sec.allow_copy, "Copying text and graphics");
                ui.checkbox(&mut sec.allow_modify, "Changing the document");
                ui.checkbox(&mut sec.allow_assemble, "Inserting, deleting and rotating pages");
                ui.checkbox(&mut sec.allow_annotate, "Adding annotations");
                ui.checkbox(&mut sec.allow_forms, "Filling existing form fields");
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut sec.owner_password).password(true).hint_text("Owner password").desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(confirm).password(true).hint_text("Verify owner password").desired_width(f32::INFINITY));
                let mismatch = !sec.owner_password.is_empty() && *confirm != sec.owner_password;
                if mismatch {
                    ui.label(RichText::new("The passwords don’t match.").color(ui.visuals().error_fg_color));
                }
                let restricts = !(sec.allow_print && sec.allow_copy && sec.allow_modify && sec.allow_annotate && sec.allow_forms && sec.allow_assemble);
                if restricts && sec.owner_password.is_empty() {
                    ui.label(RichText::new("Set an owner password so permissions can’t simply be removed.").small().color(Palette::of(ui.ctx()).text_weak));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if secondary(ui, "Remove Protection").clicked() {
                        done = Some(Some(Security::default()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!mismatch, egui::Button::new(RichText::new("Apply").color(Color32::WHITE)).fill(Palette::of(ui.ctx()).accent).min_size(vec2(84.0, 28.0))).clicked() {
                            done = Some(Some(sec.clone()));
                        }
                        if secondary(ui, "Cancel").clicked() {
                            done = Some(None);
                        }
                    });
                });
            });
            if let Some(r) = done {
                keep = false;
                if let Some(s) = r {
                    if let Some(d) = app.doc_mut() {
                        d.checkpoint("Permissions Change");
                        d.model.security = Some(s);
                    }
                    app.toast("Permissions will be applied when you save.");
                }
            }
        }
        Dialog::Preferences => keep = preferences(app, ctx),
        Dialog::Signature(strokes) => keep = signature_pad(app, ctx, strokes),
        Dialog::Print(st) => keep = print_dialog(app, ctx, st),
        Dialog::Scan(st) => keep = scan_dialog(app, ctx, st),
        Dialog::AdjustColor(st) => keep = adjust_color(app, ctx, st),
        Dialog::AdjustSize(st) => keep = adjust_size(app, ctx, st),
        Dialog::About => {
            egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(egui_phosphor::regular::FILE_MAGNIFYING_GLASS).size(48.0).color(Palette::of(ui.ctx()).accent));
                    ui.label(RichText::new("Review").font(FontId::new(20.0, bold())));
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(6.0);
                    ui.label("A fast, Preview-style viewer and editor for PDFs and images.");
                    ui.label(RichText::new("Built with Rust, egui and MuPDF. Not affiliated with Apple.").small().color(Palette::of(ui.ctx()).text_weak));
                    ui.add_space(10.0);
                    if primary(ui, "OK").clicked() || enter {
                        keep = false;
                    }
                });
            });
        }
        Dialog::Shortcuts => {
            let mut open = true;
            window("Keyboard Shortcuts").open(&mut open).resizable(true).default_height(480.0).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("sc").striped(true).num_columns(2).show(ui, |ui| {
                        for (_, a) in shortcuts() {
                            ui.label(format!("{a:?}").replace("Tool(", "").replace(')', ""));
                            ui.label(RichText::new(shortcut_text(a)).monospace());
                            ui.end_row();
                        }
                        for (k, v) in [("Pan", "Space + drag, or middle-drag"), ("Zoom", "Ctrl + scroll, or pinch"), ("Next/previous page", "Arrow keys, PgUp/PgDn, Space")] {
                            ui.label(k);
                            ui.label(RichText::new(v).monospace());
                            ui.end_row();
                        }
                    });
                });
            });
            keep = open;
        }
    }
    if keep && app.dialog.is_none() {
        app.dialog = Some(dialog);
    }
}

// ---------------------------------------------------------------------------

fn export_dialog(app: &mut App, ctx: &egui::Context, st: &mut ExportState) -> bool {
    let pal = Palette::of(ctx);
    let Some(di) = app.doc_index(st.doc) else { return false };
    let kind = app.docs[di].kind;
    let mut keep = true;
    let mut go = false;
    // A folder picked with the chooser arrives through the message queue.
    if let Some(p) = ctx.data_mut(|d| d.remove_temp::<PathBuf>(egui::Id::new("export-dest"))) {
        if let Some(dir) = p.parent() {
            st.dir = dir.to_path_buf();
        }
        if let Some(n) = p.file_name() {
            st.name = n.to_string_lossy().to_string();
            if let Some(f) = ExportFormat::from_path(&p) {
                st.format = f;
            }
        }
    }
    window(if st.pdf_only { "Export as PDF" } else { "Export" }).show(ctx, |ui| {
        ui.set_width(420.0);
        egui::Grid::new("export").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
            ui.label("Export As:");
            ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(280.0));
            ui.end_row();
            ui.label("Where:");
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(st.dir.to_string_lossy()).color(pal.text_weak)).truncate());
                if ui.button("Choose…").clicked() {
                    let tx = app.msg_tx.clone();
                    let c = ctx.clone();
                    let dir = st.dir.clone();
                    let name = st.name.clone();
                    std::thread::spawn(move || {
                        if let Some(p) = rfd::FileDialog::new().set_directory(dir).set_file_name(name).set_title("Export").save_file() {
                            c.data_mut(|d| d.insert_temp(egui::Id::new("export-dest"), p));
                        }
                        let _ = tx.send(Msg::Info(String::new()));
                        c.request_repaint();
                    });
                }
            });
            ui.end_row();
            if !st.pdf_only {
                ui.label("Format:");
                let before = st.format;
                egui::ComboBox::from_id_salt("fmt").selected_text(st.format.name()).show_ui(ui, |ui| {
                    for f in ExportFormat::all() {
                        ui.selectable_value(&mut st.format, f, f.name());
                    }
                });
                if before != st.format {
                    let stem = st.name.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or(st.name.clone());
                    st.name = format!("{stem}.{}", st.format.ext());
                }
                ui.end_row();
            }
            if st.format.has_quality() {
                ui.label("Quality:");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Least").small());
                    ui.add(egui::Slider::new(&mut st.quality, 10..=100).show_value(false));
                    ui.label(RichText::new("Best").small());
                });
                ui.end_row();
            }
            if st.format.supports_alpha() && st.format != ExportFormat::Pdf {
                ui.label("");
                ui.checkbox(&mut st.alpha, "Alpha (keep transparency)");
                ui.end_row();
            }
            if st.format == ExportFormat::Pdf && kind == Kind::Pdf {
                ui.label("");
                ui.checkbox(&mut st.encrypt, "Encrypt");
                ui.end_row();
                if st.encrypt {
                    ui.label("Password:");
                    ui.add(egui::TextEdit::singleline(&mut st.password).password(true).desired_width(200.0));
                    ui.end_row();
                    ui.label("Verify:");
                    ui.add(egui::TextEdit::singleline(&mut st.password2).password(true).desired_width(200.0));
                    ui.end_row();
                }
            }
            if kind == Kind::Image && st.format != ExportFormat::Pdf {
                if let Some(f) = app.docs[di].model.frames.get(app.docs[di].current) {
                    ui.label("File Size:");
                    let est = estimate(f.width() as u64 * f.height() as u64, st.format, st.quality);
                    ui.label(RichText::new(format!("about {}", human_size(est))).color(pal.text_weak));
                    ui.end_row();
                }
            }
            if kind == Kind::Pdf && st.format != ExportFormat::Pdf {
                ui.label("");
                ui.label(RichText::new("Exports the current page at 150 DPI.").small().color(pal.text_weak));
                ui.end_row();
            }
        });
        ui.add_space(10.0);
        let bad_pw = st.encrypt && (st.password.is_empty() || st.password != st.password2);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!bad_pw && !st.name.trim().is_empty(), egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(pal.accent).min_size(vec2(84.0, 28.0))).clicked() {
                    go = true;
                }
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
    });
    if go {
        let dest = st.dir.join(st.name.trim());
        if dest.exists() && !ctx.data(|d| d.get_temp::<bool>(egui::Id::new("export-overwrite-ok")).unwrap_or(false)) {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("export-overwrite-ok"), true));
            app.error(format!("“{}” already exists — click Save again to replace it.", st.name.trim()));
            return true;
        }
        ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("export-overwrite-ok")));
        app.settings.export_quality = st.quality;
        let security = (st.encrypt && st.format == ExportFormat::Pdf).then(|| Security {
            user_password: st.password.clone(),
            owner_password: st.password.clone(),
            allow_print: true,
            allow_copy: true,
            allow_modify: true,
            allow_annotate: true,
            allow_forms: true,
            allow_assemble: true,
        });
        // Exporting to the same format keeps working on the new file, like Save As.
        let adopt = (kind == Kind::Pdf && st.format == ExportFormat::Pdf) || (kind == Kind::Image && st.format != ExportFormat::Pdf && app.docs[di].page_count() == 1);
        app.save_to(di, dest, Some(crate::app::ExportOpts { format: st.format, quality: st.quality, alpha: st.alpha, security }), adopt && !st.pdf_only);
        return false;
    }
    keep
}

fn estimate(px: u64, f: ExportFormat, q: u8) -> u64 {
    match f {
        ExportFormat::Jpeg => px * (q as u64 * q as u64 / 60 + 20) / 100 / 4,
        ExportFormat::Png | ExportFormat::WebP => px * 2,
        ExportFormat::Tiff | ExportFormat::Bmp => px * 3,
        ExportFormat::Gif => px / 2,
        _ => px / 4,
    }
}

fn print_dialog(app: &mut App, ctx: &egui::Context, st: &mut PrintState) -> bool {
    let pal = Palette::of(ctx);
    let mut keep = true;
    let mut go = false;
    window("Print").show(ctx, |ui| {
        ui.set_width(380.0);
        egui::Grid::new("print").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
            ui.label("Printer:");
            if st.printers.is_empty() {
                ui.label(RichText::new("No printers found (is CUPS running?)").color(pal.text_weak));
            } else {
                egui::ComboBox::from_id_salt("printer").selected_text(st.printers[st.printer.min(st.printers.len() - 1)].clone()).width(220.0).show_ui(ui, |ui| {
                    for (i, p) in st.printers.iter().enumerate() {
                        ui.selectable_value(&mut st.printer, i, p);
                    }
                });
            }
            ui.end_row();
            ui.label("Copies:");
            ui.add(egui::DragValue::new(&mut st.copies).range(1..=999));
            ui.end_row();
            ui.label("Pages:");
            ui.add(egui::TextEdit::singleline(&mut st.range).hint_text("All  (e.g. 1-3, 7)").desired_width(200.0));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut st.fit, "Scale to fit paper");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut st.two_sided, "Two-sided");
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Save as PDF…").clicked() {
                if let Some(d) = app.doc() {
                    app.dialog = Some(Dialog::export(d, true, &app.settings));
                }
                keep = false;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!st.printers.is_empty() && !st.busy, egui::Button::new(RichText::new("Print").color(Color32::WHITE)).fill(pal.accent).min_size(vec2(84.0, 28.0))).clicked() {
                    go = true;
                }
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
    });
    if go {
        let printer = st.printers.get(st.printer).cloned().unwrap_or_default();
        let (copies, range, fit, two) = (st.copies, st.range.clone(), st.fit, st.two_sided);
        let tx = app.msg_tx.clone();
        let c = ctx.clone();
        let tmp = std::env::temp_dir().join(format!("review-print-{}.pdf", crate::model::next_id()));
        let Some(di) = Some(app.active).filter(|i| *i < app.docs.len()) else { return false };
        let job: Box<dyn FnOnce() -> Result<(), String> + Send> = match app.docs[di].kind {
            Kind::Model => return false,
            Kind::Pdf => {
                let sources = app.sources.iter().map(|(k, s)| (*k, (s.path.clone(), s.password.clone()))).collect();
                let plan = app.docs[di].save_plan(&tmp, &sources, &app.author());
                let src = app.docs[di].source.and_then(|s| app.sources.get(&s)).map(|s| (s.path.clone(), s.password.clone()));
                let Some((path, pw)) = src else { return false };
                Box::new(move || crate::pdf::save(&path, pw.as_deref(), &plan))
            }
            Kind::Image => {
                let frames: Vec<RgbaImage> = (0..app.docs[di].page_count()).filter_map(|i| app.docs[di].flattened_frame(i)).collect();
                let dpi = app.docs[di].image.as_ref().map(|i| i.dpi).unwrap_or(72.0).max(150.0);
                let t = tmp.clone();
                Box::new(move || crate::pdf::images_to_pdf(&frames.iter().collect::<Vec<_>>(), dpi, &t))
            }
        };
        std::thread::spawn(move || {
            let r = job().and_then(|_| crate::platform::print_file(&tmp, &printer, copies, &range, fit, two));
            let _ = std::fs::remove_file(&tmp);
            let _ = tx.send(match r {
                Ok(()) => Msg::Info(format!("Sent to {printer}")),
                Err(e) => Msg::Error(format!("Printing failed: {e}")),
            });
            c.request_repaint();
        });
        return false;
    }
    keep
}

fn scan_dialog(app: &mut App, ctx: &egui::Context, st: &mut ScanState) -> bool {
    let pal = Palette::of(ctx);
    if let Ok(d) = st.rx.try_recv() {
        st.devices = Some(d);
    }
    let modes = ["Color", "Gray", "Lineart"];
    let mut keep = true;
    let mut go = None;
    window(if st.insert.is_some() { "Insert Page from Scanner" } else { "Import from Scanner" }).show(ctx, |ui| {
        ui.set_width(380.0);
        match &st.devices {
            None => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Looking for scanners…");
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
            }
            Some(Ok(list)) if list.is_empty() => {
                ui.label("No scanners found. Check that it is on and connected, or install its SANE driver.");
            }
            Some(Ok(list)) => {
                egui::Grid::new("scan").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    ui.label("Scanner:");
                    egui::ComboBox::from_id_salt("scanner").width(240.0).selected_text(list[st.device.min(list.len() - 1)].label.clone()).show_ui(ui, |ui| {
                        for (i, d) in list.iter().enumerate() {
                            ui.selectable_value(&mut st.device, i, &d.label);
                        }
                    });
                    ui.end_row();
                    ui.label("Resolution:");
                    egui::ComboBox::from_id_salt("dpi").selected_text(format!("{} dpi", st.dpi)).show_ui(ui, |ui| {
                        for d in [75, 150, 300, 600, 1200] {
                            ui.selectable_value(&mut st.dpi, d, format!("{d} dpi"));
                        }
                    });
                    ui.end_row();
                    ui.label("Kind:");
                    ui.horizontal(|ui| {
                        for (i, m) in ["Colour", "Black & White", "Text"].iter().enumerate() {
                            ui.selectable_value(&mut st.mode, i, *m);
                        }
                    });
                    ui.end_row();
                });
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = matches!(&st.devices, Some(Ok(l)) if !l.is_empty());
                if ui.add_enabled(ready, egui::Button::new(RichText::new("Scan").color(Color32::WHITE)).fill(pal.accent).min_size(vec2(84.0, 28.0))).clicked() {
                    if let Some(Ok(list)) = &st.devices {
                        go = Some(list[st.device.min(list.len() - 1)].id.clone());
                    }
                }
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
    });
    if let Some(device) = go {
        let (dpi, mode, insert) = (st.dpi, modes[st.mode].to_string(), st.insert);
        let tx = app.msg_tx.clone();
        let c = ctx.clone();
        app.toast("Scanning…");
        std::thread::spawn(move || {
            let result = crate::scanner::scan(&device, dpi, &mode);
            let _ = tx.send(Msg::Scanned { insert, dpi, result });
            c.request_repaint();
        });
        return false;
    }
    keep
}

fn preferences(app: &mut App, ctx: &egui::Context) -> bool {
    let mut open = true;
    let before = format!("{:?}", (&app.settings.open_mode, &app.settings.background, app.settings.image_pixel_scale, &app.settings.pdf_view, app.settings.resume_last_page, app.settings.logical_page_numbers, &app.settings.author, app.settings.add_name_to_annotations, app.settings.dark_pdf, app.settings.live_text));
    window("Preferences").open(&mut open).show(ctx, |ui| {
        ui.set_width(440.0);
        let tab_id = egui::Id::new("pref-tab");
        let mut tab: usize = ctx.data(|d| d.get_temp(tab_id)).unwrap_or(0);
        ui.horizontal(|ui| {
            for (i, (icon, name)) in [(egui_phosphor::regular::GEAR, "General"), (egui_phosphor::regular::IMAGE, "Images"), (egui_phosphor::regular::FILE_PDF, "PDF")].iter().enumerate() {
                if ui.selectable_label(tab == i, format!("{icon}  {name}")).clicked() {
                    tab = i;
                }
            }
        });
        ctx.data_mut(|d| d.insert_temp(tab_id, tab));
        ui.separator();
        ui.add_space(4.0);
        let s = &mut app.settings;
        egui::Grid::new("prefs").num_columns(2).spacing(vec2(12.0, 10.0)).show(ui, |ui| match tab {
            0 => {
                ui.label("Window background:");
                egui::ComboBox::from_id_salt("bg").selected_text(format!("{:?}", s.background)).show_ui(ui, |ui| {
                    for b in [Background::System, Background::Light, Background::Dark, Background::Black] {
                        ui.selectable_value(&mut s.background, b, format!("{b:?}"));
                    }
                });
                ui.end_row();
                ui.label("Live Text:");
                ui.vertical(|ui| {
                    ui.checkbox(&mut s.live_text, "Select and search text in photos and scanned pages");
                    if !crate::ocr::available() {
                        ui.label(RichText::new("Needs Tesseract: sudo dnf install tesseract").small().color(Palette::of(ui.ctx()).text_weak));
                    }
                });
                ui.end_row();
                ui.label("When opening files:");
                ui.vertical(|ui| {
                    ui.radio_value(&mut s.open_mode, OpenMode::SameWindow, "Open groups of files in the same window");
                    ui.radio_value(&mut s.open_mode, OpenMode::OwnWindow, "Open each file in its own window");
                });
                ui.end_row();
            }
            1 => {
                ui.label("Define 100% scale as:");
                ui.vertical(|ui| {
                    ui.radio_value(&mut s.image_pixel_scale, true, "1 image pixel equals 1 screen point");
                    ui.radio_value(&mut s.image_pixel_scale, false, "Size on screen equals size on printout");
                });
                ui.end_row();
            }
            _ => {
                ui.label("Default view:");
                egui::ComboBox::from_id_salt("pdfview")
                    .selected_text(match s.pdf_view {
                        PdfViewMode::Continuous => "Continuous Scroll",
                        PdfViewMode::Single => "Single Page",
                        PdfViewMode::Two => "Two Pages",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.pdf_view, PdfViewMode::Continuous, "Continuous Scroll");
                        ui.selectable_value(&mut s.pdf_view, PdfViewMode::Single, "Single Page");
                        ui.selectable_value(&mut s.pdf_view, PdfViewMode::Two, "Two Pages");
                    });
                ui.end_row();
                ui.label("When opening PDFs:");
                ui.checkbox(&mut s.resume_last_page, "Start on the last viewed page");
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.logical_page_numbers, "Use logical page numbers (e.g. “iv”)");
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.dark_pdf, "Use dark appearance for PDF");
                ui.end_row();
                ui.label("Annotations:");
                ui.checkbox(&mut s.add_name_to_annotations, "Add name to annotations");
                ui.end_row();
                ui.label("Name:");
                ui.add_enabled(s.add_name_to_annotations, egui::TextEdit::singleline(&mut s.author).desired_width(220.0));
                ui.end_row();
            }
        });
    });
    let after = format!("{:?}", (&app.settings.open_mode, &app.settings.background, app.settings.image_pixel_scale, &app.settings.pdf_view, app.settings.resume_last_page, app.settings.logical_page_numbers, &app.settings.author, app.settings.add_name_to_annotations, app.settings.dark_pdf, app.settings.live_text));
    if before != after {
        app.settings.save();
    }
    open
}

fn signature_pad(app: &mut App, ctx: &egui::Context, strokes: &mut Vec<Vec<Pos2>>) -> bool {
    let pal = Palette::of(ctx);
    let mut keep = true;
    window("Create Signature").show(ctx, |ui| {
        ui.label("Sign with your mouse, trackpad or pen.");
        ui.add_space(6.0);
        let (r, resp) = ui.allocate_exact_size(vec2(460.0, 170.0), Sense::drag());
        ui.painter().rect_filled(r, 10.0, Color32::WHITE);
        ui.painter().rect_stroke(r, 10.0, Stroke::new(1.0, pal.separator), egui::StrokeKind::Inside);
        let base = r.min.y + r.height() * 0.72;
        ui.painter().line_segment([egui::pos2(r.min.x + 30.0, base), egui::pos2(r.max.x - 30.0, base)], Stroke::new(1.0, Color32::from_gray(200)));
        ui.painter().text(egui::pos2(r.min.x + 18.0, base - 4.0), egui::Align2::LEFT_BOTTOM, "×", FontId::proportional(16.0), Color32::from_gray(150));
        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            strokes.push(origin.filter(|p| r.contains(*p)).into_iter().collect());
        }
        if resp.dragged() {
            if let (Some(p), Some(s)) = (resp.interact_pointer_pos(), strokes.last_mut()) {
                if r.contains(p) && s.last().map(|l| (*l - p).length() > 1.0).unwrap_or(true) {
                    s.push(p);
                }
            }
        }
        for s in strokes.iter() {
            ui.painter().add(egui::Shape::line(s.clone(), Stroke::new(2.2, Color32::from_rgb(0x10, 0x10, 0x30))));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if secondary(ui, "Clear").clicked() {
                strokes.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = strokes.iter().any(|s| s.len() > 2);
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Done").color(Color32::WHITE)).fill(pal.accent).min_size(vec2(84.0, 28.0))).clicked() {
                    let mut b = egui::Rect::NOTHING;
                    for p in strokes.iter().flatten() {
                        b.extend_with(*p);
                    }
                    let w = b.width().max(1.0);
                    let sig = Signature {
                        aspect: (b.height() / w).max(0.05),
                        strokes: strokes.iter().filter(|s| s.len() > 1).map(|s| s.iter().map(|p| [(p.x - b.min.x) / w, (p.y - b.min.y) / w]).collect()).collect(),
                    };
                    app.settings.signatures.push(sig);
                    app.settings.save();
                    let idx = app.settings.signatures.len() - 1;
                    app.tool = crate::app::Tool::Signature(idx);
                    app.show_markup = true;
                    app.toast("Signature saved — click on the page to place it.");
                    keep = false;
                }
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
    });
    keep
}

fn adjust_color(app: &mut App, ctx: &egui::Context, st: &mut AdjustState) -> bool {
    let pal = Palette::of(ctx);
    let Some(di) = app.doc_index(st.doc) else { return false };
    // Results from the background worker.
    while let Ok((adj, img)) = st.rx.try_recv() {
        st.busy = false;
        st.applied = adj;
        app.docs[di].model.frames[st.slot] = Arc::new(img);
    }
    let mut keep = true;
    let mut commit = None;
    let mut open = true;
    egui::Window::new(RichText::new("Adjust Color").font(FontId::new(14.0, bold())))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().right_top() + vec2(-360.0, 120.0))
        .show(ctx, |ui| {
            ui.set_width(300.0);
            // Levels histogram with draggable black / white points.
            let (r, _) = ui.allocate_exact_size(vec2(300.0, 90.0), Sense::hover());
            ui.painter().rect_filled(r, 6.0, if pal.dark { Color32::from_gray(30) } else { Color32::from_gray(245) });
            let max = st.hist[3].iter().copied().max().unwrap_or(1).max(1) as f32;
            for (c, color) in [(0usize, Color32::from_rgba_unmultiplied(255, 60, 60, 90)), (1, Color32::from_rgba_unmultiplied(60, 200, 60, 90)), (2, Color32::from_rgba_unmultiplied(60, 120, 255, 90))] {
                let pts: Vec<Pos2> = (0..256).map(|i| egui::pos2(r.min.x + i as f32 / 255.0 * r.width(), r.max.y - (st.hist[c][i] as f32 / max).sqrt() * r.height())).collect();
                ui.painter().add(egui::Shape::line(pts, Stroke::new(1.0, color)));
            }
            for (v, label) in [(&mut st.adj.black, "black"), (&mut st.adj.white, "white")] {
                let x = r.min.x + *v * r.width();
                let hr = egui::Rect::from_center_size(egui::pos2(x, r.max.y), vec2(14.0, 18.0));
                let resp = ui.interact(hr, egui::Id::new(("lvl", label)), Sense::drag());
                if resp.dragged() {
                    *v = ((x + resp.drag_delta().x - r.min.x) / r.width()).clamp(0.0, 1.0);
                }
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![egui::pos2(x, r.max.y - 8.0), egui::pos2(x + 6.0, r.max.y + 2.0), egui::pos2(x - 6.0, r.max.y + 2.0)],
                    if label == "black" { Color32::BLACK } else { Color32::WHITE },
                    Stroke::new(1.0, pal.text_weak),
                ));
            }
            if st.adj.white < st.adj.black + 0.02 {
                st.adj.white = (st.adj.black + 0.02).min(1.0);
            }
            ui.add_space(8.0);
            egui::Grid::new("adj").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                let slider = |ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>| {
                    ui.label(label);
                    ui.add(egui::Slider::new(v, range).show_value(false));
                    ui.end_row();
                };
                slider(ui, "Exposure:", &mut st.adj.exposure, -2.0..=2.0);
                slider(ui, "Contrast:", &mut st.adj.contrast, -0.8..=0.8);
                slider(ui, "Highlights:", &mut st.adj.highlights, -1.0..=1.0);
                slider(ui, "Shadows:", &mut st.adj.shadows, -1.0..=1.0);
                slider(ui, "Saturation:", &mut st.adj.saturation, 0.0..=2.0);
                slider(ui, "Temperature:", &mut st.adj.temperature, -1.0..=1.0);
                slider(ui, "Tint:", &mut st.adj.tint, -1.0..=1.0);
                slider(ui, "Sepia:", &mut st.adj.sepia, 0.0..=1.0);
                slider(ui, "Sharpness:", &mut st.adj.sharpness, -1.0..=1.0);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if secondary(ui, "Auto Levels").clicked() {
                    let a = imaging::auto_levels(&st.original);
                    st.adj.black = a.black;
                    st.adj.white = a.white;
                }
                if secondary(ui, "Reset All").clicked() {
                    st.adj = Adjust::default();
                }
                if st.busy {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary(ui, "Done").clicked() {
                        commit = Some(true);
                    }
                    if secondary(ui, "Cancel").clicked() {
                        commit = Some(false);
                    }
                });
            });
        });
    if !open {
        commit = Some(true);
    }
    // Kick off a background render whenever the sliders moved.
    if !st.busy && st.adj != st.applied {
        st.busy = true;
        let (adj, orig, tx, c) = (st.adj, st.original.clone(), st.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let out = adj.apply(&orig);
            let _ = tx.send((adj, out));
            c.request_repaint();
        });
    }
    if let Some(ok) = commit {
        keep = false;
        let d = &mut app.docs[di];
        if ok && !st.adj.is_identity() {
            // Record the original as the undo step, then apply at full quality.
            let current = d.model.frames[st.slot].clone();
            d.model.frames[st.slot] = st.original.clone();
            d.checkpoint("Adjust Color");
            d.model.frames[st.slot] = if st.applied == st.adj { current } else { Arc::new(st.adj.apply(&st.original)) };
        } else {
            d.model.frames[st.slot] = st.original.clone();
        }
    }
    keep
}

fn adjust_size(app: &mut App, ctx: &egui::Context, st: &mut SizeState) -> bool {
    let pal = Palette::of(ctx);
    let mut keep = true;
    let mut apply = false;
    let units = ["pixels", "percent", "inches", "cm", "mm", "points"];
    let to_px = |v: f32, unit: usize, orig: u32, dpi: f32| -> f32 {
        match unit {
            0 => v,
            1 => v / 100.0 * orig as f32,
            2 => v * dpi,
            3 => v / 2.54 * dpi,
            4 => v / 25.4 * dpi,
            _ => v / 72.0 * dpi,
        }
    };
    let from_px = |px: f32, unit: usize, orig: u32, dpi: f32| -> f32 {
        match unit {
            0 => px,
            1 => px / orig as f32 * 100.0,
            2 => px / dpi,
            3 => px / dpi * 2.54,
            4 => px / dpi * 25.4,
            _ => px / dpi * 72.0,
        }
    };
    window("Image Dimensions").show(ctx, |ui| {
        ui.set_width(360.0);
        egui::Grid::new("size").num_columns(3).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
            ui.label("Fit into:");
            egui::ComboBox::from_id_salt("fit").selected_text("Custom").show_ui(ui, |ui| {
                for (w, h) in [(320u32, 240u32), (640, 480), (800, 600), (1024, 768), (1280, 1024), (1920, 1080), (2560, 1440), (3840, 2160)] {
                    if ui.button(format!("{w} × {h} pixels")).clicked() {
                        let s = (w as f32 / st.orig.0 as f32).min(h as f32 / st.orig.1 as f32);
                        st.unit = 0;
                        st.w = (st.orig.0 as f32 * s).round();
                        st.h = (st.orig.1 as f32 * s).round();
                    }
                }
            });
            ui.label("");
            ui.end_row();
            let wpx_before = to_px(st.w, st.unit, st.orig.0, st.dpi);
            let hpx_before = to_px(st.h, st.unit, st.orig.1, st.dpi);
            ui.label("Width:");
            let wr = ui.add(egui::DragValue::new(&mut st.w).range(0.01..=100000.0).max_decimals(2));
            let unit_before = st.unit;
            egui::ComboBox::from_id_salt("unit").selected_text(units[st.unit]).show_ui(ui, |ui| {
                for (i, u) in units.iter().enumerate() {
                    ui.selectable_value(&mut st.unit, i, *u);
                }
            });
            ui.end_row();
            ui.label("Height:");
            let hr = ui.add(egui::DragValue::new(&mut st.h).range(0.01..=100000.0).max_decimals(2));
            ui.label("");
            ui.end_row();
            if st.unit != unit_before {
                st.w = from_px(wpx_before, st.unit, st.orig.0, st.dpi);
                st.h = from_px(hpx_before, st.unit, st.orig.1, st.dpi);
            }
            let ratio = st.orig.1 as f32 / st.orig.0 as f32;
            if st.keep && wr.changed() {
                let wpx = to_px(st.w, st.unit, st.orig.0, st.dpi);
                st.h = from_px(wpx * ratio, st.unit, st.orig.1, st.dpi);
            }
            if st.keep && hr.changed() {
                let hpx = to_px(st.h, st.unit, st.orig.1, st.dpi);
                st.w = from_px(hpx / ratio, st.unit, st.orig.0, st.dpi);
            }
            ui.label("Resolution:");
            ui.add(egui::DragValue::new(&mut st.dpi).range(1.0..=4800.0).max_decimals(0));
            ui.label("pixels/inch");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut st.keep, "Scale proportionally");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut st.resample, "Resample image");
            ui.end_row();
        });
        let wpx = to_px(st.w, st.unit, st.orig.0, st.dpi).round().max(1.0) as u32;
        let hpx = to_px(st.h, st.unit, st.orig.1, st.dpi).round().max(1.0) as u32;
        ui.add_space(6.0);
        ui.label(RichText::new("Resulting Size").strong());
        let (nw, nh) = if st.resample { (wpx, hpx) } else { st.orig };
        ui.label(RichText::new(format!("{nw} × {nh} pixels · {} in memory (was {})", human_size(nw as u64 * nh as u64 * 4), human_size(st.orig.0 as u64 * st.orig.1 as u64 * 4))).color(pal.text_weak));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, "OK").clicked() {
                    apply = true;
                }
                if secondary(ui, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
    });
    if apply {
        keep = false;
        let wpx = to_px(st.w, st.unit, st.orig.0, st.dpi).round().max(1.0) as u32;
        let hpx = to_px(st.h, st.unit, st.orig.1, st.dpi).round().max(1.0) as u32;
        if let Some(di) = app.doc_index(st.doc) {
            let d = &mut app.docs[di];
            if st.resample && (wpx, hpx) != st.orig {
                let (sx, sy) = (wpx as f32 / st.orig.0 as f32, hpx as f32 / st.orig.1 as f32);
                d.edit_frame(st.slot, "Size Change", |img| imaging::resize(img, wpx, hpx), move |p| egui::pos2(p.x * sx, p.y * sy));
            } else if !st.resample {
                d.checkpoint("Resolution Change");
                // Same pixels, new physical size: DPI follows the requested width.
                let new_dpi = st.orig.0 as f32 / (wpx as f32 / st.dpi).max(1e-3);
                if let Some(m) = d.image.as_mut() {
                    m.dpi = new_dpi;
                }
            }
            if let Some(m) = d.image.as_mut() {
                if st.resample {
                    m.dpi = st.dpi;
                }
            }
        }
    }
    keep
}

