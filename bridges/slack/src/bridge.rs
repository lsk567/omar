use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::omar::{OmarClient, SpawnAgentRequest};
use crate::slack::{SlackClient, SlackMessage};

/// State for a single agent session mapped to a Slack thread.
#[derive(Debug, Clone)]
struct AgentSession {
    /// OMAR agent name
    agent_name: String,
    /// Slack channel ID
    channel: String,
    /// Thread timestamp (used as reply thread_ts)
    thread_ts: String,
    /// Last output cursor — tracks how much output we've already posted
    last_output_len: usize,
    /// Whether this agent has been spawned
    spawned: bool,
}

/// The bridge connecting Slack messages to OMAR agents.
pub struct Bridge {
    config: Config,
    slack: Arc<Mutex<SlackClient>>,
    omar: OmarClient,
    /// Map from session key (channel:thread_ts) to agent session
    sessions: Arc<Mutex<HashMap<String, AgentSession>>>,
}

impl Bridge {
    pub fn new(config: Config, slack: SlackClient, omar: OmarClient) -> Self {
        Self {
            config,
            slack: Arc::new(Mutex::new(slack)),
            omar,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run the bridge: connect to Slack, dispatch messages, poll agent output.
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
                warn!("OMAR API not reachable: {} — will retry on message arrival", e);
            }
        }

        // 3. Connect Socket Mode and get message stream
        let mut message_rx = {
            let slack = self.slack.lock().await;
            slack.connect_socket_mode(bot_user_id.clone()).await?
        };
        info!("Socket Mode connected, listening for messages...");

        // 4. Start output polling task
        let poll_handle = self.spawn_output_poller();

        // 5. Process incoming Slack messages
        while let Some(msg) = message_rx.recv().await {
            if let Err(e) = self.handle_message(msg).await {
                error!("Error handling message: {}", e);
            }
        }

