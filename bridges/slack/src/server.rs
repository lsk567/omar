use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::slack::SlackClient;

/// Shared state for the bridge HTTP server.
#[derive(Clone)]
pub struct ServerState {
    pub slack: Arc<Mutex<SlackClient>>,
    pub max_message_length: usize,
}

/// Request body for the reply endpoint.
#[derive(Debug, Deserialize)]
pub struct SlackReplyRequest {
    pub channel: String,
    pub thread_ts: String,
    pub text: String,
}

/// Build the bridge HTTP server router.
pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/api/slack/reply", post(handle_reply))
        .route("/api/slack/health", get(handle_health))
        .with_state(state)
}

/// POST /api/slack/reply — EA/PMs call this to send messages back to Slack.
async fn handle_reply(
    State(state): State<ServerState>,
    Json(req): Json<SlackReplyRequest>,
) -> impl IntoResponse {
    info!(
        "Reply request: channel={} thread={} text_len={}",
        req.channel,
        req.thread_ts,
        req.text.len()
    );

    let slack = state.slack.lock().await;
    match slack
        .post_message_chunked(
            &req.channel,
            &req.text,
            Some(&req.thread_ts),
            state.max_message_length,
        )
        .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => {
            error!("Failed to post reply to Slack: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
        }
    }
}

/// GET /api/slack/health — simple health check.
async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({"ok": true, "service": "omar-slack-bridge"}))
}
