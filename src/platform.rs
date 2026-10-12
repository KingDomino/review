//! Desktop integration: screenshots, speech, printing, opening windows.

use std::path::{Path, PathBuf};
use std::process::Command;

use crossbeam_channel::Sender;

use crate::app::Msg;

pub fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file() || d.join(format!("{cmd}.exe")).is_file()))
        .unwrap_or(false)
}

/// Run a PowerShell snippet without flashing a console window (Windows).
#[cfg(windows)]
pub fn powershell(script: &str) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
}

/// Reveal a file in the system file manager.
pub fn show_in_folder(path: &Path) {
    #[cfg(windows)]
    {
        let _ = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = path.parent() {
            let _ = Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

/// kind: 0 = selection, 1 = window, 2 = entire screen
#[cfg(windows)]
pub fn screenshot(kind: u8, tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let result = if kind == 2 {
            // Whole screen via .NET.
            let out = std::env::temp_dir().join(format!("review-shot-{}.png", std::process::id()));
            let script = format!(
                "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $b=[System.Windows.Forms.SystemInformation]::VirtualScreen; $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; $g=[System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($b.Left,$b.Top,0,0,$bmp.Size); $bmp.Save('{}')",
                out.display()
            );
            let _ = powershell(&script);
            let r = image::open(&out).map(|i| i.to_rgba8()).map_err(|e| e.to_string());
            let _ = std::fs::remove_file(&out);
            r
        } else {
            // Selection / window: Windows' own snipping UI, then pick the result up from the clipboard.
            let before = arboard::Clipboard::new().and_then(|mut c| c.get_image()).map(|i| i.bytes.len()).unwrap_or(0);
            let _ = Command::new("explorer").arg("ms-screenclip:").spawn();
            let start = std::time::Instant::now();
            let mut got = Err("Screenshot cancelled.".to_string());
            while start.elapsed() < std::time::Duration::from_secs(90) {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if let Ok(img) = arboard::Clipboard::new().and_then(|mut c| c.get_image()) {
                    if img.bytes.len() != before {
                        got = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()).ok_or_else(|| "bad clipboard image".into());
                        break;
                    }
                }
            }
            got
        };
        let _ = tx.send(match result {
            Ok(img) => Msg::NewImage(img, "Screenshot".into()),
            Err(e) => Msg::Error(e),
        });
        ctx.request_repaint();
    });
}

#[cfg(not(windows))]
pub fn screenshot(kind: u8, tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let out = std::env::temp_dir().join(format!("review-shot-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let o = out.to_string_lossy().to_string();
        let attempts: Vec<Vec<String>> = vec![
            {
                let mode = ["-r", "-u", "-f"][kind as usize];
                let mut v = vec!["spectacle".into(), "-b".into(), "-n".into(), mode.into()];
                if kind == 1 {
                    v.extend(["-d".into(), "1500".into()]);
                }
                v.extend(["-o".into(), o.clone()]);
                v
            },
            {
                let mode = ["-a", "-w", ""][kind as usize];
                let mut v = vec!["gnome-screenshot".to_string()];
                if !mode.is_empty() {
                    v.push(mode.into());
                }
                v.extend(["-f".into(), o.clone()]);
                v
            },
            if kind == 0 { vec!["sh".into(), "-c".into(), format!("grim -g \"$(slurp)\" '{o}'")] } else { vec!["grim".into(), o.clone()] },
            vec!["scrot".into(), if kind == 0 { "-s".into() } else if kind == 1 { "-u".into() } else { "-z".into() }, o.clone()],
        ];
        let mut tried = false;
        for a in attempts {
            let bin = if a[0] == "sh" { "grim" } else { a[0].as_str() };
            if !have(bin) {
                continue;
            }
            tried = true;
            let _ = Command::new(&a[0]).args(&a[1..]).status();
            if out.exists() {
                break;
            }
        }
        if !out.exists() {
            if !tried {
                let _ = tx.send(Msg::Error("No screenshot tool found. Install Spectacle (KDE), gnome-screenshot, or grim + slurp.".into()));
            }
            ctx.request_repaint();
            return;
        }
        match image::open(&out) {
            Ok(img) => {
                let _ = tx.send(Msg::NewImage(img.to_rgba8(), "Screenshot".into()));
            }
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("Couldn’t read the screenshot: {e}")));
            }
        }
        let _ = std::fs::remove_file(&out);
        ctx.request_repaint();
    });
}

pub fn speak(text: &str) {
    let text: String = text.chars().take(20_000).collect();
    #[cfg(windows)]
    {
        let t = text.replace('\'', "''");
        let script = format!("Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('{t}')");
        std::thread::spawn(move || {
            let _ = powershell(&script);
        });
        return;
    }
    #[allow(unreachable_code)]
    if have("spd-say") {
        let _ = Command::new("spd-say").arg("--").arg(text).spawn();
    } else if have("espeak-ng") {
        let _ = Command::new("espeak-ng").arg(text).spawn();
    }
}

