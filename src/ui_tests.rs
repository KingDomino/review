//! Headless UI tests: drive the real app with egui_kittest and save screenshots.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_kittest::Harness;

use crate::app::{Action, App};

pub fn shots_dir() -> PathBuf {
    let d = std::env::var_os("REVIEW_SHOTS").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("review-shots"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn harness(files: Vec<PathBuf>, dark: bool) -> Harness<'static, App> {
    let cfg = std::env::temp_dir().join(format!("review-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&cfg).unwrap();
    std::env::set_var("REVIEW_CONFIG_DIR", &cfg);
    Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(1.5)
        .with_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light })
        .wgpu()
        .build_eframe(move |cc| App::new(cc, files, None))
}

/// Step until documents are loaded and no tiles are pending (or timeout).
pub fn settle(h: &mut Harness<'static, App>) {
    let start = Instant::now();
    let mut quiet = 0;
    while start.elapsed() < Duration::from_secs(20) {
        h.step();
        std::thread::sleep(Duration::from_millis(40));
        let busy = h.state().docs.iter().any(|d| !matches!(d.state, crate::document::LoadState::Ready | crate::document::LoadState::Failed(_) | crate::document::LoadState::NeedsPassword { .. }));
        if busy {
            quiet = 0;
        } else {
            quiet += 1;
        }
        if quiet > 25 {
            break;
        }
    }
    h.step();
}

pub fn shot(h: &mut Harness<'static, App>, name: &str) {
    let img = h.render().expect("render");
    let p = shots_dir().join(format!("{name}.png"));
    img.save(&p).unwrap();
    eprintln!("saved {}", p.display());
}

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var("REVIEW_FIXTURES").unwrap_or_else(|_| "tests/fixtures".into()))
}

#[test]
#[ignore = "renders screenshots; run with --ignored"]
fn screenshots() {
    let fx = fixtures();
    let pdf = fx.join("sample.pdf");
    let photo = fx.join("rotated.jpg");
    let mut h = harness(vec![pdf.clone(), photo.clone()], false);
    settle(&mut h);
    h.state_mut().activate(0);
    settle(&mut h);
    shot(&mut h, "01-pdf-light");

    h.state_mut().run(Action::ToggleMarkup);
    h.state_mut().run(Action::Inspector);
    settle(&mut h);
    shot(&mut h, "02-pdf-markup-inspector");

    h.state_mut().search.query = "the".into();
    h.state_mut().start_search();
    settle(&mut h);
    shot(&mut h, "03-search");

    h.state_mut().activate(1);
    h.state_mut().run(Action::Inspector);
    settle(&mut h);
    shot(&mut h, "04-image");

    let mut d = harness(vec![pdf], true);
    settle(&mut d);
    shot(&mut d, "05-pdf-dark");
    d.state_mut().run(Action::ContactSheet);
    settle(&mut d);
    shot(&mut d, "06-contact-sheet");

    let mut e = harness(vec![], false);
    settle(&mut e);
    shot(&mut e, "07-empty");
}

