//! Image documents: decoding, metadata, edits, burning markup into pixels, export.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use egui::{Color32, Pos2};
use image::{imageops, DynamicImage, ImageFormat, Rgba, RgbaImage};

use crate::model::{bubble_points, round_rect_points, star_points, Align, Annot, MarkupKind, Shape};

pub struct Loaded {
    pub frames: Vec<Arc<RgbaImage>>,
    /// Delay of each frame for animated images (ms).
    pub delays: Vec<u32>,
    pub format: String,
    pub exif: Vec<(String, String)>,
    pub gps: Option<(f64, f64)>,
    pub dpi: f32,
    pub has_alpha: bool,
    pub color_model: String,
    /// Small preview kept when the full pixels are unloaded to save memory.
    pub thumb: Option<Arc<RgbaImage>>,
    /// Name of the embedded colour profile, if any.
    pub icc_name: Option<String>,
}

pub fn make_thumb(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let s = (320.0 / w.max(h).max(1) as f32).min(1.0);
    image::imageops::thumbnail(img, ((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1))
}

/// File extensions Review opens as images (everything else goes to MuPDF).
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "tif", "tiff", "bmp", "dib", "ico", "cur", "tga", "pnm",
    "pbm", "pgm", "ppm", "pam", "dds", "hdr", "exr", "qoi", "heic", "heif", "hif", "avif", "jp2", "j2k", "jpx",
    "jpf", "psd", "jxr", "wdp", "jxl",
    // Camera RAW
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef", "srw", "erf", "kdc",
    "dcr", "dcs", "mrw", "3fr", "iiq", "mef", "mos", "rwl", "ari",
];

pub const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef", "srw", "erf", "kdc",
    "dcr", "dcs", "mrw", "3fr", "iiq", "mef", "mos", "rwl", "ari",
];

pub fn is_image_path(p: &Path) -> bool {
    p.extension()
        .map(|e| IMAGE_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (exif, gps, orientation) = read_exif(&bytes);
    let mut loaded = decode(&bytes, &ext)?;
    loaded.exif = exif;
    loaded.gps = gps;
    let heif = loaded.format.starts_with("HEIF") || loaded.format.starts_with("AVIF");
    if orientation > 1 && loaded.frames.len() == 1 && !heif {
        let img = (*loaded.frames[0]).clone();
        loaded.frames[0] = Arc::new(apply_orientation(img, orientation));
    }
    loaded.thumb = loaded.frames.first().map(|f| Arc::new(make_thumb(f)));
    Ok(loaded)
}

fn decode(bytes: &[u8], ext: &str) -> Result<Loaded, String> {
    let guessed = image::guess_format(bytes).ok();
    let mut out = Loaded {
        frames: Vec::new(),
        delays: Vec::new(),
        format: String::new(),
        exif: Vec::new(),
        gps: None,
        dpi: 72.0,
        has_alpha: false,
        color_model: "RGB".into(),
        thumb: None,
        icc_name: None,
    };

    if matches!(ext, "heic" | "heif" | "hif" | "avif") || is_heif(bytes) {
        return decode_heif(bytes, out);
    }
    if RAW_EXTS.contains(&ext) {
        return decode_raw(bytes, ext, out);
    }
    if ext == "jxl" || bytes.starts_with(&[0xFF, 0x0A]) || bytes.starts_with(b"\0\0\0\x0CJXL ") {
        return decode_jxl(bytes, out);
    }

    match guessed {
        Some(ImageFormat::Gif) => {
            use image::AnimationDecoder;
            let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            for f in dec.into_frames().take(500) {
                let f = f.map_err(|e| e.to_string())?;
                let (n, d) = f.delay().numer_denom_ms();
                out.delays.push((n / d.max(1)).max(20));
                out.frames.push(Arc::new(f.into_buffer()));
            }
            out.format = "GIF image".into();
            out.has_alpha = true;
            out.color_model = "Indexed".into();
        }
        Some(ImageFormat::Tiff) => {
            out.frames = decode_tiff_pages(bytes)?;
            out.format = "TIFF image".into();
        }
        Some(fmt) => {
            use image::ImageDecoder;
            let mut dec = image::ImageReader::with_format(Cursor::new(bytes), fmt).into_decoder().map_err(|e| e.to_string())?;
            let icc = dec.icc_profile().ok().flatten();
            let img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
            out.has_alpha = img.color().has_alpha();
            out.color_model = color_model(&img);
            let mut rgba = img.to_rgba8();
            // Colour-manage photos with an embedded profile (e.g. Display P3 from phones).
            if let Some(icc) = icc {
                out.icc_name = crate::color::profile_name(&icc);
                crate::color::to_srgb(&mut rgba, &icc);
            }
            out.frames.push(Arc::new(rgba));
            out.format = format!("{} image", format_name(fmt));
            out.dpi = read_dpi(bytes, fmt).unwrap_or(72.0);
        }
        None => {
            // JPEG 2000, PSD, JPEG XR...: let MuPDF decode it.
            let img = mupdf::Image::from_bytes(bytes).map_err(|e| format!("Unsupported image format ({e})"))?;
            let pix = img.to_pixmap().map_err(|e| e.to_string())?;
            out.frames.push(Arc::new(crate::pdf::pixmap_to_rgba(&pix)));
            out.format = format!("{} image", ext.to_uppercase());
            let (rx, _) = img.resolution();
            if rx > 0 {
                out.dpi = rx as f32;
            }
        }
    }
    if out.frames.is_empty() {
        return Err("The image contains no frames".into());
    }
    Ok(out)
}

fn decode_raw(bytes: &[u8], ext: &str, mut out: Loaded) -> Result<Loaded, String> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::RawDevelop;
    let src = rawler::rawsource::RawSource::new_from_slice(bytes);
    let raw = rawler::decode(&src, &RawDecodeParams::default()).map_err(|e| format!("Can't read this RAW file: {e}"))?;
    let dev = RawDevelop::default().develop_intermediate(&raw).map_err(|e| e.to_string())?;
    let img = dev.to_dynamic_image().ok_or("RAW development failed")?;
    out.frames.push(Arc::new(img.to_rgba8()));
    out.format = format!("{} camera RAW ({} {})", ext.to_uppercase(), raw.clean_make, raw.clean_model);
    out.color_model = "RGB (developed RAW)".into();
    Ok(out)
}

