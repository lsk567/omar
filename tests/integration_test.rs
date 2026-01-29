//! Integration tests for OMAR
//!
//! These tests require tmux to be installed and will create/destroy
//! test sessions during execution.

use std::process::Command;
use std::thread;
use std::time::Duration;

const TEST_PREFIX: &str = "omar-test-";

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
        "echo HELLO_OMAR_TEST",
        "Enter",
    ]);

    // Give it time to execute
    thread::sleep(Duration::from_millis(500));

    // Capture pane content
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        output.contains("HELLO_OMAR_TEST"),
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
        "echo SENT_BY_OMAR",
        "Enter",
    ]);
    assert!(result.is_ok());

    // Give it time to execute
    thread::sleep(Duration::from_millis(500));

    // Capture and verify
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        output.contains("SENT_BY_OMAR"),
        "Sent command not found: {}",
        output
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

/// Test that spawning with a custom (non-claude) command works.
/// This validates opencode and other backend compatibility: omar should
/// start any command in a tmux session and inject tasks via send-keys.
#[test]
fn test_spawn_custom_command() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let session_name = format!("{}custom-cmd", TEST_PREFIX);

    // Spawn a session with a non-claude command (simulates opencode or other backend)
    let result = tmux(&["new-session", "-d", "-s", &session_name, "bash"]);
    assert!(
        result.is_ok(),
        "Should spawn session with custom command: {:?}",
        result
    );

    thread::sleep(Duration::from_millis(300));

    // Verify session is running
    let check = Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "Session with custom command should be running"
    );

    // Simulate the universal send-keys task injection pattern
    // (this is how omar sends tasks to any backend, including opencode)
    let task_text = "echo TASK_INJECTED_VIA_SENDKEYS";
    let _ = tmux(&["send-keys", "-t", &session_name, "-l", task_text]);
    let _ = tmux(&["send-keys", "-t", &session_name, "Enter"]);

    thread::sleep(Duration::from_millis(500));

    // Verify the task was injected and executed
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        output.contains("TASK_INJECTED_VIA_SENDKEYS"),
        "Task should be injected via send-keys: {}",
        output
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &session_name]);
}

/// Test that the omar binary can be built and shows help
#[test]
fn test_omar_help() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run omar");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Agent dashboard for tmux"),
        "Help should contain description: {}",
        stdout
    );
}

#[test]
fn test_omar_list_empty() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    let output = Command::new("cargo")
        .args(["run", "--", "list"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run omar list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show "No agent sessions" since we have no prefix by default
    assert!(
        stdout.contains("No agent sessions") || stdout.contains("NAME"),
        "Unexpected output: {}",
        stdout
    );
}

#[test]
fn test_omar_spawn_and_kill() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    cleanup_test_sessions();

    // Default prefix is "omar-agent-", so session name = "omar-agent-test-spawn"
    let full_session = "omar-agent-test-spawn";

    // Clean up from previous test runs
    let _ = tmux(&["kill-session", "-t", full_session]);

    // Spawn a new agent
    let output = Command::new("cargo")
        .args(["run", "--", "spawn", "-n", "test-spawn", "-c", "sleep 60"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run omar spawn");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Spawned agent: test-spawn"),
        "Should confirm spawn: {}",
        stdout
    );

    // Verify session exists with the prefixed name
    thread::sleep(Duration::from_millis(200));
    let result = Command::new("tmux")
        .args(["has-session", "-t", full_session])
        .output()
        .unwrap();
    assert!(result.status.success(), "Session should exist after spawn");

    // List should show the agent (displayed without prefix)
    let output = Command::new("cargo")
        .args(["run", "--", "list"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run omar list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test-spawn"),
        "List should show spawned agent: {}",
        stdout
    );

    // Kill the agent (using short name)
    let output = Command::new("cargo")
        .args(["run", "--", "kill", "test-spawn"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("Failed to run omar kill");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Killed agent: test-spawn"),
        "Should confirm kill: {}",
        stdout
    );

    // Verify session is gone
    thread::sleep(Duration::from_millis(100));
    let result = Command::new("tmux")
        .args(["has-session", "-t", full_session])
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "Session should not exist after kill"
    );
}
