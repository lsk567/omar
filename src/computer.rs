//! Computer use module — provides mouse, keyboard, and screenshot control
//! via Linux desktop tools (xdotool, ImageMagick import).
//!
//! Only one agent may hold the computer lock at a time.

use anyhow::{Context, Result};
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Mutex;

/// Detect X11 display environment. Returns (DISPLAY, XAUTHORITY) if found.
/// Auto-detects by scanning /tmp/.X11-unix/ for X sockets and common Xauthority paths.
fn detect_x11_env() -> Option<(String, String)> {
    // If DISPLAY is already set, use it
    if let Ok(display) = std::env::var("DISPLAY") {
        if !display.is_empty() {
            let xauth = std::env::var("XAUTHORITY").unwrap_or_default();
            return Some((display, xauth));
        }
    }

    // Auto-detect: find X socket in /tmp/.X11-unix/
    let x11_dir = std::path::Path::new("/tmp/.X11-unix");
    if !x11_dir.exists() {
        return None;
    }

    let mut display_num: Option<String> = None;
    if let Ok(entries) = std::fs::read_dir(x11_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Socket files are named X0, X1, X1001, etc.
            if let Some(num) = name.strip_prefix('X') {
                // Prefer lower-numbered displays (skip very high numbers like X1001)
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

    // Try common Xauthority locations
    // Get UID from /proc/self/status to avoid libc dependency
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

/// Shared lock for exclusive computer access.
/// Contains the agent name that currently holds the lock, or None.
#[cfg(test)]
type ComputerLock = Arc<Mutex<Option<String>>>;

/// Create a new computer lock for unit tests.
#[cfg(test)]
fn new_lock() -> ComputerLock {
    Arc::new(Mutex::new(None))
}

/// Screen dimensions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

/// Get the current screen size using xdotool.
pub fn get_screen_size() -> Result<ScreenSize> {
    let output = x11_command("xdotool")
        .arg("getdisplaygeometry")
        .output()
        .context("Failed to run xdotool — is xdotool installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool getdisplaygeometry failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() != 2 {
        anyhow::bail!(
            "Unexpected xdotool output: expected 'W H', got '{}'",
            stdout.trim()
        );
    }

    Ok(ScreenSize {
        width: parts[0].parse().context("Failed to parse width")?,
        height: parts[1].parse().context("Failed to parse height")?,
    })
}

/// Take a screenshot and return it as a base64-encoded PNG.
pub fn take_screenshot() -> Result<String> {
    // Use ImageMagick's `import` to capture the root window to stdout as PNG
    let output = x11_command("import")
        .args(["-window", "root", "png:-"])
        .output()
        .context("Failed to run import — is ImageMagick installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("import (ImageMagick) failed: {}", stderr);
    }

    // Optionally downscale large images to reduce payload size.
    // We'll pass through as-is for now; the caller can request a resize.
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&output.stdout))
}

/// Take a screenshot, resizing to fit within `max_width x max_height`.
pub fn take_screenshot_resized(max_width: u32, max_height: u32) -> Result<String> {
    let output = x11_command("import")
        .args([
            "-window",
            "root",
            "-resize",
            &format!("{}x{}>", max_width, max_height),
            "png:-",
        ])
        .output()
        .context("Failed to run import — is ImageMagick installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("import (ImageMagick) failed: {}", stderr);
    }

    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&output.stdout))
}

/// Move the mouse to the given coordinates.
pub fn mouse_move(x: i32, y: i32) -> Result<()> {
    let output = x11_command("xdotool")
        .args(["mousemove", &x.to_string(), &y.to_string()])
        .output()
        .context("Failed to run xdotool mousemove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool mousemove failed: {}", stderr);
    }
    Ok(())
}

/// Click at the given coordinates with the specified button.
pub fn mouse_click(x: i32, y: i32, button: u8) -> Result<()> {
    // Move first, then click
    mouse_move(x, y)?;

    let output = x11_command("xdotool")
        .args(["click", &button.to_string()])
        .output()
        .context("Failed to run xdotool click")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool click failed: {}", stderr);
    }
    Ok(())
}

