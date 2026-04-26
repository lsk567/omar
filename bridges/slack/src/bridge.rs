use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::omar::OmarMcp;
use crate::slack::{SlackClient, SlackMessage};

/// A Slack reply the EA queued via the `slack_reply` MCP tool. Fields
/// match the JSON written by `mcp::OmarMcpServer::slack_reply` — adding
/// fields server-side should be backwards-compatible because unknown
/// fields are ignored by serde's default behaviour.
#[derive(Debug, Deserialize)]
struct OutboxReply {
    channel: String,
    #[serde(default)]
    thread_ts: Option<String>,
    text: String,
}

pub struct Bridge {
    config: Config,
    slack: Arc<Mutex<SlackClient>>,
    omar: Arc<Mutex<OmarMcp>>,
}

impl Bridge {
    pub fn new(config: Config, slack: SlackClient, omar: OmarMcp) -> Self {
        Self {
            config,
            slack: Arc::new(Mutex::new(slack)),
            omar: Arc::new(Mutex::new(omar)),
        }
    }

    /// Run the bridge: connect to Slack, handle inbound messages via MCP,
    /// and drain the outbound slack_outbox directory in a background task.
    pub async fn run(&self) -> Result<()> {
        // 1. Authenticate with Slack.
        let bot_user_id = {
            let mut slack = self.slack.lock().await;
            slack.auth_test().await?
        };
        info!("Bot user ID: {}", bot_user_id);

        // 2. Probe the MCP server so we fail fast on a misconfigured
        //    OMAR_BINARY / missing active EA.
        match self.omar.lock().await.health_check().await {
            Ok(()) => info!(
                "MCP server reachable via {}",
                self.config.omar_binary.display()
            ),
            Err(e) => warn!(
                "MCP server not reachable at startup: {} — will retry on message arrival",
                e
            ),
        }

        // 3. Spawn the outbound outbox watcher.
        let outbox_dir = self.config.omar_dir.join("slack_outbox");
        if let Err(e) = std::fs::create_dir_all(&outbox_dir) {
            warn!(
                "Failed to create slack outbox {:?}: {} — outbound replies will be dropped",
                outbox_dir, e
            );
        }
        let slack_for_outbox = self.slack.clone();
        let max_len = self.config.max_message_length;
        tokio::spawn(async move {
            run_outbox_watcher(outbox_dir, slack_for_outbox, max_len).await;
        });

        // 4. Connect Socket Mode and get message stream.
        let mut message_rx = {
            let slack = self.slack.lock().await;
            slack.connect_socket_mode(bot_user_id.clone()).await?
        };
        info!("Socket Mode connected, listening for messages...");

        // 5. Process inbound messages — each becomes an EA-scoped event
        //    via the `omar_wake_later` MCP tool.
        while let Some(msg) = message_rx.recv().await {
            if let Err(e) = self.handle_message(msg).await {
                error!("Error handling message: {}", e);
            }
        }

        warn!("Message stream ended");
        Ok(())
    }

    /// Translate one Slack message into an EA event payload and hand it to
    /// OMAR via MCP.
    async fn handle_message(&self, msg: SlackMessage) -> Result<()> {
        let user_name = {
            let mut slack = self.slack.lock().await;
            slack.resolve_user_name(&msg.user).await
        };
        let thread_ts = msg.thread_ts.as_deref().unwrap_or(&msg.ts);

        let payload = format!(
            "[SLACK MESSAGE]\n\
             Channel: {}\n\
             Thread: {}\n\
             User: {}\n\
             Message: {}\n\
             \n\
             To reply: call the OMAR `slack_reply` MCP tool with \
             channel=\"{}\", thread_ts=\"{}\", and your reply text.",
            msg.channel, thread_ts, user_name, msg.text, msg.channel, thread_ts,
        );

        info!(
            "Routing Slack message to EA: channel={} user={} text={}...",
            msg.channel,
            user_name,
            &msg.text[..80.min(msg.text.len())]
        );

        let mut omar = self.omar.lock().await;
        omar.post_slack_event(&payload)
            .await
            .context("Failed to post Slack event via MCP")?;
        Ok(())
    }
}

/// Poll `outbox_dir` for `slack_reply` MCP tool results and forward them
/// to Slack. Files are deleted on successful delivery; failures leave the
/// file in place so the next poll retries. Survives bridge restarts —
/// anything queued while the bridge was down is drained on the next run.
async fn run_outbox_watcher(outbox_dir: PathBuf, slack: Arc<Mutex<SlackClient>>, max_len: usize) {
    const MIN_POLL_INTERVAL: Duration = Duration::from_millis(500);
    const MAX_IDLE_INTERVAL: Duration = Duration::from_secs(5);
    let mut poll_interval = MIN_POLL_INTERVAL;
    loop {
        if drain_outbox_once(&outbox_dir, &slack, max_len).await {
            poll_interval = MIN_POLL_INTERVAL;
        } else {
            poll_interval = (poll_interval * 2).min(MAX_IDLE_INTERVAL);
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn drain_outbox_once(outbox_dir: &Path, slack: &Mutex<SlackClient>, max_len: usize) -> bool {
    let entries = match std::fs::read_dir(outbox_dir) {
        Ok(entries) => entries,
        Err(_) => return false,
    };

    // Collect first, then sort by name — MCP tool writes `<ts_ns>-<uuid>.json`
    // so lexical order is chronological.
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return false;
    }

    for path in paths {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read outbox file {:?}: {}", path, e);
                continue;
            }
        };
        let reply: OutboxReply = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(e) => {
                // Malformed file — move it aside instead of retrying forever.
                warn!("Discarding malformed outbox file {:?}: {}", path, e);
                let bad = path.with_extension("json.bad");
                let _ = std::fs::rename(&path, &bad);
                continue;
            }
        };

        let slack_guard = slack.lock().await;
        match slack_guard
            .post_message_chunked(
                &reply.channel,
                &reply.text,
                reply.thread_ts.as_deref(),
                max_len,
            )
            .await
        {
            Ok(()) => {
                debug!("Delivered Slack reply from outbox: {:?}", path);
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => {
                warn!(
                    "Failed to deliver outbox reply {:?}: {} — will retry",
                    path, e
                );
                // Don't delete — next poll retries.
                break;
            }
        }
    }
    true
}
