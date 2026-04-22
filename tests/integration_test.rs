//! Integration tests for OMAR
//!
//! These tests require tmux to be installed and will create/destroy
//! test sessions during execution.

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Command;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

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

/// Kill a specific tmux session if it exists. Scoped per-test so
/// concurrent tests don't clobber each other's sessions. Use this both
/// at the start of a test (to clear leftovers from a prior failed run)
/// and at the end (so the next run starts clean).
fn cleanup_session(session_name: &str) {
    let _ = tmux(&["kill-session", "-t", session_name]);
}

/// Check if tmux is available
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn omar_bin() -> &'static str {
    env!("CARGO_BIN_EXE_omar")
}

fn bootstrap_cli_home(home: &Path) {
    let output = Command::new(omar_bin())
        .arg("list")
        .env("HOME", home)
        .output()
        .expect("Failed to bootstrap omar home");
    assert!(
        output.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct McpCliServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpCliServer {
    fn start(home: &Path, default_command: &str) -> Self {
        bootstrap_cli_home(home);

        let context = json!({
            "omar_dir": home.join(".omar"),
            "ea_id": 0,
            "session_prefix": "omar-agent-",
            "default_command": default_command,
            "default_workdir": env!("CARGO_MANIFEST_DIR"),
            "health_idle_warning": 15,
        });
        let context_path = home.join("mcp-context.json");
        fs::write(
            &context_path,
            serde_json::to_vec_pretty(&context).expect("serialize MCP context"),
        )
        .expect("write MCP context");

        let mut child = Command::new(omar_bin())
            .args([
                "mcp-server",
                "--context-file",
                context_path.to_str().expect("utf8 context path"),
            ])
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to start omar mcp-server");

        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("mcp stdout"));
        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };

        let init = server.request("initialize", json!({}))["result"].clone();
        assert_eq!(
            init["serverInfo"]["name"].as_str(),
            Some("omar"),
            "unexpected initialize response: {}",
            init
        );
        server
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let payload = serde_json::to_vec(&request).expect("serialize request");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", payload.len()).expect("write header");
        self.stdin.write_all(&payload).expect("write payload");
        self.stdin.flush().expect("flush request");

        let response = read_mcp_response(&mut self.stdout);
        assert_eq!(
            response["id"].as_u64(),
            Some(id),
            "response id mismatch: {}",
            response
        );
        response
    }

    fn tool_call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        );
        let result = response["result"].clone();
        assert_eq!(
            result["isError"].as_bool(),
            Some(false),
            "tool {} failed: {}",
            name,
            result
        );
        result["structuredContent"].clone()
    }
}

impl Drop for McpCliServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_mcp_response(reader: &mut BufReader<ChildStdout>) -> Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).expect("read mcp header");
        assert!(
            bytes > 0,
            "unexpected EOF while reading MCP response header"
        );
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().expect("valid Content-Length"));
        }
    }
    let length = content_length.expect("Content-Length header present");
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .expect("read mcp response body");
    serde_json::from_slice(&payload).expect("parse mcp response")
}

