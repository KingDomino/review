//! Import from Scanner (SANE's `scanimage`).

use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub label: String,
}

/// Scanners SANE can see. Slow (network scanners are probed), so call off the UI thread.
pub fn devices() -> Result<Vec<Device>, String> {
    let out = Command::new("scanimage")
        .args(["-f", "%d|%v %m%n"])
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "Scanning needs SANE. Install it with: sudo dnf install sane-backends sane-frontends".to_string())?;
    Ok(parse_devices(&String::from_utf8_lossy(&out.stdout)))
}

pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|l| {
            let (id, label) = l.split_once('|')?;
            let id = id.trim();
            (!id.is_empty()).then(|| Device { id: id.to_string(), label: label.trim().to_string() })
        })
        .collect()
}

/// Scan one page. `mode` is "Color", "Gray" or "Lineart".
pub fn scan(device: &str, dpi: u32, mode: &str) -> Result<image::RgbaImage, String> {
    let out = Command::new("scanimage")
        .args(["-d", device, "--format=png", "--resolution", &dpi.to_string(), "--mode", mode])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "The scanner didn't return an image.".into() } else { err });
    }
    image::load_from_memory(&out.stdout).map(|i| i.to_rgba8()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_scanimage_list() {
        let d = super::parse_devices("epson2:net:192.168.1.20|Epson PID 08CC\nhpaio:/usb/Deskjet|HP Deskjet 3630\n\n");
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].id, "hpaio:/usb/Deskjet");
        assert_eq!(d[0].label, "Epson PID 08CC");
    }
}