        // If we get here, the message channel closed
        warn!("Message stream ended");
        poll_handle.abort();
        Ok(())
    }

    /// Handle an incoming Slack message.
    async fn handle_message(&self, msg: SlackMessage) -> Result<()> {
        let session_key = self.session_key(&msg);
        let agent_name = self.agent_name_for(&msg);

        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.get(&session_key) {
            // Existing session — send message to the agent
            info!(
                "Forwarding message to existing agent '{}': {}",
                session.agent_name,
                &msg.text[..80.min(msg.text.len())]
            );

            // Resolve user name for context
            let user_name = {
                let mut slack = self.slack.lock().await;
                slack.resolve_user_name(&msg.user).await
            };
            let input = format!("[{}]: {}", user_name, msg.text);
            self.omar.send_input(&session.agent_name, &input).await?;
        } else {
            // New session — spawn an OMAR agent
            info!(
                "Spawning new agent '{}' for channel={} thread={}",
                agent_name, msg.channel, msg.thread_key()
            );

            // Resolve user name for the task description
            let user_name = {
                let mut slack = self.slack.lock().await;
                slack.resolve_user_name(&msg.user).await
            };

            let task = format!(
                "You are responding to a message from {} in Slack. \
                 Respond helpfully and concisely. When you are done, \
                 signal completion clearly.\n\n\
                 Message: {}",
                user_name, msg.text
            );

            let spawn_req = SpawnAgentRequest {
                name: agent_name.clone(),
                task,
                parent: None,
                workdir: None,
            };

            match self.omar.spawn_agent(&spawn_req).await {
                Ok(resp) => {
                    info!("Spawned agent '{}' (session: {:?})", agent_name, resp.session);

                    let thread_ts = if msg.is_threaded_reply() {
                        msg.thread_ts.clone().unwrap_or_else(|| msg.ts.clone())
                    } else {
                        // For top-level messages, we'll reply in a thread
                        msg.ts.clone()
                    };

                    sessions.insert(session_key, AgentSession {
                        agent_name: agent_name.clone(),
                        channel: msg.channel.clone(),
                        thread_ts,
                        last_output_len: 0,
                        spawned: true,
                    });
                }
                Err(e) => {
                    error!("Failed to spawn agent: {}", e);
                    // Try to post error back to Slack
                    let slack = self.slack.lock().await;
                    let thread_ts = msg.thread_ts.as_deref().unwrap_or(&msg.ts);
                    slack.post_message(
                        &msg.channel,
                        &format!(":warning: Failed to start agent: {}", e),
                        Some(thread_ts),
                    ).await.ok();
                }
            }
        }

        Ok(())
    }

    /// Generate a deterministic session key from a message.
    fn session_key(&self, msg: &SlackMessage) -> String {
        format!("{}:{}", msg.channel, msg.thread_key())
    }

    /// Generate an OMAR agent name from a message.
    /// Uses channel + thread_ts to create a unique but readable name.
    fn agent_name_for(&self, msg: &SlackMessage) -> String {
        // Take last 6 chars of channel ID and sanitize thread_ts
        let ch_suffix = if msg.channel.len() > 6 {
            &msg.channel[msg.channel.len() - 6..]
        } else {
            &msg.channel
        };
        let thread = msg.thread_key().replace('.', "-");
        // Limit length: OMAR agent names should be short
        let thread_short = if thread.len() > 12 {
            &thread[..12]
        } else {
            &thread
        };
        format!("slack-{}-{}", ch_suffix, thread_short)
    }

    /// Spawn a background task that polls OMAR agents for new output
    /// and posts it back to Slack.
    fn spawn_output_poller(&self) -> tokio::task::JoinHandle<()> {
        let sessions = self.sessions.clone();
        let slack = self.slack.clone();
        let omar = self.omar.clone();
        let poll_interval = std::time::Duration::from_millis(self.config.poll_interval_ms);
        let max_msg_len = self.config.max_message_length;

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll_interval).await;

                // Snapshot current sessions
                let session_snapshot: Vec<(String, AgentSession)> = {
                    let sessions = sessions.lock().await;
                    sessions.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                };

                for (key, session) in &session_snapshot {
                    if !session.spawned {
                        continue;
                    }

                    match omar.get_agent(&session.agent_name).await {
                        Ok(Some(detail)) => {
                            let output = detail.output.unwrap_or_default();

                            // Check if there's new output beyond what we've already posted
                            if output.len() > session.last_output_len {
                                let new_output = &output[session.last_output_len..];
                                let trimmed = new_output.trim();

                                if !trimmed.is_empty() {
                                    debug!(
                                        "New output from '{}' ({} bytes)",
                                        session.agent_name,
                                        trimmed.len()
                                    );

                                    let slack = slack.lock().await;
                                    if let Err(e) = slack.post_message_chunked(
                                        &session.channel,
                                        trimmed,
                                        Some(&session.thread_ts),
                                        max_msg_len,
                                    ).await {
                                        error!(
                                            "Failed to post output to Slack for {}: {}",
                                            session.agent_name, e
                                        );
                                    }
                                }

                                // Update cursor
                                let mut sessions = sessions.lock().await;
                                if let Some(s) = sessions.get_mut(key) {
                                    s.last_output_len = output.len();
                                }
                            }

                            // Clean up completed agents
                            if detail.health == "idle" {
                                // Check if agent output suggests completion
                                let output_lower = output.to_lowercase();
                                if output_lower.contains("[project complete]")
                                    || output_lower.contains("task completed")
                                {
                                    info!("Agent '{}' appears complete, cleaning up", session.agent_name);
                                    let mut sessions_mut = sessions.lock().await;
                                    sessions_mut.remove(key);
                                    // Optionally kill the agent
                                    if let Err(e) = omar.kill_agent(&session.agent_name).await {
                                        warn!("Failed to kill completed agent {}: {}", session.agent_name, e);
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            // Agent no longer exists — clean up session
                            debug!("Agent '{}' no longer exists, removing session", session.agent_name);
                            let mut sessions_mut = sessions.lock().await;
                            sessions_mut.remove(key);
                        }
                        Err(e) => {
                            debug!("Error checking agent '{}': {}", session.agent_name, e);
                        }
                    }
                }
            }
        })
    }
}
