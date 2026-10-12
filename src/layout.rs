//! Window chrome: menu bar, toolbar, markup toolbar, panels and toasts.

use std::time::Instant;

use egui::{vec2, Color32, CornerRadius, FontId, Frame, Margin, Response, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_phosphor::regular as ph;

use crate::app::{shortcut_text, Action, App, ShapeTool, SidebarMode, Tool};
use crate::document::Kind;
use crate::model::{Align, MarkupKind, Shape};
use crate::settings::PdfViewMode;
use crate::theme::{bold, Palette};

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let pal = Palette::of(&ctx);

    if app.slideshow.is_some() {
        egui::CentralPanel::no_frame().show(ui, |ui| crate::canvas::slideshow(app, ui));
        return;
    }

    if !app.fullscreen {
        egui::Panel::top("menubar")
            .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin::symmetric(6, 2)))
            .show_separator_line(false)
            .show(ui, |ui| menubar(app, ui));
    }
    if app.show_toolbar {
        egui::Panel::top("toolbar")
            .frame(Frame::new().fill(pal.toolbar).inner_margin(Margin { left: 8, right: 10, top: 5, bottom: 6 }).stroke(Stroke::NONE))
            .show_separator_line(true)
            .show(ui, |ui| toolbar(app, ui));
    }
    let ready = app.doc().map(|d| d.ready() && d.kind != Kind::Model).unwrap_or(false);
    if app.show_markup && ready {
        egui::Panel::top("markup")
            .frame(Frame::new().fill(pal.sidebar).inner_margin(Margin::symmetric(10, 4)))
            .show(ui, |ui| markup_bar(app, ui));
    }
    if app.settings.show_sidebar && !app.docs.is_empty() {
        let r = egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(app.settings.sidebar_width)
            .size_range(130.0..=460.0)
            .frame(Frame::new().fill(pal.sidebar).inner_margin(Margin::ZERO))
            .show(ui, |ui| crate::sidebar::show(app, ui));
        app.settings.sidebar_width = r.response.rect.width();
    }
    if app.show_inspector && !app.docs.is_empty() {
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(290.0)
            .size_range(230.0..=480.0)
            .frame(Frame::new().fill(pal.sidebar).inner_margin(Margin::same(14)))
            .show(ui, |ui| crate::inspector::show(app, ui));
    }
    let bg = match app.settings.background {
        crate::settings::Background::System => pal.canvas,
        crate::settings::Background::Light => Color32::from_rgb(0xE9, 0xE9, 0xEB),
        crate::settings::Background::Dark => Color32::from_rgb(0x28, 0x28, 0x2B),
        crate::settings::Background::Black => Color32::BLACK,
    };
    egui::CentralPanel::default().frame(Frame::new().fill(bg)).show(ui, |ui| crate::canvas::show(app, ui));
}

// ---------------------------------------------------------------------------
// Menu bar (structure recovered from Preview's MainMenu.nib)

fn item(app: &mut App, ui: &mut Ui, label: &str, a: Action) {
    let sc = shortcut_text(a);
    let btn = egui::Button::new(label).shortcut_text(RichText::new(sc).color(ui.visuals().weak_text_color()));
    if ui.add_enabled(app.enabled(a), btn).clicked() {
        app.run(a);
        ui.close();
    }
}

fn check(app: &mut App, ui: &mut Ui, label: &str, on: bool, a: Action) {
    let sc = shortcut_text(a);
    let text = format!("{}{label}", if on { "✓  " } else { "     " });
    let btn = egui::Button::new(text).shortcut_text(RichText::new(sc).color(ui.visuals().weak_text_color()));
    if ui.add_enabled(app.enabled(a), btn).clicked() {
        app.run(a);
        ui.close();
    }
}

