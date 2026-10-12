//! Photo print layout, modelled on Photoshop's Print Settings dialog (see docs/photoshop-print-analysis.md):
//! a paper size and orientation, the image's size and position on the page, and crop marks.
//! All geometry is in PDF points (1/72 inch), with the origin at the top-left of the oriented page.

use egui::{pos2, vec2, Pos2, Rect, Vec2};

pub struct Paper {
    pub name: &'static str,
    /// Portrait size in points.
    pub size: Vec2,
    /// PWG media name for CUPS (`lp -o media=…`).
    pub pwg: &'static str,
}

const IN: f32 = 72.0;
const MM: f32 = 72.0 / 25.4;

pub const PAPERS: &[Paper] = &[
    Paper { name: "US Letter", size: vec2(8.5 * IN, 11.0 * IN), pwg: "na_letter_8.5x11in" },
    Paper { name: "US Legal", size: vec2(8.5 * IN, 14.0 * IN), pwg: "na_legal_8.5x14in" },
    Paper { name: "Tabloid", size: vec2(11.0 * IN, 17.0 * IN), pwg: "na_ledger_11x17in" },
    Paper { name: "A3", size: vec2(297.0 * MM, 420.0 * MM), pwg: "iso_a3_297x420mm" },
    Paper { name: "A4", size: vec2(210.0 * MM, 297.0 * MM), pwg: "iso_a4_210x297mm" },
    Paper { name: "A5", size: vec2(148.0 * MM, 210.0 * MM), pwg: "iso_a5_148x210mm" },
    Paper { name: "4 × 6 in", size: vec2(4.0 * IN, 6.0 * IN), pwg: "na_index-4x6_4x6in" },
    Paper { name: "5 × 7 in", size: vec2(5.0 * IN, 7.0 * IN), pwg: "na_5x7_5x7in" },
    Paper { name: "8 × 10 in", size: vec2(8.0 * IN, 10.0 * IN), pwg: "na_govt-letter_8x10in" },
    Paper { name: "10 × 15 cm", size: vec2(100.0 * MM, 150.0 * MM), pwg: "om_small-photo_100x150mm" },
    Paper { name: "13 × 18 cm", size: vec2(130.0 * MM, 180.0 * MM), pwg: "om_photo-13x18_130x180mm" },
];

/// Match a printer's or the system's paper name ("Letter", "A4", "na_letter_8.5x11in", "4x6"…).
pub fn paper_by_name(name: &str) -> Option<usize> {
    let n = name.to_ascii_lowercase().replace([' ', '×'], "");
    let keys: [&[&str]; 11] = [
        &["letter"],
        &["legal"],
        &["tabloid", "ledger", "11x17"],
        &["a3"],
        &["a4"],
        &["a5"],
        &["4x6", "index-4x6"],
        &["5x7"],
        &["8x10"],
        &["10x15", "100x150"],
        &["13x18", "130x180"],
    ];
    // Exact names first ("a4"), then substrings ("iso_a4_210x297mm").
    keys.iter().position(|k| k.contains(&n.as_str())).or_else(|| keys.iter().position(|k| k.iter().any(|k| n.contains(k))))
}

/// Countries that use US Letter rather than A4.
pub fn letter_region(region: &str) -> bool {
    matches!(region.to_ascii_uppercase().as_str(), "US" | "CA" | "MX" | "PH" | "CL" | "CO" | "VE" | "CR" | "GT" | "PR" | "DO" | "PA" | "SV" | "NI" | "HN" | "BZ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Inches,
    Centimeters,
    Millimeters,
    Points,
}

impl Units {
    pub const ALL: [Units; 4] = [Units::Inches, Units::Centimeters, Units::Millimeters, Units::Points];

    pub fn name(self) -> &'static str {
        match self {
            Units::Inches => "Inches",
            Units::Centimeters => "cm",
            Units::Millimeters => "mm",
            Units::Points => "Points",
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Units::Inches => " in",
            Units::Centimeters => " cm",
            Units::Millimeters => " mm",
            Units::Points => " pt",
        }
    }

    /// Points per unit.
    pub fn pt(self) -> f32 {
        match self {
            Units::Inches => IN,
            Units::Centimeters => 10.0 * MM,
            Units::Millimeters => MM,
            Units::Points => 1.0,
        }
    }

    pub fn decimals(self) -> usize {
        match self {
            Units::Inches | Units::Centimeters => 2,
            Units::Millimeters => 1,
            Units::Points => 0,
        }
    }
}

/// Inset used by Scale to Fit Media: most printers can't print right to the edge.
pub const FIT_MARGIN: f32 = 0.25 * IN;
/// Smallest print size (an eighth of an inch).
pub const MIN_SIZE: f32 = IN / 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub paper: usize,
    pub landscape: bool,
    /// Keep the image centred on the page (Position ▸ Center).
    pub center: bool,
    /// Size the image to the page (Scale to Fit Media).
    pub fit: bool,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub crop_marks: bool,
}

