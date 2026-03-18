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

    #[serde(default)]
    pub api: ApiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Refresh interval in seconds
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: u64,

    /// Session name prefix for agent sessions
    #[serde(default = "default_session_prefix")]
    pub session_prefix: String,

    /// Show event queue in sidebar
    #[serde(default = "default_true")]
    pub show_event_queue: bool,

    /// Sidebar on right side
    #[serde(default = "default_true")]
    pub sidebar_right: bool,
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

fn default_true() -> bool {
    true
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

/// Map shorthand agent names to full commands.
///
/// - `"claude"` → `"claude --dangerously-skip-permissions"`
/// - `"codex"` → `"codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox"`
/// - `"cursor"` → `"cursor agent --yolo"`
/// - `"opencode"` → `"opencode"`
/// - anything else → error
pub fn resolve_backend(name: &str) -> Result<String, String> {
    match name {
        "claude" => Ok("claude --dangerously-skip-permissions".to_string()),
        "codex" => {
            Ok("codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox".to_string())
        }
        "cursor" => Ok("cursor agent --yolo".to_string()),
        "opencode" => Ok("opencode".to_string()),
        other => Err(format!(
            "Unknown backend '{}'. Supported: claude, codex, cursor, opencode",
            other
        )),
    }
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
            show_event_queue: true,
            sidebar_right: true,
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
    /// Default config path: ~/.omar/config.toml
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".omar")
            .join("config.toml")
    }

    /// Load config from file. Creates default config at ~/.omar/config.toml on first run.
    pub fn load(path: Option<&str>) -> Result<Self> {
        let expanded_path = match path {
            Some(p) => expand_tilde(p),
            None => Self::default_path(),
        };

        if !expanded_path.exists() {
            let config = Self::default();
            config.save();
            return Ok(config);
        }

        let contents =
            std::fs::read_to_string(&expanded_path).context("Failed to read config file")?;

        toml::from_str(&contents).context("Failed to parse config file")
    }

    /// Save config to its default path (~/.omar/config.toml)
    pub fn save(&self) {
        let path = Self::default_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(contents) = toml::to_string_pretty(self) {
            std::fs::write(&path, contents).ok();
        }
    }

    /// Number of toggleable settings
    pub fn settings_count(&self) -> usize {
        2
    }

    /// Get label and current value for a setting by index
    pub fn settings_item(&self, index: usize) -> Option<(&str, bool)> {
        match index {
            0 => Some((
                "Show event queue in sidebar",
                self.dashboard.show_event_queue,
            )),
            1 => Some(("Sidebar on right side", self.dashboard.sidebar_right)),
            _ => None,
        }
    }

    /// Toggle a setting by index and save
    pub fn toggle_setting(&mut self, index: usize) {
        match index {
            0 => self.dashboard.show_event_queue = !self.dashboard.show_event_queue,
            1 => self.dashboard.sidebar_right = !self.dashboard.sidebar_right,
            _ => {}
        }
        self.save();
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
        assert!(config.dashboard.show_event_queue);
        assert!(config.dashboard.sidebar_right);
        assert_eq!(config.health.idle_warning, 15);
        assert_eq!(config.health.idle_critical, 300);
    }

    #[test]
    fn test_settings_toggle() {
        let mut config = Config::default();
        assert!(config.dashboard.show_event_queue);
        assert_eq!(config.settings_count(), 2);
        assert_eq!(
            config.settings_item(0),
            Some(("Show event queue in sidebar", true))
        );
        // Toggle without saving to disk (just test the in-memory toggle)
        config.dashboard.show_event_queue = !config.dashboard.show_event_queue;
        assert!(!config.dashboard.show_event_queue);
    }

    #[test]
    fn test_parse_config_with_settings() {
        let toml = r#"
[dashboard]
show_event_queue = false
sidebar_right = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.dashboard.show_event_queue);
        assert!(!config.dashboard.sidebar_right);
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
    fn test_parse_config_without_default_command_uses_detected_default() {
        let toml = r#"
[agent]
default_workdir = "/workspace"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.default_workdir, "/workspace");
        assert_eq!(config.agent.default_command, default_command());
        assert_ne!(config.agent.default_command, "bash");
    }

    #[test]
    fn test_resolve_backend_known_names() {
        assert_eq!(
            resolve_backend("claude").unwrap(),
            "claude --dangerously-skip-permissions"
        );
        assert_eq!(
            resolve_backend("codex").unwrap(),
            "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(resolve_backend("cursor").unwrap(), "cursor agent --yolo");
        assert_eq!(resolve_backend("opencode").unwrap(), "opencode");
    }

    #[test]
    fn test_resolve_backend_unknown_errors() {
        assert!(resolve_backend("aider").is_err());
        assert!(resolve_backend("agent").is_err());
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
    fn test_parse_config_codex_backend() {
        let toml = r#"
[agent]
default_command = "codex"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.default_command, "codex");
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
}
