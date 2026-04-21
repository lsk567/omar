mod bridge;
mod config;
mod omar;
mod server;
mod slack;

use std::net::SocketAddr;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::bridge::Bridge;
use crate::config::Config;
use crate::omar::OmarClient;
use crate::server::{build_router, ServerState};
use crate::slack::SlackClient;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging (respects RUST_LOG env var, defaults to info)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_thread_ids(false)
        .init();

    info!("OMAR Slack Bridge starting up...");

    // Load configuration from environment
    let config = Config::from_env()?;
    info!("OMAR API: {}", config.omar_url);
    info!("Bridge HTTP port: {}", config.bridge_port);

    // Create clients
    let slack_client = SlackClient::new(&config.bot_token, &config.app_token);
    let omar_client = OmarClient::new(&config.omar_url, config.omar_ea_id);

    // Build the bridge (holds the shared SlackClient)
    let bridge = Bridge::new(config.clone(), slack_client, omar_client);

    // Start the HTTP callback server in a background task
    let server_state = ServerState {
        slack: bridge.slack_client(),
        max_message_length: config.max_message_length,
    };
    let router = build_router(server_state);
    let addr = SocketAddr::from(([0, 0, 0, 0], config.bridge_port));

    tokio::spawn(async move {
        info!("Bridge HTTP server listening on {}", addr);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind bridge HTTP server");
        axum::serve(listener, router)
            .await
            .expect("Bridge HTTP server error");
    });

    // Run the Socket Mode bridge (blocks until disconnection)
    bridge.run().await?;

    Ok(())
}
