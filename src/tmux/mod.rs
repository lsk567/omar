mod client;
mod health;
mod session;

pub use client::{DeliveryOptions, TmuxClient};
pub use health::{HealthChecker, HealthInfo, HealthState};
pub use session::Session;

/// Readiness markers for each supported backend — strings that must ALL
/// appear in a backend's rendered TUI before the pane is considered ready
/// to accept input. Single source of truth used by both the API's
/// `spawn_agent` path and the CLI `manager::spawn_worker` path.
///
/// For Claude Code specifically we require both the product banner
/// ("Claude Code") AND the input-widget footer ("? for shortcuts"): on
/// v2.1.116 the banner renders several hundred ms before the input widget
/// is actually wired up to accept keystrokes, so matching only on
/// "Claude Code" lets `deliver_prompt` fire Enter into a pane that silently
/// swallows it. The shortcuts footer is the last thing Claude Code draws
/// once the input widget is live, so its presence is a reliable signal
/// that Enter will actually submit.
pub fn backend_readiness_markers(backend: &str) -> &'static [&'static str] {
    match backend {
        "codex" => &["OpenAI Codex"],
        "cursor" => &["Cursor Agent"],
        "gemini" => &["Gemini CLI"],
        "claude" => &["Claude Code", "? for shortcuts"],
        "opencode" => &["tab agents", "ctrl+p commands"],
        _ => &[],
    }
}
