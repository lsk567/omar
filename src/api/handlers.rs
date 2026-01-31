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
use crate::manager::PM_SYSTEM_PROMPT;
use crate::memory;
use crate::projects;

/// Resolve a user-facing agent name to a full tmux session name.
/// Accepts both short names ("auth") and full names ("omar-agent-auth").
fn resolve_session_name(prefix: &str, id: &str) -> String {
    if prefix.is_empty() || id.starts_with(prefix) {
        id.to_string()
    } else {
        format!("{}{}", prefix, id)
    }
}

/// Strip the session prefix to get the user-facing short name.
fn display_name<'a>(prefix: &str, session_name: &'a str) -> &'a str {
    session_name.strip_prefix(prefix).unwrap_or(session_name)
}

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

    let prefix = app.client().prefix();
    let agents: Vec<AgentInfo> = app
        .agents()
        .iter()
        .map(|a| AgentInfo {
            id: display_name(prefix, &a.session.name).to_string(),
            status: "running".to_string(),
            health: a.health.as_str().to_string(),
            last_output: a.health_info.last_output.clone(),
        })
        .collect();

    let manager = app.manager().map(|m| AgentInfo {
        id: m.session.name.clone(),
        status: "running".to_string(),
        health: m.health.as_str().to_string(),
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

    let prefix = app.client().prefix().to_string();
    let full_id = resolve_session_name(&prefix, &id);

    // Find agent by resolved session name, or manager by raw name
    let agent = app
        .agents()
        .iter()
        .find(|a| a.session.name == full_id)
        .or_else(|| app.manager().filter(|m| m.session.name == id));

    match agent {
        Some(a) => {
            let output_tail = app
                .client()
                .capture_pane(&a.session.name, 50)
                .unwrap_or_default();

            Ok(Json(AgentDetailResponse {
                id: display_name(&prefix, &a.session.name).to_string(),
                status: "running".to_string(),
                health: a.health.as_str().to_string(),
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

    let prefix = app.client().prefix().to_string();

    // Generate full session name: prepend prefix to user-provided names,
    // or auto-generate (which already includes the prefix)
    let name = match req.name {
        Some(n) => resolve_session_name(&prefix, &n),
        None => app.generate_agent_name(),
    };

    // Check if already exists
    if app.client().has_session(&name).unwrap_or(false) {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Agent '{}' already exists", display_name(&prefix, &name)),
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

    // Save parent mapping: explicit parent, or auto-infer from running PMs
    let is_pm = req.role.as_deref() == Some("project-manager");
    if let Some(ref parent) = req.parent {
        let resolved_parent = resolve_session_name(&prefix, parent);
        memory::save_agent_parent(&name, &resolved_parent);
    } else if !is_pm {
        // Auto-infer: non-PM agents are workers. Query tmux directly
        // (app.agents() is stale — only updated on dashboard refresh).
        if let Ok(sessions) = app.client().list_sessions() {
            let pm_sessions: Vec<String> = sessions
                .iter()
                .filter(|s| {
                    s.name
                        .strip_prefix(&prefix)
                        .unwrap_or(&s.name)
                        .starts_with("pm-")
                })
                .map(|s| s.name.clone())
                .collect();
            if pm_sessions.len() == 1 {
                memory::save_agent_parent(&name, &pm_sessions[0]);
            }
            // Multiple PMs: can't auto-infer, will show as Unassigned
        }
    }

    // If a task was provided, send it via tmux send-keys after a delay
    // This works universally with any agent backend (claude, opencode, etc.)
    if let Some(task) = req.task {
        // Always persist the original (short) task for dashboard display
        memory::save_worker_task(&name, &task);

        // Build the full prompt to send: if role is "project-manager",
        // prepend the PM system prompt so the agent knows how to behave.
        // Also inject the PM's own short name so it can pass it as `parent`
        // when spawning workers.
        let full_prompt = if req.role.as_deref() == Some("project-manager") {
            let short = display_name(&prefix, &name);
            format!(
                "{}\n\nYOUR NAME: {}\nYOUR TASK: {}",
                PM_SYSTEM_PROMPT, short, task
            )
        } else {
            task
        };

        let client = app.client().clone();
        let session = name.clone();
        tokio::spawn(async move {
            // Wait for the agent process to start
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = client.send_keys_literal(&session, &full_prompt);
            // Small delay so tmux finishes buffering the text before Enter
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = client.send_keys(&session, "Enter");
        });
    }

    let short = display_name(&prefix, &name).to_string();
    Ok(Json(SpawnAgentResponse {
        id: short,
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

    let prefix = app.client().prefix().to_string();
    let session_name = resolve_session_name(&prefix, &id);

    // Don't allow killing manager via API
    if session_name == MANAGER_SESSION_NAME || id == MANAGER_SESSION_NAME {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Cannot kill manager via API".to_string(),
            }),
        ));
    }

    // Check if exists
    if !app.client().has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", id),
            }),
        ));
    }

    // Kill it
    if let Err(e) = app.client().kill_session(&session_name) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to kill agent: {}", e),
            }),
        ));
    }

    // Clean up parent mapping for the killed agent
    memory::remove_agent_parent(&session_name);

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

    let prefix = app.client().prefix().to_string();
    let session_name = resolve_session_name(&prefix, &id);

    // Check if exists
    if !app.client().has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", id),
            }),
        ));
    }

    // Send text
    if let Err(e) = app.client().send_keys_literal(&session_name, &req.text) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to send input: {}", e),
            }),
        ));
    }

    // Send enter if requested (small delay so tmux finishes buffering the text)
    if req.enter {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Err(e) = app.client().send_keys(&session_name, "Enter") {
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

/// GET /api/projects
pub async fn list_projects() -> Json<ListProjectsResponse> {
    let projects = projects::load_projects();
    let list: Vec<ProjectResponse> = projects
        .iter()
        .map(|p| ProjectResponse {
            id: p.id,
            name: p.name.clone(),
        })
        .collect();
    Json(ListProjectsResponse { projects: list })
}

/// POST /api/projects
pub async fn add_project(
    Json(req): Json<AddProjectRequest>,
) -> Result<Json<ProjectResponse>, (StatusCode, Json<ErrorResponse>)> {
    match projects::add_project(&req.name) {
        Ok(id) => Ok(Json(ProjectResponse { id, name: req.name })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to add project: {}", e),
            }),
        )),
    }
}

/// DELETE /api/projects/:id
pub async fn complete_project(
    Path(id): Path<usize>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    match projects::remove_project(id) {
        Ok(true) => Ok(Json(StatusResponse {
            status: "completed".to_string(),
            message: Some(format!("Project {} removed", id)),
        })),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Project {} not found", id),
            }),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to remove project: {}", e),
            }),
        )),
    }
}
