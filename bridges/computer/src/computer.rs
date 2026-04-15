//! Computer interaction via X11 tools (xdotool, ImageMagick import / xwd fallback).
//! Adapted from the main omar crate's computer module for standalone use.

use anyhow::{Context, Result};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// ── X11 environment detection ─────────────────────────────────────────────────

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

fn command_output_with_timeout(mut cmd: Command) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if start.elapsed() < PROBE_TIMEOUT => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Return the path of the Xauthority file to use, checking env then ~/.Xauthority.
fn find_xauthority_file() -> String {
    if let Ok(x) = std::env::var("XAUTHORITY") {
        if !x.is_empty() && std::path::Path::new(&x).exists() {
            return x;
        }
    }
    let uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u32>().ok())
        })
        .unwrap_or(1000);
    for candidate in [
        format!("/run/user/{}/gdm/Xauthority", uid),
        format!("/run/user/{}/.mutter-Xwaylandauth", uid),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return candidate;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{}/.Xauthority", home);
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    String::new()
}

/// Ask tmux for the DISPLAY value it holds in the session environment.
fn tmux_get_display() -> Option<String> {
    let mut cmd = Command::new("tmux");
    cmd.args(["show-environment", "DISPLAY"]);
    let output = command_output_with_timeout(cmd)?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout);
        for line in s.lines() {
            if let Some(val) = line.strip_prefix("DISPLAY=") {
                let v = val.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Parse `xauth list` output and return candidate DISPLAY strings.
/// Each xauth entry like `hostname/unix:11` yields both `:11` (local) and
/// `localhost:11.0` (TCP), giving the prober more options to try.
fn xauth_list_displays(xauth_file: &str) -> Vec<String> {
    let mut cmd = Command::new("xauth");
    if !xauth_file.is_empty() {
        cmd.args(["-f", xauth_file]);
    }
    cmd.arg("list");
    let output = match command_output_with_timeout(cmd) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let s = String::from_utf8_lossy(&output.stdout);
    let mut displays = Vec::new();
    for line in s.lines() {
        // Format: "hostname/unix:N  MIT-MAGIC-COOKIE-1  ..."
        let Some(display_key) = line.split_whitespace().next() else {
            continue;
        };
        // Extract the number after the last ':'
        let Some(num_str) = display_key.rsplit(':').next() else {
            continue;
        };
        let Ok(num) = num_str.parse::<u32>() else {
            continue;
        };
        // Local unix socket form: :N
        let d_local = format!(":{}", num);
        if !displays.contains(&d_local) {
            displays.push(d_local);
        }
        // TCP form: localhost:N.0
        let d_tcp = format!("localhost:{}.0", num);
        if !displays.contains(&d_tcp) {
            displays.push(d_tcp);
        }
    }
    displays
}

/// Return true if `xdotool version` succeeds on the given display.
fn probe_display(display: &str, xauth_file: &str) -> bool {
    let mut cmd = Command::new("xdotool");
    cmd.env("DISPLAY", display);
    if !xauth_file.is_empty() {
        cmd.env("XAUTHORITY", xauth_file);
    }
    cmd.arg("version");
    match command_output_with_timeout(cmd) {
        Some(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // Definite failure messages
            if stderr.contains("Can't open display")
                || stderr.contains("Authorization required")
                || stderr.contains("unable to open display")
            {
                return false;
            }
            // Success: got version output (even if exit code != 0 due to XTEST warning)
            !o.stdout.is_empty() || o.status.success()
        }
        None => false,
    }
}

/// Detect X11 display environment. Returns `(DISPLAY, XAUTHORITY)` if found.
///
/// Probing order:
/// 1. `$DISPLAY` env var (direct inheritance)
/// 2. tmux `show-environment DISPLAY` (covers SSH -X sessions in tmux)
/// 3. Displays derived from `xauth list` entries
/// 4. Unix socket scan in `/tmp/.X11-unix/`
///
/// For each candidate the function probes with `xdotool version` before
/// accepting it, so it only returns a display that actually responds.
fn detect_x11_env() -> Option<(String, String)> {
    let xauth = find_xauthority_file();

    // 1. Current environment
    if let Ok(d) = std::env::var("DISPLAY") {
        if !d.is_empty() && probe_display(&d, &xauth) {
            return Some((d, xauth.clone()));
        }
    }

    // 2. tmux session environment (SSH -X sets DISPLAY here even when the
    //    bridge process itself does not inherit it)
    if let Some(d) = tmux_get_display() {
        if probe_display(&d, &xauth) {
            return Some((d, xauth.clone()));
        }
    }

    let mut candidates = Vec::new();

    // 3. xauth list – generates :N and localhost:N.0 for each known cookie
    for d in xauth_list_displays(&xauth) {
        if !candidates.contains(&d) {
            candidates.push(d);
        }
    }

    // 4. Probe each candidate; return the first that works
    for display in &candidates {
        if probe_display(display, &xauth) {
            return Some((display.clone(), xauth));
        }
    }

    // 5. Unix socket fallback (no probe — best-effort)
    let x11_dir = std::path::Path::new("/tmp/.X11-unix");
    if !x11_dir.exists() {
        return None;
    }
    if let Ok(entries) = std::fs::read_dir(x11_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num) = name.strip_prefix('X') {
                if num.parse::<u32>().is_ok() {
                    let display = format!(":{}", num);
                    if !candidates.contains(&display) {
                        return Some((display, xauth));
                    }
                }
            }
        }
    }

    None
}

