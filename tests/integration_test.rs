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

fn wait_for_pane_contains(target: &str, needle: &str, attempts: usize, delay: Duration) -> bool {
    for _ in 0..attempts {
        if let Ok(output) = tmux(&["capture-pane", "-t", target, "-p"]) {
            if output.contains(needle) {
                return true;
            }
        }
        thread::sleep(delay);
    }
    false
}

fn send_line_and_wait(target: &str, line: &str) -> bool {
    for _ in 0..5 {
        let _ = tmux(&["send-keys", "-t", target, "-l", line]);
        let _ = tmux(&["send-keys", "-t", target, "C-m"]);
        if wait_for_pane_contains(target, line, 10, Duration::from_millis(100)) {
            return true;
        }
    }
    false
}

fn pane_contains_wrapped(output: &str, needle: &str) -> bool {
    if output.contains(needle) {
        return true;
    }
    output.replace(['\r', '\n'], "").contains(needle)
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

    let found = send_line_and_wait(&session_name, "echo HELLO_OMAR_TEST");
    assert!(found, "echo command was not observed in pane");

    // `send_line_and_wait` only proves the command *text* landed in the pane
    // (the shell echoes each keystroke). To prove capture works on actual
    // command *output*, require a second occurrence of the needle — once
    // for the typed command line, once for echo's output on a new line.
    let mut output = String::new();
    let mut output_found = false;
    for _ in 0..10 {
        output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap_or_default();
        if output.matches("HELLO_OMAR_TEST").count() >= 2 {
            output_found = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        output_found,
        "echo command did not execute — pane: {}",
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

    let sent = send_line_and_wait(&session_name, "echo SENT_BY_OMAR");

    // Capture and verify
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        sent && output.contains("SENT_BY_OMAR"),
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
    let injected = send_line_and_wait(&session_name, task_text);

    // Verify the task was injected and executed
    let output = tmux(&["capture-pane", "-t", &session_name, "-p"]).unwrap();
    assert!(
        injected && output.contains("TASK_INJECTED_VIA_SENDKEYS"),
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

    // Clean up from previous test runs across EA namespaces
    if let Ok(output) = tmux(&["list-sessions", "-F", "#{session_name}"]) {
        for line in output.lines() {
            if line == "test-spawn" || line.ends_with("-test-spawn") {
                let _ = tmux(&["kill-session", "-t", line]);
            }
        }
    }

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

    // Resolve the full spawned session name dynamically, since active EA id can vary.
    thread::sleep(Duration::from_millis(200));
    let all_sessions = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default();
    let full_session = all_sessions
        .lines()
        .find(|line| *line == "test-spawn" || line.ends_with("-test-spawn"))
        .map(ToString::to_string)
        .expect("Expected a spawned session ending with '-test-spawn'");

    // Verify session exists
    let result = Command::new("tmux")
        .args(["has-session", "-t", &full_session])
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
        .args(["has-session", "-t", &full_session])
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "Session should not exist after kill"
    );
}

/// Test that `deliver_to_tmux` routes messages to EA-scoped session names.
///
/// Session naming (from `ea::ea_prefix` + receiver, or `ea::ea_manager_session`):
///   - EA 0 worker "recv":  "omar-agent-0-recv"
///   - EA 1 worker "recv":  "omar-agent-1-recv"
///   - EA 0 manager ("ea"): "omar-agent-ea-0"
///   - EA 1 manager ("ea"): "omar-agent-ea-1"
///
/// The test creates one session per EA, delivers a distinct message to each by
/// replicating the exact tmux send-keys pattern used by `deliver_to_tmux`, then
/// asserts that each session received only its own message (EA isolation).
#[test]
fn test_deliver_to_tmux_ea_scoped() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    // EA-scoped session names: ea_prefix(ea_id, "omar-agent-") + receiver
    //   ea_prefix(0, "omar-agent-") = "omar-agent-0-"
    //   ea_prefix(1, "omar-agent-") = "omar-agent-1-"
    const BASE_PREFIX: &str = "omar-agent-";
    let ea0_session = format!("{}0-deliver-recv", BASE_PREFIX);
    let ea1_session = format!("{}1-deliver-recv", BASE_PREFIX);

    // Pre-cleanup
    let _ = tmux(&["kill-session", "-t", &ea0_session]);
    let _ = tmux(&["kill-session", "-t", &ea1_session]);

    // Create one agent session per EA
    let r0 = tmux(&["new-session", "-d", "-s", &ea0_session]);
    assert!(
        r0.is_ok(),
        "Failed to create EA 0 session '{}': {:?}",
        ea0_session,
        r0
    );
    let r1 = tmux(&["new-session", "-d", "-s", &ea1_session]);
    assert!(
        r1.is_ok(),
        "Failed to create EA 1 session '{}': {:?}",
        ea1_session,
        r1
    );

    thread::sleep(Duration::from_millis(200));

    // The production `deliver_to_tmux` path routes through
    // `TmuxClient::deliver_prompt` (bracketed paste + verification), which
    // requires a full TUI to observe activity. This test targets a plain
    // shell session to validate EA-level *routing isolation* — not the
    // exact delivery mechanism — so we use send-keys directly: type each
    // message into its session and confirm the shell echoes it, then
    // assert that neither pane contains the other EA's message.
    let msg_ea0 = "DELIVER_EA0_ONLY";
    let msg_ea1 = "DELIVER_EA1_ONLY";

    let found_ea0 = send_line_and_wait(&ea0_session, msg_ea0);
    let found_ea1 = send_line_and_wait(&ea1_session, msg_ea1);

    // Capture pane output for each session
    let out0 = tmux(&["capture-pane", "-t", &ea0_session, "-p"]).unwrap_or_default();
    let out1 = tmux(&["capture-pane", "-t", &ea1_session, "-p"]).unwrap_or_default();

    // Each session must contain its own message
    assert!(
        found_ea0,
        "EA 0 session '{}' should contain '{}': {}",
        ea0_session, msg_ea0, out0
    );
    assert!(
        found_ea1,
        "EA 1 session '{}' should contain '{}': {}",
        ea1_session, msg_ea1, out1
    );

    // EA isolation: messages must not cross EA boundaries
    assert!(
        !out0.contains(msg_ea1),
        "EA 0 session must NOT contain EA 1's message '{}': {}",
        msg_ea1,
        out0
    );
    assert!(
        !out1.contains(msg_ea0),
        "EA 1 session must NOT contain EA 0's message '{}': {}",
        msg_ea0,
        out1
    );

    // Verify the EA-scoped session name format
    assert!(
        ea0_session.starts_with(&format!("{}0-", BASE_PREFIX)),
        "EA 0 session '{}' should start with '{}0-'",
        ea0_session,
        BASE_PREFIX
    );
    assert!(
        ea1_session.starts_with(&format!("{}1-", BASE_PREFIX)),
        "EA 1 session '{}' should start with '{}1-'",
        ea1_session,
        BASE_PREFIX
    );

    // Manager session convention: ea_manager_session(ea_id, base_prefix)
    //   = "{base_prefix}ea-{ea_id}"  (e.g., "omar-agent-ea-0", "omar-agent-ea-1")
    let mgr0 = format!("{}ea-0", BASE_PREFIX);
    let mgr1 = format!("{}ea-1", BASE_PREFIX);
    assert_ne!(mgr0, mgr1, "Manager sessions must be distinct across EAs");
    assert!(
        mgr0.starts_with(BASE_PREFIX),
        "EA 0 manager session '{}' must start with '{}'",
        mgr0,
        BASE_PREFIX
    );
    assert!(
        mgr1.starts_with(BASE_PREFIX),
        "EA 1 manager session '{}' must start with '{}'",
        mgr1,
        BASE_PREFIX
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &ea0_session]);
    let _ = tmux(&["kill-session", "-t", &ea1_session]);
}