fn decode_jxl(bytes: &[u8], mut out: Loaded) -> Result<Loaded, String> {
    let dec = jxl_oxide::integration::JxlDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    out.has_alpha = img.color().has_alpha();
    out.color_model = color_model(&img);
    out.frames.push(Arc::new(img.to_rgba8()));
    out.format = "JPEG XL image".into();
    Ok(out)
}

fn decode_tiff_pages(bytes: &[u8]) -> Result<Vec<Arc<RgbaImage>>, String> {
    // The `image` crate decodes only the first IFD; use MuPDF (which treats TIFF as a
    // multi-page document) when there is more than one page.
    let first = image::load_from_memory_with_format(bytes, ImageFormat::Tiff).map(|i| i.to_rgba8());
    let doc = mupdf::Document::from_bytes(bytes, "image/tiff");
    if let Ok(doc) = doc {
        let n = doc.page_count().unwrap_or(1);
        if n > 1 {
            let mut frames = Vec::new();
            for i in 0..n.min(500) {
                let page = doc.load_page(i).map_err(|e| e.to_string())?;
                let b = page.bounds().map_err(|e| e.to_string())?;
                // MuPDF reports image pages in points at the image's DPI; render at native size.
                let scale = match &first {
                    Ok(f) if i == 0 => f.width() as f32 / (b.x1 - b.x0).max(1.0),
                    _ => 1.0,
                };
                let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
                let pix = page
                    .to_pixmap(&mupdf::Matrix::new_scale(scale, scale), &mupdf::Colorspace::device_rgb(), false, false)
                    .map_err(|e| e.to_string())?;
                frames.push(Arc::new(crate::pdf::pixmap_to_rgba(&pix)));
            }
            return Ok(frames);
        }
    }
    Ok(vec![Arc::new(first.map_err(|e| e.to_string())?)])
}

fn is_heif(bytes: &[u8]) -> bool {
    bytes.len() > 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif" | b"avis")
}

fn decode_heif(bytes: &[u8], mut out: Loaded) -> Result<Loaded, String> {
    let avif = bytes.len() > 12 && matches!(&bytes[8..12], b"avif" | b"avis");
    out.format = if avif { "AVIF image".into() } else { "HEIF image".into() };
    #[cfg(windows)]
    {
        let d = crate::wic::decode(bytes, avif)?;
        let mut img = RgbaImage::from_raw(d.width, d.height, d.rgba).ok_or("bad HEIF buffer")?;
        if let Some(icc) = &d.icc {
            if crate::color::to_srgb(&mut img, icc) {
                out.icc_name = crate::color::profile_name(icc);
            }
        }
        out.has_alpha = img.pixels().any(|p| p[3] < 255);
        out.frames.push(Arc::new(if d.orientation > 1 { apply_orientation(img, d.orientation as u32) } else { img }));
    }
    #[cfg(not(windows))]
    {
        let (w, h, rgba, alpha) = crate::heif::decode(bytes)?;
        out.frames.push(Arc::new(RgbaImage::from_raw(w, h, rgba).ok_or("bad HEIF buffer")?));
        out.has_alpha = alpha;
    }
    Ok(out)
}