fn menubar(app: &mut App, ui: &mut Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            ui.set_min_width(240.0);
            item(app, ui, "New from Clipboard", Action::NewFromClipboard);
            item(app, ui, "Open…", Action::Open);
            ui.menu_button("Open Recent", |ui| {
                let recent: Vec<_> = app.settings.recent.clone();
                if recent.is_empty() {
                    ui.add_enabled(false, egui::Button::new("No Recent Files"));
                }
                for p in recent {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    if ui.add_enabled(p.exists(), egui::Button::new(name)).on_hover_text(p.to_string_lossy()).clicked() {
                        app.open_paths(vec![p]);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Clear Menu").clicked() {
                    app.settings.recent.clear();
                    app.settings.save();
                    ui.close();
                }
            });
            ui.separator();
            item(app, ui, "Close Window", Action::Close);
            item(app, ui, "Save", Action::Save);
            item(app, ui, "Duplicate", Action::Duplicate);
            item(app, ui, "Rename…", Action::Rename);
            item(app, ui, "Revert to Saved", Action::Revert);
            ui.separator();
            item(app, ui, "Edit Permissions…", Action::EditPermissions);
            ui.separator();
            item(app, ui, "Import from Scanner…", Action::ImportScanner);
            ui.menu_button("Take Screenshot", |ui| {
                item(app, ui, "From Selection…", Action::Screenshot(0));
                item(app, ui, "From Window…", Action::Screenshot(1));
                item(app, ui, "From Entire Screen", Action::Screenshot(2));
            });
            ui.separator();
            item(app, ui, "Export As…", Action::ExportAs);
            item(app, ui, "Export as PDF…", Action::ExportPdf);
            item(app, ui, "Show in Folder", Action::ShowInFolder);
            item(app, ui, "Move to Trash", Action::MoveToTrash);
            ui.separator();
            item(app, ui, "Print…", Action::Print);
            ui.separator();
            item(app, ui, "Quit", Action::Quit);
        });
        ui.menu_button("Edit", |ui| {
            ui.set_min_width(240.0);
            let undo = app.doc().and_then(|d| d.undo_name()).map(|n| format!("Undo {n}")).unwrap_or("Undo".into());
            let redo = app.doc().and_then(|d| d.redo_name()).map(|n| format!("Redo {n}")).unwrap_or("Redo".into());
            item(app, ui, &undo, Action::Undo);
            item(app, ui, &redo, Action::Redo);
            ui.separator();
            item(app, ui, "Cut", Action::Cut);
            item(app, ui, "Copy", Action::Copy);
            item(app, ui, "Paste", Action::Paste);
            item(app, ui, "Delete", Action::Delete);
            item(app, ui, "Select All", Action::SelectAll);
            item(app, ui, "Invert Selection", Action::InvertSelection);
            ui.separator();
            ui.menu_button("Insert", |ui| {
                item(app, ui, "Page from Scanner…", Action::InsertFromScanner);
                item(app, ui, "Page from File…", Action::InsertFromFile);
                item(app, ui, "Blank Page", Action::InsertBlank);
            });
            ui.separator();
            ui.menu_button("Find", |ui| {
                item(app, ui, "Find…", Action::Find);
                item(app, ui, "Find Next", Action::FindNext);
                item(app, ui, "Find Previous", Action::FindPrev);
                item(app, ui, "Use Selection for Find", Action::UseSelForFind);
                item(app, ui, "Jump to Selection", Action::JumpToSel);
            });
            ui.menu_button("Speech", |ui| {
                item(app, ui, "Start Speaking", Action::Speak);
                item(app, ui, "Stop Speaking", Action::StopSpeaking);
            });
        });
        ui.menu_button("View", |ui| {
            ui.set_min_width(250.0);
            view_menu_items(app, ui);
            ui.separator();
            let mode = app.doc().map(|d| d.mode).unwrap_or(PdfViewMode::Continuous);
            check(app, ui, "Continuous Scroll", mode == PdfViewMode::Continuous, Action::ViewMode(PdfViewMode::Continuous));
            check(app, ui, "Single Page", mode == PdfViewMode::Single, Action::ViewMode(PdfViewMode::Single));
            check(app, ui, "Two Pages", mode == PdfViewMode::Two, Action::ViewMode(PdfViewMode::Two));
            ui.separator();
            ui.menu_button("Soft Proof with Profile", |ui| {
                ui.set_min_width(260.0);
                let none = app.proof.is_none();
                if ui.button(format!("{}None", if none { "✓  " } else { "     " })).clicked() {
                    app.set_proof(None);
                    ui.close();
                }
                ui.separator();
                let current = app.proof.as_ref().map(|p| p.name.clone());
                if let Some(p) = profile_list(ui, false, current.as_deref()) {
                    app.set_proof(Some(&p));
                }
            });
            check(app, ui, "Use Dark Appearance for PDF", app.settings.dark_pdf, Action::DarkPdf);
            check(app, ui, "Highlight Form Fields", app.highlight_fields, Action::HighlightFields);
            ui.separator();
            item(app, ui, "Actual Size", Action::ActualSize);
            item(app, ui, "Zoom to Fit", Action::ZoomFit);
            item(app, ui, "Zoom In", Action::ZoomIn);
            item(app, ui, "Zoom Out", Action::ZoomOut);
            item(app, ui, "Zoom to Selection", Action::ZoomSel);
            ui.separator();
            check(app, ui, "Show Markup Toolbar", app.show_markup, Action::ToggleMarkup);
            check(app, ui, "Show Toolbar", app.show_toolbar, Action::ToggleToolbar);
            ui.separator();
            item(app, ui, "Slideshow", Action::Slideshow);
            check(app, ui, "Full Screen", app.fullscreen, Action::Fullscreen);
        });
        ui.menu_button("Go", |ui| {
            ui.set_min_width(230.0);
            item(app, ui, "Previous Page", Action::PrevPage);
            item(app, ui, "Next Page", Action::NextPage);
            item(app, ui, "First Page", Action::FirstPage);
            item(app, ui, "Last Page", Action::LastPage);
            ui.separator();
            item(app, ui, "Previous Document", Action::PrevDoc);
            item(app, ui, "Next Document", Action::NextDoc);
            item(app, ui, "Go to Page…", Action::GoToPage);
            ui.separator();
            item(app, ui, "Back", Action::Back);
            item(app, ui, "Forward", Action::Forward);
        });
        ui.menu_button("Tools", |ui| {
            ui.set_min_width(250.0);
            ui.menu_button("Annotate", |ui| {
                ui.set_min_width(230.0);
                item(app, ui, "Highlight Text", Action::Tool(Tool::Markup(MarkupKind::Highlight)));
                item(app, ui, "Underline Text", Action::Tool(Tool::Markup(MarkupKind::Underline)));
                item(app, ui, "Strike Through Text", Action::Tool(Tool::Markup(MarkupKind::Strike)));
                ui.separator();
                item(app, ui, "Rectangle", Action::Tool(Tool::Shape(ShapeTool::Rect)));
                item(app, ui, "Rounded Rectangle", Action::Tool(Tool::Shape(ShapeTool::RoundRect)));
                item(app, ui, "Oval", Action::Tool(Tool::Shape(ShapeTool::Oval)));
                item(app, ui, "Line", Action::Tool(Tool::Shape(ShapeTool::Line)));
                item(app, ui, "Arrow", Action::Tool(Tool::Shape(ShapeTool::Arrow)));
                item(app, ui, "Polygon", Action::Tool(Tool::Polygon));
                item(app, ui, "Star", Action::Tool(Tool::Shape(ShapeTool::Star)));
                ui.separator();
                item(app, ui, "Text", Action::Tool(Tool::TextBox));
                item(app, ui, "Speech Bubble", Action::Tool(Tool::Shape(ShapeTool::Bubble)));
                ui.separator();
                item(app, ui, "Note", Action::Tool(Tool::Note));
                ui.menu_button("Signature", |ui| signature_menu(app, ui));
            });
            item(app, ui, "Show Inspector", Action::Inspector);
            check(app, ui, "Show Magnifier", app.magnifier, Action::Magnifier);
            ui.separator();
            item(app, ui, "Adjust Color…", Action::AdjustColor);
            item(app, ui, "Adjust Size…", Action::AdjustSize);
            ui.separator();
            check(app, ui, "Text Selection", app.tool == Tool::Text, Action::Tool(Tool::Text));
            check(app, ui, "Rectangular Selection", app.tool == Tool::Rect, Action::Tool(Tool::Rect));
            check(app, ui, "Instant Alpha", app.tool == Tool::InstantAlpha, Action::Tool(Tool::InstantAlpha));
            check(app, ui, "Redact", app.tool == Tool::Redact, Action::Tool(Tool::Redact));
            ui.separator();
            item(app, ui, "Add Bookmark", Action::AddBookmark);
            ui.separator();
            item(app, ui, "Rotate Left", Action::RotateLeft);
            item(app, ui, "Rotate Right", Action::RotateRight);
            item(app, ui, "Flip Horizontal", Action::FlipH);
            item(app, ui, "Flip Vertical", Action::FlipV);
            item(app, ui, "Crop", Action::Crop);
            item(app, ui, "Remove Background", Action::RemoveBackground);
            ui.separator();
            let is_img = app.doc().map(|d| d.kind == Kind::Image && d.ready()).unwrap_or(false);
            ui.add_enabled_ui(is_img, |ui| {
                ui.menu_button("Assign Profile", |ui| {
                    ui.set_min_width(260.0);
                    if let Some(p) = profile_list(ui, true, None) {
                        app.assign_profile(&p);
                    }
                });
            });
            item(app, ui, "Show Location Info", Action::ShowLocation);
        });
        ui.menu_button("Window", |ui| {
            ui.set_min_width(220.0);
            item(app, ui, "Preferences…", Action::Preferences);
            ui.separator();
            let names: Vec<(usize, String, bool)> = app.docs.iter().enumerate().map(|(i, d)| (i, d.name.clone(), d.dirty())).collect();
            for (i, name, dirty) in names {
                let label = format!("{}{name}{}", if i == app.active { "✓  " } else { "     " }, if dirty { " •" } else { "" });
                if ui.button(label).clicked() {
                    app.activate(i);
                    ui.close();
                }
            }
        });
        ui.menu_button("Help", |ui| {
            item(app, ui, "About Review", Action::About);
            if ui.button("Keyboard Shortcuts").clicked() {
                app.dialog = Some(crate::dialogs::Dialog::Shortcuts);
                ui.close();
            }
        });
    });
}

