//! API endpoint handlers — all EA-scoped via path parameter

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use super::models::*;
use crate::app::App;
use crate::computer::{self, ComputerLock};
use crate::ea;
use crate::manager::{build_agent_command, prompts_dir};
use crate::memory;
use crate::projects;
use crate::rooms::{InviteDecision, RoomError, RoomRegistry};
use crate::scheduler::{event::ScheduledEvent, Scheduler};
use crate::tmux::TmuxClient;

/// Shared state for all API handlers
pub struct ApiState {
    pub app: Arc<Mutex<App>>,
    pub scheduler: Arc<Scheduler>,
    pub rooms: Arc<RoomRegistry>,
    pub computer_lock: ComputerLock,
    pub base_prefix: String,
    pub omar_dir: PathBuf,
    pub health_idle_warning: i64,
    /// Serializes the has_session → new_session sequence to prevent concurrent
    /// spawns from racing past the collision check (TOCTOU race condition fix).
    pub spawn_lock: Arc<Mutex<()>>,
}

/// Validate EA exists, returning prefix, manager session, and state_dir.
fn resolve_ea(
    ea_id: u32,
    state: &ApiState,
) -> Result<(String, String, PathBuf), (StatusCode, Json<ErrorResponse>)> {
    let registry = ea::load_registry(&state.omar_dir);
    if !registry.iter().any(|e| e.id == ea_id) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("EA {} not found", ea_id),
            }),
        ));
    }
    let prefix = ea::ea_prefix(ea_id, &state.base_prefix);
    let manager = ea::ea_manager_session(ea_id, &state.base_prefix);
    let state_dir = ea::ea_state_dir(ea_id, &state.omar_dir);
    Ok((prefix, manager, state_dir))
}

/// Strip the session prefix to get the user-facing short name.
fn display_name<'a>(prefix: &str, session_name: &'a str) -> &'a str {
    session_name.strip_prefix(prefix).unwrap_or(session_name)
}

fn last_output_line(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect()
}

fn health_from_activity(activity: i64, idle_warning: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs() as i64;
    if now.saturating_sub(activity) <= idle_warning {
        "running".to_string()
    } else {
        "idle".to_string()
    }
}

fn timestamp_is_too_old(timestamp: u64, now: u64) -> bool {
    timestamp < now.saturating_sub(1_000_000_000)
}

fn room_error_response(err: RoomError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match err {
        RoomError::NotFound(_) => StatusCode::NOT_FOUND,
        RoomError::AlreadyExists(_) => StatusCode::CONFLICT,
        RoomError::Forbidden(_) => StatusCode::FORBIDDEN,
        RoomError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
    };
    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

fn normalize_agent_name(prefix: &str, name: &str) -> String {
    name.strip_prefix(prefix).unwrap_or(name).to_string()
}

fn ensure_agent_exists(prefix: &str, name: &str) -> bool {
    let client = TmuxClient::new(prefix);
    let session = if name.starts_with(prefix) {
        name.to_string()
    } else {
        format!("{}{}", prefix, name)
    };
    client.has_session(&session).unwrap_or(false)
}

// ── Global handlers ──

/// GET /api/health
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// GET /api/backends
/// Returns which agent backends are installed and available on the system.
pub async fn list_backends() -> Json<BackendsResponse> {
    let infos = tokio::task::spawn_blocking(|| {
        use std::process::Command;

        let backends = ["claude", "codex", "cursor", "gemini", "opencode"];
        backends
            .iter()
            .filter_map(|&name| {
                let resolved = crate::config::resolve_backend(name).ok()?;
                let executable = resolved.split_whitespace().next().unwrap_or(name);
                let available = Command::new(executable)
                    .arg("--version")
                    .output()
                    .is_ok_and(|output| output.status.success());
                Some(BackendInfo {
                    name: name.to_string(),
                    available,
                    command: resolved,
                })
            })
            .collect()
    })
    .await
    .unwrap_or_default();

    Json(BackendsResponse { backends: infos })
}

/// GET /api/eas
pub async fn list_eas(State(state): State<Arc<ApiState>>) -> Json<ListEasResponse> {
    let app = state.app.lock().await;
    let active = app.active_ea;
    let registry = ea::ensure_default_ea(&state.omar_dir)
        .unwrap_or_else(|_| ea::load_registry(&state.omar_dir));

    let eas = registry
        .iter()
        .map(|ea_info| {
            let prefix = ea::ea_prefix(ea_info.id, &state.base_prefix);
            let client = TmuxClient::new(&prefix);
            let agent_count = client.list_sessions().unwrap_or_default().len();
            EaResponse {
                id: ea_info.id,
                name: ea_info.name.clone(),
                description: ea_info.description.clone(),
                agent_count,
                is_active: ea_info.id == active,
            }
        })
        .collect();

    Json(ListEasResponse { eas, active })
}

/// POST /api/eas
/// Fix S2: Hold App lock across register_ea to serialize concurrent EA creation.
/// Without this, two concurrent create_ea calls could read-modify-write eas.json
/// simultaneously, producing duplicate EA IDs or losing one EA's registration.
pub async fn create_ea(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateEaRequest>,
) -> Result<Json<EaResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Validate name before acquiring the lock to fail fast on bad input
    ea::validate_ea_name(&req.name).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: format!("Invalid EA name: {}", e),
            }),
        )
    })?;

    // Acquire lock BEFORE register_ea to serialize concurrent creation
    let mut app = state.app.lock().await;

    let ea_id =
        ea::register_ea(&state.omar_dir, &req.name, req.description.as_deref()).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Failed to create EA: {}", e),
                }),
            )
        })?;

    // Update app's registry (still under lock)
    app.registered_eas = ea::load_registry(&state.omar_dir);

    Ok(Json(EaResponse {
        id: ea_id,
        name: req.name,
        description: req.description,
        agent_count: 0,
        is_active: false,
    }))
}

