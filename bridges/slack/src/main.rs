mod bridge;
mod config;
mod omar;
mod slack;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::bridge::Bridge;
use crate::config::Config;
use crate::omar::OmarMcp;
use crate::slack::SlackClient;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_thread_ids(false)
        .init();

    info!("OMAR Slack Bridge starting up...");

    let config = Config::from_env()?;
    info!("omar binary: {}", config.omar_binary.display());
    info!("omar dir:    {}", config.omar_dir.display());

    let slack_client = SlackClient::new(&config.bot_token, &config.app_token);
    let omar = OmarMcp::new(config.omar_binary.clone());

    let bridge = Bridge::new(config, slack_client, omar);
    bridge.run().await?;

    Ok(())
}
