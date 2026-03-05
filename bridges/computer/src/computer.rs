//! Computer interaction via X11 tools (xdotool, ImageMagick import).
//! Adapted from the main omar crate's computer module for standalone use.

use anyhow::{Context, Result};
use std::process::Command;

/// Detect X11 display environment. Returns (DISPLAY, XAUTHORITY) if found.
fn detect_x11_env() -> Option<(String, String)> {
    if let Ok(display) = std::env::var("DISPLAY") {
        if !display.is_empty() {
            let xauth = std::env::var("XAUTHORITY").unwrap_or_default();
            return Some((display, xauth));
        }
    }

    let x11_dir = std::path::Path::new("/tmp/.X11-unix");
    if !x11_dir.exists() {
        return None;
    }

    let mut display_num: Option<String> = None;
    if let Ok(entries) = std::fs::read_dir(x11_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num) = name.strip_prefix('X') {
                if let Ok(n) = num.parse::<u32>() {
                    if n < 100 {
                        display_num = Some(num.to_string());
                        break;
                    } else if display_num.is_none() {
                        display_num = Some(num.to_string());
                    }
                }
            }
        }
    }

    let display_num = display_num?;
    let display = format!(":{}", display_num);

    let uid = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u32>().ok())
        })
        .unwrap_or(1000);

    let xauth_candidates = [
        format!("/run/user/{}/gdm/Xauthority", uid),
        format!("/run/user/{}/.mutter-Xwaylandauth", uid),
        std::env::var("HOME")
            .map(|h| format!("{}/.Xauthority", h))
            .unwrap_or_default(),
    ];

    let xauth = xauth_candidates
        .iter()
        .find(|p| !p.is_empty() && std::path::Path::new(p).exists())
        .cloned()
        .unwrap_or_default();

    Some((display, xauth))
}

/// Create a Command with X11 environment variables set.
fn x11_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    if let Some((display, xauth)) = detect_x11_env() {
        cmd.env("DISPLAY", &display);
        if !xauth.is_empty() {
            cmd.env("XAUTHORITY", &xauth);
        }
    }
    cmd
}

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

/// Take a screenshot, optionally resized, returned as base64 PNG.
pub fn take_screenshot(max_width: Option<u32>, max_height: Option<u32>) -> Result<String> {
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

/// Press a key or key combination (e.g. "Return", "ctrl+s", "alt+F4").
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

/// Check if xdotool is available.
pub fn is_xdotool_available() -> bool {
    x11_command("xdotool")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if ImageMagick import is available.
pub fn is_import_available() -> bool {
    x11_command("import")
        .arg("-version")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
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
        let _ = is_import_available();
    }
}
