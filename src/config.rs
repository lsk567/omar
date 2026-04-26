#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::backend_probe;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub health: HealthConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub watchdog: WatchdogConfig,

    #[serde(default)]
    pub metrics: MetricsConfig,
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

    /// Show inspirational quotes in the status bar
    #[serde(default)]
    pub show_quotes: bool,
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
pub struct WatchdogConfig {
    /// Command to run the watchdog agent (empty = watchdog disabled).
    /// Should be an untrusted/free backend — no secrets will be passed.
    /// Example: "opencode --model openrouter/openrouter/free"
    #[serde(default)]
    pub command: String,

    /// Patterns in agent output that indicate an authentication failure
    #[serde(default = "default_auth_failure_patterns")]
    pub auth_failure_patterns: Vec<String>,

    /// Slack channel ID for watchdog alerts (empty = no Slack alerts)
    #[serde(default)]
    pub slack_channel: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Enable global spawn metrics sink at ~/.omar/metrics/spawn_metrics.jsonl
    #[serde(default)]
    pub spawn_metrics_enabled: bool,
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
/// Checks PATH for the supported first-class backends, falling back to `claude`.
fn detect_agent_command() -> String {
    detect_agent_command_from(&[
        ("claude", "claude --dangerously-skip-permissions"),
        (
            "codex",
            "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox",
        ),
        ("cursor", "cursor agent --yolo"),
        ("gemini", "gemini --yolo"),
        ("opencode", "opencode"),
    ])
    .unwrap_or_else(|| "claude --dangerously-skip-permissions".to_string())
}

fn detect_agent_command_from(candidates: &[(&str, &str)]) -> Option<String> {
    candidates
        .iter()
        .find(|(binary, _)| backend_probe::backend_version_probe_succeeds(binary))
        .map(|(_, command)| (*command).to_string())
}

fn default_command() -> String {
    detect_agent_command()
}

/// Map shorthand agent names to full commands.
///
/// - `"claude"` → `"claude --dangerously-skip-permissions"`
/// - `"codex"` → `"codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox"`
/// - `"cursor"` → `"cursor agent --yolo"`
/// - `"gemini"` → `"gemini --yolo"`
/// - `"opencode"` → `"opencode"` (opencode has no permission-skip flag)
/// - anything else → error
pub fn resolve_backend(name: &str) -> Result<String, String> {
    match name {
        "claude" => Ok("claude --dangerously-skip-permissions".to_string()),
        "codex" => {
            Ok("codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox".to_string())
        }
        "cursor" => Ok("cursor agent --yolo".to_string()),
        "gemini" => Ok("gemini --yolo".to_string()),
        "opencode" => Ok("opencode".to_string()),
        other => Err(format!(
            "Unknown backend '{}'. Supported: claude, codex, cursor, gemini, opencode",
            other
        )),
    }
}

fn default_workdir() -> String {
    ".".to_string()
}

fn default_auth_failure_patterns() -> Vec<String> {
    vec![
        // Claude Code
        "please run /login".to_string(),
        // Codex
        "401 unauthorized".to_string(),
        // TODO: opencode
        // TODO: cursor
    ]
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            refresh_interval: default_refresh_interval(),
            session_prefix: default_session_prefix(),
            show_event_queue: true,
            sidebar_right: true,
            show_quotes: false,
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

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            auth_failure_patterns: default_auth_failure_patterns(),
            slack_channel: String::new(),
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

    /// Resolve a config path from CLI input, expanding `~/`.
    pub fn resolve_path(path: Option<&str>) -> PathBuf {
        match path {
            Some(p) => expand_tilde(p),
            None => Self::default_path(),
        }
    }

    /// Load config from file. Creates default config at ~/.omar/config.toml on first run.
    pub fn load(path: Option<&str>) -> Result<Self> {
        let expanded_path = Self::resolve_path(path);

        if !expanded_path.exists() {
            let config = Self::default();
            config.save_to_path(&expanded_path);
            return Ok(config);
        }

        let contents =
            std::fs::read_to_string(&expanded_path).context("Failed to read config file")?;

        toml::from_str(&contents).context("Failed to parse config file")
    }

    /// Save config to its default path (~/.omar/config.toml)
    pub fn save(&self) {
        self.save_to_path(&Self::default_path());
    }

    /// Save config to a specific path.
    pub fn save_to_path(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(contents) = toml::to_string_pretty(self) {
            std::fs::write(path, contents).ok();
        }
    }

    /// Number of toggleable settings
    pub fn settings_count(&self) -> usize {
        3
    }

    /// Get label and current value for a setting by index
    pub fn settings_item(&self, index: usize) -> Option<(&str, bool)> {
        match index {
            0 => Some((
                "Show event queue in sidebar",
                self.dashboard.show_event_queue,
            )),
            1 => Some(("Sidebar on right side", self.dashboard.sidebar_right)),
            2 => Some(("Show inspirational quotes", self.dashboard.show_quotes)),
            _ => None,
        }
    }

    /// Toggle a setting by index and save
    pub fn toggle_setting(&mut self, index: usize) {
        match index {
            0 => self.dashboard.show_event_queue = !self.dashboard.show_event_queue,
            1 => self.dashboard.sidebar_right = !self.dashboard.sidebar_right,
            2 => self.dashboard.show_quotes = !self.dashboard.show_quotes,
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
        assert!(!config.metrics.spawn_metrics_enabled);
    }

    #[test]
    fn test_settings_toggle() {
        let mut config = Config::default();
        assert!(config.dashboard.show_event_queue);
        assert!(!config.dashboard.show_quotes);
        assert_eq!(config.settings_count(), 3);
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
            cmd.contains("claude")
                || cmd.contains("codex")
                || cmd.contains("cursor")
                || cmd.contains("gemini")
                || cmd.contains("opencode"),
            "Unexpected default command: {}",
            cmd
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_detect_agent_command_skips_hanging_probe() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().unwrap();
        let slow = temp.path().join("slow-agent");
        let fast = temp.path().join("fast-agent");
        fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::write(&fast, "#!/bin/sh\nexit 0\n").unwrap();
        for path in [&slow, &fast] {
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }

        let start = Instant::now();
        let cmd = detect_agent_command_from(&[
            (slow.to_str().unwrap(), "slow command"),
            (fast.to_str().unwrap(), "fast command"),
        ]);

        assert_eq!(cmd.as_deref(), Some("fast command"));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "hanging probe should be skipped after the bounded timeout"
        );
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
[dashboard]
refresh_interval = 5
session_prefix = "test-"
show_event_queue = false
sidebar_right = false

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
        assert!(!config.dashboard.show_event_queue);
        assert!(!config.dashboard.sidebar_right);
        assert_eq!(config.health.idle_warning, 30);
        assert_eq!(config.health.error_patterns, vec!["error", "panic"]);
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
        assert_eq!(resolve_backend("gemini").unwrap(), "gemini --yolo");
        assert_eq!(resolve_backend("opencode").unwrap(), "opencode");
    }

    #[test]
    fn test_resolve_backend_unknown_errors() {
        assert!(resolve_backend("aider --yes").is_err());
        assert!(resolve_backend("custom-agent").is_err());
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

    #[test]
    fn test_default_watchdog_config() {
        let config = Config::default();
        assert!(config.watchdog.command.is_empty());
        assert!(config.watchdog.slack_channel.is_empty());
        assert!(!config.watchdog.auth_failure_patterns.is_empty());
        assert!(config
            .watchdog
            .auth_failure_patterns
            .iter()
            .any(|p| p == "please run /login"));
    }

    #[test]
    fn test_parse_watchdog_config() {
        let toml = r#"
[watchdog]
command = "claude --dangerously-skip-permissions"
slack_channel = "C0123456789"
auth_failure_patterns = ["session expired", "login required"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.watchdog.command,
            "claude --dangerously-skip-permissions"
        );
        assert_eq!(config.watchdog.slack_channel, "C0123456789");
        assert_eq!(config.watchdog.auth_failure_patterns.len(), 2);
    }

    #[test]
    fn test_parse_config_without_watchdog_uses_defaults() {
        let toml = r#"
[agent]
default_command = "claude --dangerously-skip-permissions"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.watchdog.command.is_empty());
        assert!(!config.watchdog.auth_failure_patterns.is_empty());
    }

    #[test]
    fn test_parse_metrics_config() {
        let toml = r#"
[metrics]
spawn_metrics_enabled = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.metrics.spawn_metrics_enabled);
    }

    #[test]
    fn test_load_missing_custom_path_writes_custom_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.toml");

        let _config = Config::load(Some(path.to_str().unwrap())).unwrap();

        assert!(
            path.exists(),
            "missing explicit config path should be created"
        );
    }
}