/// DELETE /api/eas/:ea_id
pub async fn delete_ea(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<DeleteEaResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Acquire App lock before any mutation to prevent races with the dashboard.
    let mut app = state.app.lock().await;

    // Step 0: Cannot delete the last EA
    {
        let registry = ea::load_registry(&state.omar_dir);
        if registry.len() <= 1 {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "Cannot delete the only EA".to_string(),
                }),
            ));
        }
    }

    // Step 1: Validate EA exists and inspect current state before mutating registry.
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;
    let client = TmuxClient::new(&prefix);
    let sessions = client.list_sessions().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to inspect EA sessions: {}", e),
            }),
        )
    })?;

    // Step 2: Transactional cleanup. Do not unregister until cleanup succeeds.
    let mut agents_killed = 0;
    for session in &sessions {
        if session.name != manager_session {
            client.kill_session(&session.name).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: format!("Failed to kill worker session '{}': {}", session.name, e),
                    }),
                )
            })?;
            agents_killed += 1;
        }
    }

    if client.has_session(&manager_session).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!(
                    "Failed to check manager session '{}': {}",
                    manager_session, e
                ),
            }),
        )
    })? {
        client.kill_session(&manager_session).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!(
                        "Failed to kill manager session '{}': {}",
                        manager_session, e
                    ),
                }),
            )
        })?;
        agents_killed += 1;
    }

    if state_dir.exists() {
        std::fs::remove_dir_all(&state_dir).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to remove state dir {:?}: {}", state_dir, e),
                }),
            )
        })?;
    }

    let notes_path = memory::manager_notes_path(&state.omar_dir, ea_id);
    if notes_path.exists() {
        std::fs::remove_file(&notes_path).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to remove notes {:?}: {}", notes_path, e),
                }),
            )
        })?;
    }

    // Step 3: Commit registry/scheduler changes after cleanup succeeds.
    let events_cancelled = state.scheduler.cancel_by_ea(ea_id);
    ea::unregister_ea(&state.omar_dir, ea_id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to unregister EA: {}", e),
            }),
        )
    })?;

    app.registered_eas = ea::load_registry(&state.omar_dir);
    if app.active_ea == ea_id {
        let next_id = app
            .registered_eas
            .iter()
            .map(|e| e.id)
            .filter(|id| *id != ea_id)
            .min()
            .unwrap_or(0);
        let _ = app.switch_ea(next_id);
    }

    Ok(Json(DeleteEaResponse {
        deleted_ea: ea_id,
        agents_killed,
        events_cancelled,
    }))
}

/// GET /api/eas/active
pub async fn get_active_ea(State(state): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    let app = state.app.lock().await;
    Json(serde_json::json!({ "active": app.active_ea }))
}

/// PUT /api/eas/active
pub async fn switch_ea(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SwitchEaRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut app = state.app.lock().await;
    let registry = ea::load_registry(&state.omar_dir);
    if !registry.iter().any(|e| e.id == req.id) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("EA {} not found", req.id),
            }),
        ));
    }
    if let Err(e) = app.switch_ea(req.id) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to switch EA: {}", e),
            }),
        ));
    }

    Ok(Json(StatusResponse {
        status: "switched".to_string(),
        message: Some(format!("Switched to EA {}", req.id)),
    }))
}

