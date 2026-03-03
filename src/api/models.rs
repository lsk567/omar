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
    /// Optional role (e.g. "project-manager") — injects a system prompt
    pub role: Option<String>,
    /// Optional parent agent name (e.g. "pm-rest-api") for chain-of-command tracking
    pub parent: Option<String>,
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

/// Request to update an agent's status
#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
    pub status: String,
}

/// Agent summary response (lightweight card info)
#[derive(Debug, Serialize)]
pub struct AgentSummaryResponse {
    pub id: String,
    pub health: String,
    pub task: Option<String>,
    /// Self-reported status from ~/.omar/status/<session>.md
    pub status: Option<String>,
    /// Direct child agent names
    pub children: Vec<String>,
}

// ── Event Scheduler models ──

/// Request to schedule a new event
#[derive(Debug, Deserialize)]
pub struct ScheduleEventRequest {
    pub sender: String,
    pub receiver: String,
    /// Unix epoch nanoseconds, absolute
    pub timestamp: u64,
    pub payload: String,
    /// If set, the event re-schedules itself at `now + recurring_ns` after each delivery.
    pub recurring_ns: Option<u64>,
}

/// Response after scheduling an event
#[derive(Debug, Serialize)]
pub struct ScheduleEventResponse {
    pub id: String,
    pub timestamp: u64,
}

/// Event info in list response
#[derive(Debug, Serialize)]
pub struct EventInfo {
    pub id: String,
    pub sender: String,
    pub receiver: String,
    pub timestamp: u64,
    pub payload: String,
    pub created_at: u64,
}

/// Response for listing events
#[derive(Debug, Serialize)]
pub struct EventListResponse {
    pub events: Vec<EventInfo>,
}

/// Response after cancelling an event
#[derive(Debug, Serialize)]
pub struct EventCancelResponse {
    pub status: String,
    pub id: String,
}
