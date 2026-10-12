//! Colour management with Little CMS: embedded ICC profiles, Assign Profile and Soft Proof.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use lcms2::{DisallowCache, Flags, GlobalContext, Intent, PixelFormat, Profile, Transform};

/// Cache-free transforms can be shared between threads.
type SharedTransform = Transform<[u8; 4], [u8; 4], GlobalContext, DisallowCache>;

fn shared(src: &Profile, dst: &Profile) -> Option<SharedTransform> {
    Transform::new_flags_context(GlobalContext::new(), src, PixelFormat::RGBA_8, dst, PixelFormat::RGBA_8, Intent::Perceptual, Flags::NO_CACHE).ok()
}

/// An ICC profile found on the system.
#[derive(Clone, Debug)]
pub struct IccProfile {
    pub name: String,
    pub path: PathBuf,
    pub cmyk: bool,
}

/// Directories where Linux (colord, Ghostscript, users) and macOS keep ICC profiles.
fn profile_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        "/usr/share/color/icc",
        "/usr/local/share/color/icc",
        "/var/lib/colord/icc",
        "/usr/share/ghostscript/iccprofiles",
        "/System/Library/ColorSync/Profiles",
        "/Library/ColorSync/Profiles",
        "C:\\Windows\\System32\\spool\\drivers\\color",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("icc"));
    }
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".color/icc"));
    }
    v
}

/// RGB and CMYK display/output profiles available for Soft Proof and Assign Profile (cached).
pub fn system_profiles() -> &'static [IccProfile] {
    static LIST: OnceLock<Vec<IccProfile>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut out: Vec<IccProfile> = Vec::new();
        let mut stack = profile_dirs();
        let mut seen = 0;
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if ext != "icc" && ext != "icm" {
                    continue;
                }
                seen += 1;
                if seen > 400 {
                    break;
                }
                let Ok(bytes) = std::fs::read(&p) else { continue };
                let Ok(prof) = Profile::new_icc(&bytes) else { continue };
                use lcms2::ColorSpaceSignature as C;
                let cmyk = match prof.color_space() {
                    C::RgbData => false,
                    C::CmykData => true,
                    _ => continue,
                };
                let name = prof
                    .info(lcms2::InfoType::Description, lcms2::Locale::none())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
                if !out.iter().any(|o| o.name == name) {
                    out.push(IccProfile { name, path: p, cmyk });
                }
            }
        }
        out.sort_by(|a, b| a.cmyk.cmp(&b.cmyk).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        out
    })
}

pub fn profile_name(icc: &[u8]) -> Option<String> {
    Profile::new_icc(icc).ok()?.info(lcms2::InfoType::Description, lcms2::Locale::none()).filter(|s| !s.trim().is_empty())
}

fn is_srgb(p: &Profile) -> bool {
    p.info(lcms2::InfoType::Description, lcms2::Locale::none())
        .map(|d| d.to_lowercase().contains("srgb"))
        .unwrap_or(false)
}

/// Convert pixels encoded in `icc` (an RGB or grey profile) to sRGB in place.
/// Returns false when the profile is already sRGB or can't be used.
pub fn to_srgb(img: &mut RgbaImage, icc: &[u8]) -> bool {
    let Ok(src) = Profile::new_icc(icc) else { return false };
    if is_srgb(&src) || src.color_space() != lcms2::ColorSpaceSignature::RgbData {
        return false;
    }
    let Some(t) = shared(&src, &Profile::new_srgb()) else { return false };
    transform_parallel(img, &t);
    true
}

/// Interpret the pixels as being in the profile at `path` (Tools ▸ Assign Profile).
pub fn assign(img: &RgbaImage, path: &Path) -> Result<RgbaImage, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let src = Profile::new_icc(&bytes).map_err(|e| e.to_string())?;
    if src.color_space() != lcms2::ColorSpaceSignature::RgbData {
        return Err("Only RGB profiles can be assigned to an image.".into());
    }
    let t = shared(&src, &Profile::new_srgb()).ok_or("The profile can't be used for conversion.")?;
    let mut out = img.clone();
    transform_parallel(&mut out, &t);
    Ok(out)
}

fn transform_parallel(img: &mut RgbaImage, t: &SharedTransform) {
    let px: &mut [[u8; 4]] = bytemuck::cast_slice_mut(img.as_mut());
    // Little CMS transforms are safe to share once built; split the work across cores.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let chunk = px.len().div_ceil(threads).max(4096);
    std::thread::scope(|s| {
        for c in px.chunks_mut(chunk) {
            s.spawn(move || t.transform_in_place(c));
        }
    });
}

/// View ▸ Soft Proof with Profile: simulates how sRGB content will look when reproduced
/// on the device described by the profile (printer, press, display).
pub struct Proof {
    pub name: String,
    pub generation: u64,
    transform: SharedTransform,
}

impl Proof {
    pub fn new(profile: &IccProfile, generation: u64) -> Result<Arc<Proof>, String> {
        let bytes = std::fs::read(&profile.path).map_err(|e| e.to_string())?;
        let device = Profile::new_icc(&bytes).map_err(|e| e.to_string())?;
        let srgb = Profile::new_srgb();
        let transform = Transform::new_proofing_context(
            GlobalContext::new(),
            &srgb,
            PixelFormat::RGBA_8,
            &srgb,
            PixelFormat::RGBA_8,
            &device,
            Intent::Perceptual,
            Intent::RelativeColorimetric,
            Flags::NO_CACHE | Flags::SOFT_PROOFING | Flags::BLACKPOINT_COMPENSATION,
        )
        .map_err(|e| e.to_string())?;
        Ok(Arc::new(Proof { name: profile.name.clone(), generation, transform }))
    }

    pub fn apply_rgba(&self, rgba: &mut [u8]) {
        let px: &mut [[u8; 4]] = bytemuck::cast_slice_mut(rgba);
        self.transform.transform_in_place(px);
    }

    pub fn apply(&self, img: &RgbaImage) -> RgbaImage {
        let mut out = img.clone();
        transform_parallel(&mut out, &self.transform);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_proof_and_assign() {
        let profiles = system_profiles();
        let Some(cmyk) = profiles.iter().find(|p| p.cmyk) else {
            eprintln!("no CMYK profiles installed; skipping");
            return;
        };
        // Saturated RGB blue is out of gamut for print: proofing must change it.
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255]));
        let proof = Proof::new(cmyk, 1).unwrap();
        let out = proof.apply(&img);
        assert_ne!(out.get_pixel(0, 0), img.get_pixel(0, 0), "proof via {}", cmyk.name);
        if let Some(rgb) = profiles.iter().find(|p| !p.cmyk && !p.name.to_lowercase().contains("srgb")) {
            let a = assign(&img, &rgb.path).unwrap();
            assert_eq!(a.dimensions(), (4, 4));
        }
    }
}