fn drag(h: &mut Harness<'static, App>, from: egui::Pos2, to: egui::Pos2) {
    use egui::{Event, Modifiers, PointerButton};
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=12 {
        let p = from.lerp(to, i as f32 / 12.0);
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
}

fn wait_saved(h: &mut Harness<'static, App>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        h.step();
        std::thread::sleep(Duration::from_millis(30));
        if !h.state().docs.iter().any(|d| d.saving) {
            break;
        }
    }
    settle(h);
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("review-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn pdf_edit_roundtrip() {
    use crate::model::PageSrc;
    let dir = tmp("pdf");
    let src = crate::pdf::tests::sample_pdf(&dir);
    let mut h = harness(vec![src.clone()], false);
    settle(&mut h);
    assert_eq!(h.state().docs[0].page_count(), 3);

    // Draw a rectangle with the mouse in the middle of the first page.
    h.state_mut().run(Action::Tool(crate::app::Tool::Shape(crate::app::ShapeTool::Rect)));
    h.state_mut().docs[0].current = 0;
    settle(&mut h);
    let before: usize = h.state().docs[0].model.slots.iter().map(|s| s.annots.len()).sum();
    drag(&mut h, egui::pos2(620.0, 330.0), egui::pos2(760.0, 420.0));
    let after: usize = h.state().docs[0].model.slots.iter().map(|s| s.annots.len()).sum();
    assert_eq!(after, before + 1, "mouse drag should create a rectangle");
    assert!(h.state().docs[0].dirty());

    // Select all text on page 1 and highlight it.
    h.state_mut().docs[0].current = 0;
    for _ in 0..20 {
        h.state_mut().run(Action::SelectAll);
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        if h.state().sel.text.is_some() {
            break;
        }
    }
    let text = h.state_mut().selected_text().expect("text selected");
    assert!(text.contains("Alpha"), "{text:?}");
    assert!(h.state_mut().apply_markup_to_selection(crate::model::MarkupKind::Highlight));

    // Undo / redo the highlight.
    let n = |h: &Harness<'static, App>| h.state().docs[0].model.slots[0].annots.len();
    let with = n(&h);
    h.state_mut().run(Action::Undo);
    assert_eq!(n(&h), with - 1);
    h.state_mut().run(Action::Redo);
    assert_eq!(n(&h), with);

    // Page operations: rotate page 2, move page 3 to the front, insert a blank page at the end.
    h.state_mut().go_to(1);
    h.state_mut().run(Action::RotateRight);
    let id = h.state().docs[0].id;
    h.state_mut().move_pages(id, vec![2], id, 0, false);
    h.state_mut().go_to(2);
    h.state_mut().run(Action::InsertBlank);
    h.state_mut().docs[0].model.meta = Some(crate::model::MetaEdit { title: "Edited Title".into(), ..Default::default() });
    let order: Vec<u32> = h.state().docs[0].model.slots.iter().map(|s| if let PageSrc::Pdf { index, .. } = s.src { index } else { 99 }).collect();
    assert_eq!(order, vec![2, 0, 1, 99]);

    // Save as a new file and verify the result with the engine.
    let out = dir.join("edited.pdf");
    h.state_mut().save_to(0, out.clone(), None, true);
    wait_saved(&mut h);
    assert!(out.exists());
    assert!(!h.state().docs[0].dirty());
    assert_eq!(h.state().docs[0].path.as_deref(), Some(out.as_path()));

    let (eng, rx) = crate::pdf::Engine::new(std::sync::Arc::new(|| {}));
    let _s = eng.open(1, &out, None);
    let info = loop {
        if let crate::pdf::Event::Opened { result, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            break result.unwrap();
        }
    };
    assert_eq!(info.pages.len(), 4);
    assert!((info.pages[2].0.width() - 792.0).abs() < 1.0, "rotated page is landscape: {:?}", info.pages[2]);
    assert_eq!(info.meta.title, "Edited Title");
    let labels: Vec<&str> = info.annots.iter().map(|(_, a)| a.label()).collect();
    assert!(labels.contains(&"Rectangle") && labels.contains(&"Highlight"), "{labels:?}");
    _s.request_text(0);
    let t = loop {
        if let crate::pdf::Event::Text { text, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            break text;
        }
    };
    assert!(t.text(0, t.chars.len()).contains("Charlie"), "page 3 moved to the front");

    // The reloaded document is clean and closes without a prompt.
    settle(&mut h);
    h.state_mut().run(Action::Close);
    assert!(h.state().dialog.is_none());
    assert!(h.state().docs.is_empty());
}

#[test]
fn unsaved_changes_prompt() {
    let dir = tmp("prompt");
    let src = crate::pdf::tests::sample_pdf(&dir);
    let mut h = harness(vec![src], false);
    settle(&mut h);
    h.state_mut().run(Action::RotateLeft);
    h.state_mut().run(Action::Close);
    assert!(matches!(h.state().dialog, Some(crate::dialogs::Dialog::ConfirmClose { .. })));
    assert_eq!(h.state().docs.len(), 1);
}

#[test]
fn image_edit_roundtrip() {
    let dir = tmp("img");
    let path = dir.join("pic.png");
    image::RgbaImage::from_fn(400, 300, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255])).save(&path).unwrap();
    let mut h = harness(vec![path.clone()], false);
    settle(&mut h);
    let id = h.state().docs[0].id;
    assert_eq!(h.state().docs[0].model.frames[0].dimensions(), (400, 300));

    // Rectangular selection by mouse, then Crop.
    h.state_mut().run(Action::Tool(crate::app::Tool::Rect));
    settle(&mut h);
    drag(&mut h, egui::pos2(500.0, 300.0), egui::pos2(800.0, 500.0));
    assert!(h.state().sel.rect.is_some(), "drag makes a selection");
    h.state_mut().sel.rect = Some((id, 0, egui::Rect::from_min_size(egui::pos2(50.0, 40.0), egui::vec2(200.0, 150.0))));
    h.state_mut().run(Action::Crop);
    assert_eq!(h.state().docs[0].model.frames[0].dimensions(), (200, 150));
    h.state_mut().run(Action::RotateRight);
    assert_eq!(h.state().docs[0].model.frames[0].dimensions(), (150, 200));
    h.state_mut().run(Action::Undo);
    assert_eq!(h.state().docs[0].model.frames[0].dimensions(), (200, 150));
    h.state_mut().run(Action::Redo);
    assert_eq!(h.state().docs[0].model.frames[0].dimensions(), (150, 200));

    // Markup is burned in when saving an image.
    let mut a = crate::model::Annot::new(
        crate::model::Shape::Rect(egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(60.0, 60.0))),
        crate::model::Style { width: 6.0, stroke: egui::Color32::from_rgb(255, 0, 0), ..Default::default() },
    );
    a.author = String::new();
    h.state_mut().docs[0].checkpoint("Add Shape");
    h.state_mut().docs[0].model.slots[0].annots.push(a);
    settle(&mut h);
    h.state_mut().run(Action::Save);
    wait_saved(&mut h);
    let saved = image::open(&path).unwrap().to_rgba8();
    assert_eq!(saved.dimensions(), (150, 200));
    let p = saved.get_pixel(10, 40);
    assert!(p[0] > 200 && p[1] < 60, "red border burned in, got {p:?}");
    assert!(!h.state().docs[0].dirty());
}