pub fn stop_speaking() {
    #[cfg(windows)]
    {
        let _ = powershell("Get-Process powershell -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $PID } | Stop-Process -ErrorAction SilentlyContinue");
    }
    if have("spd-say") {
        let _ = Command::new("spd-say").arg("-S").spawn();
    }
}

pub fn render_page_file(path: &Path, password: Option<&str>, index: u32, dpi: f32) -> Result<image::RgbaImage, String> {
    crate::pdf::render_page_image(path, password, index, dpi / 72.0)
}

/// Printer names, with the default first.
#[cfg(windows)]
pub fn printers() -> Vec<String> {
    let out = powershell("Get-Printer | Sort-Object -Property @{Expression={$_.Default}} -Descending | ForEach-Object { $_.Name }");
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

#[cfg(windows)]
pub fn print_file(file: &Path, printer: &str, copies: u32, _ranges: &str, _fit: bool, _two_sided: bool) -> Result<(), String> {
    // Hand the PDF to the system's PDF handler; PrintTo targets a specific printer.
    let f = file.display().to_string().replace('\'', "''");
    let p = printer.replace('\'', "''");
    for _ in 0..copies.max(1) {
        let script = if p.is_empty() {
            format!("Start-Process -FilePath '{f}' -Verb Print -Wait")
        } else {
            format!("try {{ Start-Process -FilePath '{f}' -Verb PrintTo -ArgumentList '\"{p}\"' -Wait -ErrorAction Stop }} catch {{ Start-Process -FilePath '{f}' -Verb Print -Wait }}")
        };
        let o = powershell(&script).map_err(|e| e.to_string())?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
    }
    Ok(())
}

/// Printer names known to CUPS, with the default first.
#[cfg(not(windows))]
pub fn printers() -> Vec<String> {
    let mut out = Vec::new();
    let default = Command::new("lpstat").arg("-d").output().ok().and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        s.split(':').nth(1).map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
    });
    if let Ok(o) = Command::new("lpstat").arg("-a").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(name) = line.split_whitespace().next() {
                out.push(name.to_string());
            }
        }
    }
    if let Some(d) = default {
        out.retain(|p| p != &d);
        out.insert(0, d);
    }
    out
}

#[cfg(not(windows))]
pub fn print_file(file: &Path, printer: &str, copies: u32, ranges: &str, fit: bool, two_sided: bool) -> Result<(), String> {
    let mut c = Command::new("lp");
    if !printer.is_empty() {
        c.args(["-d", printer]);
    }
    c.args(["-n", &copies.max(1).to_string()]);
    if !ranges.trim().is_empty() {
        c.args(["-P", ranges.trim()]);
    }
    if fit {
        c.args(["-o", "fit-to-page"]);
    }
    if two_sided {
        c.args(["-o", "sides=two-sided-long-edge"]);
    }
    let out = c.arg(file).output().map_err(|e| format!("Couldn’t run lp: {e}. Install CUPS (cups-client)."))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// Windows: like Preview, every document group gets its own window. Each window is
// its own process, so one can't take the others down with it.

/// Open `paths` in a new Review window, cascaded from `near` (the current window's outer rect).
pub fn open_window(paths: &[PathBuf], near: Option<egui::Rect>) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.arg("--").args(paths);
    match near {
        Some(r) => cmd.env(CASCADE_ENV, format!("{} {} {} {}", r.min.x + 28.0, r.min.y + 28.0, r.width(), r.height())),
        None => cmd.env_remove(CASCADE_ENV),
    };
    if let Ok(mut child) = cmd.spawn() {
        // Reap it when it exits so it doesn't linger as a zombie while this window stays open.
        std::thread::spawn(move || child.wait());
    }
}

pub const CASCADE_ENV: &str = "REVIEW_CASCADE";

/// Position and size for a window opened by `open_window`.
pub fn cascade() -> Option<egui::Rect> {
    let v = std::env::var(CASCADE_ENV).ok()?;
    let n: Vec<f32> = v.split(' ').filter_map(|x| x.parse().ok()).collect();
    (n.len() == 4).then(|| egui::Rect::from_min_size(egui::pos2(n[0], n[1]), egui::vec2(n[2], n[3])))
}

/// File chooser via kdialog or zenity, for systems without xdg-desktop-portal.
pub fn pick_files_fallback(dir: Option<&Path>) -> Vec<PathBuf> {
    let dir = dir.map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
    let out = if have("kdialog") {
        Command::new("kdialog").args(["--multiple", "--separate-output", "--getopenfilename", &dir]).output().ok()
    } else if have("zenity") {
        Command::new("zenity").args(["--file-selection", "--multiple", "--separator=\n", "--filename", &format!("{dir}/")]).output().ok()
    } else {
        None
    };
    out.filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}
