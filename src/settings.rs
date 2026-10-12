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
    /// Photo printing: last paper and units picked (None = from the printer / region).
    pub print_paper: Option<String>,
    pub print_units: Option<String>,
    /// The settings file as this window last read or wrote it (see `save`).
    #[serde(skip)]
    synced: serde_json::Value,
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
            print_paper: None,
            print_units: None,
            synced: serde_json::Value::Null,
        }
    }
}

/// Three-way merge of `ours` and `theirs`, both derived from `base`.
fn merge(base: &serde_json::Value, ours: &serde_json::Value, theirs: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    if ours == base || theirs.is_null() {
        return if theirs.is_null() { ours.clone() } else { theirs.clone() };
    }
    match (base, ours, theirs) {
        (_, Value::Object(o), Value::Object(t)) => {
            let empty = serde_json::Map::new();
            let b = base.as_object().unwrap_or(&empty);
            let mut out = serde_json::Map::new();
            for k in o.keys().chain(t.keys()).chain(b.keys()) {
                if out.contains_key(k) {
                    continue;
                }
                let (bv, ov, tv) = (b.get(k), o.get(k), t.get(k));
                let v = match (ov, tv) {
                    (Some(ov), Some(tv)) => Some(merge(bv.unwrap_or(&Value::Null), ov, tv)),
                    // Removed on one side and untouched on the other: stays removed.
                    (Some(ov), None) => (bv != Some(ov)).then(|| ov.clone()),
                    (None, Some(tv)) => (bv != Some(tv)).then(|| tv.clone()),
                    (None, None) => None,
                };
                if let Some(v) = v {
                    out.insert(k.clone(), v);
                }
            }
            Value::Object(out)
        }
        // Lists (recent files, signatures): our version, plus what the other side added.
        (_, Value::Array(o), Value::Array(t)) => {
            let b = base.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let mut out = o.clone();
            out.extend(t.iter().filter(|v| !b.contains(v) && !o.contains(v)).cloned());
            Value::Array(out)
        }
        _ => ours.clone(),
    }
}

fn whoami() -> String {
    if cfg!(windows) {
        return std::env::var("USERNAME").unwrap_or_default();
    }
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
        let disk = Self::read_disk();
        let mut s: Settings = serde_json::from_value(disk.clone()).unwrap_or_default();
        s.synced = disk;
        s
    }

    fn read_disk() -> serde_json::Value {
        std::fs::read(config_dir().join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    /// Every window is its own process sharing this file, so saving merges: what this window
    /// changed since it last synced goes on top of what other windows have written meanwhile,
    /// and this window picks up their changes (recent files, preferences, signatures).
    pub fn save(&mut self) {
        let Ok(ours) = serde_json::to_value(&*self) else { return };
        let merged = merge(&self.synced, &ours, &Self::read_disk());
        let Ok(mut next) = serde_json::from_value::<Settings>(merged) else { return };
        next.recent.truncate(15);
        let Ok(value) = serde_json::to_value(&next) else { return };
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_vec_pretty(&value) {
            let tmp = dir.join(format!(".settings.json.{}.tmp", std::process::id()));
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("settings.json"));
            }
        }
        next.synced = value;
        *self = next;
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|x| x != &p);
        self.recent.insert(0, p);
        self.recent.truncate(15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(s: &Settings) -> serde_json::Value {
        serde_json::to_value(s).unwrap()
    }

    #[test]
    fn windows_merge_their_changes() {
        let base = Settings::default();
        // Window A opened a file; window B (started from the same file) opened another one.
        let mut a = base.clone();
        a.add_recent("/a.pdf".into());
        a.last_page.insert("/a.pdf".into(), 3);
        let mut b = base.clone();
        b.add_recent("/b.pdf".into());
        b.dark_pdf = true;
        let disk = merge(&value(&base), &value(&a), &serde_json::Value::Null);
        let disk2 = merge(&value(&base), &value(&b), &disk);
        let m: Settings = serde_json::from_value(disk2.clone()).unwrap();
        assert_eq!(m.recent, vec![PathBuf::from("/b.pdf"), PathBuf::from("/a.pdf")]);
        assert_eq!(m.last_page.get("/a.pdf"), Some(&3));
        assert!(m.dark_pdf);
        // A clears its recent menu and changes the author; B's changes survive.
        let a_base = disk.clone();
        let mut a: Settings = serde_json::from_value(disk).unwrap();
        a.recent.clear();
        a.author = "Someone".into();
        let m: Settings = serde_json::from_value(merge(&a_base, &value(&a), &disk2)).unwrap();
        assert_eq!(m.recent, vec![PathBuf::from("/b.pdf")], "B's file was opened after A last synced");
        assert!(m.dark_pdf);
        assert_eq!(m.author, "Someone");
        assert_eq!(m.last_page.get("/a.pdf"), Some(&3));
    }
}