/// Double-click at the given coordinates.
pub fn mouse_double_click(x: i32, y: i32, button: u8) -> Result<()> {
    mouse_move(x, y)?;

    let output = x11_command("xdotool")
        .args(["click", "--repeat", "2", &button.to_string()])
        .output()
        .context("Failed to run xdotool double-click")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool double-click failed: {}", stderr);
    }
    Ok(())
}

/// Click and drag from (x1, y1) to (x2, y2).
pub fn mouse_drag(x1: i32, y1: i32, x2: i32, y2: i32, button: u8) -> Result<()> {
    mouse_move(x1, y1)?;

    let output = x11_command("xdotool")
        .args([
            "mousedown",
            &button.to_string(),
            "mousemove",
            &x2.to_string(),
            &y2.to_string(),
            "mouseup",
            &button.to_string(),
        ])
        .output()
        .context("Failed to run xdotool drag")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool drag failed: {}", stderr);
    }
    Ok(())
}

/// Scroll at the given coordinates.
/// direction: "up", "down", "left", "right"
/// amount: number of scroll clicks
pub fn mouse_scroll(x: i32, y: i32, direction: &str, amount: u32) -> Result<()> {
    mouse_move(x, y)?;

    let button = match direction {
        "up" => "4",
        "down" => "5",
        "left" => "6",
        "right" => "7",
        _ => anyhow::bail!("Invalid scroll direction: {}", direction),
    };

    for _ in 0..amount {
        let output = x11_command("xdotool")
            .args(["click", button])
            .output()
            .context("Failed to run xdotool scroll")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("xdotool scroll failed: {}", stderr);
        }
    }
    Ok(())
}

/// Type text using xdotool. Handles special characters properly.
pub fn type_text(text: &str) -> Result<()> {
    let output = x11_command("xdotool")
        .args(["type", "--clearmodifiers", "--delay", "12", text])
        .output()
        .context("Failed to run xdotool type")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool type failed: {}", stderr);
    }
    Ok(())
}

/// Press a key or key combination (e.g. "Return", "ctrl+s", "alt+F4").
pub fn key_press(key: &str) -> Result<()> {
    // xdotool uses "+" for combos, same as our input format
    let output = x11_command("xdotool")
        .args(["key", "--clearmodifiers", key])
        .output()
        .context("Failed to run xdotool key")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool key failed: {}", stderr);
    }
    Ok(())
}

/// Get the current mouse position.
pub fn get_mouse_position() -> Result<(i32, i32)> {
    let output = x11_command("xdotool")
        .arg("getmouselocation")
        .output()
        .context("Failed to run xdotool getmouselocation")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("xdotool getmouselocation failed: {}", stderr);
    }

    // Output: "x:123 y:456 screen:0 window:12345678"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut x = 0i32;
    let mut y = 0i32;
    for part in stdout.split_whitespace() {
        if let Some(val) = part.strip_prefix("x:") {
            x = val.parse().context("Failed to parse x")?;
        } else if let Some(val) = part.strip_prefix("y:") {
            y = val.parse().context("Failed to parse y")?;
        }
    }
    Ok((x, y))
}

/// Check if xdotool is available on the system.
pub fn is_available() -> bool {
    x11_command("xdotool")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if ImageMagick's import command is available.
pub fn is_screenshot_available() -> bool {
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
    fn test_new_lock_is_unlocked() {
        let lock = new_lock();
        let guard = lock.try_lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn test_screen_size_struct() {
        let size = ScreenSize {
            width: 1920,
            height: 1080,
        };
        assert_eq!(size.width, 1920);
        assert_eq!(size.height, 1080);
    }

    #[test]
    fn test_is_available_returns_bool() {
        // Just test it doesn't panic
        let _ = is_available();
    }

    #[test]
    fn test_is_screenshot_available_returns_bool() {
        let _ = is_screenshot_available();
    }
}