// ── EA-scoped agent handlers ──

/// GET /api/ea/:ea_id/agents
pub async fn list_agents(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListAgentsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, _state_dir) = resolve_ea(ea_id, &state)?;
    let client = TmuxClient::new(&prefix);

    let sessions = client.list_sessions().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to list sessions: {}", e),
            }),
        )
    })?;

    let agents: Vec<AgentInfo> = sessions
        .iter()
        .filter(|s| s.name != manager_session)
        .map(|s| AgentInfo {
            id: display_name(&prefix, &s.name).to_string(),
            status: "running".to_string(),
            health: health_from_activity(s.activity, state.health_idle_warning),
            last_output: last_output_line(&client.capture_pane(&s.name, 50).unwrap_or_default()),
            auth_failure: false,
        })
        .collect();

    let manager = if client.has_session(&manager_session).unwrap_or(false) {
        let output_tail = client
            .capture_pane(&manager_session, 50)
            .unwrap_or_default();
        let activity = client
            .get_pane_activity(&manager_session)
            .unwrap_or_default();
        Some(AgentInfo {
            id: manager_session.clone(),
            status: "running".to_string(),
            health: health_from_activity(activity, state.health_idle_warning),
            last_output: last_output_line(&output_tail),
            auth_failure: false,
        })
    } else {
        None
    };

    Ok(Json(ListAgentsResponse { agents, manager }))
}

/// GET /api/ea/:ea_id/agents/:name
pub async fn get_agent(
    Path((ea_id, name)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<AgentDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, _state_dir) = resolve_ea(ea_id, &state)?;

    let session_name = if name == manager_session || name.starts_with(&prefix) {
        name.clone()
    } else {
        format!("{}{}", prefix, name)
    };

    let client = TmuxClient::new(&prefix);
    let output_tail = match client.capture_pane(&session_name, 200) {
        Ok(s) => s,
        Err(_) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Agent '{}' not found", name),
                }),
            ))
        }
    };

    Ok(Json(AgentDetailResponse {
        id: display_name(&prefix, &session_name).to_string(),
        status: "running".to_string(),
        health: health_from_activity(
            client.get_pane_activity(&session_name).unwrap_or_default(),
            state.health_idle_warning,
        ),
        last_output: last_output_line(&output_tail),
        output_tail,
        auth_failure: false,
    }))
}

/// GET /api/ea/:ea_id/agents/:name/summary
pub async fn get_agent_summary(
    Path((ea_id, name)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<AgentSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    let session_name = if name == manager_session || name.starts_with(&prefix) {
        name.clone()
    } else {
        format!("{}{}", prefix, name)
    };

    let client = TmuxClient::new(&prefix);
    if !client.has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", name),
            }),
        ));
    }

    let tasks = memory::load_worker_tasks_from(&state_dir);
    let task = tasks.get(&session_name).cloned();

    // Read status from the in-memory cache (populated by refresh()) when the
    // requested EA is the active dashboard EA; fall back to disk for other EAs.
    let status = {
        let app = state.app.lock().await;
        if app.active_ea == ea_id {
            app.agent_status(&session_name).cloned()
        } else {
            memory::load_agent_status_in(&state_dir, &session_name)
        }
    };

    let parents = memory::load_agent_parents_from(&state_dir);
    let children: Vec<String> = parents
        .iter()
        .filter(|(_, parent)| **parent == session_name)
        .map(|(child, _)| display_name(&prefix, child).to_string())
        .collect();

    Ok(Json(AgentSummaryResponse {
        id: display_name(&prefix, &session_name).to_string(),
        health: health_from_activity(
            client.get_pane_activity(&session_name).unwrap_or_default(),
            state.health_idle_warning,
        ),
        task,
        status,
        children,
    }))
}

/// PUT /api/ea/:ea_id/agents/:name/status
pub async fn update_agent_status(
    Path((ea_id, name)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<UpdateStatusRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    let session_name = if name.starts_with(&prefix) {
        name.clone()
    } else {
        format!("{}{}", prefix, name)
    };

    let client = TmuxClient::new(&prefix);
    if !client.has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", name),
            }),
        ));
    }

    memory::save_agent_status_in(&state_dir, &session_name, &req.status);

    // Write through to the in-memory cache so the dashboard reads the latest
    // status without waiting for the next refresh() cycle.
    {
        let mut app = state.app.lock().await;
        if app.active_ea == ea_id {
            app.set_agent_status(session_name.clone(), req.status.clone());
        }
    }

    Ok(Json(StatusResponse {
        status: "updated".to_string(),
        message: Some(format!("Status updated for '{}'", name)),
    }))
}