impl Layout {
    /// A layout for an image `native` points big: actual size if it fits on the page, otherwise
    /// scaled to fit; centred either way.
    pub fn new(paper: usize, native: Vec2) -> Layout {
        let mut l = Layout { paper, landscape: false, center: true, fit: false, left: 0.0, top: 0.0, width: native.x, crop_marks: false };
        let avail = l.page() - Vec2::splat(2.0 * FIT_MARGIN);
        if native.x > avail.x || native.y > avail.y {
            // Too big: fit it, turning the paper to match a wide image.
            l.fit = true;
            l.landscape = native.x > native.y;
        }
        l.update(native.y / native.x.max(1e-3));
        l
    }

    pub fn page(&self) -> Vec2 {
        let s = PAPERS[self.paper.min(PAPERS.len() - 1)].size;
        if self.landscape { vec2(s.y, s.x) } else { s }
    }

    /// Re-derive size/position from `fit` and `center` (call after any change).
    pub fn update(&mut self, aspect: f32) {
        let page = self.page();
        if self.fit {
            let avail = page - Vec2::splat(2.0 * FIT_MARGIN);
            self.width = avail.x.min(avail.y / aspect.max(1e-6));
        }
        self.width = self.width.max(MIN_SIZE).max(MIN_SIZE / aspect.max(1e-6));
        if self.center {
            let size = vec2(self.width, self.width * aspect);
            self.left = (page.x - size.x) / 2.0;
            self.top = (page.y - size.y) / 2.0;
        }
    }

    /// Where an image with this height/width ratio goes on the page.
    pub fn rect(&self, aspect: f32) -> Rect {
        let size = vec2(self.width, self.width * aspect);
        if self.center {
            let page = self.page();
            Rect::from_min_size(pos2((page.x - size.x) / 2.0, (page.y - size.y) / 2.0), size)
        } else {
            Rect::from_min_size(pos2(self.left, self.top), size)
        }
    }

    /// True when part of the image falls off the paper.
    pub fn clipped(&self, aspect: f32) -> bool {
        let r = self.rect(aspect);
        !Rect::from_min_size(Pos2::ZERO, self.page()).expand(0.01).contains_rect(r)
    }
}

/// Corner crop marks just outside `r`, as line segments (Photoshop's "Corner Crop Marks").
pub fn crop_marks(r: Rect) -> Vec<[Pos2; 2]> {
    let gap = IN / 16.0;
    let len = IN / 4.0;
    let mut v = Vec::new();
    for (c, dx, dy) in [(r.left_top(), -1.0, -1.0), (r.right_top(), 1.0, -1.0), (r.left_bottom(), -1.0, 1.0), (r.right_bottom(), 1.0, 1.0)] {
        // Horizontal mark level with the edge, vertical mark in line with the side.
        v.push([pos2(c.x + dx * gap, c.y), pos2(c.x + dx * (gap + len), c.y)]);
        v.push([pos2(c.x, c.y + dy * gap), pos2(c.x, c.y + dy * (gap + len))]);
    }
    v
}

/// One printed page: an image placed on the page.
pub struct Placed<'a> {
    pub image: &'a image::RgbaImage,
    pub rect: Rect,
}

/// Lay the images out page by page with the same size and position.
pub fn place<'a>(layout: &Layout, frames: &'a [image::RgbaImage]) -> Vec<Placed<'a>> {
    frames.iter().map(|f| Placed { image: f, rect: layout.rect(f.height() as f32 / f.width().max(1) as f32) }).collect()
}

