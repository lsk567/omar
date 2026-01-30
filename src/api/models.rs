//! API request and response models

use serde::{Deserialize, Serialize};

/// Request to spawn a new agent
#[derive(Debug, Deserialize)]
pub struct SpawnAgentRequest {
    /// Agent name (auto-generated if not provided)
    pub name: Option<String>,
    /// Task description for the agent
    pub task: Option<String>,
    /// Working directory
    pub workdir: Option<String>,
    /// Command to run (defaults to config)
    pub command: Option<String>,
}

/// Response after spawning an agent
#[derive(Debug, Serialize)]
pub struct SpawnAgentResponse {
    pub id: String,
    pub status: String,
    pub session: String,
}

/// Agent info in list response
#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub status: String,
    pub health: String,
    pub last_output: String,
}

/// Response for listing agents
#[derive(Debug, Serialize)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentInfo>,
    pub manager: Option<AgentInfo>,
}

/// Detailed agent info
#[derive(Debug, Serialize)]
pub struct AgentDetailResponse {
    pub id: String,
    pub status: String,
    pub health: String,
    pub last_output: String,
    pub output_tail: String,
}

/// Request to send input to an agent
#[derive(Debug, Deserialize)]
pub struct SendInputRequest {
    /// Text to send
    pub text: String,
    /// Whether to send Enter key after
    #[serde(default)]
    pub enter: bool,
}

/// Generic status response
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Request to add a project
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub name: String,
}

/// Single project in response
#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    pub id: usize,
    pub name: String,
}

/// Response for listing projects
#[derive(Debug, Serialize)]
pub struct ListProjectsResponse {
    pub projects: Vec<ProjectResponse>,
}