/// POST /api/ea/:ea_id/agents
pub async fn spawn_agent(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpawnAgentRequest>,
) -> Result<Json<SpawnAgentResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Note: resolve_ea validates the EA at request time. A concurrent delete_ea
    // could remove the EA between here and the tmux calls, but any resulting
    // filesystem/tmux errors are caught and returned as 500 responses downstream.
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    // Generate full session name.
    // If the request contains a non-empty `name`, that name is used as the
    // agent ID (with the EA prefix prepended once).  An absent or empty name
    // falls back to auto-generation.
    let session_name = match req.name.as_deref() {
        Some(n) if !n.trim().is_empty() => {
            let stripped = n.strip_prefix(&prefix).unwrap_or(n);
            format!("{}{}", prefix, stripped)
        }
        _ => generate_agent_name_in_ea(&prefix),
    };

    // Short name (for prompts and events)
    let short_name = session_name
        .strip_prefix(&prefix)
        .unwrap_or(&session_name)
        .to_string();

    // Parent resolution within this EA's namespace
    let parent_session = if let Some(ref parent) = req.parent {
        if parent == "ea" {
            manager_session.clone()
        } else {
            let stripped = parent.strip_prefix(&prefix).unwrap_or(parent);
            format!("{}{}", prefix, stripped)
        }
    } else {
        manager_session.clone()
    };

    let client = TmuxClient::new(&prefix);
    let prompt_parent_name = if parent_session == manager_session {
        "ea".to_string()
    } else {
        display_name(&prefix, &parent_session).to_string()
    };

    if req.backend.is_some() && req.command.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Cannot specify both 'backend' and 'command'. Use one or the other."
                    .to_string(),
            }),
        ));
    }

    // Read default_command from App (brief lock, released before blocking I/O).
    let default_command = {
        let app = state.app.lock().await;
        app.default_command().to_string()
    };

    let mut base_command = if let Some(ref backend) = req.backend {
        crate::config::resolve_backend(backend)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?
    } else {
        req.command.clone().unwrap_or(default_command)
    };

    if let Some(ref model) = req.model {
        if !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid model name. Only alphanumeric, '-', '_', '.', '/' allowed."
                        .to_string(),
                }),
            ));
        }
        base_command = format!("{} --model {}", base_command, model);
    }

    // Get workdir
    let workdir = req.workdir.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    // Build the agent command (App lock is no longer held)
    let has_agent_prompt = matches!(req.role.as_deref(), Some("project-manager") | Some("agent"))
        || req.task.is_some();
    let cmd = if has_agent_prompt {
        let task = req.task.as_deref().unwrap_or("");
        let prompt_file = prompts_dir(&state.omar_dir).join("agent.md");
        build_agent_command(
            &base_command,
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", &prompt_parent_name),
                ("{{TASK}}", task),
                ("{{EA_ID}}", &ea_id.to_string()),
            ],
        )
    } else {
        base_command.clone()
    };

    // Ensure state directory exists
    std::fs::create_dir_all(&state_dir).ok();

    // Acquire the spawn lock to make the has_session → new_session sequence
    // atomic, preventing parallel spawns from racing past the collision check
    // (TOCTOU fix for BUG C).
    let _spawn_guard = state.spawn_lock.lock().await;

    // Collision check under spawn lock
    if client.has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Agent '{}' already exists", short_name),
            }),
        ));
    }

    // Spawn the agent (blocking tmux call — spawn lock IS held, App lock is NOT)
    if let Err(e) = client.new_session(&session_name, &cmd, Some(&workdir)) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to spawn agent: {}", e),
            }),
        ));
    }

    // Release spawn lock — session now exists; subsequent requests will see it.
    drop(_spawn_guard);

    // Save parent mapping only after spawn succeeds to avoid orphaned entries
    memory::save_agent_parent_in(&state_dir, &session_name, &parent_session);

    // Send first user message after a delay
    if let Some(ref task) = req.task {
        memory::save_worker_task_in(&state_dir, &session_name, task);

        let user_msg = format!("YOUR NAME: {}\nYOUR TASK: {}", short_name, task);
        let client2 = client.clone();
        let session2 = session_name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = client2.send_keys_literal(&session2, &user_msg);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = client2.send_keys(&session2, "Enter");
        });
    }

    // Schedule a recurring status check — ea_id is structural
    if has_agent_prompt {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let interval: u64 = 60_000_000_000; // 60 seconds
        let event = ScheduledEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sender: "omar".to_string(),
            receiver: short_name.clone(),
            timestamp: now + interval,
            payload: format!(
                "[STATUS CHECK] Update your status via the API: curl -X PUT http://localhost:9876/api/ea/{}/agents/<YOUR NAME>/status -H 'Content-Type: application/json' -d '{{\"status\": \"<1-line status>\"}}'",
                ea_id
            ),
            created_at: now,
            recurring_ns: Some(interval),
            ea_id,
        };
        state.scheduler.insert(event);
    }

    Ok(Json(SpawnAgentResponse {
        id: short_name,
        status: "running".to_string(),
        session: session_name,
    }))
}