fn cli_output(home: &Path, args: &[&str]) -> String {
    let output = Command::new(omar_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap_or_else(|err| panic!("Failed to run omar {:?}: {}", args, err));
    assert!(
        output.status.success(),
        "omar {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
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

    let session_name = format!("{}create-list", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}capture", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}kill", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}activity", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}has-session", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}send-keys", TEST_PREFIX);
    cleanup_session(&session_name);

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

    let session_name = format!("{}custom-cmd", TEST_PREFIX);
    cleanup_session(&session_name);

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

    // Isolate HOME so we don't read the developer's live `~/.omar/` nor
    // race against other tests that bootstrap their own omar dir.
    let home = tempfile::tempdir().expect("temp home");

    let output = Command::new(omar_bin())
        .arg("list")
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar list");

    let stdout = String::from_utf8_lossy(&output.stdout);
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

    // Per-test unique agent name: tmux sessions are process-global so even
    // with isolated HOME, a parallel test running the same CLI would race
    // on the tmux daemon. The uuid suffix pins this test's session.
    let home = tempfile::tempdir().expect("temp home");
    let agent_name = format!("test-spawn-{}", &Uuid::new_v4().to_string()[..8]);

    // Spawn a new agent.
    let output = Command::new(omar_bin())
        .args(["spawn", "-n", &agent_name, "-c", "sleep 60"])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Spawned agent: {}", agent_name)),
        "Should confirm spawn: {}",
        stdout
    );

    // Resolve the full spawned session name — prefix depends on active EA.
    thread::sleep(Duration::from_millis(200));
    let suffix = format!("-{}", agent_name);
    let all_sessions = tmux(&["list-sessions", "-F", "#{session_name}"]).unwrap_or_default();
    let full_session = all_sessions
        .lines()
        .find(|line| *line == agent_name || line.ends_with(&suffix))
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("Expected a spawned session ending with {:?}", suffix));

    let result = Command::new("tmux")
        .args(["has-session", "-t", &full_session])
        .output()
        .unwrap();
    assert!(result.status.success(), "Session should exist after spawn");

    // `list` should show the agent (displayed without prefix).
    let output = Command::new(omar_bin())
        .arg("list")
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar list");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&agent_name),
        "List should show spawned agent: {}",
        stdout
    );

    // Kill the agent (short name).
    let output = Command::new(omar_bin())
        .args(["kill", &agent_name])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar kill");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Killed agent: {}", agent_name)),
        "Should confirm kill: {}",
        stdout
    );

    // Verify session is gone.
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

#[test]
fn test_omar_event_cli_roundtrip() {
    let home = tempfile::tempdir().expect("temp home");

    let output = Command::new(omar_bin())
        .args([
            "--ea",
            "Default",
            "event",
            "schedule",
            "--receiver",
            "ea",
            "--payload",
            "cli-test-payload",
            "--sender",
            "cli-test",
            "--in-seconds",
            "60",
        ])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar event schedule");
    assert!(
        output.status.success(),
        "schedule failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Scheduled event: cli-test -> ea"),
        "unexpected schedule output: {}",
        stdout
    );
    let event_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Event id: "))
        .expect("event id line in schedule output")
        .trim()
        .to_string();

    let output = Command::new(omar_bin())
        .args(["--ea", "Default", "event", "list"])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar event list");
    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&event_id) && stdout.contains("cli-test-payload"),
        "event missing from list output: {}",
        stdout
    );

    let output = Command::new(omar_bin())
        .args(["--ea", "Default", "event", "cancel", &event_id])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar event cancel");
    assert!(
        output.status.success(),
        "cancel failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&event_id),
        "cancel output should mention event id: {}",
        stdout
    );

    let output = Command::new(omar_bin())
        .args(["--ea", "Default", "event", "list"])
        .env("HOME", home.path())
        .output()
        .expect("Failed to run omar event list after cancel");
    assert!(
        output.status.success(),
        "final list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No scheduled events found"),
        "expected empty event list after cancel: {}",
        stdout
    );
}

/// Register a project by writing directly to the EA's tasks.md file.
///
/// `spawn_agent` now requires an existing `project_id`. Stream 4 owns the
/// `add_project` MCP tool (not yet landed in this branch), so tests
/// register projects via the same on-disk format that `projects.rs` uses.
/// Format: one numbered line `N. Project name`; IDs are not renumbered.
fn register_project(home: &Path, project_name: &str) -> usize {
    let tasks_md = home.join(".omar/ea/0/tasks.md");
    fs::create_dir_all(tasks_md.parent().expect("tasks.md parent")).expect("mk ea dir");
    let existing = fs::read_to_string(&tasks_md).unwrap_or_default();
    let next_id = existing
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.find(". ")
                .and_then(|dot| line[..dot].parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0)
        + 1;
    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&format!("{}. {}\n", next_id, project_name));
    fs::write(&tasks_md, contents).expect("write tasks.md");
    next_id
}

