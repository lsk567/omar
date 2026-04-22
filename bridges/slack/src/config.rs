use anyhow::{bail, Result};
use std::path::PathBuf;

/// Configuration for the Slack bridge, loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Slack bot token (xoxb-...) — used for Web API calls (posting messages, etc.)
    pub bot_token: String,
    /// Slack app-level token (xapp-...) — used for Socket Mode WebSocket connection
    pub app_token: String,
    /// Path to the `omar` binary. The bridge spawns `omar mcp-server` as a
    /// subprocess and talks to it over stdio.
    pub omar_binary: PathBuf,
    /// `~/.omar` — holds the `slack_outbox/` directory the bridge polls for
    /// outbound replies queued by the EA.
    pub omar_dir: PathBuf,
    /// Maximum Slack message length before chunking (Slack limit is 4000)
    pub max_message_length: usize,
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN").unwrap_or_default();
        let app_token = std::env::var("SLACK_APP_TOKEN").unwrap_or_default();

        if bot_token.is_empty() {
            bail!("SLACK_BOT_TOKEN environment variable is required (xoxb-...)");
        }
        if app_token.is_empty() {
            bail!("SLACK_APP_TOKEN environment variable is required (xapp-...)");
        }
        if !bot_token.starts_with("xoxb-") {
            bail!("SLACK_BOT_TOKEN must start with 'xoxb-'");
        }
        if !app_token.starts_with("xapp-") {
            bail!("SLACK_APP_TOKEN must start with 'xapp-'");
        }

        let omar_binary = match std::env::var("OMAR_BINARY") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => resolve_default_omar_binary(),
        };

        let omar_dir = match std::env::var("OMAR_DIR") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => dirs_home()?.join(".omar"),
        };

        let max_message_length: usize = std::env::var("MAX_MESSAGE_LENGTH")
            .unwrap_or_else(|_| "3900".to_string())
            .parse()
            .unwrap_or(3900);

        Ok(Self {
            bot_token,
            app_token,
            omar_binary,
            omar_dir,
            max_message_length,
        })
    }
}

/// Fall back to `$HOME` when `dirs::home_dir()` isn't available (we don't
/// pull in the `dirs` crate from the bridge just for this).
fn dirs_home() -> Result<PathBuf> {
    match std::env::var("HOME") {
        Ok(v) if !v.is_empty() => Ok(PathBuf::from(v)),
        _ => bail!("HOME is not set; pass OMAR_DIR explicitly"),
    }
}

/// Look for `omar` next to the currently-running bridge binary first so
/// installations that drop both binaries into the same directory work
/// without any env vars. Fall back to the bare name so a PATH lookup
/// resolves it.
fn resolve_default_omar_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("omar");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("omar")
}