/// DELETE /api/ea/:ea_id/agents/:name
pub async fn kill_agent(
    Path((ea_id, name)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    let session_name = if name.starts_with(&prefix) {
        name.clone()
    } else {
        format!("{}{}", prefix, name)
    };

    // Don't allow killing manager via API
    if session_name == manager_session {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "Cannot kill manager via API".to_string(),
            }),
        ));
    }

    let client = TmuxClient::new(&prefix);
    if !client.has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", name),
            }),
        ));
    }

    if let Err(e) = client.kill_session(&session_name) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to kill agent: {}", e),
            }),
        ));
    }

    // Clean up parent mapping and cancel pending events (EA-scoped, fix V5)
    memory::remove_agent_parent_in(&state_dir, &session_name);
    let short_name = display_name(&prefix, &session_name).to_string();
    state
        .scheduler
        .cancel_by_receiver_and_ea(&short_name, ea_id);

    Ok(Json(StatusResponse {
        status: "killed".to_string(),
        message: Some(format!("Agent '{}' killed", name)),
    }))
}

/// POST /api/ea/:ea_id/agents/:name/send
pub async fn send_input(
    Path((ea_id, name)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SendInputRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager_session, _state_dir) = resolve_ea(ea_id, &state)?;

    let session_name = if name.starts_with(&prefix) {
        name.clone()
    } else {
        format!("{}{}", prefix, name)
    };

    let client = TmuxClient::new(&prefix);
    if !client.has_session(&session_name).unwrap_or(false) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Agent '{}' not found", name),
            }),
        ));
    }

    if let Err(e) = client.send_keys_literal(&session_name, &req.text) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to send input: {}", e),
            }),
        ));
    }

    if req.enter {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Err(e) = client.send_keys(&session_name, "Enter") {
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

// ── EA-scoped project handlers ──

/// GET /api/ea/:ea_id/projects
pub async fn list_projects(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListProjectsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    let project_list = projects::load_projects_from(&state_dir);
    let list: Vec<ProjectResponse> = project_list
        .iter()
        .map(|p| ProjectResponse {
            id: p.id,
            name: p.name.clone(),
        })
        .collect();
    Ok(Json(ListProjectsResponse { projects: list }))
}

/// POST /api/ea/:ea_id/projects
pub async fn add_project(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<AddProjectRequest>,
) -> Result<Json<ProjectResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    match projects::add_project_in(&state_dir, &req.name) {
        Ok(id) => Ok(Json(ProjectResponse { id, name: req.name })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to add project: {}", e),
            }),
        )),
    }
}

/// DELETE /api/ea/:ea_id/projects/:id
pub async fn complete_project(
    Path((ea_id, id)): Path<(u32, usize)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    match projects::remove_project_in(&state_dir, id) {
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

// ── EA-scoped event handlers ──

/// POST /api/ea/:ea_id/events
pub async fn schedule_event(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScheduleEventRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let _ = resolve_ea(ea_id, &state)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_nanos() as u64;

    // Reject timestamps more than 1 second in the past to prevent event spam.
    if timestamp_is_too_old(req.timestamp, now) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "timestamp is more than 1 second in the past".to_string(),
            }),
        ));
    }

    // Reject recurring_ns < 1 second to prevent CPU DoS from tight infinite loops.
    if let Some(recurring_ns) = req.recurring_ns {
        if recurring_ns < 1_000_000_000 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "recurring_ns must be at least 1_000_000_000 (1 second)".to_string(),
                }),
            ));
        }
    }

    let event = ScheduledEvent {
        id: uuid::Uuid::new_v4().to_string(),
        sender: req.sender,
        receiver: req.receiver,
        timestamp: req.timestamp,
        payload: req.payload,
        created_at: now,
        recurring_ns: req.recurring_ns,
        ea_id,
    };

    state.scheduler.insert(event.clone());

    Ok(Json(ScheduleEventResponse {
        id: event.id,
        timestamp: req.timestamp,
        ea_id,
    }))
}