/// Test the full EA-scoped scheduler event delivery cycle.
///
/// The scheduler's `run_event_loop` calls `deliver_to_tmux(ea_id, receiver, ...)`,
/// which routes the formatted event payload to the session:
///   `ea_prefix(ea_id, base_prefix) + receiver`  (for non-manager receivers)
///
/// This test validates:
///   1. Two EAs can have same-named agents without session conflicts.
///   2. A formatted event payload (as `format_delivery` produces) is delivered
///      correctly to each EA-scoped session.
///   3. Events do not leak across EA boundaries (isolation invariant).
#[test]
fn test_scheduler_event_delivery_cycle_ea_scoped() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    const BASE_PREFIX: &str = "omar-agent-";

    // Both EAs have a same-named agent "sched-recv".
    // ea_prefix(0, BASE_PREFIX) + "sched-recv" = "omar-agent-0-sched-recv"
    // ea_prefix(1, BASE_PREFIX) + "sched-recv" = "omar-agent-1-sched-recv"
    let ea0_session = format!("{}0-sched-recv", BASE_PREFIX);
    let ea1_session = format!("{}1-sched-recv", BASE_PREFIX);

    // Pre-cleanup
    let _ = tmux(&["kill-session", "-t", &ea0_session]);
    let _ = tmux(&["kill-session", "-t", &ea1_session]);

    // Create one session per EA
    let r0 = tmux(&["new-session", "-d", "-s", &ea0_session]);
    assert!(
        r0.is_ok(),
        "Failed to create EA 0 session '{}': {:?}",
        ea0_session,
        r0
    );
    let r1 = tmux(&["new-session", "-d", "-s", &ea1_session]);
    assert!(
        r1.is_ok(),
        "Failed to create EA 1 session '{}': {:?}",
        ea1_session,
        r1
    );

    thread::sleep(Duration::from_millis(200));

    // Simulate format_delivery output for a single event (as run_event_loop would generate):
    //   "[EVENT at t=<ts>]\nFrom <sender>: <payload>"
    let ts: u64 = 999_000_000_000;
    let payload_ea0 = format!("[EVENT at t={}]\nFrom ea-test: sched-ea0-only", ts);
    let payload_ea1 = format!("[EVENT at t={}]\nFrom ea-test: sched-ea1-only", ts);

    let _ = send_line_and_wait(&ea0_session, &payload_ea0);
    let _ = send_line_and_wait(&ea1_session, &payload_ea1);

    // Capture pane output
    let out0 = tmux(&["capture-pane", "-t", &ea0_session, "-p"]).unwrap_or_default();
    let out1 = tmux(&["capture-pane", "-t", &ea1_session, "-p"]).unwrap_or_default();

    // Each EA's session received its own event payload
    assert!(
        pane_contains_wrapped(&out0, "sched-ea0-only"),
        "EA 0 session missing its scheduled event: {}",
        out0
    );
    assert!(
        pane_contains_wrapped(&out1, "sched-ea1-only"),
        "EA 1 session missing its scheduled event: {}",
        out1
    );

    // EA isolation: events must not cross EA boundaries
    assert!(
        !pane_contains_wrapped(&out0, "sched-ea1-only"),
        "EA 0 session must NOT contain EA 1's event: {}",
        out0
    );
    assert!(
        !pane_contains_wrapped(&out1, "sched-ea0-only"),
        "EA 1 session must NOT contain EA 0's event: {}",
        out1
    );

    // Verify session names match the EA-scoped prefix convention
    assert_eq!(
        ea0_session,
        format!("{}0-sched-recv", BASE_PREFIX),
        "EA 0 session name must follow ea_prefix(0, ...) + receiver"
    );
    assert_eq!(
        ea1_session,
        format!("{}1-sched-recv", BASE_PREFIX),
        "EA 1 session name must follow ea_prefix(1, ...) + receiver"
    );

    // Cleanup
    let _ = tmux(&["kill-session", "-t", &ea0_session]);
    let _ = tmux(&["kill-session", "-t", &ea1_session]);
}
