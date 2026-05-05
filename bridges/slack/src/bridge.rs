use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::omar::OmarMcp;
use crate::settings;
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

        // 2b. Resolve the bridge's persisted EA pin against the dashboard's
        //     EA registry. If the persisted name is missing or unresolvable,
        //     fall back to the first registered EA and write that back so
        //     subsequent runs are stable.
        if let Err(e) = self.resolve_target_ea().await {
            warn!(
                "Failed to resolve target EA: {} — using dashboard default",
                e
            );
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
        //    via the `schedule_event` MCP tool.
        while let Some(msg) = message_rx.recv().await {
            if let Err(e) = self.handle_message(msg, &bot_user_id).await {
                error!("Error handling message: {}", e);
            }
        }

        warn!("Message stream ended");
        Ok(())
    }

    /// Translate one Slack message into an EA event payload and hand it to
    /// OMAR via MCP. Slash commands beginning with `/ea` are handled
    /// entirely by the bridge and never reach the LLM. In channels Slack
    /// prepends the bot mention to the message text (`<@BOTID> /ea X`),
    /// so we strip a leading mention before checking the command prefix.
    async fn handle_message(&self, msg: SlackMessage, bot_user_id: &str) -> Result<()> {
        let cleaned = strip_leading_bot_mention(&msg.text, bot_user_id);
        if let Some(name) = parse_ea_command(cleaned) {
            return self.handle_ea_command(name, &msg).await;
        }

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

    /// Read the persisted target EA from `~/.omar/config.toml`, validate
    /// against the live registry, fall back to the first EA if missing,
    /// then pin the MCP client to the resolved EA. Persists the resolved
    /// name when it had to fall back so the file converges.
    async fn resolve_target_ea(&self) -> Result<()> {
        let mut omar = self.omar.lock().await;
        let (_, eas) = omar.list_eas().await.context("list_eas failed")?;
        if eas.is_empty() {
            return Err(anyhow::anyhow!("dashboard reports zero registered EAs"));
        }
        let desired = settings::load_active_ea(&self.config.omar_dir);
        let (target_id, target_name) = match desired
            .as_deref()
            .and_then(|name| eas.iter().find(|(_, n)| n == name).cloned())
        {
            Some((id, name)) => (id, name),
            None => {
                let (id, name) = eas[0].clone();
                if desired.is_some() {
                    warn!(
                        "Persisted active_ea '{}' not in registry; falling back to '{}'",
                        desired.as_deref().unwrap_or(""),
                        name
                    );
                }
                if let Err(e) = settings::save_active_ea(&self.config.omar_dir, &name) {
                    warn!("Failed to persist resolved active_ea '{}': {}", name, e);
                }
                (id, name)
            }
        };
        info!(
            "Slack bridge pinned to EA '{}' (id={})",
            target_name, target_id
        );
        omar.set_target_ea(Some(target_id));
        Ok(())
    }

    /// Apply a `/ea <name>` Slack command. Validates the name against the
    /// live EA registry, persists the selection, repoints the MCP client,
    /// and replies in the same Slack thread. Never forwards to the LLM.
    async fn handle_ea_command(&self, name: &str, msg: &SlackMessage) -> Result<()> {
        let thread_ts = msg.thread_ts.as_deref().unwrap_or(&msg.ts).to_string();
        let channel = msg.channel.clone();

        if name.is_empty() {
            self.slack_reply(&channel, &thread_ts, "Usage: `/ea <EA_name>`")
                .await;
            return Ok(());
        }

        let lookup = {
            let mut omar = self.omar.lock().await;
            omar.list_eas().await
        };
        let (_, eas) = match lookup {
            Ok(v) => v,
            Err(e) => {
                let text = format!("Failed to list EAs: {}", e);
                self.slack_reply(&channel, &thread_ts, &text).await;
                return Ok(());
            }
        };

        let matched = eas.iter().find(|(_, n)| n == name).cloned();
        match matched {
            None => {
                let available = if eas.is_empty() {
                    "(none registered)".to_string()
                } else {
                    eas.iter()
                        .map(|(_, n)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let text = format!("EA '{}' not found. Available: {}", name, available);
                self.slack_reply(&channel, &thread_ts, &text).await;
            }
            Some((id, resolved_name)) => {
                if let Err(e) = settings::save_active_ea(&self.config.omar_dir, &resolved_name) {
                    let text = format!("Failed to persist EA selection: {}", e);
                    self.slack_reply(&channel, &thread_ts, &text).await;
                    return Ok(());
                }
                {
                    let mut omar = self.omar.lock().await;
                    omar.set_target_ea(Some(id));
                }
                info!(
                    "Slack bridge re-pinned to EA '{}' (id={})",
                    resolved_name, id
                );
                let text = format!("Switched to EA '{}' (id={})", resolved_name, id);
                self.slack_reply(&channel, &thread_ts, &text).await;
            }
        }
        Ok(())
    }

    async fn slack_reply(&self, channel: &str, thread_ts: &str, text: &str) {
        let slack = self.slack.lock().await;
        if let Err(e) = slack
            .post_message_chunked(
                channel,
                text,
                Some(thread_ts),
                self.config.max_message_length,
            )
            .await
        {
            warn!("Failed to post /ea reply to Slack: {}", e);
        }
    }
}

/// Parse a Slack message body for the `/ea <name>` command. Returns
/// `Some(name)` (possibly empty) iff the trimmed body starts with `/ea`
/// followed by end-of-string or whitespace. Returns `None` when the
/// message is not an `/ea` command — those flow through to the LLM.
pub(crate) fn parse_ea_command(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("/ea")?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.trim()),
        Some(_) => None, // e.g. `/each`, not our command
    }
}

