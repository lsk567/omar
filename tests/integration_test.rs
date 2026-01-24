//! Integration tests for OMA
//!
//! These tests require tmux to be installed and will create/destroy
//! test sessions during execution.

use std::process::Command;
use std::thread;
use std::time::Duration;

const TEST_PREFIX: &str = "oma-test-";

/// Helper to run tmux commands
fn tmux(args: &[&str]) -> Result<String, String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run tmux: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "no server running" is not an error for cleanup
        if stderr.contains("no server running") || stderr.contains("no sessions") {
            Ok(String::new())
        } else {
            Err(stderr.to_string())
        }
    }
}

/// Clean up any test sessions
fn cleanup_test_sessions() {
    // List all sessions and kill ones with our test prefix
    if let Ok(output) = tmux(&["list-sessions", "-F", "#{session_name}"]) {
        for line in output.lines() {
            if line.starts_with(TEST_PREFIX) {
                let _ = tmux(&["kill-session", "-t", line]);
            }
        }
    }
}

/// Check if tmux is available
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn test_tmux_available() {
    assert!(
        tmux_available(),
        "tmux must be installed to run these tests"
    );
}

#[test]
fn test_create_and_list_session() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}create-list", TEST_PREFIX);

    // Create a session
    let result = tmux(&["new-session", "-d", "-s", &session_name, "sleep", "60"]);
    assert!(result.is_ok(), "Failed to create session: {:?}", result);

    // Give it a moment to start
    thread::sleep(Duration::from_millis(100));

    // List sessions
    let output = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap();
    assert!(
        output.contains(&session_name),
        "Session not found in list: {}",
        output
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

#[test]
fn test_capture_pane() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}capture", TEST_PREFIX);

    // Create a session with a shell
    let result = tmux(&["new-session", "-d", "-s", &session_name]);
    assert!(result.is_ok(), "Failed to create session: {:?}", result);

    // Give it time to start
    thread::sleep(Duration::from_millis(200));

    // Send echo command
    let _ = tmux(&[
        "send-keys",
        "-t",
        &session_name,
        "echo HELLO_OMA_TEST",
        "Enter",
    ]);

    // Give it time to execute
    thread::sleep(Duration::from_millis(500));

    // Capture pane content
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        output.contains("HELLO_OMA_TEST"),
        "Expected output not found: {}",
        output
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

#[test]
fn test_kill_session() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}kill", TEST_PREFIX);

    // Create a session
    let _ = tmux(&["new-session", "-d", "-s", &session_name, "sleep", "60"]);
    thread::sleep(Duration::from_millis(100));

    // Verify it exists
    let output = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap();
    assert!(output.contains(&session_name));

    // Kill it
    let result = tmux(&["kill-session", "-t", &session_name]);
    assert!(result.is_ok());

    // Verify it's gone
    thread::sleep(Duration::from_millis(100));
    let output = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default();
    assert!(
        !output.contains(&session_name),
        "Session should be killed: {}",
        output
    );
}

#[test]
fn test_session_activity() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}activity", TEST_PREFIX);

    // Create a session
    let _ = tmux(&["new-session", "-d", "-s", &session_name, "sleep", "60"]);
    thread::sleep(Duration::from_millis(100));

    // Get activity timestamp
    let output = tmux(&[
        "display-message",
        "-t",
        &session_name,
        "-p",
        "#{session_activity}",
    ])
    .unwrap();

    let activity: i64 = output.trim().parse().expect("Activity should be a number");
    assert!(activity > 0, "Activity timestamp should be positive");

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

#[test]
fn test_has_session() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}has-session", TEST_PREFIX);

    // Check non-existent session
    let result = Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .output()
        .unwrap();
    assert!(!result.status.success());

    // Create session
    let _ = tmux(&["new-session", "-d", "-s", &session_name, "sleep", "60"]);
    thread::sleep(Duration::from_millis(100));

    // Check existing session
    let result = Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .output()
        .unwrap();
    assert!(result.status.success());

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

#[test]
fn test_send_keys() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}send-keys", TEST_PREFIX);

    // Create a session with a shell
    let _ = tmux(&["new-session", "-d", "-s", &session_name]);
    thread::sleep(Duration::from_millis(200));

    // Send a command
    let result = tmux(&[
        "send-keys",
        "-t",
        &session_name,
        "echo SENT_BY_OMA",
        "Enter",
    ]);
    assert!(result.is_ok());

    // Give it time to execute
    thread::sleep(Duration::from_millis(500));

    // Capture and verify
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        output.contains("SENT_BY_OMA"),
        "Sent command not found: {}",
        output
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

/// Test that the oma binary can be built and shows help
#[test]
fn test_oma_help() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run oma");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Agent dashboard for tmux"),
        "Help should contain description: {}",
        stdout
    );
}

#[test]
fn test_oma_list_empty() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let output = Command::new("cargo")
        .args(["run", "--", "list"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run oma list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show "No agent sessions" since we use oma-agent- prefix
    assert!(
        stdout.contains("No agent sessions") || stdout.contains("NAME"),
        "Unexpected output: {}",
        stdout
    );
}
