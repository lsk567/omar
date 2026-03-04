use anyhow::{bail, Result};

/// Configuration for the Slack bridge, loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Slack bot token (xoxb-...) — used for Web API calls (posting messages, etc.)
    pub bot_token: String,
    /// Slack app-level token (xapp-...) — used for Socket Mode WebSocket connection
    pub app_token: String,
    /// OMAR API base URL (default: http://127.0.0.1:9876)
    pub omar_url: String,
    /// How often to poll agent output, in milliseconds (default: 2000)
    pub poll_interval_ms: u64,
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

        let omar_url =
            std::env::var("OMAR_URL").unwrap_or_else(|_| "http://127.0.0.1:9876".to_string());

        let poll_interval_ms: u64 = std::env::var("POLL_INTERVAL_MS")
            .unwrap_or_else(|_| "2000".to_string())
            .parse()
            .unwrap_or(2000);

        let max_message_length: usize = std::env::var("MAX_MESSAGE_LENGTH")
            .unwrap_or_else(|_| "3900".to_string())
            .parse()
            .unwrap_or(3900);

        Ok(Self {
            bot_token,
            app_token,
            omar_url,
            poll_interval_ms,
            max_message_length,
        })
    }
}
