//! Review — a fast, Preview-style viewer and editor for PDFs and images on Linux.

mod annots;
mod app;
mod canvas;
mod dialogs;
mod document;
mod heif;
mod imaging;
mod inspector;
mod layout;
mod model;
mod pdf;
mod platform;
mod render;
mod settings;
mod sidebar;
mod theme;
#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

fn usage() {
    println!(
        "Review {} — view and edit PDFs and images\n\nUsage: review [--new-window] [FILE|FOLDER]...\n\n  --new-window   don't hand files to an already-running Review\n  --version      print the version\n",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> eframe::Result {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut new_window = false;
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("--help") | Some("-h") => {
                usage();
                return Ok(());
            }
            Some("--version") | Some("-V") => {
                println!("review {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--new-window") => new_window = true,
            Some(s) if s.starts_with("file://") => files.push(PathBuf::from(percent_decode(&s[7..]))),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let settings = settings::Settings::load();
    let single = settings.open_mode == settings::OpenMode::SameWindow && !new_window;

    // Hand the files to a running instance (instant "open" for double-clicks in Dolphin).
    if single && !files.is_empty() && platform::hand_off(&files) {
        return Ok(());
    }
    let ipc = if single { platform::listen() } else { None };

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
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        persist_window: true,
        ..Default::default()
    };
    let res = eframe::run_native(
        "review",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, files, ipc)))),
    );
    if single {
        platform::cleanup_socket();
    }
    res
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
