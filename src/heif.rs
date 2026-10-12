//! HEIC / HEIF / AVIF decoding through the system libheif, loaded at runtime.
//!
//! Loading it dynamically keeps libheif out of the build: Review starts fine
//! without it and gains HEIC support as soon as the library is installed
//! (`libheif` on Fedora; HEVC photos also need `libheif-freeworld` from RPM Fusion).

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

use libloading::{Library, Symbol};

#[repr(C)]
#[derive(Clone, Copy)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

const HEIF_COLORSPACE_RGB: c_int = 1;
const HEIF_CHROMA_INTERLEAVED_RGBA: c_int = 11;
const HEIF_CHANNEL_INTERLEAVED: c_int = 10;

fn lib() -> Option<&'static Library> {
    static LIB: OnceLock<Option<Library>> = OnceLock::new();
    LIB.get_or_init(|| {
        for name in ["libheif.so.1", "libheif.so", "libheif.1.dylib", "libheif.dylib", "/opt/homebrew/lib/libheif.dylib"] {
            // SAFETY: loading a well-known system library with no special init requirements.
            if let Ok(l) = unsafe { Library::new(name) } {
                return Some(l);
            }
        }
        None
    })
    .as_ref()
}

fn check(e: HeifError) -> Result<(), String> {
    if e.code == 0 {
        return Ok(());
    }
    let msg = if e.message.is_null() {
        format!("libheif error {}", e.code)
    } else {
        // SAFETY: libheif returns a static NUL-terminated message.
        unsafe { CStr::from_ptr(e.message) }.to_string_lossy().into_owned()
    };
    Err(msg)
}

/// Decode the primary image to RGBA. Returns (width, height, pixels, has_alpha).
pub fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>, bool), String> {
    let lib = lib().ok_or(
        "HEIC/AVIF needs libheif. Install it with: sudo dnf install libheif (and libheif-freeworld from RPM Fusion for iPhone photos)",
    )?;
    // SAFETY: the signatures below match libheif's public C API (heif.h, stable since 1.x).
    unsafe {
        type Alloc = unsafe extern "C" fn() -> *mut c_void;
        type Free = unsafe extern "C" fn(*mut c_void);
        type Read = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *const c_void) -> HeifError;
        type Primary = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> HeifError;
        type HasAlpha = unsafe extern "C" fn(*const c_void) -> c_int;
        type Decode = unsafe extern "C" fn(*const c_void, *mut *mut c_void, c_int, c_int, *const c_void) -> HeifError;
        type Plane = unsafe extern "C" fn(*const c_void, c_int, *mut c_int) -> *const u8;
        type Dim = unsafe extern "C" fn(*const c_void, c_int) -> c_int;
        type Release = unsafe extern "C" fn(*const c_void);

        let sym = |n: &[u8]| -> Result<(), String> {
            let _: Symbol<*const c_void> = lib.get(n).map_err(|e| format!("libheif is too old: {e}"))?;
            Ok(())
        };
        sym(b"heif_context_read_from_memory_without_copy\0")?;
        let alloc: Symbol<Alloc> = lib.get(b"heif_context_alloc\0").map_err(|e| e.to_string())?;
        let free: Symbol<Free> = lib.get(b"heif_context_free\0").map_err(|e| e.to_string())?;
        let read: Symbol<Read> = lib.get(b"heif_context_read_from_memory_without_copy\0").map_err(|e| e.to_string())?;
        let primary: Symbol<Primary> = lib.get(b"heif_context_get_primary_image_handle\0").map_err(|e| e.to_string())?;
        let has_alpha: Symbol<HasAlpha> = lib.get(b"heif_image_handle_has_alpha_channel\0").map_err(|e| e.to_string())?;
        let decode: Symbol<Decode> = lib.get(b"heif_decode_image\0").map_err(|e| e.to_string())?;
        let plane: Symbol<Plane> = lib.get(b"heif_image_get_plane_readonly\0").map_err(|e| e.to_string())?;
        let width: Symbol<Dim> = lib.get(b"heif_image_get_width\0").map_err(|e| e.to_string())?;
        let height: Symbol<Dim> = lib.get(b"heif_image_get_height\0").map_err(|e| e.to_string())?;
        let release_handle: Symbol<Release> = lib.get(b"heif_image_handle_release\0").map_err(|e| e.to_string())?;
        let release_image: Symbol<Release> = lib.get(b"heif_image_release\0").map_err(|e| e.to_string())?;

        let ctx = alloc();
        if ctx.is_null() {
            return Err("libheif: out of memory".into());
        }
        let result = (|| {
            check(read(ctx, bytes.as_ptr().cast(), bytes.len(), std::ptr::null()))?;
            let mut handle: *mut c_void = std::ptr::null_mut();
            check(primary(ctx, &mut handle))?;
            let alpha = has_alpha(handle) != 0;
            let mut img: *mut c_void = std::ptr::null_mut();
            let r = check(decode(handle, &mut img, HEIF_COLORSPACE_RGB, HEIF_CHROMA_INTERLEAVED_RGBA, std::ptr::null()));
            release_handle(handle);
            r.map_err(|e| format!("{e}. iPhone (HEVC) photos need libheif-freeworld from RPM Fusion."))?;
            let w = width(img, HEIF_CHANNEL_INTERLEAVED);
            let h = height(img, HEIF_CHANNEL_INTERLEAVED);
            let mut stride: c_int = 0;
            let p = plane(img, HEIF_CHANNEL_INTERLEAVED, &mut stride);
            if p.is_null() || w <= 0 || h <= 0 {
                release_image(img);
                return Err("libheif returned no pixels".to_string());
            }
            let (w, h, stride) = (w as usize, h as usize, stride as usize);
            let mut out = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts(p.add(row * stride), w * 4));
            }
            release_image(img);
            Ok((w as u32, h as u32, out, alpha))
        })();
        free(ctx);
        result
    }
}