/// Menu entries for the ICC profiles installed on the system (RGB first, then CMYK).
/// Returns the profile the user picked.
fn profile_list(ui: &mut Ui, rgb_only: bool, current: Option<&str>) -> Option<crate::color::IccProfile> {
    let profiles = crate::color::system_profiles();
    if profiles.is_empty() {
        ui.add_enabled(false, egui::Button::new("No ICC profiles found (install colord)"));
        return None;
    }
    let mut picked = None;
    let mut last_cmyk = false;
    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        for p in profiles {
            if rgb_only && p.cmyk {
                continue;
            }
            if p.cmyk && !last_cmyk {
                ui.separator();
                ui.label(RichText::new("CMYK / print").small().color(ui.visuals().weak_text_color()));
            }
            last_cmyk = p.cmyk;
            let on = current == Some(p.name.as_str());
            if ui.button(format!("{}{}", if on { "✓  " } else { "     " }, p.name)).clicked() {
                picked = Some(p.clone());
                ui.close();
            }
        }
    });
    picked
}

fn view_menu_items(app: &mut App, ui: &mut Ui) {
    let shown = app.settings.show_sidebar;
    check(app, ui, "Hide Sidebar", !shown, Action::HideSidebar);
    check(app, ui, "Thumbnails", shown && app.sidebar == SidebarMode::Thumbnails && !app.contact_sheet, Action::Sidebar(SidebarMode::Thumbnails));
    check(app, ui, "Table of Contents", shown && app.sidebar == SidebarMode::Contents, Action::Sidebar(SidebarMode::Contents));
    check(app, ui, "Highlights and Notes", shown && app.sidebar == SidebarMode::Notes, Action::Sidebar(SidebarMode::Notes));
    check(app, ui, "Bookmarks", shown && app.sidebar == SidebarMode::Bookmarks, Action::Sidebar(SidebarMode::Bookmarks));
    check(app, ui, "Contact Sheet", app.contact_sheet, Action::ContactSheet);
}

