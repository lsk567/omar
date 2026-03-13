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
use crate::computer::{self, ComputerLock};
use crate::manager::{build_agent_command, prompts_dir};
use crate::memory;
use crate::projects;
use crate::scheduler::{event::ScheduledEvent, Scheduler};

/// Shared state for all API handlers
pub struct ApiState {
    pub app: Arc<Mutex<SharedApp>>,
    pub scheduler: Arc<Scheduler>,
    pub computer_lock: ComputerLock,
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

    // Unified agent role: "project-manager" and "agent" both use agent.md prompt.
    // Legacy "project-manager" role is treated as an alias for "agent".
    let has_agent_prompt = matches!(req.role.as_deref(), Some("project-manager") | Some("agent"))
        || req.task.is_some();
    let cmd = if has_agent_prompt {
        // Any agent with a role or task gets the unified agent prompt
        let parent = req.parent.as_deref().unwrap_or("ea");
        let task = req.task.as_deref().unwrap_or("");
        let prompt_file = prompts_dir().join("agent.md");
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

    // Save parent mapping if explicitly provided
    if let Some(ref parent) = req.parent {
        let resolved_parent = resolve_session_name(&prefix, parent);
        memory::save_agent_parent(&name, &resolved_parent);
    }

    // Send first user message after a delay
    if let Some(task) = req.task {
        // Always persist the original (short) task for dashboard display
        memory::save_worker_task(&name, &task);

        let short = display_name(&prefix, &name);
        let user_msg = format!("YOUR NAME: {}\nYOUR TASK: {}", short, task);

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

    // Clean up parent mapping and cancel pending events for the killed agent
    memory::remove_agent_parent(&session_name);
    let short_name = display_name(&prefix, &session_name).to_string();
    state.scheduler.cancel_by_receiver(&short_name);

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
        recurring_ns: req.recurring_ns,
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
            recurring_ns: e.recurring_ns,
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

// ── Computer Use handlers ──

/// Helper: verify the agent holds the computer lock.
async fn verify_computer_lock(
    lock: &ComputerLock,
    agent: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let guard = lock.lock().await;
    match guard.as_deref() {
        Some(holder) if holder == agent => Ok(()),
        Some(holder) => Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!(
                    "Computer is locked by '{}'. Release it first or wait.",
                    holder
                ),
            }),
        )),
        None => Err((
            StatusCode::PRECONDITION_FAILED,
            Json(ErrorResponse {
                error: "You must acquire the computer lock first (POST /api/computer/lock)"
                    .to_string(),
            }),
        )),
    }
}

/// GET /api/computer/status
/// Check computer use availability and current lock status.
pub async fn computer_status(
    State(_state): State<Arc<ApiState>>,
) -> Json<ComputerAvailabilityResponse> {
    let xdotool = computer::is_available();
    let screenshot = computer::is_screenshot_available();
    let screen_size = computer::get_screen_size()
        .ok()
        .map(|s| ScreenSizeResponse {
            width: s.width,
            height: s.height,
        });

    Json(ComputerAvailabilityResponse {
        available: xdotool && screenshot,
        xdotool,
        screenshot,
        screen_size,
    })
}

/// POST /api/computer/lock
/// Acquire exclusive access to the computer.
pub async fn computer_lock_acquire(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ComputerLockRequest>,
) -> Result<Json<ComputerLockResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.computer_lock.lock().await;

    if let Some(ref holder) = *guard {
        if holder == &req.agent {
            // Already holds the lock — idempotent
            return Ok(Json(ComputerLockResponse {
                status: "already_held".to_string(),
                held_by: Some(req.agent),
            }));
        }
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Computer is locked by '{}'", holder),
            }),
        ));
    }

    *guard = Some(req.agent.clone());
    Ok(Json(ComputerLockResponse {
        status: "acquired".to_string(),
        held_by: Some(req.agent),
    }))
}