fn format_name(f: ImageFormat) -> &'static str {
    match f {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::WebP => "WebP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Tga => "TGA",
        ImageFormat::Pnm => "PNM",
        ImageFormat::Dds => "DDS",
        ImageFormat::Hdr => "Radiance HDR",
        ImageFormat::OpenExr => "OpenEXR",
        ImageFormat::Qoi => "QOI",
        ImageFormat::Avif => "AVIF",
        _ => "Image",
    }
}

fn color_model(img: &DynamicImage) -> String {
    use image::ColorType::*;
    match img.color() {
        L8 | L16 => "Grey".into(),
        La8 | La16 => "Grey + Alpha".into(),
        Rgba8 | Rgba16 | Rgba32F => "RGB + Alpha".into(),
        _ => "RGB".into(),
    }
}

fn read_dpi(bytes: &[u8], fmt: ImageFormat) -> Option<f32> {
    match fmt {
        ImageFormat::Jpeg => {
            // JFIF APP0: units at offset 13, xdensity at 14..16
            let i = bytes.windows(5).position(|w| w == b"JFIF\0")?;
            let units = *bytes.get(i + 7)?;
            let x = u16::from_be_bytes([*bytes.get(i + 8)?, *bytes.get(i + 9)?]) as f32;
            match units {
                1 if x > 1.0 => Some(x),
                2 if x > 1.0 => Some(x * 2.54),
                _ => None,
            }
        }
        ImageFormat::Png => {
            let i = bytes.windows(4).position(|w| w == b"pHYs")?;
            let ppu = u32::from_be_bytes(bytes.get(i + 4..i + 8)?.try_into().ok()?) as f32;
            (*bytes.get(i + 12)? == 1 && ppu > 0.0).then_some(ppu * 0.0254)
        }
        _ => None,
    }
}

fn read_exif(bytes: &[u8]) -> (Vec<(String, String)>, Option<(f64, f64)>, u32) {
    use exif::{In, Reader, Tag, Value};
    let Ok(ex) = Reader::new().read_from_container(&mut Cursor::new(bytes)) else {
        return (Vec::new(), None, 1);
    };
    let orientation = ex
        .get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1);
    let wanted = [
        (Tag::Make, "Camera Make"),
        (Tag::Model, "Camera Model"),
        (Tag::LensModel, "Lens"),
        (Tag::DateTimeOriginal, "Date Taken"),
        (Tag::ExposureTime, "Exposure Time"),
        (Tag::FNumber, "F Number"),
        (Tag::PhotographicSensitivity, "ISO"),
        (Tag::FocalLength, "Focal Length"),
        (Tag::FocalLengthIn35mmFilm, "Focal Length (35mm)"),
        (Tag::Flash, "Flash"),
        (Tag::ExposureBiasValue, "Exposure Bias"),
        (Tag::MeteringMode, "Metering Mode"),
        (Tag::WhiteBalance, "White Balance"),
        (Tag::ColorSpace, "Colour Space"),
        (Tag::PixelXDimension, "Pixel Width"),
        (Tag::PixelYDimension, "Pixel Height"),
        (Tag::Software, "Software"),
        (Tag::Artist, "Artist"),
        (Tag::Copyright, "Copyright"),
        (Tag::ImageDescription, "Description"),
    ];
    let mut out = Vec::new();
    for (tag, label) in wanted {
        if let Some(f) = ex.get_field(tag, In::PRIMARY) {
            let v = f.display_value().with_unit(&ex).to_string().trim_matches('"').trim().to_string();
            if !v.is_empty() {
                out.push((label.to_string(), v));
            }
        }
    }
    let coord = |t: Tag, r: Tag| -> Option<f64> {
        let f = ex.get_field(t, In::PRIMARY)?;
        let Value::Rational(v) = &f.value else { return None };
        if v.len() < 3 {
            return None;
        }
        let d = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let neg = ex
            .get_field(r, In::PRIMARY)
            .map(|f| f.display_value().to_string().contains(['S', 'W']))
            .unwrap_or(false);
        Some(if neg { -d } else { d })
    };
    let gps = match (coord(Tag::GPSLatitude, Tag::GPSLatitudeRef), coord(Tag::GPSLongitude, Tag::GPSLongitudeRef)) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    (out, gps, orientation)
}

