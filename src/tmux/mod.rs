mod client;
mod health;
mod session;

pub use client::{DeliveryOptions, TmuxClient};
pub use health::{HealthChecker, HealthInfo, HealthState};
pub use session::Session;

/// Readiness markers for each supported backend — strings that appear in a
/// backend's startup banner once its TUI has finished drawing and is ready
/// to accept input. Single source of truth used by both the API's
/// `spawn_agent` path and the CLI `manager::spawn_worker` path.
pub fn backend_readiness_markers(backend: &str) -> &'static [&'static str] {
    match backend {
        "codex" => &["OpenAI Codex"],
        "cursor" => &["Cursor Agent"],
        "gemini" => &["Gemini CLI"],
        "claude" => &["Claude Code"],
        "opencode" => &["tab agents", "ctrl+p commands"],
        _ => &[],
    }
}
