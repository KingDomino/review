//! Live Text: on-device text recognition for photos and scanned PDF pages (Tesseract).
//!
//! Recognised words become a normal [`PageText`], so selecting, copying, searching and
//! highlighting work exactly as they do for PDF text.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use crossbeam_channel::{unbounded, Sender};
use egui::{pos2, Pos2, Rect};
use image::RgbaImage;

use crate::pdf::{PageText, TChar};

/// What a recognition result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OcrKey {
    /// A scanned PDF page (MuPDF source + page index).
    Pdf { source: u64, page: u32 },
    /// An image frame; `ptr` identifies the exact pixels (edits invalidate it).
    Image { doc: u64, frame: usize, ptr: usize },
}

pub struct OcrJob {
    pub key: OcrKey,
    pub image: Arc<RgbaImage>,
    /// Pixels per page unit of `image`, and the page-space position of its top-left corner.
    pub scale: f32,
    pub origin: Pos2,
}

pub struct OcrResult {
    pub key: OcrKey,
    pub text: Result<Arc<PageText>, String>,
}

/// Is the `tesseract` command installed?
pub fn available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| Command::new("tesseract").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false))
}

/// English plus the desktop language, limited to installed language packs.
pub fn languages() -> String {
    static LANGS: OnceLock<String> = OnceLock::new();
    LANGS
        .get_or_init(|| {
            let installed: Vec<String> = Command::new("tesseract")
                .arg("--list-langs")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().skip(1).map(|l| l.trim().to_string()).collect())
                .unwrap_or_default();
            let locale = std::env::var("LANG").unwrap_or_default();
            let iso = locale.split(['_', '.']).next().unwrap_or("").to_lowercase();
            let wanted = match iso.as_str() {
                "de" => "deu",
                "fr" => "fra",
                "es" => "spa",
                "it" => "ita",
                "pt" => "por",
                "nl" => "nld",
                "sv" => "swe",
                "da" => "dan",
                "nb" | "no" => "nor",
                "fi" => "fin",
                "pl" => "pol",
                "cs" => "ces",
                "ru" => "rus",
                "uk" => "ukr",
                "tr" => "tur",
                "ja" => "jpn",
                "zh" => "chi_sim",
                "ko" => "kor",
                _ => "",
            };
            let mut langs = vec![];
            if installed.iter().any(|l| l == "eng") || installed.is_empty() {
                langs.push("eng".to_string());
            }
            if !wanted.is_empty() && installed.iter().any(|l| l == wanted) {
                langs.push(wanted.to_string());
            }
            if langs.is_empty() {
                langs.push(installed.first().cloned().unwrap_or_else(|| "eng".into()));
            }
            langs.join("+")
        })
        .clone()
}

/// Start the recognition worker; results are delivered to `deliver`.
pub fn spawn(deliver: impl Fn(OcrResult) + Send + 'static) -> Sender<OcrJob> {
    let (tx, rx) = unbounded::<OcrJob>();
    std::thread::Builder::new()
        .name("review-ocr".into())
        .spawn(move || {
            for job in rx {
                let text = recognize(&job.image).map(|words| Arc::new(to_page_text(&words, job.scale, job.origin)));
                deliver(OcrResult { key: job.key, text });
            }
        })
        .expect("spawn OCR worker");
    tx
}

#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    /// Pixel rect in the image passed to `recognize`.
    pub rect: Rect,
    pub line: (u32, u32, u32),
}

/// Run Tesseract on an image and return its words.
pub fn recognize(img: &RgbaImage) -> Result<Vec<Word>, String> {
    // Tesseract works best with text 20–40 px tall: enlarge small images, shrink huge ones.
    let (w, h) = img.dimensions();
    let long = w.max(h) as f32;
    let k = if long < 1200.0 { 2.0 } else if long > 5000.0 { 5000.0 / long } else { 1.0 };
    let work = if (k - 1.0).abs() > 0.01 {
        image::imageops::resize(img, (w as f32 * k) as u32, (h as f32 * k) as u32, image::imageops::FilterType::Triangle)
    } else {
        img.clone()
    };
    // Flatten transparency onto white; Tesseract reads grey anyway.
    let grey = image::DynamicImage::ImageRgba8(work).to_luma8();
    let mut png = Vec::new();
    grey.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;

    let mut child = Command::new("tesseract")
        .args(["stdin", "stdout", "-l", &languages(), "--psm", "3", "tsv"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Live Text needs Tesseract ({e}). Install it with: sudo dnf install tesseract"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Write from a thread so a full stdout pipe can't deadlock us.
        let data = png;
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Tesseract failed".into());
    }
    Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout), 1.0 / k))
}

