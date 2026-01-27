#![allow(dead_code)]

use anyhow::{Context, Result};
use std::process::Command;

use super::Session;

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
            .filter(|line| line.starts_with(&self.prefix))
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
        let client = TmuxClient::new("oma-agent-");
        assert_eq!(client.prefix(), "oma-agent-");
    }

    #[test]
    fn test_client_with_different_prefix() {
        let client = TmuxClient::new("test-");
        assert_eq!(client.prefix(), "test-");
    }
}
