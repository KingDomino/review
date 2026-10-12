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

pub struct Printer {
    pub name: String,
    /// Printers that write a file instead of printing (Microsoft Print to PDF, XPS Document
    /// Writer) need a file name up front: their own "save as" prompt can't appear when
    /// printing from the background. The extension of the file they write.
    pub file_ext: Option<&'static str>,
}

/// Printers, with the default first.
#[cfg(windows)]
pub fn printers() -> Vec<Printer> {
    let out = powershell("Get-CimInstance Win32_Printer | Sort-Object -Property Default -Descending | ForEach-Object { $_.Name + [char]9 + $_.PortName + [char]9 + $_.DriverName }");
    out.map(|o| parse_windows_printers(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default()
}

/// Lines of `name<TAB>port<TAB>driver` from Win32_Printer.
#[cfg(any(windows, test))]
fn parse_windows_printers(text: &str) -> Vec<Printer> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        let (name, port, driver) = (f[0].trim(), f.get(1).map(|s| s.trim()).unwrap_or(""), f.get(2).map(|s| s.to_lowercase()).unwrap_or_default());
        // Fax opens its own wizard and can't be driven from here.
        if name.is_empty() || port.eq_ignore_ascii_case("SHRFAX:") {
            continue;
        }
        let file_ext = if driver.contains("print to pdf") {
            Some("pdf")
        } else if driver.contains("xps document writer") {
            Some("oxps")
        } else if port.eq_ignore_ascii_case("PORTPROMPT:") || port.eq_ignore_ascii_case("FILE:") {
            Some("prn")
        } else {
            None
        };
        out.push(Printer { name: name.to_string(), file_ext });
    }
    out
}

/// Run a script from `printing::windows_print_script` (photos placed at exact sizes).
#[cfg(windows)]
pub fn print_placed(script: &str) -> Result<(), String> {
    let o = powershell(script).map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&o.stderr).lines().find(|l| !l.trim().is_empty()).unwrap_or("PowerShell failed").trim().to_string();
        if err.contains("StartDocPrinter") {
            // ERROR_SPL_NO_STARTDOC: the spooler refused to start the job.
            return Err("the printer didn’t accept the job. Check that it’s online and not paused in Settings ▸ Printers & scanners.".into());
        }
        Err(err)
    }
}

/// The paper the printer is set up for, else the usual paper in the user's region.
pub fn default_paper(printer: &str) -> usize {
    printer_paper(printer)
        .and_then(|n| crate::printing::paper_by_name(&n))
        .unwrap_or_else(region_paper)
}

/// US Letter or A4, by the user's region.
pub fn region_paper() -> usize {
    if crate::printing::letter_region(&region()) { 0 } else { 4 }
}

#[cfg(windows)]
fn printer_paper(printer: &str) -> Option<String> {
    if printer.is_empty() {
        return None;
    }
    let o = powershell(&format!("(Get-PrintConfiguration -PrinterName '{}').PaperSize", printer.replace('\'', "''"))).ok()?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(windows)]
fn region() -> String {
    windows::Globalization::GeographicRegion::new().and_then(|r| r.CodeTwoLetter()).map(|s| s.to_string()).unwrap_or_default()
}

#[cfg(windows)]
pub fn print_file(file: &Path, printer: &str, copies: u32, _ranges: &str, _fit: bool, _two_sided: bool, _media: Option<&str>) -> Result<(), String> {
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

/// The default paper in the printer's CUPS options (`lpoptions -l` marks it with `*`).
#[cfg(not(windows))]
fn printer_paper(printer: &str) -> Option<String> {
    let mut c = Command::new("lpoptions");
    if !printer.is_empty() {
        c.args(["-p", printer]);
    }
    let o = c.arg("-l").output().ok()?;
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let line = text.lines().find(|l| l.starts_with("PageSize") || l.starts_with("media"))?;
    line.split_whitespace().find_map(|w| w.strip_prefix('*')).map(str::to_string)
}

/// Two-letter region from the locale (`en_US.UTF-8` → `US`).
#[cfg(not(windows))]
fn region() -> String {
    ["LC_PAPER", "LC_ALL", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .and_then(|v| v.split(['.', '@']).next().and_then(|l| l.split('_').nth(1)).map(str::to_string))
        .unwrap_or_default()
}

/// Printers known to CUPS, with the default first.
#[cfg(not(windows))]
pub fn printers() -> Vec<Printer> {
    cups_printers().into_iter().map(|name| Printer { name, file_ext: None }).collect()
}

#[cfg(not(windows))]
fn cups_printers() -> Vec<String> {
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

/// Print a PDF with CUPS. With `media`, the pages are already laid out for that paper and are
/// printed at 100% (photo printing); otherwise `fit` scales them to the printer's paper.
#[cfg(not(windows))]
pub fn print_file(file: &Path, printer: &str, copies: u32, ranges: &str, fit: bool, two_sided: bool, media: Option<&str>) -> Result<(), String> {
    let mut c = Command::new("lp");
    if !printer.is_empty() {
        c.args(["-d", printer]);
    }
    c.args(["-n", &copies.max(1).to_string()]);
    if !ranges.trim().is_empty() {
        c.args(["-P", ranges.trim()]);
    }
    if let Some(m) = media {
        c.args(["-o", &format!("media={m}"), "-o", "print-scaling=none"]);
    } else if fit {
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

#[cfg(test)]
mod tests {
    #[test]
    fn windows_printer_list() {
        let text = "Microsoft Print to PDF\tPORTPROMPT:\tMicrosoft Print To PDF\r\nOffice Laser\t192.168.1.20\tHP Universal Printing PCL 6\r\nFax\tSHRFAX:\tMicrosoft Shared Fax Driver\r\nMicrosoft XPS Document Writer\tPORTPROMPT:\tMicrosoft XPS Document Writer v4\r\n";
        let p = super::parse_windows_printers(text);
        let got: Vec<(&str, Option<&str>)> = p.iter().map(|p| (p.name.as_str(), p.file_ext)).collect();
        assert_eq!(got, vec![("Microsoft Print to PDF", Some("pdf")), ("Office Laser", None), ("Microsoft XPS Document Writer", Some("oxps"))]);
    }
}
