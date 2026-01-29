//! API endpoint handlers

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::models::*;
use crate::app::{SharedApp, MANAGER_SESSION_NAME};

/// GET /api/health
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// GET /api/agents
pub async fn list_agents(
    State(app): State<Arc<Mutex<SharedApp>>>,
) -> Result<Json<ListAgentsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut app = app.lock().await;

    // Refresh to get latest state
    if let Err(e) = app.refresh() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to refresh: {}", e),
            }),
        ));
    }

    let agents: Vec<AgentInfo> = app
        .agents()
        .iter()
        .map(|a| AgentInfo {
            id: a.session.name.clone(),
            status: "running".to_string(),
            health: a.health.as_str().to_string(),
            idle_seconds: a.health_info.idle_seconds,
            last_output: a.health_info.last_output.clone(),
        })
        .collect();

    let manager = app.manager().map(|m| AgentInfo {
        id: m.session.name.clone(),
        status: "running".to_string(),
        health: m.health.as_str().to_string(),
        idle_seconds: m.health_info.idle_seconds,
        last_output: m.health_info.last_output.clone(),
    });

    Ok(Json(ListAgentsResponse { agents, manager }))
}

/// GET /api/agents/:id
pub async fn get_agent(
    State(app): State<Arc<Mutex<SharedApp>>>,
    Path(id): Path<String>,
) -> Result<Json<AgentDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = app.lock().await;

    // Find agent by id
    let agent = app
        .agents()
        .iter()
        .find(|a| a.session.name == id)
        .or_else(|| app.manager().filter(|m| m.session.name == id));

    match agent {
        Some(a) => {
            // Get more output for detail view
            let output_tail = app
                .client()
                .capture_pane(&a.session.name, 50)
                .unwrap_or_default();

            Ok(Json(AgentDetailResponse {
                id: a.session.name.clone(),
                status: "running".to_string(),
                health: a.health.as_str().to_string(),
                idle_seconds: a.health_info.idle_seconds,
                last_output: a.health_info.last_output.clone(),
                output_tail,
            }))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", id),
            }),
        )),
    }
}

/// POST /api/agents
pub async fn spawn_agent(
    State(app): State<Arc<Mutex<SharedApp>>>,
    Json(req): Json<SpawnAgentRequest>,
) -> Result<Json<SpawnAgentResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = app.lock().await;

    // Generate name if not provided
    let name = req.name.unwrap_or_else(|| app.generate_agent_name());

    // Check if already exists
    if app.client().has_session(&name).unwrap_or(false) {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Agent '{}' already exists", name),
            }),
        ));
    }

    // Get workdir
    let workdir = req.workdir.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    // Always start an interactive session with the base command
    let base_command = req
        .command
        .unwrap_or_else(|| app.default_command().to_string());

    // Spawn the agent with the base command (no task appended)
    if let Err(e) = app
        .client()
        .new_session(&name, &base_command, Some(&workdir))
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to spawn agent: {}", e),
            }),
        ));
    }

    // If a task was provided, send it via tmux send-keys after a delay
    // This works universally with any agent backend (claude, opencode, etc.)
    if let Some(task) = req.task {
        let client = app.client().clone();
        let session = name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = client.send_keys_literal(&session, &task);
            let _ = client.send_keys(&session, "Enter");
        });
    }

    Ok(Json(SpawnAgentResponse {
        id: name.clone(),
        status: "running".to_string(),
        session: name,
    }))
}

/// DELETE /api/agents/:id
pub async fn kill_agent(
    State(app): State<Arc<Mutex<SharedApp>>>,
    Path(id): Path<String>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = app.lock().await;

    // Don't allow killing manager via API
    if id == MANAGER_SESSION_NAME {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Cannot kill manager via API".to_string(),
            }),
        ));
    }

    // Check if exists
    if !app.client().has_session(&id).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", id),
            }),
        ));
    }

    // Kill it
    if let Err(e) = app.client().kill_session(&id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to kill agent: {}", e),
            }),
        ));
    }

    Ok(Json(StatusResponse {
        status: "killed".to_string(),
        message: Some(format!("Agent '{}' killed", id)),
    }))
}

/// POST /api/agents/:id/send
pub async fn send_input(
    State(app): State<Arc<Mutex<SharedApp>>>,
    Path(id): Path<String>,
    Json(req): Json<SendInputRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = app.lock().await;

    // Check if exists
    if !app.client().has_session(&id).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", id),
            }),
        ));
    }

    // Send text
    if let Err(e) = app.client().send_keys_literal(&id, &req.text) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to send input: {}", e),
            }),
        ));
    }

    // Send enter if requested
    if req.enter {
        if let Err(e) = app.client().send_keys(&id, "Enter") {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to send Enter: {}", e),
                }),
            ));
        }
    }

    Ok(Json(StatusResponse {
        status: "sent".to_string(),
        message: None,
    }))
}
