//! HTTP API for agent orchestration — all routes EA-scoped

pub mod handlers;
pub mod models;

use axum::{
    routing::{delete, get, post, put},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

use crate::config::ApiConfig;
use handlers::ApiState;

/// Create the API router with path-scoped EA routes
pub fn create_router(state: Arc<ApiState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Global
        .route("/api/health", get(handlers::health))
        .route("/api/backends", get(handlers::list_backends))
        // EA management (global)
        .route("/api/eas", get(handlers::list_eas))
        .route("/api/eas", post(handlers::create_ea))
        .route("/api/eas/active", get(handlers::get_active_ea))
        .route("/api/eas/active", put(handlers::switch_ea))
        .route("/api/eas/:ea_id", delete(handlers::delete_ea))
        // EA-scoped: agents
        .route("/api/ea/:ea_id/agents", get(handlers::list_agents))
        .route("/api/ea/:ea_id/agents", post(handlers::spawn_agent))
        .route("/api/ea/:ea_id/agents/:name", get(handlers::get_agent))
        .route("/api/ea/:ea_id/agents/:name", delete(handlers::kill_agent))
        .route(
            "/api/ea/:ea_id/agents/:name/summary",
            get(handlers::get_agent_summary),
        )
        .route(
            "/api/ea/:ea_id/agents/:name/status",
            put(handlers::update_agent_status),
        )
        .route(
            "/api/ea/:ea_id/agents/:name/send",
            post(handlers::send_input),
        )
        // EA-scoped: projects
        .route("/api/ea/:ea_id/projects", get(handlers::list_projects))
        .route("/api/ea/:ea_id/projects", post(handlers::add_project))
        .route(
            "/api/ea/:ea_id/projects/:id",
            delete(handlers::complete_project),
        )
        // EA-scoped: events
        .route("/api/ea/:ea_id/events", post(handlers::schedule_event))
        .route("/api/ea/:ea_id/events", get(handlers::list_events))
        .route("/api/ea/:ea_id/events/:id", delete(handlers::cancel_event))
        // EA-scoped: meeting rooms
        .route("/api/ea/:ea_id/rooms", post(handlers::create_room))
        .route("/api/ea/:ea_id/rooms", get(handlers::list_rooms))
        .route("/api/ea/:ea_id/rooms/:room", get(handlers::get_room))
        .route("/api/ea/:ea_id/rooms/:room", delete(handlers::close_room))
        .route(
            "/api/ea/:ea_id/rooms/:room/invites",
            post(handlers::create_room_invite),
        )
        .route(
            "/api/ea/:ea_id/rooms/:room/invites",
            get(handlers::list_room_invites),
        )
        .route(
            "/api/ea/:ea_id/rooms/:room/invites/:invite_id/respond",
            post(handlers::respond_room_invite),
        )
        .route(
            "/api/ea/:ea_id/rooms/:room/invites/:invite_id",
            delete(handlers::cancel_room_invite),
        )
        .route(
            "/api/ea/:ea_id/rooms/:room/messages",
            post(handlers::send_room_message),
        )
        .route(
            "/api/ea/:ea_id/rooms/:room/transcript",
            get(handlers::get_room_transcript),
        )
        // Computer use (global — one screen, one mouse)
        .route("/api/computer/status", get(handlers::computer_status))
        .route("/api/computer/lock", post(handlers::computer_lock_acquire))
        .route(
            "/api/computer/lock",
            delete(handlers::computer_lock_release),
        )
        .route(
            "/api/computer/screenshot",
            post(handlers::computer_screenshot),
        )
        .route("/api/computer/mouse", post(handlers::computer_mouse))
        .route("/api/computer/keyboard", post(handlers::computer_keyboard))
        .route(
            "/api/computer/screen-size",
            get(handlers::computer_screen_size),
        )
        .route(
            "/api/computer/mouse-position",
            get(handlers::computer_mouse_position),
        )
        .layer(cors)
        .with_state(state)
}

/// Start the API server
pub async fn start_server(state: Arc<ApiState>, config: &ApiConfig) -> anyhow::Result<()> {
    let router = create_router(state);
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;

    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, router).await?;

    Ok(())
}
