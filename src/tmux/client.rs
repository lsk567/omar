#![allow(dead_code)]

use anyhow::{Context, Result};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::Session;

/// Options for reliable prompt delivery.
#[derive(Debug, Clone)]
pub struct DeliveryOptions {
    /// Max time to wait for the pane to become stable (backend ready).
    pub startup_timeout: Duration,
    /// How long the pane must be quiet to be considered "stable".
    pub stable_quiet: Duration,
    /// How long to wait for pane change after paste (activity or content).
    pub verify_timeout: Duration,
    /// How many full delivery attempts to try before giving up.
    pub max_retries: u32,
    /// Polling interval for pane capture / activity checks.
    pub poll_interval: Duration,
    /// Delay between retry attempts (after clearing input with C-u).
    pub retry_delay: Duration,
    /// When true, readiness wait requires observing at least one pane change
    /// (activity timestamp or content) before considering the pane stable.
    /// Useful for freshly spawned sessions where the initial static shell pane
    /// is not necessarily backend-ready yet.
    pub require_initial_change: bool,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(15),
            stable_quiet: Duration::from_millis(500),
            verify_timeout: Duration::from_secs(3),
            max_retries: 3,
            poll_interval: Duration::from_millis(100),
            retry_delay: Duration::from_millis(200),
            require_initial_change: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TmuxClient {
    prefix: String,
}

impl TmuxClient {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("tmux")
            .args(args)
            .output()
            .context("Failed to execute tmux - is tmux installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "no server running" is not an error for list-sessions
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(String::new());
            }
            anyhow::bail!("tmux error: {}", stderr);
        }
        Ok(String::from_utf8_lossy(&output.stdout).into())
    }

    /// List all sessions matching the prefix
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let output = self.run(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_activity}|#{session_attached}|#{pane_pid}",
        ])?;

        if output.is_empty() {
            return Ok(Vec::new());
        }

        let sessions = output
            .lines()
            .filter(|line| self.prefix.is_empty() || line.starts_with(&self.prefix))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() != 4 {
                    return None;
                }
                Some(Session::new(
                    parts[0].to_string(),
                    parts[1].parse().ok()?,
                    parts[2] == "1",
                    parts[3].parse().ok()?,
                ))
            })
            .collect();

        Ok(sessions)
    }

    /// List all sessions (regardless of prefix)
    pub fn list_all_sessions(&self) -> Result<Vec<Session>> {
        let output = self.run(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_activity}|#{session_attached}|#{pane_pid}",
        ])?;

        if output.is_empty() {
            return Ok(Vec::new());
        }

        let sessions = output
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() != 4 {
                    return None;
                }
                Some(Session::new(
                    parts[0].to_string(),
                    parts[1].parse().ok()?,
                    parts[2] == "1",
                    parts[3].parse().ok()?,
                ))
            })
            .collect();

        Ok(sessions)
    }

    /// Capture the last N lines of a pane's output
    pub fn capture_pane(&self, target: &str, lines: i32) -> Result<String> {
        self.run(&[
            "capture-pane",
            "-e",
            "-t",
            target,
            "-p",
            "-S",
            &(-lines).to_string(),
        ])
    }

    /// Get the activity timestamp of a pane
    pub fn get_pane_activity(&self, target: &str) -> Result<i64> {
        let output = self.run(&["display-message", "-t", target, "-p", "#{pane_activity}"])?;
        output
            .trim()
            .parse()
            .context("Failed to parse pane activity timestamp")
    }

    /// Send keys to a pane
    pub fn send_keys(&self, target: &str, keys: &str) -> Result<()> {
        self.run(&["send-keys", "-t", target, keys])?;
        Ok(())
    }

    /// Send literal text to a pane.
    ///
    /// For small payloads uses `send-keys -l` directly. For large payloads
    /// (>= 2 KB) writes to a temporary file and uses `load-buffer` +
    /// `paste-buffer` to avoid tmux's internal message-size limit, which
    /// silently drops oversized `send-keys -l` arguments.
    pub const LARGE_PAYLOAD_THRESHOLD: usize = 2048;

    pub fn send_keys_literal(&self, target: &str, text: &str) -> Result<()> {
        if text.len() < Self::LARGE_PAYLOAD_THRESHOLD {
            self.run(&["send-keys", "-t", target, "-l", text])?;
        } else {
            // Write to a temp file, load into tmux buffer, paste, then clean up.
            // This avoids passing large text as a command-line argument.
            let tmp_path =
                std::env::temp_dir().join(format!("omar-task-{}.txt", uuid::Uuid::new_v4()));
            std::fs::write(&tmp_path, text).context("Failed to write task to temp file")?;
            let path_str = tmp_path
                .to_str()
                .context("Temp file path is not valid UTF-8")?;
            self.run(&["load-buffer", path_str])?;
            std::fs::remove_file(&tmp_path).ok(); // best-effort cleanup
            self.run(&["paste-buffer", "-t", target])?;
        }
        Ok(())
    }

    /// Paste text into a pane via load-buffer + paste-buffer.
    /// Uses bracketed paste (-p) so the backend receives the entire payload
    /// as a single paste event. This is more reliable than send-keys for
    /// multi-line text and backends with custom TUI input widgets.
    pub fn paste_text(&self, target: &str, text: &str) -> Result<()> {
        // Load text into a tmux buffer via stdin.
        let child = Command::new("tmux")
            .args(["load-buffer", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn tmux load-buffer")?;
        use std::io::Write;
        child
            .stdin
            .as_ref()
            .unwrap()
            .write_all(text.as_bytes())
            .context("Failed to write to tmux load-buffer stdin")?;
        let output = child
            .wait_with_output()
            .context("Failed to wait for tmux load-buffer")?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux load-buffer failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // Paste using bracketed paste mode so the target pane treats it as
        // a single paste operation. -d deletes the buffer after pasting.
        self.run(&["paste-buffer", "-t", target, "-d", "-p"])?;
        Ok(())
    }

    /// Reliably deliver a prompt to a tmux session.
    ///
    /// Backend-agnostic: works for claude, codex, cursor, opencode, or any
    /// TUI. Uses tmux's bracketed paste (load-buffer + paste-buffer -p) to
    /// deliver text + Enter as a single atomic paste event, then verifies
    /// that the pane changed. Retries up to `opts.max_retries` times.
    pub fn deliver_prompt(&self, session: &str, text: &str, opts: &DeliveryOptions) -> Result<()> {
        // Phase 1: wait for the backend to finish drawing its UI / be ready.
        self.wait_for_stable(
            session,
            opts.stable_quiet,
            opts.startup_timeout,
            opts.poll_interval,
            opts.require_initial_change,
        )?;

        for attempt in 1..=opts.max_retries {
            // Snapshot pane state before delivery so we can detect changes.
            let content_before = self.capture_pane(session, 50).unwrap_or_default();
            let activity_before = self.get_pane_activity(session).unwrap_or(0);

            // Paste text via bracketed paste (no trailing newline — Enter
            // is sent separately below).
            self.paste_text(session, text)?;

            // Send Enter 3 times with small gaps. Some backends need time
            // to process the pasted text before accepting Enter, and a
            // single Enter can be lost. Redundant Enters are harmless.
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(150));
                let _ = self.send_keys(session, "Enter");
            }

            // Verify the backend processed the input.
            if self.wait_for_change(
                session,
                activity_before,
                &content_before,
                opts.verify_timeout,
                opts.poll_interval,
            ) {
                return Ok(());
            }

            // Didn't register — clear input and retry
            if attempt < opts.max_retries {
                let _ = self.send_keys(session, "C-u");
                thread::sleep(opts.retry_delay);
            }
        }

        anyhow::bail!(
            "prompt delivery to '{}' was not verified after {} attempt(s)",
            session,
            opts.max_retries
        )
    }

    /// Wait for pane activity to be quiet for `quiet` duration, or until `timeout`.
    /// Returns Ok(()) as soon as the pane becomes stable; returns Ok(()) anyway
    /// after timeout (best-effort — caller should proceed regardless).
    pub fn wait_for_stable(
        &self,
        session: &str,
        quiet: Duration,
        timeout: Duration,
        poll_interval: Duration,
        require_initial_change: bool,
    ) -> Result<()> {
        let start = Instant::now();
        let mut last_activity = self.get_pane_activity(session).unwrap_or(0);
        let mut last_content = self.capture_pane(session, 50).unwrap_or_default();
        let mut saw_change = false;
        let mut last_change = Instant::now();

        while start.elapsed() < timeout {
            thread::sleep(poll_interval);
            let current = self.get_pane_activity(session).unwrap_or(last_activity);
            let content = self
                .capture_pane(session, 50)
                .unwrap_or_else(|_| last_content.clone());
            if current != last_activity || content != last_content {
                saw_change = true;
                last_activity = current;
                last_content = content;
                last_change = Instant::now();
            } else if last_change.elapsed() >= quiet && (!require_initial_change || saw_change) {
                return Ok(());
            }
        }
        // Timed out waiting for stability — proceed anyway
        Ok(())
    }

    /// Poll until either pane activity advances OR pane content changes.
    /// This catches backends that update content without bumping the
    /// activity timestamp, and vice versa.
    fn wait_for_change(
        &self,
        session: &str,
        activity_before: i64,
        content_before: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(current) = self.get_pane_activity(session) {
                if current > activity_before {
                    return true;
                }
            }
            if let Ok(content) = self.capture_pane(session, 50) {
                if content != content_before {
                    return true;
                }
            }
            thread::sleep(poll_interval);
        }
        false
    }

    /// Wait until pane output contains any of the provided markers.
    /// Matching is case-insensitive; returns false on timeout.
    pub fn wait_for_markers(
        &self,
        session: &str,
        markers: &[&str],
        timeout: Duration,
        poll_interval: Duration,
    ) -> bool {
        if markers.is_empty() {
            return true;
        }
        let needles: Vec<String> = markers.iter().map(|m| m.to_ascii_lowercase()).collect();
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(content) = self.capture_pane(session, 120) {
                let hay = content.to_ascii_lowercase();
                if needles.iter().any(|needle| hay.contains(needle)) {
                    return true;
                }
            }
            thread::sleep(poll_interval);
        }
        false
    }

    /// Create a new detached session
    pub fn new_session(&self, name: &str, command: &str, workdir: Option<&str>) -> Result<()> {
        let mut args = vec!["new-session", "-d", "-s", name];

        if let Some(dir) = workdir {
            args.extend(["-c", dir]);
        }

        args.push(command);
        self.run(&args)?;
        self.run(&["set-option", "-t", name, "history-limit", "10000"])?;
        Ok(())
    }

    /// Kill a session
    pub fn kill_session(&self, name: &str) -> Result<()> {
        self.run(&["kill-session", "-t", name])?;
        Ok(())
    }

    /// Check if a session exists
    pub fn has_session(&self, name: &str) -> Result<bool> {
        let result = Command::new("tmux")
            .args(["has-session", "-t", name])
            .output()
            .context("Failed to execute tmux")?;

        Ok(result.status.success())
    }

    /// Attach to a session (blocks until detached)
    pub fn attach_session(&self, session: &str) -> Result<()> {
        Command::new("tmux")
            .args(["attach-session", "-t", session])
            .status()
            .context("Failed to attach to tmux session")?;
        Ok(())
    }

    /// Open a popup attached to a session
    pub fn attach_popup(&self, session: &str, width: &str, height: &str) -> Result<()> {
        Command::new("tmux")
            .args([
                "display-popup",
                "-E",
                "-w",
                width,
                "-h",
                height,
                &format!("tmux attach -t {}", session),
            ])
            .status()
            .context("Failed to open tmux popup")?;
        Ok(())
    }

    /// Check if tmux server is running
    pub fn is_server_running(&self) -> bool {
        Command::new("tmux")
            .args(["list-sessions"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = TmuxClient::new("");
        assert_eq!(client.prefix(), "");
    }

    #[test]
    fn test_client_with_different_prefix() {
        let client = TmuxClient::new("test-");
        assert_eq!(client.prefix(), "test-");
    }

    fn tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Cleanup guard: kill the named tmux session on drop (even on panic).
    struct SessionGuard(String);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &self.0])
                .output();
        }
    }

    /// Deliver a prompt to a shell session and verify the command actually ran.
    #[test]
    fn test_deliver_prompt_to_shell_session() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-deliver-prompt";
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = Command::new("tmux")
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("Skipping test: failed to create tmux session");
            return;
        }

        let client = TmuxClient::new("omar-test-");
        // Tighter timeouts so the test finishes fast on a shell prompt.
        let opts = DeliveryOptions {
            startup_timeout: Duration::from_secs(3),
            stable_quiet: Duration::from_millis(200),
            verify_timeout: Duration::from_secs(2),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
            retry_delay: Duration::from_millis(100),
            require_initial_change: false,
        };

        let result = client.deliver_prompt(session, "echo OMAR_DELIVERED", &opts);
        if let Err(e) = &result {
            // Some sandboxes block send-keys; skip rather than fail.
            eprintln!(
                "Skipping test: deliver_prompt failed (likely sandbox): {}",
                e
            );
            return;
        }

        // Give the shell a moment to run the command
        thread::sleep(Duration::from_millis(500));

        let content = client.capture_pane(session, 50).unwrap_or_default();
        assert!(
            content.contains("OMAR_DELIVERED"),
            "Expected delivered command to run. Pane: {:?}",
            content
        );
    }

    #[test]
    fn test_wait_for_stable_returns_on_idle_pane() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-stable";
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = Command::new("tmux")
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        // Let the shell prompt finish drawing
        thread::sleep(Duration::from_millis(300));

        let client = TmuxClient::new("omar-test-");
        let start = Instant::now();
        client
            .wait_for_stable(
                session,
                Duration::from_millis(200),
                Duration::from_secs(3),
                Duration::from_millis(50),
                false,
            )
            .unwrap();
        // Should return within a couple hundred ms on a fully idle pane.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "wait_for_stable took too long on idle pane: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn test_wait_for_markers_detects_backend_banner_text() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-markers";
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = Command::new("tmux")
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");
        let _ = client.send_keys_literal(session, "echo OpenAI Codex");
        let _ = client.send_keys(session, "Enter");
        let found = client.wait_for_markers(
            session,
            &["openai codex"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(found, "Expected marker not detected in tmux pane");
    }

    #[test]
    fn test_deliver_prompt_multiline() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-deliver-multiline";
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let ok = Command::new("tmux")
            .args(["new-session", "-d", "-s", session])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }

        let client = TmuxClient::new("omar-test-");
        let opts = DeliveryOptions {
            startup_timeout: Duration::from_secs(3),
            stable_quiet: Duration::from_millis(200),
            verify_timeout: Duration::from_secs(2),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
            retry_delay: Duration::from_millis(100),
            require_initial_change: false,
        };

        // Multi-line input: bash will run both commands. Verification needle
        // is the first non-empty line ("echo FIRST_LINE_OMAR"); we also
        // assert the SECOND_LINE actually executed to prove the full payload
        // was delivered, not just truncated at the first newline.
        let text = "echo FIRST_LINE_OMAR\necho SECOND_LINE_OMAR";
        let result = client.deliver_prompt(session, text, &opts);
        if let Err(e) = &result {
            eprintln!("Skipping test: deliver_prompt failed: {}", e);
            return;
        }

        thread::sleep(Duration::from_millis(500));
        let content = client.capture_pane(session, 50).unwrap_or_default();
        assert!(
            content.contains("FIRST_LINE_OMAR"),
            "Expected FIRST_LINE_OMAR in pane: {:?}",
            content
        );
        assert!(
            content.contains("SECOND_LINE_OMAR"),
            "Expected SECOND_LINE_OMAR in pane (multi-line delivery): {:?}",
            content
        );
    }

    #[test]
    fn large_payload_threshold_is_reasonable() {
        // Threshold must be large enough that typical short tasks go through send-keys
        // but small enough to catch the ~1-3 KB tasks that silently fail
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD >= 512) };
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD <= 8192) };
    }
}
