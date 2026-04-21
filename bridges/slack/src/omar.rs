#![allow(dead_code)]

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Client for the OMAR HTTP API (localhost:9876).
///
/// All agent/event/project endpoints are EA-scoped under `/api/ea/{ea_id}/...`
/// (see `src/api/mod.rs`). Global endpoints like `/api/health` are unscoped.
#[derive(Clone)]
pub struct OmarClient {
    client: Client,
    base_url: String,
    ea_id: u32,
}

// -- Request/Response models matching OMAR's api/models.rs --

#[derive(Debug, Serialize)]
pub struct SpawnAgentRequest {
    pub name: String,
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SpawnAgentResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentDetail {
    pub id: String,
    pub health: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendInputRequest {
    pub text: String,
    #[serde(default)]
    pub enter: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentListItem {
    pub id: String,
    pub health: String,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

impl OmarClient {
    /// Create a new client bound to a specific EA id. All agent/event/project
    /// endpoints will be routed to `/api/ea/{ea_id}/...`.
    pub fn new(base_url: &str, ea_id: u32) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            ea_id,
        }
    }

    /// Build an EA-scoped URL. `suffix` must start with `/` (e.g. `/events`,
    /// `/agents/foo`). The result has the form
    /// `{base}/api/ea/{ea_id}{suffix}`.
    pub fn ea_url(&self, suffix: &str) -> String {
        // Enforce the invariant in release builds too — a missing leading
        // slash would produce `/api/ea/0agents` and 404 silently.
        assert!(
            suffix.starts_with('/'),
            "ea_url suffix must start with '/': {}",
            suffix
        );
        format!("{}/api/ea/{}{}", self.base_url, self.ea_id, suffix)
    }

    /// Build a global (non-EA-scoped) API URL — used for `/api/health` and
    /// similar manager-wide endpoints.
    pub fn global_url(&self, suffix: &str) -> String {
        assert!(
            suffix.starts_with('/'),
            "global_url suffix must start with '/': {}",
            suffix
        );
        format!("{}/api{}", self.base_url, suffix)
    }

    /// Check if OMAR API is reachable.
    pub async fn health_check(&self) -> Result<bool> {
        let resp = self.client.get(self.global_url("/health")).send().await?;
        Ok(resp.status().is_success())
    }

    /// Spawn a new OMAR agent.
    pub async fn spawn_agent(&self, req: &SpawnAgentRequest) -> Result<SpawnAgentResponse> {
        let resp = self
            .client
            .post(self.ea_url("/agents"))
            .json(req)
            .send()
            .await
            .context("Failed to spawn OMAR agent")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to spawn agent ({}): {}", status, body);
        }

        resp.json().await.context("Failed to parse spawn response")
    }

    /// Get agent details including recent output.
    pub async fn get_agent(&self, name: &str) -> Result<Option<AgentDetail>> {
        let resp = self
            .client
            .get(self.ea_url(&format!("/agents/{}", name)))
            .send()
            .await
            .context("Failed to get OMAR agent")?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to get agent ({}): {}", status, body);
        }

        let detail = resp.json().await.context("Failed to parse agent detail")?;
        Ok(Some(detail))
    }

    /// Send input text to an agent's tmux session.
    pub async fn send_input(&self, name: &str, text: &str) -> Result<()> {
        let req = SendInputRequest {
            text: text.to_string(),
            enter: true,
        };

        let resp = self
            .client
            .post(self.ea_url(&format!("/agents/{}/send", name)))
            .json(&req)
            .send()
            .await
            .context("Failed to send input to OMAR agent")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "Failed to send input to agent {} ({}): {}",
                name, status, body
            );
        } else {
            debug!("Sent input to agent {}", name);
        }

        Ok(())
    }

    /// List all agents.
    pub async fn list_agents(&self) -> Result<Vec<AgentListItem>> {
        let resp = self
            .client
            .get(self.ea_url("/agents"))
            .send()
            .await
            .context("Failed to list OMAR agents")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to list agents ({}): {}", status, body);
        }

        // OMAR returns { agents: [...], manager: ... }
        let body: serde_json::Value = resp.json().await?;
        let agents: Vec<AgentListItem> = if let Some(arr) = body.get("agents") {
            serde_json::from_value(arr.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(agents)
    }

    /// Post an event to the OMAR event queue.
    pub async fn post_event(&self, sender: &str, receiver: &str, payload: &str) -> Result<()> {
        // Use current time in nanoseconds for immediate delivery
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let body = serde_json::json!({
            "sender": sender,
            "receiver": receiver,
            "timestamp": timestamp_ns,
            "payload": payload,
        });

        let resp = self
            .client
            .post(self.ea_url("/events"))
            .json(&body)
            .send()
            .await
            .context("Failed to post event to OMAR")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to post event ({}): {}", status, body_text);
        }

        debug!(
            "Posted event to '{}': {}...",
            receiver,
            &payload[..80.min(payload.len())]
        );
        Ok(())
    }

    /// Kill an agent.
    pub async fn kill_agent(&self, name: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.ea_url(&format!("/agents/{}", name)))
            .send()
            .await
            .context("Failed to kill OMAR agent")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Failed to kill agent {} ({}): {}", name, status, body);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ea_url_uses_ea_prefix_for_zero_ea_id() {
        let c = OmarClient::new("http://127.0.0.1:9876", 0);
        assert_eq!(c.ea_url("/events"), "http://127.0.0.1:9876/api/ea/0/events");
        assert_eq!(c.ea_url("/agents"), "http://127.0.0.1:9876/api/ea/0/agents");
        assert_eq!(
            c.ea_url("/agents/foo"),
            "http://127.0.0.1:9876/api/ea/0/agents/foo"
        );
        assert_eq!(
            c.ea_url("/agents/foo/send"),
            "http://127.0.0.1:9876/api/ea/0/agents/foo/send"
        );
    }

    #[test]
    fn ea_url_respects_custom_ea_id() {
        let c = OmarClient::new("http://127.0.0.1:9876", 3);
        assert_eq!(c.ea_url("/events"), "http://127.0.0.1:9876/api/ea/3/events");
    }

    #[test]
    fn ea_url_trims_trailing_slash_from_base() {
        let c = OmarClient::new("http://127.0.0.1:9876/", 0);
        // No double slash between base and /api
        assert_eq!(c.ea_url("/events"), "http://127.0.0.1:9876/api/ea/0/events");
    }

    #[test]
    fn global_url_is_not_ea_scoped() {
        let c = OmarClient::new("http://127.0.0.1:9876", 0);
        assert_eq!(c.global_url("/health"), "http://127.0.0.1:9876/api/health");
    }

    /// Regression guard: before PR #61 the client posted to `/api/events`
    /// and `/api/agents`. The OMAR server now expects `/api/ea/{id}/...`.
    /// If this invariant ever regresses, Slack @mentions silently 404 again.
    #[test]
    fn no_pre_multi_ea_endpoints_leak_through() {
        let c = OmarClient::new("http://127.0.0.1:9876", 0);
        for suffix in ["/events", "/agents", "/agents/some-name"] {
            let url = c.ea_url(suffix);
            assert!(
                url.contains("/api/ea/"),
                "URL should contain /api/ea/: {}",
                url
            );
            // Make sure we didn't accidentally build `/api/events` etc.
            assert!(
                !url.contains("/api/events") && !url.contains("/api/agents"),
                "URL must not use pre-multi-EA path: {}",
                url
            );
        }
    }
}