/// DELETE /api/computer/lock
/// Release the computer lock.
pub async fn computer_lock_release(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ComputerLockRequest>,
) -> Result<Json<ComputerLockResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.computer_lock.lock().await;

    match guard.as_deref() {
        Some(holder) if holder == req.agent => {
            *guard = None;
            Ok(Json(ComputerLockResponse {
                status: "released".to_string(),
                held_by: None,
            }))
        }
        Some(holder) => Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: format!("Lock held by '{}', not '{}'", holder, req.agent),
            }),
        )),
        None => Ok(Json(ComputerLockResponse {
            status: "not_held".to_string(),
            held_by: None,
        })),
    }
}

/// POST /api/computer/screenshot
/// Take a screenshot (must hold the lock).
pub async fn computer_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScreenshotRequest>,
) -> Result<Json<ScreenshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent).await?;

    let result = if let (Some(w), Some(h)) = (req.max_width, req.max_height) {
        computer::take_screenshot_resized(w, h)
    } else {
        computer::take_screenshot()
    };

    match result {
        Ok(image_base64) => {
            let size = computer::get_screen_size().unwrap_or(computer::ScreenSize {
                width: 0,
                height: 0,
            });
            Ok(Json(ScreenshotResponse {
                image_base64,
                width: size.width,
                height: size.height,
                format: "png".to_string(),
            }))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Screenshot failed: {}", e),
            }),
        )),
    }
}

/// POST /api/computer/mouse
/// Perform a mouse action (must hold the lock).
pub async fn computer_mouse(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<MouseRequest>,
) -> Result<Json<ComputerActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent).await?;

    let result = match req.action.as_str() {
        "move" => computer::mouse_move(req.x, req.y),
        "click" => computer::mouse_click(req.x, req.y, req.button),
        "double_click" => computer::mouse_double_click(req.x, req.y, req.button),
        "drag" => {
            let to_x = req.to_x.ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "drag requires to_x".to_string(),
                    }),
                )
            })?;
            let to_y = req.to_y.ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "drag requires to_y".to_string(),
                    }),
                )
            })?;
            computer::mouse_drag(req.x, req.y, to_x, to_y, req.button)
        }
        "scroll" => {
            let dir = req.scroll_direction.as_deref().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "scroll requires scroll_direction".to_string(),
                    }),
                )
            })?;
            computer::mouse_scroll(req.x, req.y, dir, req.scroll_amount)
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!(
                        "Unknown mouse action: '{}'. Use move/click/double_click/drag/scroll",
                        other
                    ),
                }),
            ));
        }
    };

    match result {
        Ok(()) => Ok(Json(ComputerActionResponse {
            status: "ok".to_string(),
            message: Some(format!("mouse {} at ({}, {})", req.action, req.x, req.y)),
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Mouse action failed: {}", e),
            }),
        )),
    }
}

/// POST /api/computer/keyboard
/// Perform a keyboard action (must hold the lock).
pub async fn computer_keyboard(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<KeyboardRequest>,
) -> Result<Json<ComputerActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent).await?;

    let result = match req.action.as_str() {
        "type" => computer::type_text(&req.text),
        "key" => computer::key_press(&req.text),
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Unknown keyboard action: '{}'. Use type/key", other),
                }),
            ));
        }
    };

    match result {
        Ok(()) => Ok(Json(ComputerActionResponse {
            status: "ok".to_string(),
            message: Some(format!("keyboard {}", req.action)),
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Keyboard action failed: {}", e),
            }),
        )),
    }
}

/// GET /api/computer/screen-size
/// Get screen dimensions (no lock required).
pub async fn computer_screen_size(
) -> Result<Json<ScreenSizeResponse>, (StatusCode, Json<ErrorResponse>)> {
    match computer::get_screen_size() {
        Ok(size) => Ok(Json(ScreenSizeResponse {
            width: size.width,
            height: size.height,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to get screen size: {}", e),
            }),
        )),
    }
}

/// GET /api/computer/mouse-position
/// Get current mouse position (no lock required).
pub async fn computer_mouse_position(
) -> Result<Json<MousePositionResponse>, (StatusCode, Json<ErrorResponse>)> {
    match computer::get_mouse_position() {
        Ok((x, y)) => Ok(Json(MousePositionResponse { x, y })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to get mouse position: {}", e),
            }),
        )),
    }
}