#[test]
fn move_pages_between_documents() {
    let dir = tmp("move");
    let a = crate::pdf::tests::sample_pdf(&dir);
    let dir2 = tmp("move2");
    let b = crate::pdf::tests::sample_pdf(&dir2);
    let mut h = harness(vec![a, b], false);
    settle(&mut h);
    let (ida, idb) = (h.state().docs[0].id, h.state().docs[1].id);
    h.state_mut().move_pages(ida, vec![0, 1], idb, 3, false);
    assert_eq!(h.state().docs[0].page_count(), 1);
    assert_eq!(h.state().docs[1].page_count(), 5);
    let out = dir2.join("merged.pdf");
    h.state_mut().save_to(1, out.clone(), None, false);
    wait_saved(&mut h);
    let doc = mupdf::Document::open(out.to_string_lossy().as_ref()).unwrap();
    assert_eq!(doc.page_count().unwrap(), 5);
}

#[test]
#[ignore = "renders screenshots; run with --ignored"]
fn screenshots_tools() {
    use crate::model::{Annot, Shape, Style};
    let fx = fixtures();
    let mut h = harness(vec![fx.join("sample.pdf")], false);
    settle(&mut h);
    // A selected arrow + rectangle + text box being edited + open note.
    h.state_mut().run(Action::ToggleMarkup);
    let id = h.state().docs[0].id;
    {
        let d = &mut h.state_mut().docs[0];
        let mut r = Annot::new(Shape::Rect(egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(160.0, 90.0))), Style { width: 3.0, ..Default::default() });
        r.author = "Me".into();
        let rid = r.id;
        d.model.slots[0].annots.push(r);
        d.model.slots[0].annots.push(Annot::new(Shape::Line { a: egui::pos2(120.0, 330.0), b: egui::pos2(290.0, 250.0), arrow: true }, Style { width: 3.0, stroke: egui::Color32::from_rgb(0x0A, 0x84, 0xFF), ..Default::default() }));
        d.model.slots[0].annots.push(Annot::new(Shape::Bubble(egui::Rect::from_min_size(egui::pos2(380.0, 330.0), egui::vec2(150.0, 70.0))), Style { fill: Some(egui::Color32::WHITE), width: 2.0, stroke: egui::Color32::BLACK, ..Default::default() }));
        d.model.slots[0].annots.push(Annot::new(Shape::Star(egui::Rect::from_min_size(egui::pos2(470.0, 120.0), egui::vec2(60.0, 60.0))), Style { fill: Some(egui::Color32::from_rgb(0xFF, 0xCC, 0x00)), width: 1.5, stroke: egui::Color32::from_rgb(0xFF, 0x95, 0x00), ..Default::default() }));
        let t = Annot::new(Shape::Text { rect: egui::Rect::from_min_size(egui::pos2(110.0, 420.0), egui::vec2(220.0, 40.0)), text: "Review this paragraph".into() }, Style { width: 0.0, font_size: 16.0, text_color: egui::Color32::from_rgb(0xD0, 0x10, 0x10), ..Default::default() });
        let tid = t.id;
        d.model.slots[0].annots.push(t);
        d.edits = 1;
        h.state_mut().sel.annot = Some((id, 0, rid));
        let _ = tid;
    }
    settle(&mut h);
    shot(&mut h, "10-markup-selected");

    // Dialogs
    h.state_mut().dialog = Some(crate::dialogs::Dialog::export(&h.state().docs[0], false, &h.state().settings));
    settle(&mut h);
    shot(&mut h, "11-export");
    h.state_mut().dialog = Some(crate::dialogs::Dialog::Preferences);
    settle(&mut h);
    shot(&mut h, "12-preferences");
    h.state_mut().dialog = Some(crate::dialogs::Dialog::Permissions(crate::model::Security { allow_print: true, allow_copy: true, ..Default::default() }, String::new()));
    settle(&mut h);
    shot(&mut h, "13-permissions");
    h.state_mut().dialog = Some(crate::dialogs::Dialog::ConfirmClose { docs: vec![id], quit: true });
    settle(&mut h);
    shot(&mut h, "14-confirm");
    h.state_mut().dialog = None;
    h.state_mut().run(Action::ViewMode(crate::settings::PdfViewMode::Two));
    h.state_mut().settings.dark_pdf = true;
    settle(&mut h);
    shot(&mut h, "15-two-pages-dark-pdf");

    // Image editing
    let mut i = harness(vec![fx.join("text_photo.jpg")], false);
    settle(&mut i);
    i.state_mut().run(Action::ToggleMarkup);
    let did = i.state().docs[0].id;
    i.state_mut().sel.rect = Some((did, 0, egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(220.0, 140.0))));
    settle(&mut i);
    shot(&mut i, "16-image-selection");
    i.state_mut().sel.rect = None;
    i.state_mut().run(Action::AdjustColor);
    if let Some(crate::dialogs::Dialog::AdjustColor(st)) = i.state_mut().dialog.as_mut() {
        st.adj.saturation = 1.6;
        st.adj.temperature = 0.5;
    }
    for _ in 0..30 {
        i.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    settle(&mut i);
    shot(&mut i, "17-adjust-color");
    i.state_mut().dialog = Some(crate::dialogs::Dialog::adjust_size(&i.state().docs[0]));
    settle(&mut i);
    shot(&mut i, "18-adjust-size");
    i.state_mut().dialog = Some(crate::dialogs::Dialog::Signature(vec![
        (0..40).map(|k| egui::pos2(500.0 + k as f32 * 6.0, 420.0 + (k as f32 * 0.5).sin() * 30.0)).collect(),
    ]));
    settle(&mut i);
    shot(&mut i, "19-signature");
}