/// Strip a leading `<@BOTID>` (or `<@BOTID|name>`) Slack mention so the
/// rest of the message can be parsed as if it had been sent in a DM.
/// Slack only prepends this mention in channel/group conversations; DMs
/// arrive without it. Only the bot's own mention is stripped — mentions
/// of other users pass through untouched so the agent still sees them.
pub(crate) fn strip_leading_bot_mention<'a>(text: &'a str, bot_user_id: &str) -> &'a str {
    if bot_user_id.is_empty() {
        return text;
    }
    let trimmed = text.trim_start();
    let prefix = format!("<@{}", bot_user_id);
    let rest = match trimmed.strip_prefix(&prefix) {
        Some(rest) => rest,
        None => return text,
    };
    let after_close = if let Some(rest) = rest.strip_prefix('>') {
        rest
    } else if let Some(after_pipe) = rest.strip_prefix('|') {
        match after_pipe.find('>') {
            Some(idx) => &after_pipe[idx + 1..],
            None => return text,
        }
    } else {
        // Prefix matched the bot id but the next char isn't '>' or '|',
        // so this is `<@BOTIDX...>` — a different user whose id starts
        // with ours. Leave the message alone.
        return text;
    };
    after_close.trim_start()
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

#[cfg(test)]
mod tests {
    use super::{parse_ea_command, strip_leading_bot_mention};

    #[test]
    fn parses_ea_with_name() {
        assert_eq!(parse_ea_command("/ea Research"), Some("Research"));
    }

    #[test]
    fn parses_ea_with_extra_whitespace() {
        assert_eq!(parse_ea_command("   /ea   Research  "), Some("Research"));
    }

    #[test]
    fn parses_ea_with_empty_argument() {
        assert_eq!(parse_ea_command("/ea"), Some(""));
        assert_eq!(parse_ea_command("/ea   "), Some(""));
    }

    #[test]
    fn rejects_non_ea_commands() {
        assert_eq!(parse_ea_command("/each"), None);
        assert_eq!(parse_ea_command("/eat lunch"), None);
        assert_eq!(parse_ea_command("hello /ea Research"), None);
        assert_eq!(parse_ea_command("regular message"), None);
    }

    #[test]
    fn strip_mention_removes_bare_id() {
        assert_eq!(
            strip_leading_bot_mention("<@U12345> /ea Research", "U12345"),
            "/ea Research"
        );
    }

    #[test]
    fn strip_mention_removes_id_with_label() {
        assert_eq!(
            strip_leading_bot_mention("<@U12345|omar-bot> /ea Research", "U12345"),
            "/ea Research"
        );
    }

    #[test]
    fn strip_mention_handles_leading_whitespace() {
        assert_eq!(
            strip_leading_bot_mention("   <@U12345>   /ea Research", "U12345"),
            "/ea Research"
        );
    }

    #[test]
    fn strip_mention_leaves_text_alone_without_mention() {
        assert_eq!(
            strip_leading_bot_mention("/ea Research", "U12345"),
            "/ea Research"
        );
        assert_eq!(
            strip_leading_bot_mention("regular message", "U12345"),
            "regular message"
        );
    }

    #[test]
    fn strip_mention_only_strips_bot_id() {
        // Another user mentioned at the start — leave it alone so the
        // agent still sees the @who.
        assert_eq!(
            strip_leading_bot_mention("<@UOTHER> hi", "U12345"),
            "<@UOTHER> hi"
        );
        // Bot id is a *prefix* of another user's id (no terminator) —
        // must not be stripped.
        assert_eq!(
            strip_leading_bot_mention("<@U12345X> hi", "U12345"),
            "<@U12345X> hi"
        );
    }

    #[test]
    fn strip_mention_then_parse_ea() {
        let stripped = strip_leading_bot_mention("<@U12345> /ea Research", "U12345");
        assert_eq!(parse_ea_command(stripped), Some("Research"));
    }
}