/// Parse Tesseract's TSV output (level 5 rows are words). `unscale` maps back to input pixels.
pub fn parse_tsv(tsv: &str, unscale: f32) -> Vec<Word> {
    let mut words = Vec::new();
    for line in tsv.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 || f[0] != "5" {
            continue;
        }
        let num = |i: usize| f[i].trim().parse::<f32>().unwrap_or(0.0);
        let conf = num(10);
        let text = f[11].trim();
        if text.is_empty() || conf < 35.0 {
            continue;
        }
        let rect = Rect::from_min_size(pos2(num(6), num(7)) * unscale, egui::vec2(num(8), num(9)) * unscale);
        words.push(Word { text: text.to_string(), rect, line: (num(2) as u32, num(3) as u32, num(4) as u32) });
    }
    words
}

/// Turn words into per-character boxes in page space, with spaces between words.
pub fn to_page_text(words: &[Word], scale: f32, origin: Pos2) -> PageText {
    let mut out = PageText::default();
    let mut line_no: u32 = 0;
    let mut current: Option<(u32, u32, u32)> = None;
    let mut line_rect = Rect::NOTHING;
    let to_page = |r: Rect| Rect::from_min_max(origin + r.min.to_vec2() / scale, origin + r.max.to_vec2() / scale);
    for w in words {
        let r = to_page(w.rect);
        if current != Some(w.line) {
            if current.is_some() {
                out.lines.push(line_rect);
                line_no += 1;
            }
            current = Some(w.line);
            line_rect = Rect::NOTHING;
        } else if let Some(prev) = out.chars.last().map(|c| c.rect) {
            // A space spanning the gap to the previous word.
            let gap = Rect::from_min_max(pos2(prev.max.x, r.min.y), pos2(r.min.x.max(prev.max.x + 0.1), r.max.y));
            out.chars.push(TChar { c: ' ', rect: gap, line: line_no });
        }
        let n = w.text.chars().count().max(1) as f32;
        for (i, c) in w.text.chars().enumerate() {
            let x0 = r.min.x + r.width() * i as f32 / n;
            let x1 = r.min.x + r.width() * (i + 1) as f32 / n;
            out.chars.push(TChar { c, rect: Rect::from_min_max(pos2(x0, r.min.y), pos2(x1, r.max.y)), line: line_no });
        }
        line_rect = line_rect.union(r);
    }
    if current.is_some() {
        out.lines.push(line_rect);
    }
    out
}

/// Case-insensitive search through recognised text; returns (char ranges) of matches.
pub fn find(text: &PageText, query: &str) -> Vec<(usize, usize)> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    if q.is_empty() {
        return Vec::new();
    }
    let hay: Vec<char> = text.chars.iter().map(|c| c.c.to_lowercase().next().unwrap_or(c.c)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + q.len() <= hay.len() {
        if hay[i..i + q.len()] == q[..] {
            out.push((i, i + q.len()));
            i += q.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_to_page_text() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t10\t10\t40\t20\t95\tHello\n\
                   5\t1\t1\t1\t1\t2\t60\t10\t50\t20\t93\tworld\n\
                   5\t1\t1\t1\t2\t1\t10\t40\t30\t20\t90\tBye\n\
                   5\t1\t1\t1\t2\t2\t50\t40\t30\t20\t10\tjunk\n";
        let words = parse_tsv(tsv, 1.0);
        assert_eq!(words.len(), 3, "low-confidence word dropped");
        let t = to_page_text(&words, 2.0, pos2(100.0, 0.0));
        assert_eq!(t.text(0, t.chars.len()), "Hello world\nBye");
        assert_eq!(t.lines.len(), 2);
        assert!((t.chars[0].rect.min.x - 105.0).abs() < 0.01, "scaled into page space");
        assert_eq!(find(&t, "WORLD"), vec![(6, 11)]);
    }

    #[test]
    fn recognizes_real_text() {
        if !available() {
            eprintln!("tesseract not installed; skipping");
            return;
        }
        // Render text with our own font and read it back.
        let mut img = RgbaImage::from_pixel(700, 120, image::Rgba([255, 255, 255, 255]));
        let a = crate::model::Annot::new(
            crate::model::Shape::Text { rect: Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(680.0, 80.0)), text: "Review reads photos".into() },
            crate::model::Style { font_size: 48.0, text_color: egui::Color32::BLACK, width: 0.0, ..Default::default() },
        );
        img = crate::imaging::burn_annotations(&img, &[a], crate::theme::INTER);
        let words = recognize(&img).expect("tesseract runs");
        let all: Vec<String> = words.iter().map(|w| w.text.to_lowercase()).collect();
        assert!(all.iter().any(|w| w.contains("review")) && all.iter().any(|w| w.contains("photos")), "{all:?}");
    }
}
