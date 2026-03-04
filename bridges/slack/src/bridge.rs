use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::omar::OmarClient;
use crate::slack::{SlackClient, SlackMessage};

/// The bridge connecting Slack messages to the OMAR EA via the event queue.
pub struct Bridge {
    config: Config,
    slack: Arc<Mutex<SlackClient>>,
    omar: OmarClient,
}

impl Bridge {
    pub fn new(config: Config, slack: SlackClient, omar: OmarClient) -> Self {
        Self {
            config,
            slack: Arc::new(Mutex::new(slack)),
            omar,
        }
    }

    /// Returns the shared SlackClient for use by the HTTP server.
    pub fn slack_client(&self) -> Arc<Mutex<SlackClient>> {
        self.slack.clone()
    }

    /// Run the bridge: connect to Slack, dispatch messages as events to the EA.
    pub async fn run(&self) -> Result<()> {
        // 1. Authenticate with Slack
        let bot_user_id = {
            let mut slack = self.slack.lock().await;
            slack.auth_test().await?
        };
        info!("Bot user ID: {}", bot_user_id);

        // 2. Verify OMAR API is reachable
        match self.omar.health_check().await {
            Ok(true) => info!("OMAR API is reachable at {}", self.config.omar_url),
            Ok(false) => warn!("OMAR API returned non-success status"),
            Err(e) => {
                warn!(
                    "OMAR API not reachable: {} -- will retry on message arrival",
                    e
                );
            }
        }

        // 3. Connect Socket Mode and get message stream
        let mut message_rx = {
            let slack = self.slack.lock().await;
            slack.connect_socket_mode(bot_user_id.clone()).await?
        };
        info!("Socket Mode connected, listening for messages...");

        // 4. Process incoming Slack messages — route to EA via event queue
        while let Some(msg) = message_rx.recv().await {
            if let Err(e) = self.handle_message(msg).await {
                error!("Error handling message: {}", e);
            }
        }

        warn!("Message stream ended");
        Ok(())
    }

    /// Handle an incoming Slack message by posting it as an event to the EA.
    async fn handle_message(&self, msg: SlackMessage) -> Result<()> {
        // Resolve user name for context
        let user_name = {
            let mut slack = self.slack.lock().await;
            slack.resolve_user_name(&msg.user).await
        };

        let thread_ts = msg.thread_ts.as_deref().unwrap_or(&msg.ts);

        // Build self-contained event payload with reply instructions
        let payload = format!(
            "[SLACK MESSAGE]\n\
             Channel: {}\n\
             Thread: {}\n\
             User: {}\n\
             Message: {}\n\
             \n\
             To reply: curl -X POST http://localhost:{}/api/slack/reply \
             -H \"Content-Type: application/json\" \
             -d '{{\"channel\":\"{}\",\"thread_ts\":\"{}\",\"text\":\"your reply\"}}'",
            msg.channel,
            thread_ts,
            user_name,
            msg.text,
            self.config.bridge_port,
            msg.channel,
            thread_ts,
        );

        info!(
            "Routing Slack message to EA: channel={} user={} text={}...",
            msg.channel,
            user_name,
            &msg.text[..80.min(msg.text.len())]
        );

        self.omar.post_event("slack-bridge", "ea", &payload).await?;

        Ok(())
    }
}