fn signature_menu(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let sigs = app.settings.signatures.clone();
    let mut remove = None;
    for (i, s) in sigs.iter().enumerate() {
        ui.horizontal(|ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(170.0, 170.0 * s.aspect.clamp(0.2, 0.6)), Sense::click());
            let fill = if resp.hovered() { pal.hover } else { Color32::TRANSPARENT };
            ui.painter().rect_filled(r, 6.0, fill);
            let inner = r.shrink(8.0);
            for st in &s.strokes {
                let pts: Vec<egui::Pos2> = st.iter().map(|p| inner.min + vec2(p[0], p[1] / s.aspect.max(0.01) * inner.height() / inner.width()) * inner.width()).collect();
                ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6, pal.text)));
            }
            if resp.clicked() {
                app.set_tool(Tool::Signature(i));
                ui.close();
            }
            if ui.small_button(ph::TRASH).on_hover_text("Delete signature").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        app.settings.signatures.remove(i);
        app.settings.save();
    }
    if !sigs.is_empty() {
        ui.separator();
    }
    if ui.button(format!("{}  Create Signature…", ph::PLUS)).clicked() {
        app.run(Action::NewSignature);
        ui.close();
    }
}

// ---------------------------------------------------------------------------
// Toolbar

/// A flat icon button in the toolbar style.
pub fn tool_button(ui: &mut Ui, icon: &str, tip: &str, selected: bool, enabled: bool) -> Response {
    let pal = Palette::of(ui.ctx());
    let size = vec2(32.0, 28.0);
    let (r, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let fill = if selected {
        pal.selected
    } else if resp.is_pointer_button_down_on() && enabled {
        pal.hover.gamma_multiply(2.2)
    } else if resp.hovered() && enabled {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, 7.0, fill);
    let color = if !enabled {
        pal.text_weak.gamma_multiply(0.5)
    } else if selected {
        pal.accent
    } else {
        pal.text.gamma_multiply(0.85)
    };
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, icon, FontId::proportional(17.0), color);
    resp.on_hover_text(tip)
}

