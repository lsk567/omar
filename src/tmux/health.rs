#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use super::{Session, TmuxClient};

/// Health state of an agent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// Agent is actively producing output
    Running,
    /// Agent has not produced new output recently
    Idle,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthState::Running => "running",
            HealthState::Idle => "idle",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HealthState::Running => "●",
            HealthState::Idle => "○",
        }
    }
}

/// Checks health of agent sessions based on tmux activity timestamp.
/// If no new terminal lines have been produced in `idle_threshold` seconds,
/// the session is considered Idle; otherwise Running.
pub struct HealthChecker {
    client: TmuxClient,
    idle_threshold: i64,
}

impl HealthChecker {
    pub fn new(client: TmuxClient, idle_threshold: i64) -> Self {
        Self {
            client,
            idle_threshold,
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

        if idle < self.idle_threshold {
            HealthState::Running
        } else {
            HealthState::Idle
        }
    }

    /// Check health and return additional info
    pub fn check_detailed(&self, session: &Session) -> HealthInfo {
        let idle_seconds = self.idle_seconds(session);
        let state = self.check(session);

        let last_output = self
            .client
            .capture_pane(&session.name, 5)
            .unwrap_or_default()
            .lines()
            .next_back()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect();

        HealthInfo {
            state,
            idle_seconds,
            last_output,
        }
    }
}

/// Detailed health information for a session
#[derive(Debug, Clone)]
pub struct HealthInfo {
    pub state: HealthState,
    pub idle_seconds: i64,
    pub last_output: String,
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
        assert_eq!(HealthState::Running.as_str(), "running");
        assert_eq!(HealthState::Idle.as_str(), "idle");
    }

    #[test]
    fn test_health_state_icons() {
        assert_eq!(HealthState::Running.icon(), "●");
        assert_eq!(HealthState::Idle.icon(), "○");
    }

    #[test]
    fn test_idle_display() {
        let info = HealthInfo {
            state: HealthState::Running,
            idle_seconds: 30,
            last_output: String::new(),
        };
        assert_eq!(info.idle_display(), "30s");

        let info = HealthInfo {
            state: HealthState::Running,
            idle_seconds: 120,
            last_output: String::new(),
        };
        assert_eq!(info.idle_display(), "2m");

        let info = HealthInfo {
            state: HealthState::Running,
            idle_seconds: 7200,
            last_output: String::new(),
        };
        assert_eq!(info.idle_display(), "2h");
    }
}