/// Screen position of page-space point `p` on slot `slot` (must be visible).
fn on_page(h: &Harness<'static, App>, slot: usize, p: egui::Pos2) -> egui::Pos2 {
    let app = h.state();
    let (_, r) = app.page_rects.iter().find(|(i, _)| *i == slot).copied().expect("page visible");
    app.docs[app.active].model.slots[slot].to_screen(r, p)
}

fn click(h: &mut Harness<'static, App>, at: egui::Pos2) {
    use egui::{Event, Modifiers, PointerButton};
    h.event(Event::PointerMoved(at));
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
}

#[test]
fn pointer_interactions() {
    use crate::model::Shape;
    let dir = tmp("ptr");
    let src = crate::pdf::tests::sample_pdf(&dir);
    let mut h = harness(vec![src], false);
    settle(&mut h);
    // Text arrives asynchronously; hover the page until it's cached.
    for _ in 0..40 {
        let p = on_page(&h, 0, egui::pos2(150.0, 90.0));
        h.event(egui::Event::PointerMoved(p));
        h.step();
        std::thread::sleep(Duration::from_millis(15));
        if !h.state().texts.is_empty() {
            break;
        }
    }

    // 1. Drag across the first line selects its text ("Page 1 Alpha needle" at y≈85, x 72..~300).
    let a = on_page(&h, 0, egui::pos2(73.0, 86.0));
    let b = on_page(&h, 0, egui::pos2(200.0, 86.0));
    drag(&mut h, a, b);
    let sel = h.state_mut().selected_text().unwrap_or_default();
    assert!(sel.contains("Page 1"), "drag-selected {sel:?}");

    // 2. Highlighter tool: dragging over text creates a highlight annotation.
    h.state_mut().sel.text = None;
    h.state_mut().run(Action::Tool(crate::app::Tool::Markup(crate::model::MarkupKind::Highlight)));
    drag(&mut h, a, b);
    let has_hl = h.state().docs[0].model.slots[0].annots.iter().any(|x| matches!(x.shape, Shape::Markup { .. }));
    assert!(has_hl, "highlight created");

    // 3. Ctrl+Z through the keyboard removes it again.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    let has_hl = h.state().docs[0].model.slots[0].annots.iter().any(|x| matches!(x.shape, Shape::Markup { .. }));
    assert!(!has_hl, "undo via Ctrl+Z");

    // 4. Text tool: click, type, click away.
    h.state_mut().tool = crate::app::Tool::Text;
    h.state_mut().run(Action::Tool(crate::app::Tool::TextBox));
    let at = on_page(&h, 0, egui::pos2(100.0, 400.0));
    click(&mut h, at);
    assert!(h.state().sel.editing.is_some(), "text box is being edited");
    h.step();
    h.event(egui::Event::Text("Hello Fedora".into()));
    h.step();
    h.step();
    let away = on_page(&h, 0, egui::pos2(500.0, 700.0));
    click(&mut h, away);
    h.step();
    let texts: Vec<String> = h.state().docs[0].model.slots[0].annots.iter().filter_map(|x| x.text().map(|t| t.to_string())).collect();
    assert!(texts.iter().any(|t| t == "Hello Fedora"), "typed text box: {texts:?}");
    assert!(h.state().sel.editing.is_none());

    // 5. Note tool: click places a note and opens its editor.
    h.state_mut().run(Action::Tool(crate::app::Tool::Note));
    let at = on_page(&h, 0, egui::pos2(500.0, 200.0));
    click(&mut h, at);
    let notes = h.state().docs[0].model.slots[0].annots.iter().filter(|x| matches!(x.shape, Shape::Note { .. })).count();
    assert_eq!(notes, 1);

    // 6. Link-free click on empty space clears the selection; Delete removes a selected shape.
    h.state_mut().sel.editing = None;
    h.step();
    let n_before = h.state().docs[0].model.slots[0].annots.len();
    let id = h.state().docs[0].model.slots[0].annots.last().unwrap().id;
    let did = h.state().docs[0].id;
    h.state_mut().sel.annot = Some((did, 0, id));
    h.key_press(egui::Key::Delete);
    h.step();
    assert_eq!(h.state().docs[0].model.slots[0].annots.len(), n_before - 1, "Delete key removes annotation");

    // 7. Drag thumbnail 3 above thumbnail 1 to reorder pages.
    settle(&mut h);
    let t3 = h.state().thumb_rects.iter().find(|(_, i, _)| *i == 2).map(|(_, _, r)| *r).expect("thumb 3 visible");
    let t1 = h.state().thumb_rects.iter().find(|(_, i, _)| *i == 0).map(|(_, _, r)| *r).unwrap();
    drag(&mut h, t3.center(), egui::pos2(t1.center().x, t1.min.y + 8.0));
    let order: Vec<u32> = h.state().docs[0].model.slots.iter().map(|s| if let crate::model::PageSrc::Pdf { index, .. } = s.src { index } else { 99 }).collect();
    assert_eq!(order, vec![2, 0, 1], "thumbnail drag reorders pages");
}