fn apply_orientation(img: RgbaImage, o: u32) -> RgbaImage {
    match o {
        2 => imageops::flip_horizontal(&img),
        3 => imageops::rotate180(&img),
        4 => imageops::flip_vertical(&img),
        5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
        6 => imageops::rotate90(&img),
        7 => imageops::flip_horizontal(&imageops::rotate270(&img)),
        8 => imageops::rotate270(&img),
        _ => img,
    }
}

// ---------------------------------------------------------------------------
// Edits

pub fn rotate(img: &RgbaImage, deg: i32) -> RgbaImage {
    match deg.rem_euclid(360) {
        90 => imageops::rotate90(img),
        180 => imageops::rotate180(img),
        270 => imageops::rotate270(img),
        _ => img.clone(),
    }
}

pub fn crop(img: &RgbaImage, r: egui::Rect) -> RgbaImage {
    let x = r.min.x.max(0.0).round() as u32;
    let y = r.min.y.max(0.0).round() as u32;
    let w = (r.width().round() as u32).min(img.width().saturating_sub(x)).max(1);
    let h = (r.height().round() as u32).min(img.height().saturating_sub(y)).max(1);
    imageops::crop_imm(img, x, y, w, h).to_image()
}

pub fn resize(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let filter = if w < img.width() { imageops::FilterType::Lanczos3 } else { imageops::FilterType::CatmullRom };
    imageops::resize(img, w.max(1), h.max(1), filter)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adjust {
    pub black: f32,
    pub gamma: f32,
    pub white: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub sepia: f32,
    pub sharpness: f32,
}

impl Default for Adjust {
    fn default() -> Self {
        Self {
            black: 0.0,
            gamma: 1.0,
            white: 1.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
            sepia: 0.0,
            sharpness: 0.0,
        }
    }
}

impl Adjust {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Per-channel 8-bit lookup tables for the tonal part of the pipeline.
    fn luts(&self) -> [[u8; 256]; 3] {
        let mut out = [[0u8; 256]; 3];
        // Temperature/tint as channel gains.
        let t = self.temperature;
        let gains = [1.0 + 0.25 * t - 0.05 * self.tint, 1.0 + 0.12 * self.tint, 1.0 - 0.25 * t - 0.05 * self.tint];
        let exp = 2f32.powf(self.exposure);
        let span = (self.white - self.black).max(0.01);
        for (c, lut) in out.iter_mut().enumerate() {
            for (i, v) in lut.iter_mut().enumerate() {
                let mut x = i as f32 / 255.0;
                x = ((x - self.black) / span).clamp(0.0, 1.0);
                x = x.powf(1.0 / self.gamma.max(0.05));
                x *= exp * gains[c];
                // Shadows lift / highlights recover with smooth weights.
                let sw = (1.0 - x).clamp(0.0, 1.0).powi(2);
                let hw = x.clamp(0.0, 1.0).powi(2);
                x += self.shadows * 0.5 * sw * x.max(0.05);
                x += self.highlights * 0.35 * hw;
                // Contrast around mid-grey.
                x = (x - 0.5) * (1.0 + self.contrast) + 0.5;
                *v = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        out
    }

    pub fn apply(&self, src: &RgbaImage) -> RgbaImage {
        let luts = self.luts();
        let sat = self.saturation;
        let sepia = self.sepia.clamp(0.0, 1.0);
        let mut out = src.clone();
        let w = out.width() as usize;
        let rows = out.height() as usize;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let chunk_rows = rows.div_ceil(threads).max(1);
        std::thread::scope(|s| {
            for chunk in out.as_mut().chunks_mut(chunk_rows * w * 4) {
                s.spawn(|| {
                    for px in chunk.chunks_exact_mut(4) {
                        let mut r = luts[0][px[0] as usize] as f32;
                        let mut g = luts[1][px[1] as usize] as f32;
                        let mut b = luts[2][px[2] as usize] as f32;
                        if sat != 1.0 {
                            let l = 0.299 * r + 0.587 * g + 0.114 * b;
                            r = l + (r - l) * sat;
                            g = l + (g - l) * sat;
                            b = l + (b - l) * sat;
                        }
                        if sepia > 0.0 {
                            let sr = 0.393 * r + 0.769 * g + 0.189 * b;
                            let sg = 0.349 * r + 0.686 * g + 0.168 * b;
                            let sb = 0.272 * r + 0.534 * g + 0.131 * b;
                            r += (sr - r) * sepia;
                            g += (sg - g) * sepia;
                            b += (sb - b) * sepia;
                        }
                        px[0] = r.clamp(0.0, 255.0) as u8;
                        px[1] = g.clamp(0.0, 255.0) as u8;
                        px[2] = b.clamp(0.0, 255.0) as u8;
                    }
                });
            }
        });
        if self.sharpness.abs() > 0.01 {
            let blurred = imageops::blur(&out, 1.2);
            let k = self.sharpness * 1.5;
            for (o, bl) in out.pixels_mut().zip(blurred.pixels()) {
                for c in 0..3 {
                    let v = o[c] as f32 + (o[c] as f32 - bl[c] as f32) * k;
                    o[c] = v.clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

/// Levels that clip 0.5% at each end, like Preview's Auto Levels.
pub fn auto_levels(img: &RgbaImage) -> Adjust {
    let hist = histogram(img);
    let total: u64 = hist[3].iter().sum();
    let cut = total / 200;
    let mut acc = 0;
    let mut lo = 0;
    for (i, v) in hist[3].iter().enumerate() {
        acc += v;
        if acc > cut {
            lo = i;
            break;
        }
    }
    acc = 0;
    let mut hi = 255;
    for (i, v) in hist[3].iter().enumerate().rev() {
        acc += v;
        if acc > cut {
            hi = i;
            break;
        }
    }
    Adjust { black: lo as f32 / 255.0, white: (hi as f32 / 255.0).max(lo as f32 / 255.0 + 0.05), ..Adjust::default() }
}

/// R, G, B and luminance histograms (256 bins).
pub fn histogram(img: &RgbaImage) -> [[u64; 256]; 4] {
    let mut h = [[0u64; 256]; 4];
    let step = ((img.width() as u64 * img.height() as u64) / 400_000).max(1) as usize;
    for px in img.pixels().step_by(step) {
        if px[3] == 0 {
            continue;
        }
        h[0][px[0] as usize] += 1;
        h[1][px[1] as usize] += 1;
        h[2][px[2] as usize] += 1;
        let l = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) as usize;
        h[3][l.min(255)] += 1;
    }
    h
}

/// Instant Alpha: the region connected to `seed` whose colour is within `tolerance` (0-255).
pub fn flood_mask(img: &RgbaImage, seeds: &[(u32, u32)], tolerance: f32) -> Vec<bool> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut mask = vec![false; w * h];
    let mut stack: Vec<(usize, usize, [f32; 3])> = Vec::new();
    for &(x, y) in seeds {
        if (x as usize) < w && (y as usize) < h {
            let p = img.get_pixel(x, y);
            stack.push((x as usize, y as usize, [p[0] as f32, p[1] as f32, p[2] as f32]));
        }
    }
    let tol2 = tolerance * tolerance * 3.0;
    while let Some((x, y, c)) = stack.pop() {
        let i = y * w + x;
        if mask[i] {
            continue;
        }
        let p = img.get_pixel(x as u32, y as u32);
        let d = (p[0] as f32 - c[0]).powi(2) + (p[1] as f32 - c[1]).powi(2) + (p[2] as f32 - c[2]).powi(2);
        if d > tol2 && p[3] > 0 {
            continue;
        }
        mask[i] = true;
        if x > 0 {
            stack.push((x - 1, y, c));
        }
        if x + 1 < w {
            stack.push((x + 1, y, c));
        }
        if y > 0 {
            stack.push((x, y - 1, c));
        }
        if y + 1 < h {
            stack.push((x, y + 1, c));
        }
    }
    mask
}

/// Remove Background: flood from the image border, then feather the edge.
pub fn remove_background(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut seeds = Vec::new();
    let step = (w.max(h) / 64).max(1);
    for x in (0..w).step_by(step as usize) {
        seeds.push((x, 0));
        seeds.push((x, h - 1));
    }
    for y in (0..h).step_by(step as usize) {
        seeds.push((0, y));
        seeds.push((w - 1, y));
    }
    let mask = flood_mask(img, &seeds, 28.0);
    apply_mask(img, &mask, true)
}

/// Make masked pixels transparent (or keep only them when `invert`).
pub fn apply_mask(img: &RgbaImage, mask: &[bool], feather: bool) -> RgbaImage {
    let mut out = img.clone();
    let w = img.width() as usize;
    for (i, px) in out.pixels_mut().enumerate() {
        if mask[i] {
            px[3] = 0;
        } else if feather {
            let (x, y) = (i % w, i / w);
            let n = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]
                .iter()
                .filter(|(a, b)| *a < w && *b < img.height() as usize && mask[b * w + a])
                .count();
            if n > 0 {
                px[3] = (px[3] as f32 * (1.0 - n as f32 * 0.18)) as u8;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Burning markup into pixels (images have no annotation layer)

pub fn burn_annotations(img: &RgbaImage, annots: &[Annot], font: &[u8]) -> RgbaImage {
    use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let (w, h) = (img.width(), img.height());
    let Some(mut pm) = Pixmap::new(w, h) else { return img.clone() };
    // tiny-skia uses premultiplied RGBA.
    for (dst, src) in pm.pixels_mut().iter_mut().zip(img.pixels()) {
        *dst = tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
    }
    let paint_of = |c: Color32| {
        let mut p = Paint::default();
        p.set_color_rgba8(c.r(), c.g(), c.b(), c.a());
        p.anti_alias = true;
        p
    };
    let poly_path = |pts: &[Pos2], close: bool| {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            if i == 0 {
                pb.move_to(p.x, p.y);
            } else {
                pb.line_to(p.x, p.y);
            }
        }
        if close {
            pb.close();
        }
        pb.finish()
    };
    for a in annots {
        let st = &a.style;
        let stroke = Stroke { width: st.width.max(0.5), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        let draw_closed = |pm: &mut Pixmap, path: Option<tiny_skia::Path>| {
            if let Some(path) = path {
                if let Some(f) = st.fill {
                    pm.fill_path(&path, &paint_of(f), FillRule::Winding, Transform::identity(), None);
                }
                if st.width > 0.0 {
                    pm.stroke_path(&path, &paint_of(st.stroke), &stroke, Transform::identity(), None);
                }
            }
        };
        match &a.shape {
            egui_shape @ (Shape::Rect(_) | Shape::Redact(_)) => {
                let r = match egui_shape {
                    Shape::Rect(r) | Shape::Redact(r) => *r,
                    _ => unreachable!(),
                };
                let path = tiny_skia::Rect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y).map(PathBuilder::from_rect);
                if matches!(egui_shape, Shape::Redact(_)) {
                    if let Some(p) = path {
                        pm.fill_path(&p, &paint_of(Color32::BLACK), FillRule::Winding, Transform::identity(), None);
                    }
                } else {
                    draw_closed(&mut pm, path);
                }
            }
            Shape::Oval(r) => {
                let path = tiny_skia::Rect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y).and_then(PathBuilder::from_oval);
                draw_closed(&mut pm, path);
            }
            Shape::RoundRect(r) => draw_closed(&mut pm, poly_path(&round_rect_points(*r), true)),
            Shape::Star(r) => draw_closed(&mut pm, poly_path(&star_points(*r), true)),
            Shape::Bubble(r) => draw_closed(&mut pm, poly_path(&bubble_points(*r), true)),
            Shape::Polygon(p) => draw_closed(&mut pm, poly_path(p, true)),
            Shape::Line { a: p0, b: p1, arrow } => {
                if let Some(path) = poly_path(&[*p0, *p1], false) {
                    pm.stroke_path(&path, &paint_of(st.stroke), &stroke, Transform::identity(), None);
                }
                if *arrow {
                    let head = arrow_head(*p0, *p1, st.width);
                    if let Some(path) = poly_path(&head, false) {
                        pm.stroke_path(&path, &paint_of(st.stroke), &stroke, Transform::identity(), None);
                    }
                }
            }
            Shape::Ink(strokes) | Shape::Signature { strokes, .. } => {
                for s in strokes {
                    if let Some(path) = poly_path(s, false) {
                        pm.stroke_path(&path, &paint_of(st.stroke), &stroke, Transform::identity(), None);
                    }
                }
            }
            Shape::Markup { kind, quads } => {
                for q in quads {
                    let r = crate::model::points_bounds(q.iter());
                    let (rect, color) = match kind {
                        MarkupKind::Highlight => (r, st.stroke.gamma_multiply(0.45)),
                        MarkupKind::Underline => (egui::Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 1.5), r.max), st.stroke),
                        MarkupKind::Strike => {
                            let c = r.center().y;
                            (egui::Rect::from_min_max(egui::pos2(r.min.x, c - 0.75), egui::pos2(r.max.x, c + 0.75)), st.stroke)
                        }
                    };
                    if let Some(rr) = tiny_skia::Rect::from_ltrb(rect.min.x, rect.min.y, rect.max.x, rect.max.y) {
                        pm.fill_rect(rr, &paint_of(color), Transform::identity(), None);
                    }
                }
            }
            Shape::Text { rect, text } => {
                if let Some(f) = st.fill {
                    if let Some(rr) = tiny_skia::Rect::from_ltrb(rect.min.x, rect.min.y, rect.max.x, rect.max.y) {
                        pm.fill_rect(rr, &paint_of(f), Transform::identity(), None);
                    }
                }
                draw_text(&mut pm, font, text, *rect, st.font_size, st.text_color, st.align);
            }
            Shape::Note { .. } => {}
        }
    }
    let mut out = RgbaImage::new(w, h);
    for (dst, src) in out.pixels_mut().zip(pm.pixels()) {
        let c = src.demultiply();
        *dst = Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

pub fn arrow_head(a: Pos2, b: Pos2, width: f32) -> Vec<Pos2> {
    let d = (b - a).normalized();
    let len = (6.0 + width * 3.0).min((b - a).length() * 0.5);
    let n = egui::vec2(-d.y, d.x);
    vec![b - d * len + n * len * 0.6, b, b - d * len - n * len * 0.6]
}

fn draw_text(pm: &mut tiny_skia::Pixmap, font: &[u8], text: &str, rect: egui::Rect, size: f32, color: Color32, align: Align) {
    use ab_glyph::{Font, FontRef, ScaleFont};
    let Ok(font) = FontRef::try_from_slice(font) else { return };
    let sf = font.as_scaled(size);
    let line_h = sf.height() + sf.line_gap();
    let pad = 4.0;
    let max_w = rect.width() - 2.0 * pad;
    // Greedy word wrap.
    let mut lines: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if text_width(&sf, &trial) > max_w && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                cur = trial;
            }
        }
        lines.push(cur);
    }
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.pixels_mut();
    for (li, line) in lines.iter().enumerate() {
        let lw = text_width(&sf, line);
        let x0 = match align {
            Align::Left => rect.min.x + pad,
            Align::Center => rect.center().x - lw / 2.0,
            Align::Right => rect.max.x - pad - lw,
        };
        let baseline = rect.min.y + pad + sf.ascent() + li as f32 * line_h;
        let mut x = x0;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let glyph = id.with_scale_and_position(size, ab_glyph::point(x, baseline));
            if let Some(og) = font.outline_glyph(glyph) {
                let bb = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    let px = bb.min.x as i32 + gx as i32;
                    let py = bb.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 || px >= w || py >= h {
                        return;
                    }
                    let i = (py * w + px) as usize;
                    let a = cov.clamp(0.0, 1.0) * color.a() as f32 / 255.0;
                    let d = data[i].demultiply();
                    let mix = |s: u8, t: u8| (s as f32 * a + t as f32 * (1.0 - a)).round() as u8;
                    let na = (a * 255.0 + d.alpha() as f32 * (1.0 - a)).round() as u8;
                    data[i] = tiny_skia::ColorU8::from_rgba(mix(color.r(), d.red()), mix(color.g(), d.green()), mix(color.b(), d.blue()), na)
                        .premultiply();
                });
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
    }
}

fn text_width<F: ab_glyph::Font>(sf: &ab_glyph::PxScaleFont<&F>, s: &str) -> f32 {
    use ab_glyph::ScaleFont;
    s.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum()
}

// ---------------------------------------------------------------------------
// Export

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    WebP,
    Tiff,
    Bmp,
    Gif,
    Pdf,
}

impl ExportFormat {
    pub fn all() -> Vec<ExportFormat> {
        vec![Self::Png, Self::Jpeg, Self::WebP, Self::Tiff, Self::Bmp, Self::Gif, Self::Pdf]
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::WebP => "WebP",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Gif => "GIF",
            Self::Pdf => "PDF",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::Pdf => "pdf",
        }
    }
    pub fn has_quality(self) -> bool {
        matches!(self, Self::Jpeg)
    }
    pub fn supports_alpha(self) -> bool {
        matches!(self, Self::Png | Self::WebP | Self::Tiff | Self::Gif)
    }
    pub fn from_path(p: &Path) -> Option<Self> {
        let e = p.extension()?.to_string_lossy().to_lowercase();
        Some(match e.as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "webp" => Self::WebP,
            "tif" | "tiff" => Self::Tiff,
            "bmp" => Self::Bmp,
            "gif" => Self::Gif,
            "pdf" => Self::Pdf,
            _ => return None,
        })
    }
}