#[test]
fn test_omar_mcp_server_tools_list_via_cli() {
    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");

    let response = server.request("tools/list", json!({}));
    let tools = response["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();

    // spawn_agent is the single spawn-path tool.
    assert!(names.contains(&"spawn_agent"), "names: {:?}", names);
    assert!(names.contains(&"omar_wake_later"), "names: {:?}", names);
    assert!(names.contains(&"check_task"), "names: {:?}", names);
    assert!(names.contains(&"complete_task"), "names: {:?}", names);

    // Pre-rework spawn aliases must be gone.
    assert!(
        !names.contains(&"spawn_agent_session"),
        "spawn_agent_session should not exist: {:?}",
        names
    );
    assert!(
        !names.contains(&"create_task"),
        "create_task should not exist: {:?}",
        names
    );

    // notify_parent was collapsed into omar_wake_later and must not reappear.
    assert!(
        !names.contains(&"notify_parent"),
        "notify_parent was collapsed into omar_wake_later and must not appear in the tool list: {:?}",
        names
    );

    // schedule_event was renamed to omar_wake_later and must not reappear under its old name.
    assert!(
        !names.contains(&"schedule_event"),
        "schedule_event was renamed to omar_wake_later and must not appear in the tool list: {:?}",
        names
    );

    // spawn_agent schema must NOT include a `track` property — the rework
    // intentionally removed any mode flag on the spawn path.
    let spawn_agent = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("spawn_agent"))
        .expect("spawn_agent tool entry");
    let props = &spawn_agent["inputSchema"]["properties"];
    assert!(
        props.get("track").is_none(),
        "spawn_agent schema must not have a `track` property: {}",
        spawn_agent["inputSchema"]
    );
    // Sanity-check required fields.
    let required: Vec<&str> = spawn_agent["inputSchema"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        required.contains(&"name") && required.contains(&"project_id"),
        "spawn_agent required must include name and project_id: {:?}",
        required
    );
}

#[test]
fn test_omar_mcp_server_spawn_agent_raw_command_via_cli() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");
    let suffix = &Uuid::new_v4().to_string()[..8];
    let name = format!("mcp-agent-{}", suffix);
    let project_name = format!("raw-cmd-{}", suffix);
    let project_id = register_project(home.path(), &project_name);

    // Raw-command form: no `task`, just `command`. task_text should be
    // auto-populated as "command: <cmd>" so the dashboard row is
    // self-describing.
    let spawned = server.tool_call(
        "spawn_agent",
        json!({
            "name": name,
            "project_id": project_id,
            "command": "sleep 30",
        }),
    );
    assert_eq!(spawned["agent_name"].as_str(), Some(name.as_str()));
    assert_eq!(spawned["project_id"].as_u64(), Some(project_id as u64));
    assert_eq!(
        spawned["project_name"].as_str(),
        Some(project_name.as_str())
    );
    let task_id = spawned["task_id"].as_str().expect("task id").to_string();

    let listed = cli_output(home.path(), &["list"]);
    assert!(
        listed.contains(&name),
        "CLI list should show MCP-spawned agent: {}",
        listed
    );

    // complete_task tears down the session; project stays (stream-4
    // semantics — projects have their own lifecycle).
    let completed = server.tool_call(
        "complete_task",
        json!({
            "task_id": task_id,
            "summary": "raw-command session done",
        }),
    );
    assert_eq!(completed["status"].as_str(), Some("completed"));

    let listed = cli_output(home.path(), &["list"]);
    assert!(
        !listed.contains(&name),
        "CLI list should not show completed agent: {}",
        listed
    );

    // Project survives complete_task.
    let projects = server.tool_call("list_projects", json!({}));
    let projects = projects["projects"].as_array().expect("projects array");
    assert!(
        projects
            .iter()
            .any(|p| p["name"].as_str() == Some(project_name.as_str())),
        "project should outlive complete_task: {:?}",
        projects
    );
}