fn toolbar(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // Sidebar / view menu
        let sb = tool_button(ui, ph::SIDEBAR_SIMPLE, "Show or hide the sidebar", app.settings.show_sidebar, true);
        if sb.clicked() {
            app.run(Action::HideSidebar);
        }
        let menu = egui::containers::menu::MenuButton::new(RichText::new(ph::CARET_DOWN).size(11.0).color(pal.text_weak)).config(egui::containers::menu::MenuConfig::default());
        menu.ui(ui, |ui| {
            ui.set_min_width(220.0);
            view_menu_items(app, ui);
            ui.separator();
            let mode = app.doc().map(|d| d.mode).unwrap_or(PdfViewMode::Continuous);
            check(app, ui, "Continuous Scroll", mode == PdfViewMode::Continuous, Action::ViewMode(PdfViewMode::Continuous));
            check(app, ui, "Single Page", mode == PdfViewMode::Single, Action::ViewMode(PdfViewMode::Single));
            check(app, ui, "Two Pages", mode == PdfViewMode::Two, Action::ViewMode(PdfViewMode::Two));
        });
        ui.add_space(8.0);

        // Title block
        let (name, sub, dirty, id) = match app.doc() {
            Some(d) => {
                let mut sub = d.subtitle(app.settings.logical_page_numbers);
                if let Some(p) = &app.proof {
                    sub = format!("{sub} · Proof: {}", p.name);
                }
                (d.name.clone(), sub, d.dirty(), d.id)
            }
            None => ("Review".to_string(), String::new(), false, 0),
        };
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_max_width(ui.available_width() * 0.42);
            if let Some((rid, mut text)) = app.renaming.clone().filter(|(rid, _)| *rid == id) {
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(220.0).font(FontId::new(13.5, bold())));
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if r.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        rename(app, rid, text);
                    }
                    app.renaming = None;
                } else {
                    app.renaming = Some((rid, text));
                }
            } else {
                let title = RichText::new(format!("{name}{}", if dirty { " — Edited" } else { "" })).font(FontId::new(13.5, bold())).color(pal.text);
                let r = ui.add(egui::Label::new(title).truncate().sense(Sense::click()));
                if r.double_clicked() && id != 0 {
                    app.run(Action::Rename);
                }
                r.on_hover_text("Double-click to rename");
            }
            if !sub.is_empty() {
                ui.add(egui::Label::new(RichText::new(sub).size(11.0).color(pal.text_weak)).truncate());
            }
        });

        // Right side, laid out right-to-left.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            search_field(app, ui);
            ui.add_space(6.0);
            let ready = app.doc().map(|d| d.ready()).unwrap_or(false);
            let is_pdf = app.doc().map(|d| d.is_pdf()).unwrap_or(false);
            let is_img = app.doc().map(|d| d.kind == Kind::Image && d.ready()).unwrap_or(false);
            if tool_button(ui, ph::INFO, "Show Inspector (Ctrl+I)", app.show_inspector, ready).clicked() {
                app.run(Action::Inspector);
            }
            if tool_button(ui, ph::EXPORT, "Export As…", false, ready).clicked() {
                app.run(Action::ExportAs);
            }
            if is_pdf && app.doc().and_then(|d| d.info.as_ref()).map(|i| !i.fields.is_empty()).unwrap_or(false) {
                if tool_button(ui, ph::TEXTBOX, "Highlight form fields", app.highlight_fields, true).clicked() {
                    app.run(Action::HighlightFields);
                }
            }
            if is_img && tool_button(ui, ph::SPARKLE, "Remove Background (Ctrl+Shift+K)", false, true).clicked() {
                app.run(Action::RemoveBackground);
            }
            if is_pdf {
                egui::containers::menu::MenuButton::new(RichText::new(ph::CARET_DOWN).size(10.0).color(pal.text_weak)).ui(ui, |ui| {
                    ui.horizontal(|ui| {
                        for c in [
                            Color32::from_rgb(0xFF, 0xD6, 0x0A),
                            Color32::from_rgb(0x34, 0xC7, 0x59),
                            Color32::from_rgb(0x0A, 0x84, 0xFF),
                            Color32::from_rgb(0xFF, 0x37, 0x8C),
                            Color32::from_rgb(0xBF, 0x5A, 0xF2),
                        ] {
                            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                            ui.painter().circle_filled(r.center(), 9.0, c);
                            if app.markup_color == c {
                                ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, pal.text));
                            }
                            if resp.clicked() {
                                app.markup_color = c;
                                app.set_tool(Tool::Markup(MarkupKind::Highlight));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    for (label, k) in [("Highlight", MarkupKind::Highlight), ("Underline", MarkupKind::Underline), ("Strikethrough", MarkupKind::Strike)] {
                        if ui.selectable_label(app.tool == Tool::Markup(k), label).clicked() {
                            app.tool = Tool::Text;
                            app.set_tool(Tool::Markup(k));
                            ui.close();
                        }
                    }
                });
                // Highlight button with colour/style dropdown, like Preview.
                let hl_on = matches!(app.tool, Tool::Markup(_));
                let resp = tool_button(ui, ph::HIGHLIGHTER, "Highlight (Alt+Shift+H)", hl_on, true);
                if resp.clicked() {
                    let kind = if let Tool::Markup(k) = app.tool { k } else { MarkupKind::Highlight };
                    app.set_tool(Tool::Markup(kind));
                }
            }
            let editable = ready && app.doc().map(|d| d.kind != Kind::Model).unwrap_or(false);
            if tool_button(ui, ph::ARROW_COUNTER_CLOCKWISE, "Rotate Left (Ctrl+L)", false, editable).clicked() {
                app.run(Action::RotateLeft);
            }
            if tool_button(ui, ph::PEN_NIB, "Show Markup Toolbar (Ctrl+Shift+A)", app.show_markup, editable).clicked() {
                app.run(Action::ToggleMarkup);
            }
            ui.add_space(6.0);
            // Zoom segment
            if !ready {
                return;
            }
            let zoom = app.doc().map(|d| d.zoom).unwrap_or(1.0);
            if tool_button(ui, ph::MAGNIFYING_GLASS_PLUS, "Zoom In (Ctrl++)", false, ready).clicked() {
                app.run(Action::ZoomIn);
            }
            let zl = ui.add(egui::Label::new(RichText::new(format!("{:.0}%", zoom * 100.0)).size(11.5).color(pal.text_weak)).sense(Sense::click()));
            if zl.on_hover_text("Click for Actual Size, double-click to fit").clicked() {
                app.run(Action::ActualSize);
            }
            if tool_button(ui, ph::MAGNIFYING_GLASS_MINUS, "Zoom Out (Ctrl+−)", false, ready).clicked() {
                app.run(Action::ZoomOut);
            }
        });
    });
}

fn rename(app: &mut App, id: u64, new_name: String) {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() || new_name.contains('/') {
        return;
    }
    let Some(i) = app.doc_index(id) else { return };
    match app.docs[i].path.clone() {
        Some(p) => {
            let dest = p.with_file_name(&new_name);
            if dest == p {
                return;
            }
            if dest.exists() {
                app.error(format!("“{new_name}” already exists."));
                return;
            }
            match std::fs::rename(&p, &dest) {
                Ok(()) => {
                    // The PDF worker keeps its open file handle, so nothing else needs to change.
                    if let Some(s) = app.docs[i].source.and_then(|s| app.sources.get_mut(&s)) {
                        s.path = dest.clone();
                    }
                    app.docs[i].path = Some(dest);
                    app.docs[i].name = new_name;
                    app.update_title();
                }
                Err(e) => app.error(format!("Couldn’t rename: {e}")),
            }
        }
        None => app.docs[i].name = new_name,
    }
}