/// Create a `Command` with X11 environment variables set.
pub fn x11_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    if let Some((display, xauth)) = detect_x11_env() {
        cmd.env("DISPLAY", &display);
        if !xauth.is_empty() {
            cmd.env("XAUTHORITY", &xauth);
        }
    }
    cmd
}

// ── Screen geometry ───────────────────────────────────────────────────────────

/// Screen dimensions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

/// Get the current screen size.
pub fn get_screen_size() -> Result<ScreenSize> {
    let output = x11_command("xdotool")
        .arg("getdisplaygeometry")
        .output()
        .context("xdotool not found")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool getdisplaygeometry failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() != 2 {
        anyhow::bail!("Unexpected xdotool output: '{}'", stdout.trim());
    }

    Ok(ScreenSize {
        width: parts[0].parse().context("Failed to parse width")?,
        height: parts[1].parse().context("Failed to parse height")?,
    })
}

// ── Screenshot ────────────────────────────────────────────────────────────────

/// Take a screenshot using ImageMagick `import` (preferred) or `xwd` + Python
/// PIL (fallback). Returns a base64-encoded PNG.
pub fn take_screenshot(max_width: Option<u32>, max_height: Option<u32>) -> Result<String> {
    // Try ImageMagick import first
    if let Ok(result) = screenshot_via_import(max_width, max_height) {
        return Ok(result);
    }
    // Fall back to xwd + Python PIL
    screenshot_via_xwd(max_width, max_height)
}