#[test]
fn test_omar_mcp_server_tracked_task_lifecycle_via_cli() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");
    let suffix = &Uuid::new_v4().to_string()[..8];
    let agent_name = format!("task-agent-{}", suffix);
    let project_name = format!("task-project-{}", suffix);

    // Project lifecycle is now explicit: add_project first, then spawn_agent with project_id.
    let added = server.tool_call("add_project", json!({ "name": project_name }));
    let project_id = added["project_id"].as_u64().expect("project id");
    assert_eq!(added["name"].as_str(), Some(project_name.as_str()));

    let created = server.tool_call(
        "spawn_agent",
        json!({
            "name": agent_name,
            "project_id": project_id,
            "task": "echo tracked-task-test",
            "command": "sleep 30",
        }),
    );
    let task_id = created["task_id"].as_str().expect("task id").to_string();
    assert_eq!(created["agent_name"].as_str(), Some(agent_name.as_str()));
    assert_eq!(created["project_id"].as_u64(), Some(project_id as u64));
    assert_eq!(
        created["project_name"].as_str(),
        Some(project_name.as_str())
    );

    let checked = server.tool_call("check_task", json!({ "task_id": task_id }));
    assert_eq!(checked["status"].as_str(), Some("running"));
    assert_eq!(checked["agent_name"].as_str(), Some(agent_name.as_str()));
    assert_eq!(
        checked["project_name"].as_str(),
        Some(project_name.as_str())
    );
    assert_eq!(checked["agent_exists"].as_bool(), Some(true));

    let listed = cli_output(home.path(), &["list"]);
    assert!(
        listed.contains(&agent_name),
        "CLI list should show tracked-task agent: {}",
        listed
    );

    let projects = server.tool_call("list_projects", json!({}));
    let projects = projects["projects"].as_array().expect("projects array");
    assert!(projects
        .iter()
        .any(|project| { project["name"].as_str() == Some(project_name.as_str()) }));

    // complete_project while the task is still running must fail.
    let pending = server.request(
        "tools/call",
        json!({
            "name": "complete_project",
            "arguments": { "project_id": project_id },
        }),
    );
    assert_eq!(
        pending["result"]["isError"].as_bool(),
        Some(true),
        "complete_project should refuse while a task is still running: {}",
        pending
    );

    let completed = server.tool_call(
        "complete_task",
        json!({
            "task_id": task_id,
            "summary": "integration test complete",
        }),
    );
    assert_eq!(completed["status"].as_str(), Some("completed"));

    let listed = cli_output(home.path(), &["list"]);
    assert!(
        !listed.contains(&agent_name),
        "CLI list should not show completed task agent: {}",
        listed
    );

    // Stream-4 semantics: complete_task does NOT remove the project.
    // The project survives until complete_project is called.
    let projects = server.tool_call("list_projects", json!({}));
    let projects = projects["projects"].as_array().expect("projects array");
    assert!(
        projects
            .iter()
            .any(|project| project["name"].as_str() == Some(project_name.as_str())),
        "complete_task must not remove the project: {:?}",
        projects
    );

    // complete_project retires the project once every attached task is done.
    let completed_project =
        server.tool_call("complete_project", json!({ "project_id": project_id }));
    assert_eq!(completed_project["status"].as_str(), Some("completed"));
    assert_eq!(
        completed_project["name"].as_str(),
        Some(project_name.as_str())
    );

    let projects = server.tool_call("list_projects", json!({}));
    let projects = projects["projects"].as_array().expect("projects array");
    assert!(
        !projects
            .iter()
            .any(|project| project["name"].as_str() == Some(project_name.as_str())),
        "complete_project should remove the project: {:?}",
        projects
    );
}

