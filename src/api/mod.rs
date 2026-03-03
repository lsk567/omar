//! HTTP API for agent orchestration

pub mod handlers;
pub mod models;

use axum::{
    routing::{delete, get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

use crate::config::ApiConfig;
use handlers::ApiState;

/// Create the API router
pub fn create_router(state: Arc<ApiState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/agents", get(handlers::list_agents))
        .route("/api/agents", post(handlers::spawn_agent))
        .route("/api/agents/:id", get(handlers::get_agent))
        .route("/api/agents/:id", delete(handlers::kill_agent))
        .route("/api/agents/:id/summary", get(handlers::get_agent_summary))
        .route("/api/agents/:id/send", post(handlers::send_input))
        .route("/api/projects", get(handlers::list_projects))
        .route("/api/projects", post(handlers::add_project))
        .route("/api/projects/:id", delete(handlers::complete_project))
        .route("/api/events", post(handlers::schedule_event))
        .route("/api/events", get(handlers::list_events))
        .route("/api/events/:id", delete(handlers::cancel_event))
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