#[test]
fn shortcut_specificity() {
    let dir = tmp("keys");
    let src = crate::pdf::tests::sample_pdf(&dir);
    let mut h = harness(vec![src], false);
    settle(&mut h);
    // Ctrl+Shift+A toggles the markup toolbar (must not be eaten by Ctrl+A).
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::A);
    h.step();
    assert!(h.state().show_markup);
    // Ctrl+Shift+S opens Export (must not be Save).
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S);
    h.step();
    assert!(matches!(h.state().dialog, Some(crate::dialogs::Dialog::Export(_))));
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.state().dialog.is_none());
    // Ctrl+Alt+3 shows the table of contents (must not switch to Two Pages via Ctrl+3).
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::Num3);
    h.step();
    assert_eq!(h.state().sidebar, crate::app::SidebarMode::Contents);
    assert_eq!(h.state().docs[0].mode, crate::settings::PdfViewMode::Continuous);
    // Ctrl+3 does switch to Two Pages.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num3);
    h.step();
    assert_eq!(h.state().docs[0].mode, crate::settings::PdfViewMode::Two);
}

#[test]
#[ignore = "renders screenshots of REVIEW_PDF; run with --ignored"]
fn screenshots_pdf() {
    let Some(pdf) = std::env::var_os("REVIEW_PDF").map(PathBuf::from) else { return };
    let mut h = harness(vec![pdf], false);
    settle(&mut h);
    h.state_mut().docs[0].fit = crate::document::Fit::Page;
    settle(&mut h);
    shot(&mut h, "20-pdf-fit-page");
    // Zoom far in and look at the bottom-right corner, where tiles are partial.
    h.state_mut().docs[0].fit = crate::document::Fit::None;
    h.state_mut().docs[0].zoom = 3.0;
    settle(&mut h);
    for _ in 0..80 {
        h.key_press(egui::Key::End);
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(-400.0, -400.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move });
        h.step();
    }
    settle(&mut h);
    shot(&mut h, "21-pdf-bottom-right");
}

