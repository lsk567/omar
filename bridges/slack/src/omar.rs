#![allow(dead_code)]

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Client for the OMAR HTTP API (localhost:9876).
#[derive(Clone)]
pub struct OmarClient {
    client: Client,
    base_url: String,
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
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Check if OMAR API is reachable.
    pub async fn health_check(&self) -> Result<bool> {
        let resp = self
            .client
            .get(format!("{}/api/health", self.base_url))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    /// Spawn a new OMAR agent.
    pub async fn spawn_agent(&self, req: &SpawnAgentRequest) -> Result<SpawnAgentResponse> {
        let resp = self
            .client
            .post(format!("{}/api/agents", self.base_url))
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
            .get(format!("{}/api/agents/{}", self.base_url, name))
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
            .post(format!("{}/api/agents/{}/send", self.base_url, name))
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
            .get(format!("{}/api/agents", self.base_url))
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

    /// Kill an agent.
    pub async fn kill_agent(&self, name: &str) -> Result<()> {
        let resp = self
            .client
            .delete(format!("{}/api/agents/{}", self.base_url, name))
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