fn search_field(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let enabled = app.doc().map(|d| d.is_pdf()).unwrap_or(false);
    let (r, _) = ui.allocate_exact_size(vec2(190.0, 28.0), Sense::hover());
    ui.painter().rect(r, 8.0, if pal.dark { Color32::from_rgb(0x3A, 0x3A, 0x3E) } else { Color32::from_rgb(0xEC, 0xEC, 0xF0) }, Stroke::NONE, StrokeKind::Inside);
    ui.painter().text(r.left_center() + vec2(14.0, 0.0), egui::Align2::CENTER_CENTER, ph::MAGNIFYING_GLASS, FontId::proportional(14.0), pal.text_weak);
    let inner = egui::Rect::from_min_max(r.min + vec2(26.0, 0.0), r.max - vec2(if app.search.query.is_empty() { 6.0 } else { 26.0 }, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let ui = &mut child;
    let mut q = app.search.query.clone();
    let te = egui::TextEdit::singleline(&mut q).frame(egui::Frame::NONE).background_color(Color32::TRANSPARENT).hint_text("Search").vertical_align(egui::Align::Center).margin(Margin::symmetric(2, 6));
    let resp = ui.add_enabled_ui(enabled, |ui| ui.put(inner, te)).inner;
    if app.search.focus {
        resp.request_focus();
        app.search.focus = false;
    }
    if resp.changed() {
        app.search.query = q;
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        if ui.input(|i| i.modifiers.shift) {
            app.step_hit(-1);
        } else if app.search.generation > 0 && !app.search.hits.is_empty() && app.search.doc == app.doc().map(|d| d.id).unwrap_or(0) && app.ctx.data(|d| d.get_temp::<String>(egui::Id::new("last-q"))) == Some(app.search.query.clone()) {
            app.step_hit(1);
        } else {
            app.ctx.data_mut(|d| d.insert_temp(egui::Id::new("last-q"), app.search.query.clone()));
            app.start_search();
        }
        resp.request_focus();
    }
    if !app.search.query.is_empty() {
        let xr = egui::Rect::from_center_size(r.right_center() - vec2(14.0, 0.0), vec2(20.0, 20.0));
        let x = ui.put(xr, egui::Button::new(RichText::new(ph::X_CIRCLE).size(14.0).color(pal.text_weak)).frame(false));
        if x.clicked() {
            app.search.query.clear();
            app.start_search();
        }
    }
}

// ---------------------------------------------------------------------------
// Markup toolbar

const PALETTE: [Color32; 12] = [
    Color32::from_rgb(0x1D, 0x1D, 0x1F),
    Color32::from_rgb(0x8E, 0x8E, 0x93),
    Color32::WHITE,
    Color32::from_rgb(0xFF, 0x3B, 0x30),
    Color32::from_rgb(0xFF, 0x95, 0x00),
    Color32::from_rgb(0xFF, 0xCC, 0x00),
    Color32::from_rgb(0x34, 0xC7, 0x59),
    Color32::from_rgb(0x00, 0xC7, 0xBE),
    Color32::from_rgb(0x0A, 0x84, 0xFF),
    Color32::from_rgb(0x5E, 0x5C, 0xE6),
    Color32::from_rgb(0xBF, 0x5A, 0xF2),
    Color32::from_rgb(0xFF, 0x2D, 0x55),
];

/// Colour swatch grid; returns the picked colour (`None` inside means "no fill").
fn color_grid(ui: &mut Ui, current: Option<Color32>, allow_none: bool) -> Option<Option<Color32>> {
    let pal = Palette::of(ui.ctx());
    let mut picked = None;
    egui::Grid::new(ui.next_auto_id()).spacing(vec2(6.0, 6.0)).show(ui, |ui| {
        for (i, c) in PALETTE.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
            ui.painter().circle_filled(r.center(), 9.5, *c);
            ui.painter().circle_stroke(r.center(), 9.5, Stroke::new(1.0, Color32::from_black_alpha(40)));
            if current == Some(*c) {
                ui.painter().circle_stroke(r.center(), 11.5, Stroke::new(1.5, pal.accent));
            }
            if resp.clicked() {
                picked = Some(Some(*c));
            }
            if i % 6 == 5 {
                ui.end_row();
            }
        }
    });
    ui.horizontal(|ui| {
        if allow_none && ui.selectable_label(current.is_none(), "No Fill").clicked() {
            picked = Some(None);
        }
        let mut c = current.unwrap_or(Color32::BLACK);
        if ui.color_edit_button_srgba(&mut c).changed() {
            picked = Some(Some(c));
        }
        ui.label(RichText::new("Custom").small());
    });
    picked
}

fn markup_bar(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let is_img = app.doc().map(|d| d.kind == Kind::Image).unwrap_or(false);
    let is_pdf = app.doc().map(|d| d.is_pdf()).unwrap_or(false);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let t = app.tool;
        if tool_button(ui, ph::CURSOR_TEXT, "Text Selection", t == Tool::Text, true).clicked() {
            app.tool = Tool::Text;
        }
        if tool_button(ui, ph::SELECTION, "Rectangular Selection", t == Tool::Rect, true).clicked() {
            app.set_tool(Tool::Rect);
        }
        if is_img && tool_button(ui, ph::MAGIC_WAND, "Instant Alpha — drag on a colour to select it, then press Delete", t == Tool::InstantAlpha, true).clicked() {
            app.set_tool(Tool::InstantAlpha);
        }
        sep(ui, &pal);
        if tool_button(ui, ph::SCRIBBLE, "Sketch", t == Tool::Sketch, true).clicked() {
            app.set_tool(Tool::Sketch);
        }
        // Shapes dropdown
        let shape_icon = |s: ShapeTool| match s {
            ShapeTool::Line => ph::LINE_SEGMENT,
            ShapeTool::Arrow => ph::ARROW_UP_RIGHT,
            ShapeTool::Rect => ph::SQUARE,
            ShapeTool::RoundRect => ph::RECTANGLE,
            ShapeTool::Oval => ph::CIRCLE,
            ShapeTool::Star => ph::STAR,
            ShapeTool::Bubble => ph::CHAT_CIRCLE,
        };
        let shape_on = matches!(t, Tool::Shape(_) | Tool::Polygon);
        let current_icon = match t {
            Tool::Shape(s) => shape_icon(s),
            Tool::Polygon => ph::POLYGON,
            _ => ph::SHAPES,
        };
        let sresp = tool_button(ui, current_icon, "Shapes", shape_on, true);
        egui::Popup::menu(&sresp).show(|ui| {
            ui.horizontal(|ui| {
                for s in [ShapeTool::Line, ShapeTool::Arrow, ShapeTool::Rect, ShapeTool::RoundRect, ShapeTool::Oval, ShapeTool::Bubble, ShapeTool::Star] {
                    if tool_button(ui, shape_icon(s), &format!("{s:?}"), t == Tool::Shape(s), true).clicked() {
                        app.tool = Tool::Text;
                        app.set_tool(Tool::Shape(s));
                        ui.close();
                    }
                }
                if tool_button(ui, ph::POLYGON, "Polygon (click points, double-click to finish)", t == Tool::Polygon, true).clicked() {
                    app.tool = Tool::Text;
                    app.set_tool(Tool::Polygon);
                    ui.close();
                }
            });
        });
        if tool_button(ui, ph::TEXT_T, "Text (Alt+Shift+T)", t == Tool::TextBox, true).clicked() {
            app.set_tool(Tool::TextBox);
        }
        let sig = tool_button(ui, ph::SIGNATURE, "Sign", matches!(t, Tool::Signature(_)), true);
        egui::Popup::menu(&sig).show(|ui| signature_menu(app, ui));
        if tool_button(ui, ph::NOTE, "Note (Alt+Shift+N)", t == Tool::Note, is_pdf).clicked() {
            app.set_tool(Tool::Note);
        }
        if is_pdf && tool_button(ui, ph::PROHIBIT, "Redact — removes the content underneath when saved", t == Tool::Redact, true).clicked() {
            app.set_tool(Tool::Redact);
        }
        sep(ui, &pal);
        if tool_button(ui, ph::ARROW_COUNTER_CLOCKWISE, "Rotate Left", false, true).clicked() {
            app.run(Action::RotateLeft);
        }
        if tool_button(ui, ph::ARROW_CLOCKWISE, "Rotate Right", false, true).clicked() {
            app.run(Action::RotateRight);
        }
        if tool_button(ui, ph::CROP, "Crop to selection (Ctrl+K)", false, app.enabled(Action::Crop)).clicked() {
            app.run(Action::Crop);
        }
        if is_img {
            if tool_button(ui, ph::SLIDERS_HORIZONTAL, "Adjust Color…", false, true).clicked() {
                app.run(Action::AdjustColor);
            }
            if tool_button(ui, ph::RESIZE, "Adjust Size…", false, true).clicked() {
                app.run(Action::AdjustSize);
            }
        }
        sep(ui, &pal);
        style_controls(app, ui);
    });
}

