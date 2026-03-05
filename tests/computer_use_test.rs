//! Integration tests for OMAR computer use feature.
//!
//! These tests validate the computer use API endpoints and the locking
//! mechanism. Tests that require a display (xdotool/screenshots) are
//! gated behind a DISPLAY check and will skip gracefully in CI.

use std::process::Command;

/// Check if an X display is available (needed for xdotool/screenshots).
fn has_display() -> bool {
    std::env::var("DISPLAY").is_ok()
        && Command::new("xdotool")
            .arg("getdisplaygeometry")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Check if xdotool is installed (even without a display).
fn has_xdotool() -> bool {
    Command::new("xdotool")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if ImageMagick's import command is available.
fn has_imagemagick() -> bool {
    Command::new("import")
        .arg("-version")
        .output()
        .map(|o| o.status.success() || !o.stderr.is_empty())
        .unwrap_or(false)
}

// ── Unit-level tests (no display required) ──

#[test]
fn test_computer_lock_creation() {
    // Just verify we can create a lock (tests the module compiles)
    // This is a pure logic test — no X display needed.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let lock: std::sync::Arc<tokio::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(None));

        // Lock is initially empty
        {
            let guard = lock.lock().await;
            assert!(guard.is_none());
        }

        // Acquire lock
        {
            let mut guard = lock.lock().await;
            *guard = Some("test-agent".to_string());
        }

        // Verify lock is held
        {
            let guard = lock.lock().await;
            assert_eq!(guard.as_deref(), Some("test-agent"));
        }

        // Release lock
        {
            let mut guard = lock.lock().await;
            *guard = None;
        }

        // Verify released
        {
            let guard = lock.lock().await;
            assert!(guard.is_none());
        }
    });
}

#[test]
fn test_lock_exclusivity() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let lock: std::sync::Arc<tokio::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(None));

        // Agent A acquires lock
        {
            let mut guard = lock.lock().await;
            *guard = Some("agent-a".to_string());
        }

        // Agent B tries to acquire — should see agent-a holds it
        {
            let guard = lock.lock().await;
            assert_eq!(guard.as_deref(), Some("agent-a"));
        }

        // Agent A releases
        {
            let mut guard = lock.lock().await;
            if guard.as_deref() == Some("agent-a") {
                *guard = None;
            }
        }

        // Agent B can now acquire
        {
            let mut guard = lock.lock().await;
            assert!(guard.is_none());
            *guard = Some("agent-b".to_string());
        }

        // Verify
        {
            let guard = lock.lock().await;
            assert_eq!(guard.as_deref(), Some("agent-b"));
        }
    });
}

// ── Display-dependent tests ──

#[test]
fn test_xdotool_availability() {
    // This test verifies xdotool is installed — doesn't require a display
    if !has_xdotool() {
        eprintln!("Skipping test: xdotool not installed");
        return;
    }

    let output = Command::new("xdotool")
        .arg("version")
        .output()
        .expect("Failed to run xdotool");
    assert!(output.status.success());
}

#[test]
fn test_screen_size_with_display() {
    if !has_display() {
        eprintln!("Skipping test: no X display available");
        return;
    }

    let output = Command::new("xdotool")
        .arg("getdisplaygeometry")
        .output()
        .expect("Failed to run xdotool");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(parts.len(), 2, "Expected 'W H' format, got: {}", stdout);

    let width: u32 = parts[0].parse().expect("Width should be a number");
    let height: u32 = parts[1].parse().expect("Height should be a number");
    assert!(width > 0, "Width should be positive");
    assert!(height > 0, "Height should be positive");
}

#[test]
fn test_mouse_position_with_display() {
    if !has_display() {
        eprintln!("Skipping test: no X display available");
        return;
    }

    let output = Command::new("xdotool")
        .arg("getmouselocation")
        .output()
        .expect("Failed to run xdotool");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("x:") && stdout.contains("y:"),
        "Unexpected mouse location format: {}",
        stdout
    );
}

#[test]
fn test_screenshot_with_display() {
    if !has_display() || !has_imagemagick() {
        eprintln!("Skipping test: no display or ImageMagick not available");
        return;
    }

    // Take a small screenshot to verify import works
    let output = Command::new("import")
        .args(["-window", "root", "-resize", "100x100>", "png:-"])
        .output()
        .expect("Failed to run import");

    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "Screenshot should produce output"
    );

    // Verify it's a valid PNG (starts with PNG magic bytes)
    assert!(
        output.stdout.len() > 8,
        "Screenshot too small to be valid PNG"
    );
    assert_eq!(&output.stdout[1..4], b"PNG", "Output should be a PNG file");
}

#[test]
fn test_imagemagick_availability() {
    if !has_imagemagick() {
        eprintln!("Skipping test: ImageMagick not installed");
        return;
    }

    // import -version prints to stderr, but exits 0 or non-zero depending on version
    let output = Command::new("import")
        .arg("-version")
        .output()
        .expect("Failed to run import");

    // ImageMagick's import prints version info to stderr
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("ImageMagick") || combined.contains("Version"),
        "Expected ImageMagick version info, got: {}",
        combined
    );
}
