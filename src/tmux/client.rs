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

    /// Capture pane output as plain text (no ANSI escapes). Required for
    /// substring matching — e.g. Claude Code renders its banner as
    /// `Claude<ESC>[0m <ESC>[1mCode`, so an ANSI capture does *not* contain
    /// the contiguous string "Claude Code".
    pub fn capture_pane_plain(&self, target: &str, lines: i32) -> Result<String> {
        self.run(&[
            "capture-pane",
            "-t",
            target,
            "-p",
            "-S",
            &(-lines).to_string(),
        ])
    }

    /// Get the activity timestamp of a pane.
    ///
    /// Uses `#{window_activity}` — the per-pane `#{pane_activity}` format is
    /// empty on tmux 3.6a (macOS Homebrew) unless `monitor-activity` is
    /// enabled, which breaks every readiness check (empty string → parse
    /// error → caller's `unwrap_or_default()` yields 0 → health always
    /// reads as "idle", which makes EAs replace working agents). Window
    /// activity is universally populated and tracks the most recent
    /// input/output in the window. OMAR worker sessions always have
    /// exactly one window and one pane, so window-level granularity is
    /// equivalent to pane-level.
    pub fn get_pane_activity(&self, target: &str) -> Result<i64> {
        let output = self.run(&["display-message", "-t", target, "-p", "#{window_activity}"])?;
        output
            .trim()
            .parse()
            .context("Failed to parse window activity timestamp")
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
            // `--` ensures payloads beginning with `-` are treated as text,
            // not tmux flags (e.g. manager orchestration bullet lines).
            self.run(&["send-keys", "-t", target, "-l", "--", text])?;
        } else {
            // Write to a temp file, load into tmux buffer, paste, then clean up.
            // This avoids passing large text as a command-line argument.
            let tmp_path =
                std::env::temp_dir().join(format!("omar-task-{}.txt", uuid::Uuid::new_v4()));
            std::fs::write(&tmp_path, text).context("Failed to write task to temp file")?;
            let path_str = tmp_path
                .to_str()
                .context("Temp file path is not valid UTF-8")?;
            let buffer_name = format!("omar-{}", uuid::Uuid::new_v4());
            self.run(&["load-buffer", "-b", &buffer_name, path_str])?;
            std::fs::remove_file(&tmp_path).ok(); // best-effort cleanup
            self.run(&["paste-buffer", "-b", &buffer_name, "-t", target, "-d"])?;
        }
        Ok(())
    }

    /// Paste text into a pane via load-buffer + paste-buffer.
    /// Uses bracketed paste (-p) so the backend receives the entire payload
    /// as a single paste event. This is more reliable than send-keys for
    /// multi-line text and backends with custom TUI input widgets.
    ///
    /// A unique named buffer (`-b <uuid>`) is used per call so concurrent
    /// deliveries — e.g. an initial-task spawn racing a scheduler event —
    /// cannot clobber each other's payload via the shared unnamed buffer.
    /// `-d` on paste-buffer deletes the buffer after pasting, so buffers
    /// do not accumulate on error paths either.
    ///
    /// `-r` disables tmux's default LF→CR replacement inside the paste.
    /// Without it, every `\n` in the payload is delivered to the target pane
    /// as `\r`. TUI backends like Claude Code run the terminal in raw mode
    /// (ICRNL off), so those CRs are not translated back to LF and each one
    /// reads as an Enter keypress inside the input widget — turning a single
    /// multi-line prompt into a cascade of blank submissions. Reproduced on
    /// tmux 3.2a (Ubuntu 22.04); newer tmux builds on macOS appear to mask
    /// this but `-r` makes behavior identical across versions.
    pub fn paste_text(&self, target: &str, text: &str) -> Result<()> {
        let buffer_name = format!("omar-{}", uuid::Uuid::new_v4());
        // Load text into a tmux buffer via stdin.
        let child = Command::new("tmux")
            .args(["load-buffer", "-b", &buffer_name, "-"])
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
        // Paste from the named buffer using bracketed paste mode so the
        // target pane treats it as a single paste operation. `-d` deletes
        // the buffer after pasting. `-r` preserves LFs verbatim — see the
        // doc comment above for why this matters for raw-mode TUIs.
        self.run(&[
            "paste-buffer",
            "-b",
            &buffer_name,
            "-t",
            target,
            "-d",
            "-p",
            "-r",
        ])?;
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
        )?;

        // Phase 2: settle delay after markers/stability confirm readiness.
        // Claude Code v2.1.116 paints its banner + input-widget frame
        // several hundred ms before the widget is actually wired up to
        // accept keystrokes; without this pause, the Enter presses fired
        // by this function land before the widget is live and get
        // swallowed, while the pasted payload (which hits the terminal
        // input stream directly) has already bumped the activity
        // timestamp — masking the failure from wait_for_change.
        thread::sleep(Duration::from_millis(500));

        for attempt in 1..=opts.max_retries {
            // Paste text via bracketed paste (no trailing newline — Enter
            // is sent separately below). `paste_text` uses the `-r` flag so
            // LFs survive verbatim in raw-mode TUIs.
            self.paste_text(session, text)?;

            // Snapshot pane state AFTER paste but BEFORE the Enter keys,
            // so wait_for_change below actually verifies that the Enter
            // submitted the prompt — not merely that paste_text drew the
            // payload into the input widget. Without this ordering, a
            // swallowed Enter still appears "successful" because the
            // paste itself counts as a pane change.
            let content_before = self.capture_pane(session, 50).unwrap_or_default();
            let activity_before = self.get_pane_activity(session).unwrap_or(0);

            // Send Enter 3 times with 400 ms gaps. The wider gap gives
            // Claude Code's input widget time to finish ingesting the
            // paste (it runs a debounce before accepting submit) before
            // we try to trigger submission. Redundant Enters are harmless
            // on all supported backends.
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(400));
                let _ = self.send_keys(session, "Enter");
            }

            // Verify the backend processed the Enter (not just the paste).
            if self.wait_for_change(
                session,
                activity_before,
                &content_before,
                opts.verify_timeout,
                opts.poll_interval,
            ) {
                // Belt-and-suspenders: one more trailing Enter after
                // verification. If the widget had only just become live
                // during the retry window, the observed pane change may
                // have been a caret blink or spinner rather than actual
                // submission — this extra Enter guarantees submit in
                // that edge case and is a no-op when the prompt has
                // already been accepted.
                let _ = self.send_keys(session, "Enter");
                return Ok(());
            }

            // Didn't register — clear input and retry
            if attempt < opts.max_retries {
                let _ = self.send_keys(session, "C-u");
                thread::sleep(opts.retry_delay);
            }
        }

        // Best-effort: even if verification failed, the prompt may have been
        // delivered (some backends are slow to reflect changes). Log so the
        // operator can investigate if the agent appears stuck.
        eprintln!(
            "[deliver_prompt] verification failed after {} retries for session '{}'",
            opts.max_retries, session
        );
        Ok(())
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
    ) -> Result<()> {
        let start = Instant::now();
        let mut last_activity = self.get_pane_activity(session).unwrap_or(0);
        let mut last_change = Instant::now();

        while start.elapsed() < timeout {
            thread::sleep(poll_interval);
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

    /// Wait until pane output contains ALL of the provided markers.
    /// Matching is case-insensitive; returns false on timeout.
    ///
    /// All-match (rather than any-match) semantics matter for backends like
    /// Claude Code v2.1.116, where the product banner paints hundreds of
    /// ms before the input widget is actually ready to accept keystrokes.
    /// Matching on only one of several markers would let the caller fire
    /// Enter into a pane that silently swallows it.
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
            if let Ok(content) = self.capture_pane_plain(session, 120) {
                let hay = content.to_ascii_lowercase();
                if needles.iter().all(|needle| hay.contains(needle)) {
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

    /// Cleanup guard: remove a path on drop (even on panic or early return).
    /// Best-effort — missing files are fine.
    struct TempPathGuard(std::path::PathBuf);
    impl Drop for TempPathGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Deliver a prompt to a shell session and verify the command actually ran.
    #[test]
    fn test_deliver_prompt_to_shell_session() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-client-test-deliver-prompt";
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

        let client = TmuxClient::new("omar-client-test-");
        // Tighter timeouts so the test finishes fast on a shell prompt.
        let opts = DeliveryOptions {
            startup_timeout: Duration::from_secs(3),
            stable_quiet: Duration::from_millis(200),
            verify_timeout: Duration::from_secs(2),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
            retry_delay: Duration::from_millis(100),
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

        let session = "omar-client-test-wait-stable";
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

        let client = TmuxClient::new("omar-client-test-");
        let start = Instant::now();
        client
            .wait_for_stable(
                session,
                Duration::from_millis(200),
                Duration::from_secs(3),
                Duration::from_millis(50),
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
    fn test_deliver_prompt_multiline() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-client-test-deliver-multiline";
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

        let client = TmuxClient::new("omar-client-test-");
        let opts = DeliveryOptions {
            startup_timeout: Duration::from_secs(3),
            stable_quiet: Duration::from_millis(200),
            verify_timeout: Duration::from_secs(2),
            max_retries: 3,
            poll_interval: Duration::from_millis(50),
            retry_delay: Duration::from_millis(100),
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
    fn test_send_keys_literal_accepts_leading_dash() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-client-test-send-literal-dash";
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

        let client = TmuxClient::new("omar-client-test-");
        if let Err(e) = client.send_keys_literal(session, "-OMAR_DASH_LITERAL_TEST") {
            eprintln!("Skipping test: send_keys_literal failed: {}", e);
            return;
        }
        let _ = client.send_keys(session, "Enter");
        thread::sleep(Duration::from_millis(300));

        let content = client.capture_pane(session, 50).unwrap_or_default();
        assert!(
            content.contains("-OMAR_DASH_LITERAL_TEST"),
            "Expected literal payload starting with '-' in pane: {:?}",
            content
        );
    }

    /// Regression: `wait_for_markers` must require ALL markers to be
    /// present, not any-of. Claude Code v2.1.116 paints "Claude Code" in
    /// its banner several hundred ms before the input widget is actually
    /// wired up to accept Enter; any-of matching returned true immediately
    /// when only the banner was visible and let `deliver_prompt` fire
    /// Enter into a pane that would silently swallow it. With all-of
    /// semantics, we don't succeed until every marker (banner + input-
    /// widget prompt glyph "❯" for claude) has rendered.
    #[test]
    fn test_wait_for_markers_requires_all_markers() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-wait-markers-all";
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

        // Print only the first marker. With the old any-of semantics
        // wait_for_markers would return true; with all-of it must not.
        let _ = client.send_keys_literal(session, "echo FIRST_MARKER_ONLY");
        let _ = client.send_keys(session, "Enter");
        thread::sleep(Duration::from_millis(300));

        let found = client.wait_for_markers(
            session,
            &["FIRST_MARKER_ONLY", "SECOND_MARKER_MISSING"],
            Duration::from_millis(600),
            Duration::from_millis(50),
        );
        assert!(
            !found,
            "wait_for_markers must require ALL markers; returning true \
             when only one is present regresses the Claude Code v2.1.116 \
             Enter-swallow fix"
        );

        // Now print the second marker too; both present -> must return true.
        let _ = client.send_keys_literal(session, "echo SECOND_MARKER_MISSING");
        let _ = client.send_keys(session, "Enter");
        let found = client.wait_for_markers(
            session,
            &["FIRST_MARKER_ONLY", "SECOND_MARKER_MISSING"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must return true once ALL markers are present"
        );
    }

    /// Regression: the claude readiness marker set includes "❯" (U+276F),
    /// the non-ASCII input-widget prompt glyph. `wait_for_markers` lowercases
    /// the hay with `to_ascii_lowercase`, which leaves "❯" byte-identical,
    /// so substring matching must still find it. This test locks in that
    /// UTF-8 markers work, so nobody later "fixes" readiness matching with a
    /// full-Unicode lowercase transform that would shift the bytes and break
    /// the match. Also guards against a prior bug where `capture_pane` (with
    /// `-e`, ANSI-inclusive) was used for marker matching, which inserts
    /// escape sequences mid-string and breaks multi-byte char boundaries.
    #[test]
    fn test_wait_for_markers_matches_claude_prompt_glyph() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-claude-glyph-marker";
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

        // Put "Claude Code" and "❯" directly into the pane as literal
        // bytes via `send-keys -l`. Avoids running a shell command so
        // the test is independent of the sandbox's default shell actually
        // executing inputs.
        let _ = client.send_keys_literal(session, "Claude Code ❯ ");

        let found = client.wait_for_markers(
            session,
            &["Claude Code", "❯"],
            Duration::from_secs(3),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must match the non-ASCII \"❯\" glyph used as \
             Claude Code's input prompt — regresses the v2.1.116 Enter- \
             swallow fix if missing"
        );
    }

    #[test]
    fn large_payload_threshold_is_reasonable() {
        // Threshold must be large enough that typical short tasks go through send-keys
        // but small enough to catch the ~1-3 KB tasks that silently fail
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD >= 512) };
        const { assert!(TmuxClient::LARGE_PAYLOAD_THRESHOLD <= 8192) };
    }

    /// Regression: on tmux 3.2a (Ubuntu 22.04), `paste-buffer` without `-r`
    /// replaces every LF in the buffer with CR by default. TUI backends like
    /// Claude Code run the terminal in raw mode (ICRNL off), so those CRs
    /// arrive unchanged and each reads as an Enter keypress in the input
    /// widget — turning a multi-line prompt into a cascade of blank
    /// submissions. `paste_text` must use `-r` so LFs survive verbatim.
    ///
    /// This test reproduces the raw-mode environment that exposes the bug:
    /// without `stty -icrnl` the TTY driver translates CR→LF on input and
    /// the test would pass trivially regardless of the flag, masking
    /// regressions. A naive `cat > file` + `C-d` reader also won't work
    /// because `-icanon` disables EOF interpretation; we use
    /// `dd bs=1 count=N` instead so the reader exits deterministically
    /// after the expected number of bytes.
    #[test]
    fn test_paste_text_preserves_lf_in_raw_mode() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-test-paste-preserves-lf";
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .output();
        let _guard = SessionGuard(session.to_string());

        let tmp_path =
            std::env::temp_dir().join(format!("omar-paste-lf-{}.txt", uuid::Uuid::new_v4()));
        let tmp_str = match tmp_path.to_str() {
            Some(s) => s,
            None => {
                eprintln!(
                    "Skipping test: temp path is not valid UTF-8: {:?}",
                    tmp_path
                );
                return;
            }
        };
        // Single-quote-escape the path for the shell command below so a
        // TMPDIR containing spaces or metacharacters doesn't break the test
        // (which would mask the regression by spuriously skipping).
        let quoted_tmp_path = format!("'{}'", tmp_str.replace('\'', r"'\''"));
        // Clean up the tmp file on every exit path (skip, assert fail, panic).
        let _tmp_guard = TempPathGuard(tmp_path.clone());

        // Start with an rc-less, non-login shell so users' rc files can't
        // break session startup on CI runners with unusual setups.
        let shell_cmd = "/bin/bash --norc --noprofile -i";
        let ok = Command::new("tmux")
            .args(["new-session", "-d", "-s", session, shell_cmd])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("Skipping test: failed to create tmux session");
            return;
        }

        // Give the shell a moment to draw, then put the pane into the raw-mode
        // conditions that expose the bug (ICRNL off, non-canonical input) and
        // start a deterministic N-byte reader.
        thread::sleep(Duration::from_millis(200));
        let payload = "line1\nline2\nline3";
        let reader_cmd = format!(
            "stty -icrnl -icanon; dd bs=1 count={} of={} 2>/dev/null",
            payload.len(),
            quoted_tmp_path,
        );

        let client = TmuxClient::new("omar-test-");
        if client.send_keys_literal(session, &reader_cmd).is_err() {
            eprintln!("Skipping test: send_keys_literal failed (sandbox?)");
            return;
        }
        if client.send_keys(session, "Enter").is_err() {
            eprintln!("Skipping test: send_keys Enter failed (sandbox?)");
            return;
        }
        thread::sleep(Duration::from_millis(200));

        if client.paste_text(session, payload).is_err() {
            eprintln!("Skipping test: paste_text failed (sandbox?)");
            return;
        }

        // Wait for dd to finish collecting the payload and flush the file.
        // Poll briefly instead of sleeping blindly so slow runners still pass.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(meta) = std::fs::metadata(&tmp_path) {
                if meta.len() as usize >= payload.len() {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        let got = match std::fs::read(&tmp_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Skipping test: tmp file not produced: {}", e);
                return;
            }
        };

        // The core assertion: LFs must survive. With the bug, every \n in
        // the payload arrives as \r, so the file would be "line1\rline2\rline3".
        assert_eq!(
            got,
            payload.as_bytes(),
            "paste_text must preserve LF (got {:?}, expected {:?}) — \
             tmux is likely mangling \\n → \\r because `-r` is missing from \
             the paste-buffer call in paste_text",
            String::from_utf8_lossy(&got),
            payload,
        );
    }

    /// Regression: wait_for_markers must match text that renders with ANSI
    /// escapes between words (e.g. Claude Code's `Claude<ESC>[0m Code` banner).
    /// A prior change made wait_for_markers use `capture-pane -e`, which
    /// included escapes and broke this matching, silently delaying initial
    /// task delivery by the full 45s timeout on every spawn.
    #[test]
    fn test_wait_for_markers_ignores_ansi_escapes() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-client-test-markers-ansi";
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

        let client = TmuxClient::new("omar-client-test-");
        // Emit "Claude<ESC>[0m <ESC>[1mCode" — the exact split CC renders
        // in its banner. The raw terminal shows "Claude Code"; an ANSI
        // capture does not.
        let _ = client.send_keys_literal(session, "printf 'Claude\\033[0m \\033[1mCode\\n'");
        let _ = client.send_keys(session, "Enter");
        thread::sleep(Duration::from_millis(200));

        let found = client.wait_for_markers(
            session,
            &["Claude Code"],
            Duration::from_secs(2),
            Duration::from_millis(50),
        );
        assert!(
            found,
            "wait_for_markers must match 'Claude Code' even when rendered with ANSI escapes between the words"
        );
    }

    /// Regression: get_pane_activity must return a parseable timestamp on
    /// every supported tmux. On tmux 3.6a (macOS Homebrew), `#{pane_activity}`
    /// is empty unless `monitor-activity` is enabled, so parsing fails and
    /// callers that `unwrap_or_default()` get 0 — which makes
    /// `health_from_activity` always report "idle". This caused EAs to
    /// replace working agents. `#{window_activity}` is populated universally.
    #[test]
    fn test_get_pane_activity_returns_recent_timestamp() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = "omar-client-test-pane-activity";
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

        let client = TmuxClient::new("omar-client-test-");
        let activity = client
            .get_pane_activity(session)
            .expect("get_pane_activity must succeed on freshly created session");

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Activity should be within the last 10 minutes — if it's 0 or ancient,
        // the underlying format string is empty on this tmux (regression).
        assert!(
            now - activity < 600,
            "get_pane_activity returned a stale timestamp ({}s old) — format string is probably empty on this tmux version",
            now - activity
        );
    }
}