/// PowerShell that prints the PNGs at exact positions through .NET's PrintDocument, so the
/// system PDF handler can't rescale them. Rects are in points on the oriented page.
#[cfg(any(windows, test))]
pub fn windows_print_script(printer: &str, copies: u32, layout: &Layout, pages: &[(std::path::PathBuf, Rect)], to_file: Option<&std::path::Path>) -> String {
    let q = |s: &str| s.replace('\'', "''");
    let h = |pt: f32| (pt / 72.0 * 100.0).round(); // hundredths of an inch
    let paper = PAPERS[layout.paper.min(PAPERS.len() - 1)].size;
    let mut items = String::new();
    for (path, r) in pages {
        let marks: Vec<String> = if layout.crop_marks {
            crop_marks(*r).iter().map(|[a, b]| format!("@({},{},{},{})", h(a.x), h(a.y), h(b.x), h(b.y))).collect()
        } else {
            Vec::new()
        };
        items += &format!(
            "@{{f='{}';x={};y={};w={};h={};m=@({})}},",
            q(&path.to_string_lossy()),
            h(r.min.x),
            h(r.min.y),
            h(r.width()),
            h(r.height()),
            marks.join(",")
        );
    }
    let items = items.trim_end_matches(',');
    format!(
        r#"$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
$d = New-Object System.Drawing.Printing.PrintDocument
$d.DocumentName = 'Review'
if ('{printer}' -ne '') {{ $d.PrinterSettings.PrinterName = '{printer}' }}
if (-not $d.PrinterSettings.IsValid) {{ throw 'Printer not found' }}
$d.PrinterSettings.Copies = {copies}
{to_file}
$d.DefaultPageSettings.Landscape = ${landscape}
$ps = $d.PrinterSettings.PaperSizes | Where-Object {{ [math]::Abs($_.Width - {pw}) -le 4 -and [math]::Abs($_.Height - {ph}) -le 4 }} | Select-Object -First 1
if (-not $ps) {{ $ps = New-Object System.Drawing.Printing.PaperSize('Custom', {pw}, {ph}) }}
$d.DefaultPageSettings.PaperSize = $ps
$pages = @({items})
$st = @{{ i = 0 }}
$d.add_PrintPage({{ param($s, $e)
  $p = $pages[$st.i]
  $g = $e.Graphics
  $g.PageUnit = [System.Drawing.GraphicsUnit]::Display
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.TranslateTransform(-$e.PageSettings.HardMarginX, -$e.PageSettings.HardMarginY)
  $img = [System.Drawing.Image]::FromFile($p.f)
  $g.DrawImage($img, [single]$p.x, [single]$p.y, [single]$p.w, [single]$p.h)
  $img.Dispose()
  $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::Black), 0.5
  foreach ($m in $p.m) {{ $g.DrawLine($pen, [single]$m[0], [single]$m[1], [single]$m[2], [single]$m[3]) }}
  $st.i++
  $e.HasMorePages = $st.i -lt $pages.Count
}})
$d.Print()
"#,
        printer = q(printer),
        to_file = to_file.map(|f| format!("$d.PrinterSettings.PrintToFile = $true\n$d.PrinterSettings.PrintFileName = '{}'", q(&f.to_string_lossy()))).unwrap_or_default(),
        copies = copies.clamp(1, 999),
        landscape = if layout.landscape { "true" } else { "false" },
        pw = h(paper.x),
        ph = h(paper.y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_geometry() {
        // A 3:2 photo, 3000x2000 px at 300 ppi = 10 x 6.67 in: too big for Letter portrait,
        // so it starts landscape and fitted.
        let native = vec2(10.0 * IN, 20.0 / 3.0 * IN);
        let mut l = Layout::new(0, native);
        assert!(l.landscape && l.fit && l.center);
        let aspect = 2.0 / 3.0;
        let r = l.rect(aspect);
        assert!((r.center() - (l.page() / 2.0).to_pos2()).length() < 0.01);
        assert!(!l.clipped(aspect));
        // Print it at exactly 6 x 4 in, 1 in from the top-left corner.
        l.fit = false;
        l.center = false;
        l.width = 6.0 * IN;
        l.left = IN;
        l.top = IN;
        l.update(aspect);
        let r = l.rect(aspect);
        assert_eq!(r, Rect::from_min_size(pos2(IN, IN), vec2(6.0 * IN, 4.0 * IN)));
        // Dragged half off the page.
        l.left = 9.0 * IN;
        assert!(l.clipped(aspect));
        // Tiny sizes are clamped.
        l.width = 0.0;
        l.update(aspect);
        assert!(l.width >= MIN_SIZE / aspect - 0.01);
        assert_eq!(crop_marks(r).len(), 8);
    }

    #[test]
    fn paper_names() {
        assert_eq!(paper_by_name("Letter"), Some(0));
        assert_eq!(paper_by_name("na_letter_8.5x11in"), Some(0));
        assert_eq!(paper_by_name("A4"), Some(4));
        assert_eq!(paper_by_name("iso_a4_210x297mm"), Some(4));
        assert_eq!(paper_by_name("4x6"), Some(6));
        assert_eq!(paper_by_name("Executive"), None);
        assert!(letter_region("us") && !letter_region("DE"));
    }

    #[test]
    fn windows_script_places_exactly() {
        let mut l = Layout::new(0, vec2(3.0 * IN, 2.0 * IN));
        l.center = false;
        l.left = IN;
        l.top = 2.0 * IN;
        l.crop_marks = true;
        let s = windows_print_script("Office's printer", 2, &l, &[("C:\\t\\p.png".into(), l.rect(2.0 / 3.0))], None);
        assert!(s.contains("x=100;y=200;w=300;h=200"), "{s}");
        assert!(s.contains("'Office''s printer'"));
        assert!(s.contains("Copies = 2") && s.contains("Landscape = $false"));
        assert!(s.contains("[math]::Abs($_.Width - 850)"));
        assert!(!s.contains("PrintToFile"));
        // Print to PDF writes straight to the file chosen in Review.
        let s = windows_print_script("Microsoft Print to PDF", 1, &l, &[("C:\\t\\p.png".into(), l.rect(2.0 / 3.0))], Some(std::path::Path::new("C:\\Users\\me\\photo.pdf")));
        assert!(s.contains("PrintToFile = $true") && s.contains("PrintFileName = 'C:\\Users\\me\\photo.pdf'"), "{s}");
    }
}