/// GET /api/ea/:ea_id/events
pub async fn list_events(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let _ = resolve_ea(ea_id, &state)?;

    let events = if let Some(receiver) = params.get("receiver") {
        // Filter by both EA and receiver
        state
            .scheduler
            .list_by_ea(ea_id)
            .into_iter()
            .filter(|e| e.receiver == *receiver)
            .collect()
    } else {
        state.scheduler.list_by_ea(ea_id)
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

    Ok(Json(EventListResponse { events }))
}

/// DELETE /api/ea/:ea_id/events/:id
/// Fix V4 + Fix S1: Atomic EA-scoped event cancellation.
/// Uses cancel_if_ea to avoid TOCTOU window where cancel + re-insert
/// briefly removes the event from the queue.
pub async fn cancel_event(
    Path((ea_id, id)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let _ = resolve_ea(ea_id, &state)?;

    match state.scheduler.cancel_if_ea(&id, ea_id) {
        Ok(_event) => Ok((
            StatusCode::OK,
            Json(serde_json::json!(EventCancelResponse {
                status: "cancelled".to_string(),
                id,
            })),
        )),
        Err(true) => {
            // Event exists but belongs to a different EA
            Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Event '{}' not found in EA {}", id, ea_id),
                }),
            ))
        }
        Err(false) => {
            // Event not found at all
            Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Event '{}' not found", id),
                }),
            ))
        }
    }
}

// ── EA-scoped meeting room handlers ──

/// POST /api/ea/:ea_id/rooms
pub async fn create_room(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateRoomRequest>,
) -> Result<Json<RoomSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let created_by = normalize_agent_name(&prefix, &req.created_by);
    let room = state
        .rooms
        .create_room(ea_id, req.name.trim(), created_by.trim())
        .map_err(room_error_response)?;
    Ok(Json(RoomSummaryResponse {
        name: room.name,
        created_by: room.created_by,
        participant_count: room.participant_count,
        message_count: room.message_count,
        last_activity_at: room.last_activity_at,
    }))
}

/// GET /api/ea/:ea_id/rooms
pub async fn list_rooms(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListRoomsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let rooms = state
        .rooms
        .list_rooms(ea_id)
        .into_iter()
        .map(|r| RoomSummaryResponse {
            name: r.name,
            created_by: r.created_by,
            participant_count: r.participant_count,
            message_count: r.message_count,
            last_activity_at: r.last_activity_at,
        })
        .collect();
    Ok(Json(ListRoomsResponse { rooms }))
}

/// GET /api/ea/:ea_id/rooms/:room
pub async fn get_room(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<RoomDetailResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let snapshot = state.rooms.get_room(ea_id, &room).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Room '{}' not found", room),
            }),
        )
    })?;
    Ok(Json(RoomDetailResponse {
        name: snapshot.name,
        created_by: snapshot.created_by,
        participants: snapshot.participants,
        created_at: snapshot.created_at,
        last_activity_at: snapshot.last_activity_at,
    }))
}

/// DELETE /api/ea/:ea_id/rooms/:room
pub async fn close_room(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    state
        .rooms
        .close_room(ea_id, &room, "manual close")
        .map_err(room_error_response)?;
    Ok(Json(StatusResponse {
        status: "closed".to_string(),
        message: Some(format!("Closed room '{}'", room)),
    }))
}

/// POST /api/ea/:ea_id/rooms/:room/invites
pub async fn create_room_invite(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateInviteRequest>,
) -> Result<Json<InviteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let invited_agent = normalize_agent_name(&prefix, &req.invited_agent);
    let invited_by = normalize_agent_name(&prefix, &req.invited_by);

    if !ensure_agent_exists(&prefix, &invited_agent) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: format!("Invited agent '{}' does not exist", invited_agent),
            }),
        ));
    }

    let invite = state
        .rooms
        .create_invite(
            ea_id,
            &room,
            &invited_by,
            &invited_agent,
            req.message,
            req.expires_at,
        )
        .map_err(room_error_response)?;

    Ok(Json(InviteResponse {
        id: invite.id,
        invited_agent: invite.invited_agent,
        invited_by: invite.invited_by,
        message: invite.message,
        created_at: invite.created_at,
        expires_at: invite.expires_at,
        status: invite.status.as_str().to_string(),
        responded_at: invite.responded_at,
        reason: invite.reason,
    }))
}

