#![allow(dead_code)]

use regex::Regex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Session, TmuxClient};

/// Health state of an agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Agent is actively working (spinning, thinking, outputting)
    Working,
    /// Agent is waiting for user input
    WaitingForInput,
    /// Agent has been idle for a while (warning)
    Idle,
    /// Agent appears stuck or has errors
    Stuck,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthState::Working => "working",
            HealthState::WaitingForInput => "waiting",
            HealthState::Idle => "idle",
            HealthState::Stuck => "stuck",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HealthState::Working => "●",
            HealthState::WaitingForInput => "◆",
            HealthState::Idle => "○",
            HealthState::Stuck => "✖",
        }
    }

    pub fn icon_colored(&self) -> &'static str {
        match self {
            HealthState::Working => "🟢",
            HealthState::WaitingForInput => "🔵",
            HealthState::Idle => "🟡",
            HealthState::Stuck => "🔴",
        }
    }
}

/// Checks health of agent sessions
pub struct HealthChecker {
    client: TmuxClient,
    idle_warning: i64,
    idle_critical: i64,
    error_pattern: Option<Regex>,
    working_pattern: Regex,
    waiting_pattern: Regex,
}

impl HealthChecker {
    pub fn new(
        client: TmuxClient,
        idle_warning: i64,
        idle_critical: i64,
        error_patterns: &[String],
    ) -> Self {
        let error_pattern = if error_patterns.is_empty() {
            None
        } else {
            let pattern = error_patterns
                .iter()
                .map(|p| regex::escape(p))
                .collect::<Vec<_>>()
                .join("|");
            Regex::new(&format!("(?i){}", pattern)).ok()
        };

        // Patterns indicating Claude is actively working
        // Spinner characters, "Thinking", progress indicators
        let working_pattern = Regex::new(concat!(
            r"(?i)",
            r"(thinking|working|reading|writing|running|analyzing|searching)",
            r"|[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]", // Braille spinner
            r"|[◐◓◑◒]",       // Circle spinner
            r"|[▖▘▝▗]",       // Block spinner
            r"|[⣾⣽⣻⢿⡿⣟⣯⣷]",   // Dots spinner
            r"|\.{3,}",       // Ellipsis (loading...)
        ))
        .expect("Invalid working pattern");

        // Patterns indicating Claude is waiting for user input
        let waiting_pattern = Regex::new(concat!(
            r"(?m)",
            r"(^>\s*$)",                           // Just a prompt
            r"|(waiting for|enter|input|confirm)", // Waiting keywords
            r"|(\?\s*$)",                          // Ends with question mark
            r"|(yes/no|y/n|\[Y/n\]|\[y/N\])",      // Yes/no prompts
            r"|(Press .* to continue)",            // Press key prompts
            r"|(^claude\s*>\s*$)",                 // Claude prompt
        ))
        .expect("Invalid waiting pattern");

        Self {
            client,
            idle_warning,
            idle_critical,
            error_pattern,
            working_pattern,
            waiting_pattern,
        }
    }

    /// Get current Unix timestamp
    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Calculate idle time in seconds
    pub fn idle_seconds(&self, session: &Session) -> i64 {
        Self::now() - session.activity
    }

    /// Check the health of a session
    pub fn check(&self, session: &Session) -> HealthState {
        let idle = self.idle_seconds(session);

        // Get recent output for pattern matching
        let output = self
            .client
            .capture_pane(&session.name, 30)
            .unwrap_or_default();

        // Check for error patterns first (highest priority)
        if let Some(ref pattern) = self.error_pattern {
            if pattern.is_match(&output) {
                return HealthState::Stuck;
            }
        }

        // Check if waiting for input (look at last few lines)
        let last_lines: String = output.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        if self.waiting_pattern.is_match(&last_lines) {
            return HealthState::WaitingForInput;
        }

        // Check if actively working (recent output has working indicators)
        if idle < 10 && self.working_pattern.is_match(&output) {
            return HealthState::Working;
        }

        // Recent activity means working
        if idle < self.idle_warning {
            return HealthState::Working;
        }

        // Critical idle time - definitely stuck
        if idle > self.idle_critical {
            return HealthState::Stuck;
        }

        // Warning idle time
        HealthState::Idle
    }

    /// Check health and return additional info
    pub fn check_detailed(&self, session: &Session) -> HealthInfo {
        let idle_seconds = self.idle_seconds(session);

        let output = self
            .client
            .capture_pane(&session.name, 30)
            .unwrap_or_default();

        let state = self.check(session);

        let last_output = output
            .lines()
            .next_back()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect();

        let has_errors = self
            .error_pattern
            .as_ref()
            .map(|p| p.is_match(&output))
            .unwrap_or(false);

        HealthInfo {
            state,
            idle_seconds,
            last_output,
            has_errors,
        }
    }
}

/// Detailed health information for a session
#[derive(Debug, Clone)]
pub struct HealthInfo {
    pub state: HealthState,
    pub idle_seconds: i64,
    pub last_output: String,
    pub has_errors: bool,
}

impl HealthInfo {
    /// Format idle time as human-readable string
    pub fn idle_display(&self) -> String {
        let secs = self.idle_seconds;
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h", secs / 3600)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_state_display() {
        assert_eq!(HealthState::Working.as_str(), "working");
        assert_eq!(HealthState::WaitingForInput.as_str(), "waiting");
        assert_eq!(HealthState::Idle.as_str(), "idle");
        assert_eq!(HealthState::Stuck.as_str(), "stuck");
    }

    #[test]
    fn test_health_state_icons() {
        assert_eq!(HealthState::Working.icon(), "●");
        assert_eq!(HealthState::WaitingForInput.icon(), "◆");
        assert_eq!(HealthState::Idle.icon(), "○");
        assert_eq!(HealthState::Stuck.icon(), "✖");
    }

    #[test]
    fn test_idle_display() {
        let info = HealthInfo {
            state: HealthState::Working,
            idle_seconds: 30,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "30s");

        let info = HealthInfo {
            state: HealthState::Working,
            idle_seconds: 120,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "2m");

        let info = HealthInfo {
            state: HealthState::Working,
            idle_seconds: 7200,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "2h");
    }

    #[test]
    fn test_working_pattern() {
        let client = TmuxClient::new("test-");
        let checker = HealthChecker::new(client, 60, 300, &[]);

        // Test spinner characters
        assert!(checker.working_pattern.is_match("⠋ Loading"));
        assert!(checker.working_pattern.is_match("Thinking..."));
        assert!(checker.working_pattern.is_match("Reading file"));
        assert!(checker.working_pattern.is_match("◐ Working"));
    }

    #[test]
    fn test_waiting_pattern() {
        let client = TmuxClient::new("test-");
        let checker = HealthChecker::new(client, 60, 300, &[]);

        // Test prompt patterns
        assert!(checker.waiting_pattern.is_match(">"));
        assert!(checker.waiting_pattern.is_match("Continue? [Y/n]"));
        assert!(checker.waiting_pattern.is_match("Are you sure?"));
        assert!(checker.waiting_pattern.is_match("Press Enter to continue"));
    }
}
