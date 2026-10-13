//! AVIF (and HEIF with codecs other than HEVC) on Windows through the Windows Imaging Component,
//! i.e. the codecs the Photos app uses. HEIC photos are decoded in-process by `heic-decoder`.

use windows::core::HSTRING;
use windows::Win32::Foundation::WINCODEC_ERR_COMPONENTNOTFOUND;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::VT_UI2;

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// EXIF-style orientation (1 = upright); WIC leaves applying it to the app.
    pub orientation: u16,
    pub icc: Option<Vec<u8>>,
}

pub fn decode(bytes: &[u8], avif: bool) -> Result<Decoded, String> {
    // SAFETY: plain COM calls on this thread; every interface is released when dropped.
    unsafe {
        // Image-decoding threads have no COM apartment yet; an existing one is fine too.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
        let stream = factory.CreateStream().map_err(|e| e.message())?;
        stream.InitializeFromMemory(bytes).map_err(|e| e.message())?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(|e| {
            if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
                missing_codec(avif)
            } else {
                e.message()
            }
        })?;
        let frame = decoder.GetFrame(0).map_err(|e| missing_or(e, avif))?;
        let converter = factory.CreateFormatConverter().map_err(|e| e.message())?;
        converter
            .Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(|e| missing_or(e, avif))?;
        let (mut width, mut height) = (0u32, 0u32);
        converter.GetSize(&mut width, &mut height).map_err(|e| e.message())?;
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut rgba).map_err(|e| missing_or(e, avif))?;
        Ok(Decoded { width, height, rgba, orientation: orientation(&frame), icc: icc_profile(&factory, &frame) })
    }
}

/// The decoder is present but the video codec it relies on (HEVC / AV1) isn't.
fn missing_or(e: windows::core::Error, avif: bool) -> String {
    if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
        missing_codec(avif)
    } else {
        e.message()
    }
}

fn missing_codec(avif: bool) -> String {
    if avif {
        "Opening AVIF images needs Microsoft’s free AV1 Video Extension.".into()
    } else {
        "Opening HEIC photos needs Microsoft’s HEIF Image Extensions and HEVC Video Extensions.".into()
    }
}

/// Microsoft Store pages for the codecs named in `missing_codec`'s message.
pub fn codec_links(error: &str) -> &'static [(&'static str, &'static str)] {
    if error.contains("AV1 Video Extension") {
        &[("Get AV1 Video Extension", "ms-windows-store://pdp/?ProductId=9MVZQVXJBQ9V")]
    } else if error.contains("HEVC Video Extensions") {
        &[
            ("Get HEIF Image Extensions", "ms-windows-store://pdp/?ProductId=9PMMSR1CGPWG"),
            ("Get HEVC Video Extensions", "ms-windows-store://pdp/?ProductId=9NMZLZ57R3T7"),
        ]
    } else {
        &[]
    }
}

unsafe fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    let Ok(reader) = frame.GetMetadataQueryReader() else { return 1 };
    let mut v = PROPVARIANT::default();
    if reader.GetMetadataByName(&HSTRING::from("System.Photo.Orientation"), &mut v).is_err() {
        return 1;
    }
    let inner = &v.Anonymous.Anonymous;
    if inner.vt == VT_UI2 {
        inner.Anonymous.uiVal.clamp(1, 8)
    } else {
        1
    }
}

unsafe fn icc_profile(factory: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> Option<Vec<u8>> {
    let mut count = 0u32;
    frame.GetColorContexts(&mut [], &mut count).ok()?;
    if count == 0 {
        return None;
    }
    let mut contexts: Vec<Option<IWICColorContext>> = (0..count).map(|_| factory.CreateColorContext().ok()).collect();
    frame.GetColorContexts(&mut contexts, &mut count).ok()?;
    for c in contexts.into_iter().flatten() {
        if c.GetType().ok()? != WICColorContextProfile {
            continue;
        }
        let mut len = 0u32;
        c.GetProfileBytes(&mut [], &mut len).ok()?;
        let mut buf = vec![0u8; len as usize];
        c.GetProfileBytes(&mut buf, &mut len).ok()?;
        buf.truncate(len as usize);
        return Some(buf);
    }
    None
}
