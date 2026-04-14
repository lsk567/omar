#![allow(dead_code)]

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Slack Web API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ConnectionsOpenResponse {
    ok: bool,
    url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthTestResponse {
    pub ok: bool,
    pub user_id: Option<String>,
    pub bot_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostMessageResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    ok: bool,
    user: Option<UserProfile>,
}

#[derive(Debug, Deserialize)]
struct UserProfile {
    real_name: Option<String>,
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// Socket Mode envelope types
// ---------------------------------------------------------------------------

/// Top-level envelope received over the Socket Mode WebSocket.
#[derive(Debug, Deserialize)]
pub struct SocketEnvelope {
    pub envelope_id: Option<String>,
    #[serde(rename = "type")]
    pub envelope_type: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub retry_attempt: Option<u32>,
    pub retry_reason: Option<String>,
    #[serde(default)]
    pub accepts_response_payload: bool,
}

/// Acknowledgement sent back over the WebSocket.
#[derive(Debug, Serialize)]
struct SocketAck {
    envelope_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Parsed Slack event types
// ---------------------------------------------------------------------------

/// A Slack message event extracted from the Socket Mode payload.
#[derive(Debug, Clone)]
pub struct SlackMessage {
    /// Slack channel ID (C..., D..., G...)
    pub channel: String,
    /// User ID of the sender (may be empty for bot messages)
    pub user: String,
    /// Message text content
    pub text: String,
    /// Message timestamp (Slack's unique message ID within a channel)
    pub ts: String,
    /// Thread parent timestamp — present if this is a threaded reply
    pub thread_ts: Option<String>,
    /// Channel type: "channel", "group", "im", "mpim"
    pub channel_type: Option<String>,
    /// Bot ID if message is from a bot
    pub bot_id: Option<String>,
    /// Message subtype (None for regular messages)
    pub subtype: Option<String>,
}

impl SlackMessage {
    /// The effective thread key — thread_ts if in a thread, else the message's own ts.
    pub fn thread_key(&self) -> &str {
        self.thread_ts.as_deref().unwrap_or(&self.ts)
    }

    /// Whether this message is part of a thread (a reply).
    pub fn is_threaded_reply(&self) -> bool {
        match &self.thread_ts {
            Some(tts) => tts != &self.ts,
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// SlackClient
// ---------------------------------------------------------------------------

/// Slack API client handling both Socket Mode WebSocket and Web API calls.
pub struct SlackClient {
    http: Client,
    bot_token: String,
    app_token: String,
    pub bot_user_id: Option<String>,
    user_name_cache: std::collections::HashMap<String, String>,
}

impl SlackClient {
    pub fn new(bot_token: &str, app_token: &str) -> Self {
        Self {
            http: Client::new(),
            bot_token: bot_token.to_string(),
            app_token: app_token.to_string(),
            bot_user_id: None,
            user_name_cache: std::collections::HashMap::new(),
        }
    }

    // -- Web API calls --

    /// Fetch the bot's own user ID via auth.test.
    pub async fn auth_test(&mut self) -> Result<String> {
        let resp: AuthTestResponse = self
            .http
            .post("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            anyhow::bail!("auth.test failed: {:?}", resp.error);
        }

        let user_id = resp.user_id.context("auth.test did not return user_id")?;
        self.bot_user_id = Some(user_id.clone());
        info!("Authenticated as bot user: {}", user_id);
        Ok(user_id)
    }

    /// Post a message to a Slack channel, optionally in a thread.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp: PostMessageResponse = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            warn!("chat.postMessage failed: {:?}", resp.error);
        }
        Ok(())
    }

    /// Post a message, splitting into chunks if it exceeds max_length.
    pub async fn post_message_chunked(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
        max_length: usize,
    ) -> Result<()> {
        if text.len() <= max_length {
            return self.post_message(channel, text, thread_ts).await;
        }

        // Split on line boundaries when possible
        let mut remaining = text;
        while !remaining.is_empty() {
            let chunk_end = if remaining.len() <= max_length {
                remaining.len()
            } else {
                // Find a newline break point
                remaining[..max_length]
                    .rfind('\n')
                    .map(|p| p + 1) // include the newline
                    .unwrap_or(max_length)
            };
            let chunk = &remaining[..chunk_end];
            self.post_message(channel, chunk, thread_ts).await?;
            remaining = &remaining[chunk_end..];
        }
        Ok(())
    }

    /// Resolve a Slack user ID to a display name (cached).
    pub async fn resolve_user_name(&mut self, user_id: &str) -> String {
        if let Some(cached) = self.user_name_cache.get(user_id) {
            return cached.clone();
        }

        let name = match self.fetch_user_name(user_id).await {
            Ok(Some(n)) => n,
            Ok(None) => user_id.to_string(),
            Err(e) => {
                debug!("Failed to resolve user {}: {}", user_id, e);
                user_id.to_string()
            }
        };

        self.user_name_cache
            .insert(user_id.to_string(), name.clone());
        name
    }

    async fn fetch_user_name(&self, user_id: &str) -> Result<Option<String>> {
        let resp: UserInfoResponse = self
            .http
            .get("https://slack.com/api/users.info")
            .bearer_auth(&self.bot_token)
            .query(&[("user", user_id)])
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Ok(None);
        }

        Ok(resp.user.and_then(|u| u.real_name.or(u.name)))
    }

    /// Request a Socket Mode WebSocket URL from Slack.
    async fn get_websocket_url(&self) -> Result<String> {
        let resp: ConnectionsOpenResponse = self
            .http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&self.app_token)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            anyhow::bail!(
                "apps.connections.open failed: {:?}",
                resp.error.unwrap_or_else(|| "unknown error".into())
            );
        }

        resp.url.context("No WebSocket URL in response")
    }

    // -- Socket Mode connection --

    /// Connect to Slack Socket Mode and return a channel of parsed messages.
    ///
    /// This spawns a background task that:
    /// 1. Obtains a WebSocket URL via apps.connections.open
    /// 2. Connects to the WebSocket
    /// 3. Listens for events and acknowledges them
    /// 4. Sends parsed SlackMessage events through the returned channel
    /// 5. Reconnects automatically on disconnection
    pub async fn connect_socket_mode(
        &self,
        bot_user_id: String,
    ) -> Result<mpsc::UnboundedReceiver<SlackMessage>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let app_token = self.app_token.clone();
        let http = self.http.clone();

        tokio::spawn(async move {
            loop {
                match run_socket_mode_loop(&http, &app_token, &bot_user_id, &tx).await {
                    Ok(()) => {
                        info!("Socket Mode connection closed, reconnecting in 5s...");
                    }
                    Err(e) => {
                        error!("Socket Mode error: {}, reconnecting in 5s...", e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        Ok(rx)
    }
}

/// How long to wait for any WebSocket message before assuming the connection is dead.
/// Slack Socket Mode typically sends pings every ~30s, so 90s is generous.
const WS_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// How often to send a client-side ping to detect dead connections.
const WS_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Internal: run the Socket Mode WebSocket loop. Returns on disconnection.
async fn run_socket_mode_loop(
    http: &Client,
    app_token: &str,
    bot_user_id: &str,
    tx: &mpsc::UnboundedSender<SlackMessage>,
) -> Result<()> {
    // 1. Get WebSocket URL
    let ws_url_str = {
        let resp: ConnectionsOpenResponse = http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(app_token)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            anyhow::bail!(
                "apps.connections.open failed: {:?}",
                resp.error.unwrap_or_else(|| "unknown".into())
            );
        }
        resp.url.context("No WebSocket URL")?
    };

    info!(
        "Connecting to Slack Socket Mode: {}...",
        &ws_url_str[..60.min(ws_url_str.len())]
    );

    // 2. Connect WebSocket (pass as string — tokio-tungstenite accepts &str)
    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url_str).await?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    info!("Socket Mode connected");

    // 3. Process messages with read deadline and periodic keepalive ping.
    //
    // We track a persistent read deadline that is only reset after a
    // successful read.  This avoids the pitfall where `tokio::select!`
    // drops and recreates the timeout future on every ping tick, which
    // would prevent the read timeout from ever firing.
    let mut ping_interval = tokio::time::interval(WS_PING_INTERVAL);
    ping_interval.tick().await; // consume the immediate first tick

    let mut read_deadline = tokio::time::Instant::now() + WS_READ_TIMEOUT;

    loop {
        tokio::select! {
            // Branch 1: incoming WebSocket message
            msg_option = ws_read.next() => {
                let msg = match msg_option {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        error!("WebSocket read error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                };

                // Got a message — reset the read deadline
                read_deadline = tokio::time::Instant::now() + WS_READ_TIMEOUT;

                match msg {
                    WsMessage::Text(text) => {
                        let action = handle_socket_message(&text, bot_user_id, tx, &mut ws_write).await;
                        if action == LoopAction::Reconnect {
                            info!("Disconnect requested, breaking loop to reconnect");
                            break;
                        }
                    }
                    WsMessage::Ping(data) => {
                        if let Err(e) = ws_write.send(WsMessage::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    WsMessage::Close(_) => {
                        info!("WebSocket closed by server");
                        break;
                    }
                    _ => {}
                }
            }
            // Branch 2: periodic client-side ping to detect dead connections
            _ = ping_interval.tick() => {
                debug!("Sending keepalive ping");
                if let Err(e) = ws_write.send(WsMessage::Ping(vec![])).await {
                    error!("Failed to send keepalive ping: {}", e);
                    break;
                }
            }
            // Branch 3: read deadline expired — no data received for too long
            _ = tokio::time::sleep_until(read_deadline) => {
                warn!("No WebSocket message received in {:?}, assuming dead connection", WS_READ_TIMEOUT);
                break;
            }
        }
    }

    Ok(())
}

/// Signals whether the WebSocket loop should continue or reconnect.
#[derive(Debug, PartialEq, Eq)]
enum LoopAction {
    Continue,
    Reconnect,
}

/// Parse a Socket Mode envelope and handle it.
/// Returns `LoopAction::Reconnect` when Slack requests a disconnect.
async fn handle_socket_message<S>(
    text: &str,
    bot_user_id: &str,
    tx: &mpsc::UnboundedSender<SlackMessage>,
    ws_write: &mut S,
) -> LoopAction
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let envelope: SocketEnvelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            debug!(
                "Failed to parse envelope: {} — raw: {}",
                e,
                &text[..200.min(text.len())]
            );
            return LoopAction::Continue;
        }
    };

    // Always acknowledge the envelope first (Slack requires this within 3 seconds)
    if let Some(ref envelope_id) = envelope.envelope_id {
        let ack = SocketAck {
            envelope_id: envelope_id.clone(),
            payload: None,
        };
        if let Ok(ack_json) = serde_json::to_string(&ack) {
            if let Err(e) = ws_write.send(WsMessage::Text(ack_json)).await {
                error!("Failed to send ack: {}", e);
            }
        }
    }

    // Route based on envelope type
    match envelope.envelope_type.as_deref() {
        Some("events_api") => {
            if let Some(payload) = envelope.payload {
                handle_events_api_payload(payload, bot_user_id, tx);
            }
            LoopAction::Continue
        }
        Some("hello") => {
            info!("Received Socket Mode hello");
            LoopAction::Continue
        }
        Some("disconnect") => {
            info!("Received Socket Mode disconnect request — will reconnect");
            LoopAction::Reconnect
        }
        Some(other) => {
            debug!("Ignoring envelope type: {}", other);
            LoopAction::Continue
        }
        None => {
            debug!("Envelope with no type");
            LoopAction::Continue
        }
    }
}

/// Handle an events_api payload — extract message events.
fn handle_events_api_payload(
    payload: serde_json::Value,
    bot_user_id: &str,
    tx: &mpsc::UnboundedSender<SlackMessage>,
) {
    // The event is nested under payload.event
    let event = match payload.get("event") {
        Some(e) => e,
        None => {
            debug!("events_api payload has no 'event' field");
            return;
        }
    };

    let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if event_type != "message" && event_type != "app_mention" {
        debug!("Ignoring event type: {}", event_type);
        return;
    }

    // Filter subtypes — only handle regular messages and bot_message
    let subtype = event.get("subtype").and_then(|s| s.as_str());
    match subtype {
        None | Some("bot_message") => {} // process these
        Some(st) => {
            debug!("Ignoring message subtype: {}", st);
            return;
        }
    }

    // Extract message fields
    let channel = event
        .get("channel")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let user = event
        .get("user")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let text = event
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let ts = event
        .get("ts")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let thread_ts = event
        .get("thread_ts")
        .and_then(|t| t.as_str())
        .map(String::from);
    let channel_type = event
        .get("channel_type")
        .and_then(|c| c.as_str())
        .map(String::from);
    let bot_id = event
        .get("bot_id")
        .and_then(|b| b.as_str())
        .map(String::from);

    if text.is_empty() || channel.is_empty() {
        return;
    }

    // Skip our own messages to avoid echo loops
    let is_self = user == bot_user_id || bot_id.is_some();
    if is_self {
        debug!("Skipping own message in {}", channel);
        return;
    }

    let msg = SlackMessage {
        channel,
        user,
        text,
        ts,
        thread_ts,
        channel_type,
        bot_id,
        subtype: subtype.map(String::from),
    };

    debug!(
        "Received message in #{}: {}",
        msg.channel,
        &msg.text[..80.min(msg.text.len())]
    );

    if tx.send(msg).is_err() {
        error!("Message channel closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple in-memory sink that collects sent WebSocket messages.
    struct MockWsSink {
        sent: Vec<WsMessage>,
    }

    impl MockWsSink {
        fn new() -> Self {
            Self { sent: Vec::new() }
        }
    }

    impl futures_util::Sink<WsMessage> for MockWsSink {
        type Error = String;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(self: std::pin::Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
            self.get_mut().sent.push(item);
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_disconnect_envelope_triggers_reconnect() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut sink = MockWsSink::new();

        let envelope = r#"{"envelope_id":"abc123","type":"disconnect"}"#;
        let action = handle_socket_message(envelope, "U_BOT", &tx, &mut sink).await;

        assert_eq!(action, LoopAction::Reconnect);
        // Should still ack the envelope
        assert_eq!(sink.sent.len(), 1);
        let ack_text = match &sink.sent[0] {
            WsMessage::Text(t) => t.clone(),
            _ => panic!("Expected Text message"),
        };
        assert!(ack_text.contains("abc123"));
    }

    #[tokio::test]
    async fn test_hello_envelope_continues() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut sink = MockWsSink::new();

        let envelope = r#"{"type":"hello"}"#;
        let action = handle_socket_message(envelope, "U_BOT", &tx, &mut sink).await;

        assert_eq!(action, LoopAction::Continue);
    }

    #[tokio::test]
    async fn test_events_api_forwards_message() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = MockWsSink::new();

        let envelope = r#"{
            "envelope_id": "ev_123",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "channel": "C123",
                    "user": "U_USER",
                    "text": "hello world",
                    "ts": "1234567890.000100"
                }
            }
        }"#;

        let action = handle_socket_message(envelope, "U_BOT", &tx, &mut sink).await;

        assert_eq!(action, LoopAction::Continue);
        // Should have forwarded the message
        let msg = rx.try_recv().expect("Should have received a message");
        assert_eq!(msg.channel, "C123");
        assert_eq!(msg.user, "U_USER");
        assert_eq!(msg.text, "hello world");
    }

    #[tokio::test]
    async fn test_own_messages_are_skipped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = MockWsSink::new();

        // Message from the bot itself
        let envelope = r#"{
            "envelope_id": "ev_456",
            "type": "events_api",
            "payload": {
                "event": {
                    "type": "message",
                    "channel": "C123",
                    "user": "U_BOT",
                    "text": "I said something",
                    "ts": "1234567890.000200"
                }
            }
        }"#;

        let action = handle_socket_message(envelope, "U_BOT", &tx, &mut sink).await;

        assert_eq!(action, LoopAction::Continue);
        // Should NOT have forwarded (own message)
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_malformed_envelope_continues() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut sink = MockWsSink::new();

        let action = handle_socket_message("not json", "U_BOT", &tx, &mut sink).await;
        assert_eq!(action, LoopAction::Continue);
        assert!(sink.sent.is_empty()); // no ack sent for unparseable envelope
    }

    #[test]
    fn test_slack_message_thread_key() {
        let msg = SlackMessage {
            channel: "C123".into(),
            user: "U1".into(),
            text: "hi".into(),
            ts: "1234.5678".into(),
            thread_ts: Some("1234.0000".into()),
            channel_type: None,
            bot_id: None,
            subtype: None,
        };
        assert_eq!(msg.thread_key(), "1234.0000");
        assert!(msg.is_threaded_reply());

        let msg2 = SlackMessage {
            thread_ts: None,
            ..msg.clone()
        };
        assert_eq!(msg2.thread_key(), "1234.5678");
        assert!(!msg2.is_threaded_reply());
    }
}