fn screenshot_via_import(max_width: Option<u32>, max_height: Option<u32>) -> Result<String> {
    let mut cmd = x11_command("import");
    cmd.args(["-window", "root"]);
    if let (Some(w), Some(h)) = (max_width, max_height) {
        cmd.args(["-resize", &format!("{}x{}>", w, h)]);
    }
    cmd.arg("png:-");
    let output = cmd.output().context("ImageMagick import not found")?;
    if !output.status.success() {
        anyhow::bail!("import failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&output.stdout))
}

fn screenshot_via_xwd(max_width: Option<u32>, max_height: Option<u32>) -> Result<String> {
    // Capture raw XWD data from xwd
    let xwd_output = x11_command("xwd")
        .args(["-root", "-silent"])
        .output()
        .context("xwd not found")?;
    if !xwd_output.status.success() {
        anyhow::bail!(
            "xwd failed: {}",
            String::from_utf8_lossy(&xwd_output.stderr)
        );
    }

    // Convert XWD → PNG via a Python one-liner (requires Pillow)
    let resize_code = match (max_width, max_height) {
        (Some(w), Some(h)) => format!("img.thumbnail(({w}, {h}))"),
        _ => String::new(),
    };
    let py_script = format!(
        r#"
import sys, struct, io
from PIL import Image
data = sys.stdin.buffer.read()
header_size = struct.unpack('>I', data[:4])[0]
width = struct.unpack('>I', data[16:20])[0]
height = struct.unpack('>I', data[20:24])[0]
bpl = struct.unpack('>I', data[48:52])[0]
ncolors = struct.unpack('>I', data[96:100])[0]
offset = header_size + ncolors * 12
raw = data[offset:]
img = Image.frombytes('RGBX', (width, height), raw, 'raw', 'BGRX', bpl, 1).convert('RGB')
{resize_code}
buf = io.BytesIO()
img.save(buf, 'PNG')
sys.stdout.buffer.write(buf.getvalue())
"#
    );

    let mut child = Command::new("python3")
        .args(["-c", &py_script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("python3 not available for XWD→PNG conversion")?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(&xwd_output.stdout)
            .context("Failed to pipe XWD data to python3")?;
    }

    let py_output = child
        .wait_with_output()
        .context("python3 conversion failed")?;

    if !py_output.status.success() || py_output.stdout.is_empty() {
        anyhow::bail!(
            "XWD→PNG conversion failed: {}",
            String::from_utf8_lossy(&py_output.stderr)
        );
    }

    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&py_output.stdout))
}

// ── Mouse and keyboard actions ────────────────────────────────────────────────

/// Move mouse to coordinates.
pub fn mouse_move(x: i32, y: i32) -> Result<()> {
    let output = x11_command("xdotool")
        .args(["mousemove", &x.to_string(), &y.to_string()])
        .output()
        .context("xdotool mousemove failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool mousemove failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Click at coordinates with specified button (1=left, 2=middle, 3=right).
pub fn mouse_click(x: i32, y: i32, button: u8) -> Result<()> {
    mouse_move(x, y)?;

    let output = x11_command("xdotool")
        .args(["click", &button.to_string()])
        .output()
        .context("xdotool click failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool click failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Drag from one point to another.
pub fn mouse_drag(from_x: i32, from_y: i32, to_x: i32, to_y: i32, button: u8) -> Result<()> {
    mouse_move(from_x, from_y)?;

    let output = x11_command("xdotool")
        .args([
            "mousedown",
            &button.to_string(),
            "mousemove",
            &to_x.to_string(),
            &to_y.to_string(),
            "mouseup",
            &button.to_string(),
        ])
        .output()
        .context("xdotool drag failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool drag failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Scroll at coordinates in the given direction (up/down/left/right).
pub fn mouse_scroll(x: i32, y: i32, direction: &str, amount: u32) -> Result<()> {
    mouse_move(x, y)?;

    let button = match direction {
        "up" => "4",
        "down" => "5",
        "left" => "6",
        "right" => "7",
        other => anyhow::bail!("Invalid scroll direction: {}", other),
    };

    for _ in 0..amount {
        let output = x11_command("xdotool")
            .args(["click", button])
            .output()
            .context("xdotool scroll failed")?;
        if !output.status.success() {
            anyhow::bail!(
                "xdotool scroll failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(())
}

/// Type text.
pub fn type_text(text: &str) -> Result<()> {
    let output = x11_command("xdotool")
        .args(["type", "--clearmodifiers", "--delay", "12", text])
        .output()
        .context("xdotool type failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool type failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Press a key or key combination (e.g. `"Return"`, `"ctrl+s"`, `"alt+F4"`).
pub fn key_press(keys: &str) -> Result<()> {
    let output = x11_command("xdotool")
        .args(["key", "--clearmodifiers", keys])
        .output()
        .context("xdotool key failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "xdotool key failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

// ── Availability checks ───────────────────────────────────────────────────────

/// Check if xdotool can connect to a working X display.
pub fn is_xdotool_available() -> bool {
    match detect_x11_env() {
        None => false,
        Some((display, xauth)) => {
            let mut cmd = Command::new("xdotool");
            cmd.env("DISPLAY", &display);
            if !xauth.is_empty() {
                cmd.env("XAUTHORITY", &xauth);
            }
            cmd.arg("version");
            match cmd.output() {
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    !stderr.contains("Can't open display")
                        && !stderr.contains("Authorization required")
                        && (!o.stdout.is_empty() || o.status.success())
                }
                Err(_) => false,
            }
        }
    }
}

/// Check if screenshot capability is available (ImageMagick `import` or
/// `xwd` + Python PIL).
pub fn is_screenshot_available() -> bool {
    // ImageMagick import
    if x11_command("import")
        .arg("-version")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    // xwd + python3 Pillow
    let xwd_ok = x11_command("xwd")
        .arg("--help")
        .output()
        .map(|_| true)
        .unwrap_or(false);
    let py_ok = Command::new("python3")
        .args(["-c", "from PIL import Image"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    xwd_ok && py_ok
}

/// Kept for API compatibility; delegates to `is_screenshot_available`.
#[allow(dead_code)]
pub fn is_import_available() -> bool {
    is_screenshot_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_size_struct() {
        let s = ScreenSize {
            width: 1920,
            height: 1080,
        };
        assert_eq!(s.width, 1920);
        assert_eq!(s.height, 1080);
    }

    #[test]
    fn test_availability_checks_dont_panic() {
        let _ = is_xdotool_available();
        let _ = is_screenshot_available();
    }

    #[test]
    fn test_xauth_list_displays_parses_correctly() {
        // simulate output: "sparky/unix:11  MIT-MAGIC-COOKIE-1  abc"
        // We can't call xauth in unit tests, but we test the number extraction logic.
        let line = "sparky/unix:11";
        let num_str = line.rsplit(':').next().unwrap();
        let num: u32 = num_str.parse().unwrap();
        assert_eq!(num, 11);
    }
}
