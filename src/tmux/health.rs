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
    /// Patterns that indicate auth failure (case-insensitive match)
    auth_failure_patterns: Vec<String>,
}

impl HealthChecker {
    pub fn new(client: TmuxClient, _idle_threshold: i64) -> Self {
        Self {
            client,
            last_frames: HashMap::new(),
            auth_failure_patterns: Vec::new(),
        }
    }

    pub fn with_auth_failure_patterns(mut self, patterns: Vec<String>) -> Self {
        self.auth_failure_patterns = patterns;
        self
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

        let frame = self.last_frames.get(session_name);

        let last_output = frame
            .map(|f| {
                f.lines()
                    .next_back()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(80)
                    .collect()
            })
            .unwrap_or_default();

        let auth_failure = frame
            .map(|f| {
                let lower = f.to_lowercase();
                self.auth_failure_patterns
                    .iter()
                    .any(|pat| lower.contains(&pat.to_lowercase()))
            })
            .unwrap_or(false);

        HealthInfo {
            state,
            last_output,
            auth_failure,
        }
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
    /// Whether auth failure patterns were detected in the pane output
    pub auth_failure: bool,
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
    fn test_auth_failure_detection_in_frame() {
        let patterns = vec!["session expired".to_string(), "login required".to_string()];

        // Simulate: frame content contains auth failure
        let frame_with_auth_failure =
            "Some output\nError: Your session expired. Please sign in again.\nMore output";
        let lower = frame_with_auth_failure.to_lowercase();
        let detected = patterns
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()));
        assert!(detected);

        // Simulate: normal frame content
        let normal_frame = "Building project...\nCompilation successful\nRunning tests";
        let lower = normal_frame.to_lowercase();
        let detected = patterns
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()));
        assert!(!detected);
    }

    #[test]
    fn test_auth_failure_case_insensitive() {
        let patterns = vec!["session expired".to_string()];
        let frame = "SESSION EXPIRED: please log in";
        let lower = frame.to_lowercase();
        let detected = patterns
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()));
        assert!(detected);
    }
}
