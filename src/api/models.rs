//! API request and response models

use serde::{Deserialize, Serialize};

/// Request to spawn a new agent
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpawnAgentRequest {
    /// Agent name (auto-generated if not provided)
    pub name: Option<String>,
    /// Task description for the agent
    pub task: Option<String>,
    /// Working directory
    pub workdir: Option<String>,
    /// Command to run (defaults to config)
    pub command: Option<String>,
    /// Optional role (e.g. "project-manager") — injects the agent prompt for supported backends
    pub role: Option<String>,
    /// Optional parent agent name (e.g. "pm-rest-api") for chain-of-command tracking
    pub parent: Option<String>,
}

impl SpawnAgentRequest {
    pub fn with_fallbacks(mut self, fallback: Self) -> Self {
        if self.name.is_none() {
            self.name = fallback.name;
        }
        if self.task.is_none() {
            self.task = fallback.task;
        }
        if self.workdir.is_none() {
            self.workdir = fallback.workdir;
        }
        if self.command.is_none() {
            self.command = fallback.command;
        }
        if self.role.is_none() {
            self.role = fallback.role;
        }
        if self.parent.is_none() {
            self.parent = fallback.parent;
        }
        self
    }
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
    /// If set, this is a cron job that repeats every `recurring_ns` nanoseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurring_ns: Option<u64>,
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

// ── Computer Use models ──

/// Request to acquire the computer use lock
#[derive(Debug, Deserialize)]
pub struct ComputerLockRequest {
    /// Agent requesting the lock
    pub agent: String,
}

/// Response for lock operations
#[derive(Debug, Serialize)]
pub struct ComputerLockResponse {
    pub status: String,
    /// Agent currently holding the lock (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_by: Option<String>,
}

/// Request for a screenshot
#[derive(Debug, Deserialize)]
pub struct ScreenshotRequest {
    /// Agent requesting the screenshot (must hold the lock)
    pub agent: String,
    /// Max width for resizing (optional)
    pub max_width: Option<u32>,
    /// Max height for resizing (optional)
    pub max_height: Option<u32>,
}

/// Response containing a screenshot
#[derive(Debug, Serialize)]
pub struct ScreenshotResponse {
    pub image_base64: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
}

/// Request for mouse actions
#[derive(Debug, Deserialize)]
pub struct MouseRequest {
    /// Agent performing the action (must hold the lock)
    pub agent: String,
    /// Action: "move", "click", "double_click", "drag", "scroll"
    pub action: String,
    /// X coordinate
    pub x: i32,
    /// Y coordinate
    pub y: i32,
    /// Mouse button (1=left, 2=middle, 3=right). Default: 1
    #[serde(default = "default_mouse_button")]
    pub button: u8,
    /// For drag: destination X
    pub to_x: Option<i32>,
    /// For drag: destination Y
    pub to_y: Option<i32>,
    /// For scroll: direction ("up", "down", "left", "right")
    pub scroll_direction: Option<String>,
    /// For scroll: amount (number of clicks)
    #[serde(default = "default_scroll_amount")]
    pub scroll_amount: u32,
}

fn default_mouse_button() -> u8 {
    1
}

fn default_scroll_amount() -> u32 {
    3
}

/// Request for keyboard actions
#[derive(Debug, Deserialize)]
pub struct KeyboardRequest {
    /// Agent performing the action (must hold the lock)
    pub agent: String,
    /// Action: "type" or "key"
    pub action: String,
    /// For "type": text to type. For "key": key combo (e.g. "ctrl+s", "Return")
    pub text: String,
}

/// Generic computer use response (for mouse/keyboard)
#[derive(Debug, Serialize)]
pub struct ComputerActionResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Response for screen size
#[derive(Debug, Serialize)]
pub struct ScreenSizeResponse {
    pub width: u32,
    pub height: u32,
}

/// Response for mouse position
#[derive(Debug, Serialize)]
pub struct MousePositionResponse {
    pub x: i32,
    pub y: i32,
}

/// Response for computer use availability check
#[derive(Debug, Serialize)]
pub struct ComputerAvailabilityResponse {
    pub available: bool,
    pub xdotool: bool,
    pub screenshot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_size: Option<ScreenSizeResponse>,
}