/// GET /api/ea/:ea_id/rooms/:room/invites
pub async fn list_room_invites(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListInvitesResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let invites = state
        .rooms
        .list_invites(ea_id, &room)
        .map_err(room_error_response)?
        .into_iter()
        .map(|i| InviteResponse {
            id: i.id,
            invited_agent: i.invited_agent,
            invited_by: i.invited_by,
            message: i.message,
            created_at: i.created_at,
            expires_at: i.expires_at,
            status: i.status.as_str().to_string(),
            responded_at: i.responded_at,
            reason: i.reason,
        })
        .collect();
    Ok(Json(ListInvitesResponse { invites }))
}

/// POST /api/ea/:ea_id/rooms/:room/invites/:invite_id/respond
pub async fn respond_room_invite(
    Path((ea_id, room, invite_id)): Path<(u32, String, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RespondInviteRequest>,
) -> Result<Json<InviteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let agent = normalize_agent_name(&prefix, &req.agent);
    let decision = match req.response.as_str() {
        "accept" => InviteDecision::Accept,
        "decline" => InviteDecision::Decline,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("Invalid response '{}'. Use 'accept' or 'decline'", other),
                }),
            ))
        }
    };

    let invite = state
        .rooms
        .respond_invite(ea_id, &room, &invite_id, &agent, decision, req.reason)
        .map_err(room_error_response)?;

    Ok(Json(InviteResponse {
        id: invite.id,
        invited_agent: invite.invited_agent,
        invited_by: invite.invited_by,
        message: invite.message,
        created_at: invite.created_at,
        expires_at: invite.expires_at,
        status: invite.status.as_str().to_string(),
        responded_at: invite.responded_at,
        reason: invite.reason,
    }))
}

/// DELETE /api/ea/:ea_id/rooms/:room/invites/:invite_id
pub async fn cancel_room_invite(
    Path((ea_id, room, invite_id)): Path<(u32, String, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CancelInviteRequest>,
) -> Result<Json<InviteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let cancelled_by = normalize_agent_name(&prefix, &req.cancelled_by);
    let invite = state
        .rooms
        .cancel_invite(ea_id, &room, &invite_id, &cancelled_by)
        .map_err(room_error_response)?;
    Ok(Json(InviteResponse {
        id: invite.id,
        invited_agent: invite.invited_agent,
        invited_by: invite.invited_by,
        message: invite.message,
        created_at: invite.created_at,
        expires_at: invite.expires_at,
        status: invite.status.as_str().to_string(),
        responded_at: invite.responded_at,
        reason: invite.reason,
    }))
}

/// POST /api/ea/:ea_id/rooms/:room/messages
pub async fn send_room_message(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RoomMessageRequest>,
) -> Result<Json<RoomMessageResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let sender = normalize_agent_name(&prefix, &req.sender);
    let (msg, recipients) = state
        .rooms
        .post_message(ea_id, &room, &sender, &req.payload)
        .map_err(room_error_response)?;

    for receiver in &recipients {
        let event = ScheduledEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sender: format!("room:{}:{}", room, sender),
            receiver: receiver.clone(),
            timestamp: msg.created_at,
            payload: msg.payload.clone(),
            created_at: msg.created_at,
            recurring_ns: None,
            ea_id,
        };
        state.scheduler.insert(event);
    }

    Ok(Json(RoomMessageResponse {
        id: msg.id,
        sender,
        payload: msg.payload,
        created_at: msg.created_at,
        fanout_count: recipients.len(),
    }))
}

/// GET /api/ea/:ea_id/rooms/:room/transcript
pub async fn get_room_transcript(
    Path((ea_id, room)): Path<(u32, String)>,
    State(state): State<Arc<ApiState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<TranscriptResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (_prefix, _manager, _state_dir) = resolve_ea(ea_id, &state)?;
    let snapshot = state.rooms.get_room(ea_id, &room).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Room '{}' not found", room),
            }),
        )
    })?;
    let mut messages = snapshot.transcript;
    if let Some(limit) = params.get("limit").and_then(|s| s.parse::<usize>().ok()) {
        if messages.len() > limit {
            let start = messages.len() - limit;
            messages = messages.split_off(start);
        }
    }
    let messages = messages
        .into_iter()
        .map(|m| TranscriptMessageResponse {
            id: m.id,
            sender: m.sender,
            payload: m.payload,
            created_at: m.created_at,
            delivered_to: m.delivered_to,
            system: m.system,
        })
        .collect();
    Ok(Json(TranscriptResponse { room, messages }))
}

