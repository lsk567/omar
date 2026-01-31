#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::sandbox::SandboxConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub health: HealthConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub api: ApiConfig,

    #[serde(default)]
    pub sandbox: SandboxConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Whether to enable the HTTP API
    #[serde(default = "default_api_enabled")]
    pub enabled: bool,

    /// Host to bind to
    #[serde(default = "default_api_host")]
    pub host: String,

    /// Port to listen on
    #[serde(default = "default_api_port")]
    pub port: u16,
}

fn default_refresh_interval() -> u64 {
    1
}

fn default_session_prefix() -> String {
    "omar-agent-".to_string()
}

fn default_idle_warning() -> i64 {
    15
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

/// Detect which agent command is available on the system.
/// Checks PATH for `claude` first, then `opencode`, falling back to `claude`.
fn detect_agent_command() -> String {
    use std::process::Command;

    if Command::new("claude")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return "claude --dangerously-skip-permissions".to_string();
    }

    if Command::new("opencode")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return "opencode".to_string();
    }

    // Fallback to claude even if not found (user may install it later)
    "claude --dangerously-skip-permissions".to_string()
}

fn default_command() -> String {
    detect_agent_command()
}

fn default_workdir() -> String {
    ".".to_string()
}

fn default_api_enabled() -> bool {
    true
}

fn default_api_host() -> String {
    "127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    9876
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

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: default_api_enabled(),
            host: default_api_host(),
            port: default_api_port(),
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
            .join("omar")
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
        assert_eq!(config.dashboard.refresh_interval, 1);
        assert_eq!(config.dashboard.session_prefix, "omar-agent-");
        assert_eq!(config.health.idle_warning, 15);
        assert_eq!(config.health.idle_critical, 300);
    }

    #[test]
    fn test_default_command_detects_agent() {
        let cmd = detect_agent_command();
        // Should return a non-empty command regardless of what's installed
        assert!(!cmd.is_empty());
        // Should be one of the known agent commands
        assert!(
            cmd.contains("claude") || cmd.contains("opencode"),
            "Unexpected default command: {}",
            cmd
        );
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

    #[test]
    fn test_parse_config_opencode_backend() {
        let toml = r#"
[agent]
default_command = "opencode"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.default_command, "opencode");
    }

    #[test]
    fn test_parse_config_custom_backend() {
        let toml = r#"
[agent]
default_command = "aider --yes"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.default_command, "aider --yes");
    }

    #[test]
    fn test_sandbox_defaults_when_absent() {
        let toml = r#"
[dashboard]
refresh_interval = 1
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.sandbox.enabled);
        assert_eq!(config.sandbox.image, "ubuntu:22.04");
        assert_eq!(config.sandbox.network, "bridge");
    }

    #[test]
    fn test_sandbox_config_parsing() {
        let toml = r#"
[sandbox]
enabled = true
image = "node:20"
network = "none"

[sandbox.limits]
memory = "8g"
cpus = 4.0
pids_limit = 512

[sandbox.filesystem]
workspace_access = "ro"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.sandbox.enabled);
        assert_eq!(config.sandbox.image, "node:20");
        assert_eq!(config.sandbox.network, "none");
        assert_eq!(config.sandbox.limits.memory, "8g");
        assert_eq!(config.sandbox.limits.cpus, 4.0);
        assert_eq!(config.sandbox.limits.pids_limit, 512);
        assert_eq!(config.sandbox.filesystem.workspace_access, "ro");
    }
}