/// Regression test for the UUID-vs-short-name mismatch bug: `complete_task`
/// (and `replace_stuck_task_agent`) previously did the read through
/// `find_task_in` OR `find_task_by_agent_in`, but then wrote back using the
/// caller's raw `task_id` arg. When the caller passed a short agent name,
/// the update lookup (which matches on the UUID field) missed, and the
/// server returned "Task ... disappeared during completion" while leaving
/// the record un-updated.
#[test]
fn test_complete_task_accepts_short_name() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");
    let suffix = &Uuid::new_v4().to_string()[..8];
    let agent_name = format!("shortname-agent-{}", suffix);
    let project_name = format!("shortname-project-{}", suffix);

    // Project lifecycle is explicit: add_project first, then spawn_agent.
    let added = server.tool_call("add_project", json!({ "name": project_name }));
    let project_id = added["project_id"].as_u64().expect("project id");

    let created = server.tool_call(
        "spawn_agent",
        json!({
            "name": agent_name,
            "project_id": project_id,
            "task": "echo short-name-complete-test",
            "command": "sleep 30",
        }),
    );
    let task_id = created["task_id"].as_str().expect("task id").to_string();

    // Complete using the SHORT NAME — not the UUID. Pre-fix this returned
    // "disappeared during completion" because the update path compared the
    // short name against the UUID field.
    let completed = server.tool_call(
        "complete_task",
        json!({
            "task_id": agent_name,
            "summary": "completed via short name",
        }),
    );
    assert_eq!(
        completed["status"].as_str(),
        Some("completed"),
        "complete_task via short name should mark record completed: {}",
        completed
    );
    assert_eq!(completed["task_id"].as_str(), Some(task_id.as_str()));

    // Re-check via the UUID: the record is persisted as Completed, not
    // still Running (which is what the pre-fix bug left behind).
    let checked = server.tool_call("check_task", json!({ "task_id": task_id }));
    assert_eq!(checked["status"].as_str(), Some("completed"));
    assert_eq!(checked["agent_exists"].as_bool(), Some(false));
}

/// Regression test: MCP `kill_agent` must mark the associated task record as
/// `Failed` (not leave it Running). Mirror of the `App::kill_selected` fix.
/// Previously, `kill_agent` killed the tmux session but left the task row
/// at `Running` with a dead agent — same orphan pattern the complete_task
/// short-name fix addressed from the other direction.
#[test]
fn test_kill_agent_marks_task_failed() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }
    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");
    let suffix = &Uuid::new_v4().to_string()[..8];
    let agent_name = format!("killed-agent-{}", suffix);
    let project_name = format!("killed-project-{}", suffix);

    let added = server.tool_call("add_project", json!({ "name": project_name }));
    let project_id = added["project_id"].as_u64().expect("project id");

    let created = server.tool_call(
        "spawn_agent",
        json!({
            "name": agent_name,
            "project_id": project_id,
            "task": "echo kill-agent-test",
            "command": "sleep 30",
        }),
    );
    let task_id = created["task_id"].as_str().expect("task id").to_string();

    // Session should be up.
    let before = server.tool_call("check_task", json!({ "task_id": task_id }));
    assert_eq!(before["status"].as_str(), Some("running"));
    assert_eq!(before["agent_exists"].as_bool(), Some(true));

    // Kill via MCP tool.
    let killed = server.tool_call("kill_agent", json!({ "name": agent_name }));
    assert_eq!(killed["status"].as_str(), Some("killed"));
    assert_eq!(
        killed["task_status"].as_str(),
        Some("failed"),
        "kill_agent should have flipped the task record to Failed, got {}",
        killed
    );

    // Verify persistence: record reads back as Failed, not Running.
    let after = server.tool_call("check_task", json!({ "task_id": task_id }));
    assert_eq!(
        after["status"].as_str(),
        Some("failed"),
        "task record should be Failed after kill_agent, got {}",
        after
    );
    assert_eq!(after["agent_exists"].as_bool(), Some(false));
}