// ── Computer Use handlers (global — not EA-scoped) ──

/// Helper: verify the agent holds the computer lock.
async fn verify_computer_lock(
    lock: &ComputerLock,
    agent: &str,
    ea_id: Option<u32>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let expected = match ea_id {
        Some(ea) => format!("{}:{}", ea, agent),
        None => agent.to_string(),
    };
    let guard = lock.lock().await;
    match guard.as_deref() {
        Some(holder) if holder == expected => Ok(()),
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
    let display = screen_size.is_some();
    let screenshot_ready = display && screenshot;

    Json(ComputerAvailabilityResponse {
        available: xdotool && screenshot_ready,
        xdotool,
        screenshot,
        display,
        screenshot_ready,
        screen_size,
    })
}

/// POST /api/computer/lock
/// Fix V6: Lock owner uses "{ea_id}:{agent}" when ea_id is provided,
/// preventing cross-EA identity collisions.
pub async fn computer_lock_acquire(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ComputerLockRequest>,
) -> Result<Json<ComputerLockResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.computer_lock.lock().await;

    // Build qualified owner name
    let owner = match req.ea_id {
        Some(ea) => format!("{}:{}", ea, req.agent),
        None => req.agent.clone(), // backward compat
    };

    if let Some(ref holder) = *guard {
        if *holder == owner {
            return Ok(Json(ComputerLockResponse {
                status: "already_held".to_string(),
                held_by: Some(owner),
            }));
        }
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: format!("Computer is locked by '{}'", holder),
            }),
        ));
    }

    *guard = Some(owner.clone());
    Ok(Json(ComputerLockResponse {
        status: "acquired".to_string(),
        held_by: Some(owner),
    }))
}

/// DELETE /api/computer/lock
/// Fix V6: Uses EA-qualified owner name for release comparison.
pub async fn computer_lock_release(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ComputerLockRequest>,
) -> Result<Json<ComputerLockResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.computer_lock.lock().await;

    // Build qualified owner name (must match acquire format)
    let owner = match req.ea_id {
        Some(ea) => format!("{}:{}", ea, req.agent),
        None => req.agent.clone(),
    };

    match guard.as_deref() {
        Some(holder) if holder == owner => {
            *guard = None;
            Ok(Json(ComputerLockResponse {
                status: "released".to_string(),
                held_by: None,
            }))
        }
        Some(holder) => Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: format!("Lock held by '{}', not '{}'", holder, owner),
            }),
        )),
        None => Ok(Json(ComputerLockResponse {
            status: "not_held".to_string(),
            held_by: None,
        })),
    }
}

/// POST /api/computer/screenshot
pub async fn computer_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScreenshotRequest>,
) -> Result<Json<ScreenshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent, req.ea_id).await?;

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
pub async fn computer_mouse(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<MouseRequest>,
) -> Result<Json<ComputerActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent, req.ea_id).await?;

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
pub async fn computer_keyboard(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<KeyboardRequest>,
) -> Result<Json<ComputerActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    verify_computer_lock(&state.computer_lock, &req.agent, req.ea_id).await?;

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

// ── Helpers ──

/// Generate a unique agent name within an EA, using tmux has-session for checking.
fn generate_agent_name_in_ea(prefix: &str) -> String {
    for i in 1..1000 {
        let name = format!("{}{}", prefix, i);
        let result = std::process::Command::new("tmux")
            .args(["has-session", "-t", &name])
            .output();
        match result {
            Ok(output) if !output.status.success() => return name,
            _ => continue,
        }
    }
    // Fallback: use UUID suffix
    format!("{}{}", prefix, &uuid::Uuid::new_v4().to_string()[..8])
}

#[cfg(test)]
mod tests {
    use super::timestamp_is_too_old;

    #[test]
    fn timestamp_guard_accepts_exact_one_second_boundary() {
        let now = 5_000_000_000;
        assert!(!timestamp_is_too_old(4_000_000_000, now));
    }

    #[test]
    fn timestamp_guard_rejects_values_more_than_one_second_old() {
        let now = 5_000_000_000;
        assert!(timestamp_is_too_old(3_999_999_999, now));
    }

    #[test]
    fn timestamp_guard_handles_large_timestamps_without_overflow() {
        let now = 5_000_000_000;
        assert!(!timestamp_is_too_old(u64::MAX, now));
    }
}
