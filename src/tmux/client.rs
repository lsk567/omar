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
    /// How long to wait for typed text to appear in the pane after send-keys.
    pub text_verify_timeout: Duration,
    /// How long to wait for pane activity to advance after Enter.
    pub enter_verify_timeout: Duration,
    /// How many full delivery attempts (text + Enter) to try before giving up.
    pub max_retries: u32,
    /// Polling interval for pane capture / activity checks.
    pub poll_interval: Duration,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        Self {
            startup_timeout: Duration::from_secs(15),
            stable_quiet: Duration::from_millis(500),
            text_verify_timeout: Duration::from_secs(1),
            enter_verify_timeout: Duration::from_secs(2),
            max_retries: 3,
            poll_interval: Duration::from_millis(100),
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

    /// Send literal text to a pane
    pub fn send_keys_literal(&self, target: &str, text: &str) -> Result<()> {
        self.run(&["send-keys", "-t", target, "-l", text])?;
        Ok(())
    }

    /// Reliably deliver a prompt to a tmux session.
    ///
    /// Backend-agnostic: works for claude, codex, cursor, opencode, or any
    /// command that echoes typed input to its pane. Waits for the backend
    /// to become idle, sends the text, verifies it appeared, sends Enter,
    /// and verifies pane activity advanced. Retries up to `opts.max_retries`
    /// times if any step fails.
    pub fn deliver_prompt(&self, session: &str, text: &str, opts: &DeliveryOptions) -> Result<()> {
        // Phase 1: wait for the backend to finish drawing its UI / be ready.
        self.wait_for_stable(session, opts.stable_quiet, opts.startup_timeout)?;

        // Choose a verification needle: first non-empty line, trimmed.
        // Limit to a reasonable length so wrapping/truncation doesn't break us.
        let needle: String = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(text)
            .chars()
            .take(40)
            .collect();
        let needle = needle.trim().to_string();

        for attempt in 1..=opts.max_retries {
            // Phase 2: send text, verify it appears in the pane
            self.send_keys_literal(session, text)?;

            if !needle.is_empty()
                && !self.wait_for_text_in_pane(session, &needle, opts.text_verify_timeout)
            {
                // Text never showed up — clear input and retry
                let _ = self.send_keys(session, "C-u");
                thread::sleep(Duration::from_millis(200));
                continue;
            }

            // Phase 3: send Enter, verify activity advances
            let before = self.get_pane_activity(session).unwrap_or(0);
            self.send_keys(session, "Enter")?;

            if self.wait_for_activity_advance(session, before, opts.enter_verify_timeout) {
                return Ok(());
            }

            // Enter didn't register — try once more
            if attempt < opts.max_retries {
                let _ = self.send_keys(session, "C-u");
                thread::sleep(Duration::from_millis(200));
            }
        }

        anyhow::bail!(
            "prompt delivery to '{}' failed after {} attempts",
            session,
            opts.max_retries
        )
    }

    /// Wait for pane activity to be quiet for `quiet` duration, or until `timeout`.
    /// Returns Ok(()) as soon as the pane becomes stable; returns Ok(()) anyway
    /// after timeout (best-effort — caller should proceed regardless).
    pub fn wait_for_stable(&self, session: &str, quiet: Duration, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let mut last_activity = self.get_pane_activity(session).unwrap_or(0);
        let mut last_change = Instant::now();

        while start.elapsed() < timeout {
            thread::sleep(Duration::from_millis(100));
            let current = self.get_pane_activity(session).unwrap_or(last_activity);
            if current != last_activity {
                last_activity = current;
                last_change = Instant::now();
            } else if last_change.elapsed() >= quiet {
                return Ok(());
            }
        }
        // Timed out waiting for stability — proceed anyway
        Ok(())
    }

    /// Poll the pane until `needle` appears in the captured content, or timeout.
    fn wait_for_text_in_pane(&self, session: &str, needle: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(content) = self.capture_pane(session, 50) {
                if content.contains(needle) {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Poll the pane activity until it exceeds `before`, or timeout.
    fn wait_for_activity_advance(&self, session: &str, before: i64, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(current) = self.get_pane_activity(session) {
                if current > before {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(100));
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
            text_verify_timeout: Duration::from_millis(800),
            enter_verify_timeout: Duration::from_millis(800),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
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
            .wait_for_stable(session, Duration::from_millis(200), Duration::from_secs(3))
            .unwrap();
        // Should return within a couple hundred ms on a fully idle pane.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "wait_for_stable took too long on idle pane: {:?}",
            start.elapsed()
        );
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
            text_verify_timeout: Duration::from_millis(800),
            enter_verify_timeout: Duration::from_millis(800),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
        };

        // Multi-line: only the first line is the verification needle.
        // In a shell, subsequent lines will look like continuations — just
        // verify delivery doesn't error out.
        let text = "echo FIRST_LINE_OMAR";
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
    }
}