/// Edge tiles are narrower than a full tile; they must not be stretched (regression).
#[test]
fn edge_tiles_not_stretched() {
    let dir = tmp("edge");
    // A 612x792 page with a black 30pt square in the bottom-right corner.
    let content = "0 g 572 10 30 30 re f";
    let pdf = format!(
        "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >> endobj\n4 0 obj << /Length {} >> stream\n{content}\nendstream endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n",
        content.len()
    );
    let path = dir.join("corner.pdf");
    std::fs::write(&path, pdf).unwrap();
    let mut h = harness(vec![path], false);
    settle(&mut h);
    for fit in [crate::document::Fit::Page, crate::document::Fit::Width] {
        h.state_mut().docs[0].fit = fit;
        settle(&mut h);
        let img = h.render().unwrap();
        let ppp = 1.5;
        let px = |p: egui::Pos2| *img.get_pixel((p.x * ppp) as u32, (p.y * ppp) as u32);
        let check = |p: egui::Pos2, dark: bool, what: &str| {
            if p.y * ppp >= img.height() as f32 - 2.0 {
                return; // off-screen at this zoom
            }
            let c = px(p);
            let is_dark = c[0] < 60 && c[1] < 60 && c[2] < 60;
            assert_eq!(is_dark, dark, "{what} at {p:?} ({fit:?}) was {c:?}");
        };
        let slot = h.state().docs[0].model.slots[0].clone();
        let (_, r) = *h.state().page_rects.first().expect("page visible");
        check(slot.to_screen(r, egui::pos2(587.0, 767.0)), true, "square centre");
        check(slot.to_screen(r, egui::pos2(555.0, 767.0)), false, "left of square");
        check(slot.to_screen(r, egui::pos2(587.0, 740.0)), false, "above square");
        let (_, _, tr) = *h.state().thumb_rects.first().expect("thumbnail visible");
        check(slot.to_screen(tr, egui::pos2(590.0, 770.0)), true, "thumbnail square");
        check(slot.to_screen(tr, egui::pos2(540.0, 770.0)), false, "thumbnail left of square");
    }
}
