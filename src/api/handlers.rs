//! API endpoint handlers

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use super::models::*;
use crate::app::{SharedApp, MANAGER_SESSION_NAME};
use crate::manager::{build_agent_command, prompts_dir};
use crate::memory;
use crate::projects;
use crate::scheduler::{event::ScheduledEvent, Scheduler};

/// Shared state for all API handlers
pub struct ApiState {
    pub app: Arc<Mutex<SharedApp>>,
    pub scheduler: Arc<Scheduler>,
}

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
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListAgentsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut app = state.app.lock().await;

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
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<AgentDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = state.app.lock().await;

    let prefix = app.client().prefix().to_string();
    let full_id = resolve_session_name(&prefix, &id);

    // Find agent by resolved session name, or manager by resolved/raw name
    let agent = app
        .agents()
        .iter()
        .find(|a| a.session.name == full_id)
        .or_else(|| {
            app.manager()
                .filter(|m| m.session.name == full_id || m.session.name == id)
        });

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

/// GET /api/agents/:id/summary
pub async fn get_agent_summary(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<AgentSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut app = state.app.lock().await;

    if let Err(e) = app.refresh() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to refresh: {}", e),
            }),
        ));
    }

    let prefix = app.client().prefix().to_string();
    let full_id = resolve_session_name(&prefix, &id);

    // Find agent
    let agent = app
        .agents()
        .iter()
        .find(|a| a.session.name == full_id)
        .or_else(|| {
            app.manager()
                .filter(|m| m.session.name == full_id || m.session.name == id)
        });

    match agent {
        Some(a) => {
            let session = a.session.name.clone();
            let health = a.health.as_str().to_string();

            let tasks = memory::load_worker_tasks();
            let task = tasks.get(&session).cloned();

            let status = memory::load_agent_status(&session);

            let parents = memory::load_agent_parents();
            let children: Vec<String> = parents
                .iter()
                .filter(|(_, parent)| **parent == session)
                .map(|(child, _)| display_name(&prefix, child).to_string())
                .collect();

            Ok(Json(AgentSummaryResponse {
                id: display_name(&prefix, &session).to_string(),
                health,
                task,
                status,
                children,
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
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpawnAgentRequest>,
) -> Result<Json<SpawnAgentResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = state.app.lock().await;

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

    // Build the agent command — with system prompt via native CLI flag if a role is set
    let base_command = req
        .command
        .unwrap_or_else(|| app.default_command().to_string());

    let is_pm = req.role.as_deref() == Some("project-manager");
    let cmd = if is_pm {
        let prompt_file = prompts_dir().join("project-manager.md");
        build_agent_command(&base_command, &prompt_file, &[])
    } else if req.task.is_some() && req.role.as_deref() != Some("project-manager") {
        // Worker with a task — use worker prompt with template substitutions
        let parent = req.parent.as_deref().unwrap_or("ea");
        let task = req.task.as_deref().unwrap_or("");
        let prompt_file = prompts_dir().join("worker.md");
        build_agent_command(
            &base_command,
            &prompt_file,
            &[("{{PARENT_NAME}}", parent), ("{{TASK}}", task)],
        )
    } else {
        base_command.clone()
    };

    // Spawn the agent — system prompt set at process start
    if let Err(e) = app.client().new_session(&name, &cmd, Some(&workdir)) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to spawn agent: {}", e),
            }),
        ));
    }

    // Save parent mapping: explicit parent, or auto-infer from running PMs
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

    // Send first user message after a delay (role-dependent content)
    if let Some(task) = req.task {
        // Always persist the original (short) task for dashboard display
        memory::save_worker_task(&name, &task);

        let user_msg = if is_pm {
            let short = display_name(&prefix, &name);
            format!("YOUR NAME: {}\nYOUR TASK: {}", short, task)
        } else {
            "Start working on your assigned task now.".to_string()
        };

        let client = app.client().clone();
        let session = name.clone();
        tokio::spawn(async move {
            // Wait for the agent process to start
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = client.send_keys_literal(&session, &user_msg);
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
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = state.app.lock().await;

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
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<SendInputRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let app = state.app.lock().await;

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

// ── Event Scheduler handlers ──

/// POST /api/events
pub async fn schedule_event(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScheduleEventRequest>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let event = ScheduledEvent {
        id: id.clone(),
        sender: req.sender,
        receiver: req.receiver,
        timestamp: req.timestamp,
        payload: req.payload,
        created_at: now,
    };

    state.scheduler.insert(event);

    Json(ScheduleEventResponse {
        id,
        timestamp: req.timestamp,
    })
}

/// GET /api/events
pub async fn list_events(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let events = if let Some(receiver) = params.get("receiver") {
        state.scheduler.list_by_receiver(receiver)
    } else {
        state.scheduler.list()
    };

    let events: Vec<EventInfo> = events
        .into_iter()
        .map(|e| EventInfo {
            id: e.id,
            sender: e.sender,
            receiver: e.receiver,
            timestamp: e.timestamp,
            payload: e.payload,
            created_at: e.created_at,
        })
        .collect();

    Json(EventListResponse { events })
}

/// DELETE /api/events/:id
pub async fn cancel_event(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.scheduler.cancel(&id) {
        Some(_) => (
            StatusCode::OK,
            Json(serde_json::json!(EventCancelResponse {
                status: "cancelled".to_string(),
                id,
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!(ErrorResponse {
                error: format!("Event '{}' not found", id),
            })),
        ),
    }
}
