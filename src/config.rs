#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub health: HealthConfig,

    #[serde(default)]
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Refresh interval in seconds
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: u64,

    /// Session name prefix for agent sessions
    #[serde(default = "default_session_prefix")]
    pub session_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// Seconds of inactivity before warning (yellow)
    #[serde(default = "default_idle_warning")]
    pub idle_warning: i64,

    /// Seconds of inactivity before critical (red/stuck)
    #[serde(default = "default_idle_critical")]
    pub idle_critical: i64,

    /// Patterns in output that indicate an error
    #[serde(default = "default_error_patterns")]
    pub error_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Default command to run for new agents
    #[serde(default = "default_command")]
    pub default_command: String,

    /// Default working directory
    #[serde(default = "default_workdir")]
    pub default_workdir: String,
}

fn default_refresh_interval() -> u64 {
    2
}

fn default_session_prefix() -> String {
    String::new() // No prefix - session name matches agent name
}

fn default_idle_warning() -> i64 {
    60
}

fn default_idle_critical() -> i64 {
    300
}

fn default_error_patterns() -> Vec<String> {
    vec![
        "error".to_string(),
        "failed".to_string(),
        "rate limit".to_string(),
        "exception".to_string(),
    ]
}

fn default_command() -> String {
    "claude --dangerously-skip-permissions".to_string()
}

fn default_workdir() -> String {
    ".".to_string()
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            refresh_interval: default_refresh_interval(),
            session_prefix: default_session_prefix(),
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            idle_warning: default_idle_warning(),
            idle_critical: default_idle_critical(),
            error_patterns: default_error_patterns(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_command: default_command(),
            default_workdir: default_workdir(),
        }
    }
}

impl Config {
    /// Load config from file, or return defaults if file doesn't exist
    pub fn load(path: &str) -> Result<Self> {
        let expanded_path = expand_tilde(path);

        if !expanded_path.exists() {
            return Ok(Self::default());
        }

        let contents =
            std::fs::read_to_string(&expanded_path).context("Failed to read config file")?;

        toml::from_str(&contents).context("Failed to parse config file")
    }

    /// Get the default config path
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("oma")
            .join("config.toml")
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.dashboard.refresh_interval, 2);
        assert_eq!(config.dashboard.session_prefix, ""); // No prefix by default
        assert_eq!(config.health.idle_warning, 60);
        assert_eq!(config.health.idle_critical, 300);
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
[dashboard]
refresh_interval = 5
session_prefix = "test-"

[health]
idle_warning = 30
idle_critical = 120
error_patterns = ["error", "panic"]

[agent]
default_command = "bash"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.dashboard.refresh_interval, 5);
        assert_eq!(config.dashboard.session_prefix, "test-");
        assert_eq!(config.health.idle_warning, 30);
        assert_eq!(config.health.error_patterns, vec!["error", "panic"]);
    }
}
