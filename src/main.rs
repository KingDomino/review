//! Review — a fast, Preview-style viewer and editor for PDFs and images.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod annots;
mod app;
mod canvas;
mod color;
mod dialogs;
mod document;
mod heif;
mod imaging;
mod inspector;
mod layout;
mod model;
mod model3d;
mod ocr;
mod pdf;
mod platform;
mod printing;
mod render;
mod scanner;
mod settings;
mod sidebar;
mod theme;
mod view3d;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

fn usage() {
    println!(
        "Review {} — view and edit PDFs and images\n\nUsage: review [FILE|FOLDER]...\n\nThe files open together in a new window.\n\n  --version      print the version\n",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> eframe::Result {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut options_done = false;
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("--") if !options_done => options_done = true,
            Some("--help") | Some("-h") if !options_done => {
                usage();
                return Ok(());
            }
            Some("--version") | Some("-V") if !options_done => {
                println!("review {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some(s) if s.starts_with("file://") => files.push(PathBuf::from(percent_decode(&s[7..]))),
            _ => files.push(PathBuf::from(a)),
        }
    }
    // Preferences ▸ "Open each file in its own window": one window per file (a folder stays one group).
    if settings::Settings::load().open_mode == settings::OpenMode::OwnWindow && files.len() > 1 {
        for f in files.split_off(1) {
            platform::open_window(&[f], None);
        }
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/review.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Review")
        .with_app_id("review")
        .with_inner_size([1180.0, 820.0])
        .with_min_inner_size([520.0, 360.0])
        .with_transparent(false);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    // A window opened from another one cascades from it instead of restoring the saved geometry.
    let cascade = platform::cascade();
    if let Some(r) = cascade {
        viewport = viewport.with_position(r.min).with_inner_size(r.size());
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        persist_window: cascade.is_none(),
        ..Default::default()
    };
    eframe::run_native("review", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, files)))))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
