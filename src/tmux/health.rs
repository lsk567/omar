#![allow(dead_code)]

use std::collections::HashMap;

use super::TmuxClient;

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

/// Checks health of agent sessions by comparing pane content between frames.
/// If the pane content has changed since the last check, the session is Running;
/// otherwise it is Idle.
pub struct HealthChecker {
    client: TmuxClient,
    /// Last captured pane content per session name
    last_frames: HashMap<String, String>,
}

impl HealthChecker {
    pub fn new(client: TmuxClient, _idle_threshold: i64) -> Self {
        Self {
            client,
            last_frames: HashMap::new(),
        }
    }

    /// Check the health of a session by comparing against the previous frame.
    /// Returns Running if pane content changed, Idle if unchanged.
    pub fn check(&mut self, session_name: &str) -> HealthState {
        let current = self
            .client
            .capture_pane(session_name, 50)
            .unwrap_or_default();

        let changed = match self.last_frames.get(session_name) {
            Some(prev) => *prev != current,
            None => true, // First check — assume running
        };

        self.last_frames.insert(session_name.to_string(), current);

        if changed {
            HealthState::Running
        } else {
            HealthState::Idle
        }
    }

    /// Check health and return additional info
    pub fn check_detailed(&mut self, session_name: &str) -> HealthInfo {
        let state = self.check(session_name);

        let last_output = self
            .last_frames
            .get(session_name)
            .map(|frame| {
                frame
                    .lines()
                    .next_back()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(80)
                    .collect()
            })
            .unwrap_or_default();

        HealthInfo { state, last_output }
    }

    /// Remove stale entries for sessions that no longer exist
    pub fn retain_sessions(&mut self, active_sessions: &[String]) {
        self.last_frames
            .retain(|name, _| active_sessions.contains(name));
    }
}

/// Detailed health information for a session
#[derive(Debug, Clone)]
pub struct HealthInfo {
    pub state: HealthState,
    pub last_output: String,
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
}