fn sep(ui: &mut Ui, pal: &Palette) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, pal.separator);
    ui.add_space(4.0);
}

/// Line width, border colour, fill colour and text style — acting on the selected
/// annotation if there is one, otherwise on the defaults for new shapes.
fn style_controls(app: &mut App, ui: &mut Ui) {
    let pal = Palette::of(ui.ctx());
    let selected = app.sel.annot;
    let sel_annot = selected.and_then(|(d, s, id)| {
        app.docs.iter().find(|x| x.id == d).and_then(|doc| doc.model.slots.get(s)).and_then(|sl| sl.annots.iter().find(|a| a.id == id)).cloned()
    });
    let is_text = matches!(sel_annot.as_ref().map(|a| &a.shape), Some(Shape::Text { .. })) || (sel_annot.is_none() && app.tool == Tool::TextBox);
    let mut style = match &sel_annot {
        Some(a) => a.style.clone(),
        None if is_text => app.text_style.clone(),
        None => app.shape_style.clone(),
    };
    let before = style.clone();

    // Line width
    let lw = tool_button(ui, ph::LINE_SEGMENTS, "Shape Style", false, true);
    egui::Popup::menu(&lw).show(|ui| {
        ui.set_min_width(160.0);
        for w in [0.5f32, 1.0, 2.0, 3.0, 5.0, 8.0, 12.0] {
            let (r, resp) = ui.allocate_exact_size(vec2(150.0, 22.0), Sense::click());
            if resp.hovered() || (style.width - w).abs() < 0.01 {
                ui.painter().rect_filled(r, 4.0, pal.hover);
            }
            ui.painter().line_segment([r.left_center() + vec2(10.0, 0.0), r.right_center() - vec2(40.0, 0.0)], Stroke::new(w.min(10.0), pal.text));
            ui.painter().text(r.right_center() - vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, format!("{w}"), FontId::proportional(11.0), pal.text_weak);
            if resp.clicked() {
                style.width = w;
                ui.close();
            }
        }
    });
    // Border colour
    let br = swatch_button(ui, Some(style.stroke), "Border Color", false);
    egui::Popup::menu(&br).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        if let Some(Some(c)) = color_grid(ui, Some(style.stroke), false) {
            style.stroke = c;
        }
    });
    // Fill colour
    let fr = swatch_button(ui, style.fill, "Fill Color", true);
    egui::Popup::menu(&fr).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        if let Some(c) = color_grid(ui, style.fill, true) {
            style.fill = c;
        }
    });
    // Text style
    let ts = tool_button(ui, ph::TEXT_AA, "Text Style", false, true);
    egui::Popup::menu(&ts).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(220.0);
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut style.font_size).range(4.0..=144.0).speed(0.3).suffix(" pt"));
            ui.color_edit_button_srgba(&mut style.text_color);
        });
        ui.horizontal(|ui| {
            for (icon, a) in [(ph::TEXT_ALIGN_LEFT, Align::Left), (ph::TEXT_ALIGN_CENTER, Align::Center), (ph::TEXT_ALIGN_RIGHT, Align::Right)] {
                if tool_button(ui, icon, &format!("{a:?}"), style.align == a, true).clicked() {
                    style.align = a;
                }
            }
        });
    });

    if style != before {
        match selected {
            Some((d, s, id)) => {
                if let Some(i) = app.doc_index(d) {
                    let doc = &mut app.docs[i];
                    doc.checkpoint("Change of Annotation Properties");
                    if let Some(a) = doc.model.slots.get_mut(s).and_then(|sl| sl.annots.iter_mut().find(|a| a.id == id)) {
                        a.style = style.clone();
                        a.dirty = true;
                    }
                }
                if is_text {
                    app.text_style = crate::model::Style { width: 0.0, ..style.clone() };
                } else {
                    app.shape_style = style;
                }
            }
            None if is_text => app.text_style = style,
            None => app.shape_style = style,
        }
    }
}