/// Encode and write atomically. `quality` is 0..=100.
pub fn export(img: &RgbaImage, fmt: ExportFormat, quality: u8, keep_alpha: bool, dpi: f32, dest: &Path) -> Result<(), String> {
    let tmp = crate::pdf::tmp_path(dest);
    let res = (|| -> Result<(), String> {
        let flat = || -> RgbaImage {
            let mut o = img.clone();
            for p in o.pixels_mut() {
                let a = p[3] as f32 / 255.0;
                for c in 0..3 {
                    p[c] = (p[c] as f32 * a + 255.0 * (1.0 - a)).round() as u8;
                }
                p[3] = 255;
            }
            o
        };
        match fmt {
            ExportFormat::Pdf => return crate::pdf::images_to_pdf(&[img], dpi, &tmp),
            ExportFormat::Jpeg => {
                let rgb = DynamicImage::ImageRgba8(flat()).to_rgb8();
                let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
                let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), quality.clamp(1, 100));
                enc.encode_image(&rgb).map_err(|e| e.to_string())?;
            }
            _ => {
                let image_fmt = match fmt {
                    ExportFormat::Png => ImageFormat::Png,
                    ExportFormat::WebP => ImageFormat::WebP,
                    ExportFormat::Tiff => ImageFormat::Tiff,
                    ExportFormat::Bmp => ImageFormat::Bmp,
                    _ => ImageFormat::Gif,
                };
                let data = if keep_alpha && fmt.supports_alpha() { img.clone() } else { flat() };
                let dynimg = if keep_alpha && fmt.supports_alpha() {
                    DynamicImage::ImageRgba8(data)
                } else {
                    DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(data).to_rgb8())
                };
                dynimg.save_with_format(&tmp, image_fmt).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) => std::fs::rename(&tmp, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient() -> RgbaImage {
        RgbaImage::from_fn(64, 48, |x, y| Rgba([(x * 4) as u8, (y * 5) as u8, 128, 255]))
    }

    #[test]
    fn edits_and_roundtrip() {
        let img = gradient();
        assert_eq!(rotate(&img, 90).dimensions(), (48, 64));
        assert_eq!(crop(&img, egui::Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(10.0, 8.0))).dimensions(), (10, 8));
        assert_eq!(resize(&img, 32, 24).dimensions(), (32, 24));
        let adj = Adjust { exposure: 1.0, saturation: 0.0, ..Default::default() }.apply(&img);
        let p = adj.get_pixel(10, 10);
        assert_eq!(p[0], p[1]);
        assert!(Adjust::default().apply(&img).as_raw() == img.as_raw(), "identity must not change pixels");
        let dir = crate::pdf::tests::tmpdir("img");
        for fmt in [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::WebP, ExportFormat::Tiff, ExportFormat::Bmp, ExportFormat::Pdf] {
            let dest = dir.join(format!("out.{}", fmt.ext()));
            export(&img, fmt, 90, true, 72.0, &dest).unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
            let loaded = if fmt == ExportFormat::Pdf { None } else { Some(load(&dest).unwrap()) };
            if let Some(l) = loaded {
                assert_eq!(l.frames[0].dimensions(), (64, 48), "{fmt:?}");
            }
        }
    }

    #[test]
    fn background_removal_and_burn_in() {
        let mut img = RgbaImage::from_pixel(40, 40, Rgba([250, 250, 250, 255]));
        for y in 10..30 {
            for x in 10..30 {
                img.put_pixel(x, y, Rgba([200, 20, 20, 255]));
            }
        }
        let cut = remove_background(&img);
        assert_eq!(cut.get_pixel(0, 0)[3], 0);
        assert_eq!(cut.get_pixel(20, 20)[3], 255);
        let font = include_bytes!("../assets/fonts/Inter-Regular.ttf");
        let a = Annot::new(Shape::Rect(egui::Rect::from_min_size(egui::pos2(2.0, 2.0), egui::vec2(10.0, 10.0))), crate::model::Style::default());
        let t = Annot::new(
            Shape::Text { rect: egui::Rect::from_min_size(egui::pos2(0.0, 20.0), egui::vec2(40.0, 20.0)), text: "Hi".into() },
            crate::model::Style { font_size: 14.0, ..Default::default() },
        );
        let out = burn_annotations(&img, &[a, t], font);
        assert_ne!(out.get_pixel(2, 7), img.get_pixel(2, 7));
    }
}

#[cfg(test)]
mod sample_tests {
    /// Decode real files when provided, e.g. REVIEW_SAMPLE_IMAGES=a.dng:b.jxl
    #[test]
    fn decodes_sample_files() {
        let Ok(list) = std::env::var("REVIEW_SAMPLE_IMAGES") else { return };
        for p in list.split(':').filter(|p| !p.is_empty()) {
            let l = super::load(std::path::Path::new(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
            let f = &l.frames[0];
            assert!(f.width() > 10 && f.height() > 10, "{p}");
            eprintln!("{p}: {} {}x{}", l.format, f.width(), f.height());
        }
    }
}
