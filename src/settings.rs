//! Preferences, recent files and saved signatures (~/.config/review/settings.json).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfViewMode {
    Continuous,
    Single,
    Two,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Background {
    System,
    Light,
    Dark,
    Black,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenMode {
    /// Open groups of files in the same window
    SameWindow,
    /// Open each file in its own window
    OwnWindow,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub open_mode: OpenMode,
    pub background: Background,
    /// Images: true = 1 image pixel equals 1 screen point, false = size on screen equals size on printout
    pub image_pixel_scale: bool,
    pub pdf_view: PdfViewMode,
    pub resume_last_page: bool,
    pub logical_page_numbers: bool,
    pub author: String,
    pub add_name_to_annotations: bool,
    pub dark_pdf: bool,
    pub show_sidebar: bool,
    pub sidebar_width: f32,
    pub recent: Vec<PathBuf>,
    /// Page last viewed per file (for "Start on the last viewed page").
    pub last_page: HashMap<String, u32>,
    /// Signatures: strokes normalised to a 1x(aspect) box.
    pub signatures: Vec<Signature>,
    pub export_quality: u8,
    pub live_thumbnails: bool,
    /// Live Text: recognise text in photos and scanned pages.
    pub live_text: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Signature {
    pub aspect: f32,
    pub strokes: Vec<Vec<[f32; 2]>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            open_mode: OpenMode::SameWindow,
            background: Background::System,
            image_pixel_scale: true,
            pdf_view: PdfViewMode::Continuous,
            resume_last_page: true,
            logical_page_numbers: true,
            author: whoami(),
            add_name_to_annotations: true,
            dark_pdf: false,
            show_sidebar: true,
            sidebar_width: 190.0,
            recent: Vec::new(),
            last_page: HashMap::new(),
            signatures: Vec::new(),
            export_quality: 85,
            live_thumbnails: true,
            live_text: true,
        }
    }
}

fn whoami() -> String {
    // Full name from the GECOS field, falling back to the login name.
    let user = std::env::var("USER").unwrap_or_default();
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() > 4 && f[0] == user {
                let name = f[4].split(',').next().unwrap_or("").trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    user
}

pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("REVIEW_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("review")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(config_dir().join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = dir.join(".settings.json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("settings.json"));
            }
        }
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|x| x != &p);
        self.recent.insert(0, p);
        self.recent.truncate(15);
    }
}
