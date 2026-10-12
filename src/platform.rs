//! Desktop integration: screenshots, speech, printing, single-instance hand-off.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::app::Msg;

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// kind: 0 = selection, 1 = window, 2 = entire screen
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
    if have("spd-say") {
        let _ = Command::new("spd-say").arg("--").arg(text).spawn();
    } else if have("espeak-ng") {
        let _ = Command::new("espeak-ng").arg(text).spawn();
    }
}

pub fn stop_speaking() {
    if have("spd-say") {
        let _ = Command::new("spd-say").arg("-S").spawn();
    }
}

pub fn render_page_file(path: &Path, password: Option<&str>, index: u32, dpi: f32) -> Result<image::RgbaImage, String> {
    crate::pdf::render_page_image(path, password, index, dpi / 72.0)
}

/// Printer names known to CUPS, with the default first.
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
// Single instance: the first Review listens on a socket; later launches hand
// their files over and exit, so files open as tabs in the existing window.

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_default();
    dir.join(format!("review-{user}.sock"))
}

#[cfg(unix)]
pub fn hand_off(paths: &[PathBuf]) -> bool {
    use std::os::unix::net::UnixStream;
    let Ok(mut s) = UnixStream::connect(socket_path()) else { return false };
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let mut msg = String::from("OPEN\n");
    for p in paths {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        msg.push_str(&p.to_string_lossy());
        msg.push('\n');
    }
    msg.push('\n');
    s.write_all(msg.as_bytes()).is_ok()
}

#[cfg(unix)]
pub fn listen() -> Option<Receiver<Vec<PathBuf>>> {
    use std::os::unix::net::UnixListener;
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    let (tx, rx) = unbounded();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut paths = Vec::new();
            let reader = BufReader::new(stream);
            for (i, line) in reader.lines().map_while(Result::ok).enumerate() {
                if i == 0 {
                    if line != "OPEN" {
                        break;
                    }
                    continue;
                }
                if line.is_empty() {
                    break;
                }
                paths.push(PathBuf::from(line));
            }
            if tx.send(paths).is_err() {
                break;
            }
        }
    });
    Some(rx)
}

#[cfg(not(unix))]
pub fn hand_off(_: &[PathBuf]) -> bool {
    false
}

#[cfg(not(unix))]
pub fn listen() -> Option<Receiver<Vec<PathBuf>>> {
    None
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

pub fn cleanup_socket() {
    let _ = std::fs::remove_file(socket_path());
}