fn swatch_button(ui: &mut Ui, c: Option<Color32>, tip: &str, fill: bool) -> Response {
    let pal = Palette::of(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 7.0, pal.hover);
    }
    let sw = egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0));
    match c {
        Some(c) if fill => {
            ui.painter().rect(sw, 4.0, c, Stroke::new(1.0, Color32::from_black_alpha(60)), StrokeKind::Inside);
        }
        Some(c) => {
            ui.painter().rect_stroke(sw.shrink(1.5), 4.0, Stroke::new(3.0, c), StrokeKind::Inside);
        }
        None => {
            ui.painter().rect_stroke(sw, 4.0, Stroke::new(1.0, pal.text_weak), StrokeKind::Inside);
            ui.painter().line_segment([sw.left_bottom(), sw.right_top()], Stroke::new(1.2, Color32::from_rgb(0xFF, 0x3B, 0x30)));
        }
    }
    resp.on_hover_text(tip)
}

// ---------------------------------------------------------------------------

pub fn toasts(app: &mut App, ctx: &egui::Context) {
    let now = Instant::now();
    let screen = ctx.content_rect();
    let mut y = screen.max.y - 24.0;
    for t in app.toasts.iter().rev() {
        let left = (t.until - now).as_secs_f32().max(0.0);
        let a = (left / 0.25).min(1.0);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new(("toast", t.text.as_str()))));
        let color = if t.error { Color32::from_rgb(0xD7, 0x3A, 0x30) } else { Color32::from_rgb(0x2B, 0x2B, 0x2E) };
        let galley = painter.layout(t.text.clone(), FontId::proportional(13.0), Color32::WHITE.gamma_multiply(a), 420.0);
        let size = galley.size() + vec2(28.0, 14.0);
        let r = egui::Rect::from_center_size(egui::pos2(screen.center().x, y - size.y / 2.0), size);
        painter.add(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha((60.0 * a) as u8) }.as_shape(r, CornerRadius::same(10)));
        painter.rect_filled(r, 10.0, color.gamma_multiply(a.max(0.0)));
        painter.galley(r.min + vec2(14.0, 7.0), galley, Color32::WHITE);
        y -= size.y + 8.0;
    }
}

