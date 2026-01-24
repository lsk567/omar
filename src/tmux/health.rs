#![allow(dead_code)]

use regex::Regex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Session, TmuxClient};

/// Health state of an agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Agent is active and working normally
    Ok,
    /// Agent has been idle for a while (warning)
    Idle,
    /// Agent appears stuck or has errors
    Stuck,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthState::Ok => "ok",
            HealthState::Idle => "idle",
            HealthState::Stuck => "stuck",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HealthState::Ok => "●",
            HealthState::Idle => "○",
            HealthState::Stuck => "✖",
        }
    }

    pub fn icon_colored(&self) -> &'static str {
        match self {
            HealthState::Ok => "🟢",
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

        Self {
            client,
            idle_warning,
            idle_critical,
            error_pattern,
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

        // Critical idle time - definitely stuck
        if idle > self.idle_critical {
            return HealthState::Stuck;
        }

        // Check for error patterns in recent output
        if let Some(ref pattern) = self.error_pattern {
            if let Ok(output) = self.client.capture_pane(&session.name, 20) {
                if pattern.is_match(&output) {
                    return HealthState::Stuck;
                }
            }
        }

        // Warning idle time
        if idle > self.idle_warning {
            return HealthState::Idle;
        }

        HealthState::Ok
    }

    /// Check health and return additional info
    pub fn check_detailed(&self, session: &Session) -> HealthInfo {
        let idle_seconds = self.idle_seconds(session);
        let state = self.check(session);

        let last_output = self
            .client
            .capture_pane(&session.name, 1)
            .unwrap_or_default()
            .trim()
            .chars()
            .take(80)
            .collect();

        let has_errors = self
            .error_pattern
            .as_ref()
            .map(|p| {
                self.client
                    .capture_pane(&session.name, 20)
                    .map(|o| p.is_match(&o))
                    .unwrap_or(false)
            })
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

    fn mock_session(name: &str, activity: i64) -> Session {
        Session::new(name.to_string(), activity, false, 12345)
    }

    #[test]
    fn test_health_state_display() {
        assert_eq!(HealthState::Ok.as_str(), "ok");
        assert_eq!(HealthState::Idle.as_str(), "idle");
        assert_eq!(HealthState::Stuck.as_str(), "stuck");
    }

    #[test]
    fn test_health_state_icons() {
        assert_eq!(HealthState::Ok.icon(), "●");
        assert_eq!(HealthState::Idle.icon(), "○");
        assert_eq!(HealthState::Stuck.icon(), "✖");
    }

    #[test]
    fn test_idle_display() {
        let info = HealthInfo {
            state: HealthState::Ok,
            idle_seconds: 30,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "30s");

        let info = HealthInfo {
            state: HealthState::Ok,
            idle_seconds: 120,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "2m");

        let info = HealthInfo {
            state: HealthState::Ok,
            idle_seconds: 7200,
            last_output: String::new(),
            has_errors: false,
        };
        assert_eq!(info.idle_display(), "2h");
    }

    // Integration tests that require tmux are in tests/integration.rs
}