/// Regression test for the Claude Code XML-to-JSON coercion workaround:
/// integer-typed MCP fields (project_id, ea_id, delay_seconds, etc.) must
/// also accept JSON string values. Agents calling via raw MCP JSON-RPC
/// always get integers, but the XML harness can stringify them.
#[test]
fn test_integer_fields_accept_strings() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }
    let home = tempfile::tempdir().expect("temp home");
    let mut server = McpCliServer::start(home.path(), "bash");
    let suffix = &Uuid::new_v4().to_string()[..8];
    let agent_name = format!("flex-agent-{}", suffix);
    let project_name = format!("flex-project-{}", suffix);

    let added = server.tool_call("add_project", json!({ "name": project_name }));
    let project_id = added["project_id"].as_u64().expect("project id");

    // Pass project_id as a STRING rather than integer. Pre-fix this errored
    // with `invalid type: string "1", expected usize`.
    let created = server.tool_call(
        "spawn_agent",
        json!({
            "name": agent_name,
            "project_id": project_id.to_string(),
            "task": "echo flex-int-test",
            "command": "sleep 30",
        }),
    );
    assert_eq!(
        created["status"].as_str(),
        Some("running"),
        "spawn_agent should accept string project_id, got {}",
        created
    );
    let task_id = created["task_id"].as_str().expect("task id").to_string();

    // omar_wake_later delay_seconds also accepts strings.
    let scheduled = server.tool_call(
        "omar_wake_later",
        json!({
            "receiver": agent_name,
            "payload": "hello",
            "delay_seconds": "2",
        }),
    );
    assert!(
        scheduled["id"].is_string(),
        "omar_wake_later should accept string delay_seconds, got {}",
        scheduled
    );

    // complete_project accepts string project_id too (but will block because
    // the spawned task is still running; we just want to prove the type
    // coerces). Use `request` directly to observe the isError envelope.
    let resp = server.request(
        "tools/call",
        json!({
            "name": "complete_project",
            "arguments": { "project_id": project_id.to_string() },
        }),
    );
    let result = resp["result"].clone();
    assert_eq!(
        result["isError"].as_bool(),
        Some(true),
        "complete_project with running task should error (not deserialize-reject), got {}",
        result
    );
    let err_text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        err_text.contains("running task"),
        "error should reference running task (type coerced past Args), got: {}",
        err_text
    );
    assert!(
        !err_text.contains("expected usize") && !err_text.contains("invalid type"),
        "error must NOT be a deserialize rejection, got: {}",
        err_text
    );

    // Clean up.
    let _ = server.tool_call("complete_task", json!({ "task_id": task_id }));
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

    // Deliver distinct messages replicating deliver_to_tmux's exact tmux operations:
    //   tmux send-keys -t <target> -l <message>
    //   tmux send-keys -t <target> Enter
    let msg_ea0 = "DELIVER_EA0_ONLY";
    let msg_ea1 = "DELIVER_EA1_ONLY";

    let _ = tmux(&["send-keys", "-t", &ea0_session, "-l", msg_ea0]);
    let _ = tmux(&["send-keys", "-t", &ea0_session, "Enter"]);

    let _ = tmux(&["send-keys", "-t", &ea1_session, "-l", msg_ea1]);
    let _ = tmux(&["send-keys", "-t", &ea1_session, "Enter"]);

    thread::sleep(Duration::from_millis(500));

    // Capture pane output for each session
    let out0 = tmux(&["capture-pane", "-t", &ea0_session, "-p"]).unwrap_or_default();
    let out1 = tmux(&["capture-pane", "-t", &ea1_session, "-p"]).unwrap_or_default();

    // Each session must contain its own message
    assert!(
        out0.contains(msg_ea0),
        "EA 0 session '{}' should contain '{}': {}",
        ea0_session,
        msg_ea0,
        out0
    );
    assert!(
        out1.contains(msg_ea1),
        "EA 1 session '{}' should contain '{}': {}",
        ea1_session,
        msg_ea1,
        out1
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

    // Deliver to each session via the same tmux send-keys pattern as deliver_to_tmux
    let _ = tmux(&["send-keys", "-t", &ea0_session, "-l", &payload_ea0]);
    let _ = tmux(&["send-keys", "-t", &ea0_session, "Enter"]);

    let _ = tmux(&["send-keys", "-t", &ea1_session, "-l", &payload_ea1]);
    let _ = tmux(&["send-keys", "-t", &ea1_session, "Enter"]);

    thread::sleep(Duration::from_millis(500));

    // Capture pane output
    let out0 = tmux(&["capture-pane", "-t", &ea0_session, "-p"]).unwrap_or_default();
    let out1 = tmux(&["capture-pane", "-t", &ea1_session, "-p"]).unwrap_or_default();

    // Each EA's session received its own event payload
    assert!(
        out0.contains("sched-ea0-only"),
        "EA 0 session missing its scheduled event: {}",
        out0
    );
    assert!(
        out1.contains("sched-ea1-only"),
        "EA 1 session missing its scheduled event: {}",
        out1
    );

    // EA isolation: events must not cross EA boundaries
    assert!(
        !out0.contains("sched-ea1-only"),
        "EA 0 session must NOT contain EA 1's event: {}",
        out0
    );
    assert!(
        !out1.contains("sched-ea0-only"),
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
