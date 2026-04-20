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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use super::models::*;
use crate::app::App;
use crate::computer::{self, ComputerLock};
use crate::ea;
use crate::manager::{build_worker_agent_command, prompts_dir, PromptDeliveryMode};
use crate::memory;
use crate::projects;
use crate::scheduler::{event::ScheduledEvent, Scheduler};
use crate::tmux::{DeliveryOptions, TmuxClient};

/// Shared state for all API handlers
pub struct ApiState {
    pub app: Arc<Mutex<App>>,
    pub scheduler: Arc<Scheduler>,
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

fn normalize_backend_name(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "codex" => Some("codex"),
        "cursor" => Some("cursor"),
        "gemini" => Some("gemini"),
        "claude" | "claude-code" | "claude_code" => Some("claude"),
        "opencode" => Some("opencode"),
        _ => None,
    }
}

fn strip_command_token_wrappers(token: &str) -> &str {
    token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'))
}

fn infer_backend_name(explicit_backend: Option<&str>, command: &str) -> Option<&'static str> {
    if let Some(name) = explicit_backend.and_then(normalize_backend_name) {
        return Some(name);
    }

    for token in command.split_whitespace() {
        let token = strip_command_token_wrappers(token);
        if token.is_empty() {
            continue;
        }

        let executable = std::path::Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(token);
        let executable = strip_command_token_wrappers(executable);

        if let Some(name) = normalize_backend_name(executable) {
            return Some(name);
        }
    }

    None
}

fn timestamp_is_too_old(timestamp: u64, now: u64) -> bool {
    timestamp < now.saturating_sub(1_000_000_000)
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

    // Build the agent command (App lock is no longer held).
    //
    // For worker agents we use `build_worker_agent_command`, which folds the
    // YOUR NAME / YOUR TASK header into the initial prompt for group-2
    // backends (cursor/gemini/opencode). `delivery_mode` tells us whether a
    // follow-up `deliver_prompt` is still required below.
    let has_agent_prompt = matches!(req.role.as_deref(), Some("project-manager") | Some("agent"))
        || req.task.is_some();
    let (cmd, delivery_mode) = if has_agent_prompt {
        let task = req.task.as_deref().unwrap_or("");
        let prompt_file = prompts_dir(&state.omar_dir).join("agent.md");
        let spawn_cmd = build_worker_agent_command(
            &base_command,
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", &prompt_parent_name),
                ("{{TASK}}", task),
                ("{{EA_ID}}", &ea_id.to_string()),
            ],
            &short_name,
            task,
        );
        (spawn_cmd.command, Some(spawn_cmd.delivery_mode))
    } else {
        (base_command.clone(), None)
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

    let backend_name = infer_backend_name(req.backend.as_deref(), &base_command);

    // Deliver first user message using readiness-aware prompt delivery.
    //
    // For group-2 backends (cursor/gemini/opencode) the spawn command
    // already contains the task header, so this step is skipped — issuing
    // it anyway would duplicate the task or land while the backend is
    // still processing the combined initial prompt.
    let skip_task_delivery = matches!(delivery_mode, Some(PromptDeliveryMode::InitialUserMessage));
    if let Some(ref task) = req.task {
        memory::save_worker_task_in(&state_dir, &session_name, task);
    }
    if let (Some(task), false) = (req.task.as_ref(), skip_task_delivery) {
        let user_msg = format!("YOUR NAME: {}\nYOUR TASK: {}", short_name, task);
        let client2 = client.clone();
        let session2 = session_name.clone();
        let readiness_markers: Vec<&'static str> = backend_name
            .map(crate::tmux::backend_readiness_markers)
            .unwrap_or(&[])
            .to_vec();
        tokio::spawn(async move {
            let session_label = session2.clone();

            match tokio::task::spawn_blocking(move || {
                // Markers are the authoritative readiness signal when we know
                // the backend. If they succeed, the TUI has rendered and is
                // accepting input — no need to also wait for a follow-up
                // content change in wait_for_stable (Claude Code's TUI is
                // pixel-stable after its banner draws, so requiring an
                // *additional* change would time out for no reason).
                let markers_proved_ready = if !readiness_markers.is_empty() {
                    let detected = client2.wait_for_markers(
                        &session2,
                        &readiness_markers,
                        Duration::from_secs(60),
                        Duration::from_millis(250),
                    );
                    if !detected {
                        eprintln!(
                            "readiness markers timed out for {}; proceeding with prompt delivery",
                            session2
                        );
                    }
                    detected
                } else {
                    false
                };
                let opts = DeliveryOptions {
                    startup_timeout: Duration::from_secs(45),
                    stable_quiet: Duration::from_millis(800),
                    verify_timeout: Duration::from_secs(6),
                    max_retries: 4,
                    poll_interval: Duration::from_millis(120),
                    retry_delay: Duration::from_millis(250),
                    require_initial_change: !markers_proved_ready,
                };
                client2.deliver_prompt(&session2, &user_msg, &opts)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("prompt delivery failed for {}: {}", session_label, e),
                Err(e) => eprintln!("prompt delivery task failed for {}: {}", session_label, e),
            }
        });
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

// ── Logging handlers (Action Reasoning & Goal Alignment) ──

/// Maximum accepted byte length for `LogRequest.agent_name`.
pub(crate) const LOG_AGENT_NAME_MAX: usize = 128;
/// Maximum accepted byte length for `LogRequest.action`.
pub(crate) const LOG_ACTION_MAX: usize = 512;
/// Maximum accepted byte length for `LogRequest.justification`.
pub(crate) const LOG_JUSTIFICATION_MAX: usize = 2048;

/// Validate an agent name for use as a filename component.
///
/// Rejects empty strings, inputs that would escape the target directory
/// (`..`, `.`, leading `.`, path separators), and enforces a conservative
/// alphabet + length cap. On success returns `Ok(())`; on failure returns
/// a human-readable reason suitable for a 400 response body.
pub(crate) fn validate_log_agent_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("agent_name must not be empty".to_string());
    }
    if name.len() > LOG_AGENT_NAME_MAX {
        return Err(format!(
            "agent_name must be at most {} bytes",
            LOG_AGENT_NAME_MAX
        ));
    }
    if name == "." || name == ".." {
        return Err("agent_name must not be '.' or '..'".to_string());
    }
    if name.starts_with('.') {
        return Err("agent_name must not start with '.'".to_string());
    }
    for ch in name.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !ok {
            return Err(format!(
                "agent_name contains invalid character {:?}; allowed: [A-Za-z0-9._-]",
                ch
            ));
        }
    }
    Ok(())
}

/// Validate free-form text fields on a `LogRequest`.
pub(crate) fn validate_log_text(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), String> {
    if value.len() > max {
        return Err(format!("{} must be at most {} bytes", field, max));
    }
    Ok(())
}

/// Build the EA-scoped agent hierarchy path (root → leaf) for a given caller.
///
/// Returns a Vec of short display names, starting with `"ea"` and ending with
/// the caller's own display name. Cycles in the parent map are broken.
/// Pure function — extracted so it can be unit-tested without a live tmux/App.
pub(crate) fn build_hierarchy_path(
    parents: &HashMap<String, String>,
    prefix: &str,
    manager_session: &str,
    agent_name: &str,
) -> Vec<String> {
    // Normalize the caller's agent name to a full tmux session name.
    // "ea" is the reserved alias for the EA manager session.
    let start = if agent_name == "ea" {
        manager_session.to_string()
    } else if agent_name.starts_with(prefix) {
        agent_name.to_string()
    } else {
        format!("{}{}", prefix, agent_name)
    };

    let mut current = start;
    let mut path_rev: Vec<String> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Walk upward from the caller to the EA manager.
    loop {
        if !visited.insert(current.clone()) {
            break; // cycle guard
        }
        if current == manager_session {
            path_rev.push("ea".to_string());
            break;
        }
        path_rev.push(display_name(prefix, &current).to_string());
        match parents.get(&current) {
            Some(p) => current = p.clone(),
            None => break,
        }
    }

    // Ensure the EA sits at the root of the hierarchy even when the caller
    // has no registered parent yet (e.g. the EA itself logging on startup).
    if path_rev.last().map(|s| s.as_str()) != Some("ea") {
        path_rev.push("ea".to_string());
    }
    // path_rev is leaf -> root; reverse for human-readable root -> leaf.
    path_rev.reverse();
    path_rev
}

/// Persist a single justification entry to disk.
///
/// Returns the path that was appended to on success. Factored out of
/// `log_justification` so it can be exercised end-to-end from tests without
/// constructing a full `ApiState` (which requires tmux + real `App`).
///
/// Safety invariants enforced by the caller:
///   - `short_name` must have already passed `validate_log_agent_name` — it is
///     used directly as a filename component.
///   - `hierarchy_path` must be non-empty and root at `"ea"`.
pub(crate) async fn write_justification_entry(
    omar_dir: &std::path::Path,
    session_id: &str,
    ea_id: u32,
    short_name: &str,
    entry: &LogEntry,
) -> std::io::Result<PathBuf> {
    let log_dir = omar_dir
        .join("logs")
        .join(session_id)
        .join(format!("ea_{}", ea_id));
    tokio::fs::create_dir_all(&log_dir).await?;

    let log_file = log_dir.join(format!("{}.jsonl", short_name));
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .await?;
    f.write_all(format!("{}\n", line).as_bytes()).await?;
    // Force the write through the tokio blocking pool before returning so
    // concurrent readers (including our own tests using std::fs) see it.
    f.flush().await?;
    Ok(log_file)
}

/// POST /api/ea/:ea_id/logs
///
/// Append a structured justification entry (JSONL) for an agent action.
/// Each agent writes to its own file under
/// `~/.omar/logs/<session_id>/ea_<ea_id>/<agent-short-name>.jsonl`,
/// eliminating write contention across agents and EAs.
pub async fn log_justification(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<LogRequest>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Fix 1: Reject inputs that would escape the EA-scoped log directory or
    // produce a filename we don't control. Validate before touching disk.
    if let Err(e) = validate_log_agent_name(&req.agent_name) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })));
    }
    // Fix 4: Bound request payload sizes so a single JSONL line always fits
    // inside PIPE_BUF (4096 bytes on Linux), keeping concurrent `append` writes
    // atomic with respect to each other.
    if let Err(e) = validate_log_text("action", &req.action, LOG_ACTION_MAX) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })));
    }
    if let Err(e) = validate_log_text("justification", &req.justification, LOG_JUSTIFICATION_MAX) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })));
    }

    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    let parents = memory::load_agent_parents_from(&state_dir);
    let hierarchy_path = build_hierarchy_path(&parents, &prefix, &manager_session, &req.agent_name);

    // Human-readable log line (printed via tracing so it shows up in the
    // dashboard's debug console and anywhere tracing is wired up).
    tracing::info!(
        "Justification [ea={}] [{}] Action: {} | Reason: {}",
        ea_id,
        hierarchy_path.join(" > "),
        req.action,
        req.justification
    );

    let (session_id, omar_dir) = {
        let app = state.app.lock().await;
        (app.session_id.clone(), state.omar_dir.clone())
    };

    let short_name = hierarchy_path
        .last()
        .cloned()
        .unwrap_or_else(|| req.agent_name.clone());

    let entry = LogEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        ea_id,
        agent_name: req.agent_name.clone(),
        hierarchy_path,
        action: req.action.clone(),
        justification: req.justification.clone(),
    };

    if let Err(e) =
        write_justification_entry(&omar_dir, &session_id, ea_id, &short_name, &entry).await
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to write log entry: {}", e),
            }),
        ));
    }

    Ok(Json(StatusResponse {
        status: "logged".to_string(),
        message: None,
    }))
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
    use super::{
        build_hierarchy_path, infer_backend_name, timestamp_is_too_old, validate_log_agent_name,
        validate_log_text, write_justification_entry, LogEntry, LOG_ACTION_MAX, LOG_AGENT_NAME_MAX,
        LOG_JUSTIFICATION_MAX,
    };
    use std::collections::HashMap;

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

    #[test]
    fn infer_backend_name_scans_wrapped_tokens() {
        assert_eq!(
            infer_backend_name(None, "env FOO=bar codex --model o3"),
            Some("codex")
        );
        assert_eq!(
            infer_backend_name(None, "npx opencode --prompt hi"),
            Some("opencode")
        );
        assert_eq!(infer_backend_name(None, "(cursor) --fast"), Some("cursor"));
    }

    // ── build_hierarchy_path tests (Action Reasoning & Goal Alignment) ──

    fn ea_setup() -> (String, String) {
        // Matches the production naming: prefix "omar-agent-0-" and manager
        // session "omar-agent-ea-0" for EA 0.
        ("omar-agent-0-".to_string(), "omar-agent-ea-0".to_string())
    }

    #[test]
    fn hierarchy_for_ea_alias_is_just_ea() {
        let (prefix, manager) = ea_setup();
        let parents: HashMap<String, String> = HashMap::new();
        assert_eq!(
            build_hierarchy_path(&parents, &prefix, &manager, "ea"),
            vec!["ea".to_string()]
        );
    }

    #[test]
    fn hierarchy_for_direct_child_of_ea() {
        let (prefix, manager) = ea_setup();
        let mut parents = HashMap::new();
        parents.insert(format!("{}api", prefix), manager.clone());
        assert_eq!(
            build_hierarchy_path(&parents, &prefix, &manager, "api"),
            vec!["ea".to_string(), "api".to_string()]
        );
    }

    #[test]
    fn hierarchy_accepts_full_session_name_as_input() {
        let (prefix, manager) = ea_setup();
        let mut parents = HashMap::new();
        parents.insert(format!("{}api", prefix), manager.clone());
        assert_eq!(
            build_hierarchy_path(&parents, &prefix, &manager, &format!("{}api", prefix)),
            vec!["ea".to_string(), "api".to_string()]
        );
    }

    #[test]
    fn hierarchy_for_multi_level_descent() {
        let (prefix, manager) = ea_setup();
        let mut parents = HashMap::new();
        parents.insert(format!("{}api", prefix), manager.clone());
        parents.insert(format!("{}auth", prefix), format!("{}api", prefix));
        parents.insert(format!("{}jwt", prefix), format!("{}auth", prefix));
        assert_eq!(
            build_hierarchy_path(&parents, &prefix, &manager, "jwt"),
            vec![
                "ea".to_string(),
                "api".to_string(),
                "auth".to_string(),
                "jwt".to_string(),
            ]
        );
    }

    #[test]
    fn hierarchy_breaks_cycles_without_panicking() {
        // Cycle: a -> b -> a. Should terminate; ea is prepended as the root.
        let (prefix, manager) = ea_setup();
        let mut parents = HashMap::new();
        parents.insert(format!("{}a", prefix), format!("{}b", prefix));
        parents.insert(format!("{}b", prefix), format!("{}a", prefix));
        let path = build_hierarchy_path(&parents, &prefix, &manager, "a");
        // Must terminate, must start with ea, must contain a and b.
        assert_eq!(path.first().map(|s| s.as_str()), Some("ea"));
        assert!(path.iter().any(|s| s == "a"));
        assert!(path.iter().any(|s| s == "b"));
    }

    #[test]
    fn hierarchy_for_orphan_agent_still_has_ea_root() {
        // Agent exists with no parent entry (e.g., spawned directly by EA
        // before parent map was persisted). Path must still root at ea.
        let (prefix, manager) = ea_setup();
        let parents: HashMap<String, String> = HashMap::new();
        let path = build_hierarchy_path(&parents, &prefix, &manager, "orphan");
        assert_eq!(path.first().map(|s| s.as_str()), Some("ea"));
        assert_eq!(path.last().map(|s| s.as_str()), Some("orphan"));
    }

    // ── validate_log_agent_name tests (path-traversal hardening) ──

    #[test]
    fn validate_agent_name_accepts_normal_names() {
        assert!(validate_log_agent_name("ea").is_ok());
        assert!(validate_log_agent_name("review").is_ok());
        assert!(validate_log_agent_name("api-worker_1.v2").is_ok());
        assert!(validate_log_agent_name("t-127").is_ok());
    }

    #[test]
    fn validate_agent_name_rejects_path_traversal() {
        assert!(validate_log_agent_name("..").is_err());
        assert!(validate_log_agent_name(".").is_err());
        assert!(validate_log_agent_name("../evil").is_err());
        assert!(validate_log_agent_name("..\\evil").is_err());
        assert!(validate_log_agent_name("a/b").is_err());
        assert!(validate_log_agent_name("a\\b").is_err());
        assert!(validate_log_agent_name(".hidden").is_err());
    }

    #[test]
    fn validate_agent_name_rejects_empty_and_oversize() {
        assert!(validate_log_agent_name("").is_err());
        let big = "a".repeat(LOG_AGENT_NAME_MAX + 1);
        assert!(validate_log_agent_name(&big).is_err());
        let ok_max = "a".repeat(LOG_AGENT_NAME_MAX);
        assert!(validate_log_agent_name(&ok_max).is_ok());
    }

    #[test]
    fn validate_agent_name_rejects_control_and_null_bytes() {
        assert!(validate_log_agent_name("evil\0").is_err());
        assert!(validate_log_agent_name("evil\nname").is_err());
        assert!(validate_log_agent_name("space name").is_err());
    }

    // ── validate_log_text tests (payload size cap) ──

    #[test]
    fn validate_text_accepts_up_to_limit() {
        assert!(validate_log_text("action", "", LOG_ACTION_MAX).is_ok());
        let at_limit = "x".repeat(LOG_ACTION_MAX);
        assert!(validate_log_text("action", &at_limit, LOG_ACTION_MAX).is_ok());
    }

    #[test]
    fn validate_text_rejects_oversize() {
        let over = "x".repeat(LOG_JUSTIFICATION_MAX + 1);
        assert!(validate_log_text("justification", &over, LOG_JUSTIFICATION_MAX).is_err());
    }

    // ── write_justification_entry end-to-end (handler IO path) ──

    #[tokio::test]
    async fn write_entry_creates_ea_scoped_directory_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let omar_dir = tmp.path().to_path_buf();
        let session_id = "20260417_120000";
        let entry = LogEntry {
            timestamp: "2026-04-17T12:00:00Z".to_string(),
            ea_id: 7,
            agent_name: "api".to_string(),
            hierarchy_path: vec!["ea".to_string(), "api".to_string()],
            action: "spawn worker".to_string(),
            justification: "need parallelism".to_string(),
        };

        let path = write_justification_entry(&omar_dir, session_id, 7, "api", &entry)
            .await
            .expect("write must succeed");

        let expected = omar_dir
            .join("logs")
            .join(session_id)
            .join("ea_7")
            .join("api.jsonl");
        assert_eq!(path, expected);
        assert!(expected.exists(), "log file must exist at EA-scoped path");
    }

    #[tokio::test]
    async fn write_entry_serializes_ea_id_in_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let omar_dir = tmp.path().to_path_buf();
        let entry = LogEntry {
            timestamp: "2026-04-17T12:00:00Z".to_string(),
            ea_id: 3,
            agent_name: "ea".to_string(),
            hierarchy_path: vec!["ea".to_string()],
            action: "start".to_string(),
            justification: "boot".to_string(),
        };
        write_justification_entry(&omar_dir, "sid", 3, "ea", &entry)
            .await
            .unwrap();

        let contents = std::fs::read_to_string(
            omar_dir
                .join("logs")
                .join("sid")
                .join("ea_3")
                .join("ea.jsonl"),
        )
        .unwrap();
        let trimmed = contents.trim_end();
        assert!(trimmed.ends_with('}'), "entry must be valid JSON line");
        let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap();
        assert_eq!(parsed["ea_id"], 3);
        assert_eq!(parsed["agent_name"], "ea");
        assert_eq!(parsed["action"], "start");
        assert_eq!(parsed["hierarchy_path"], serde_json::json!(["ea"]));
    }

    #[tokio::test]
    async fn write_entry_appends_on_repeat_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let omar_dir = tmp.path().to_path_buf();
        for i in 0..3 {
            let entry = LogEntry {
                timestamp: format!("2026-04-17T12:00:0{}Z", i),
                ea_id: 0,
                agent_name: "worker".to_string(),
                hierarchy_path: vec!["ea".to_string(), "worker".to_string()],
                action: format!("step {}", i),
                justification: "because".to_string(),
            };
            write_justification_entry(&omar_dir, "sid", 0, "worker", &entry)
                .await
                .unwrap();
        }
        let contents = std::fs::read_to_string(
            omar_dir
                .join("logs")
                .join("sid")
                .join("ea_0")
                .join("worker.jsonl"),
        )
        .unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3, "each call must append one line");
        for (i, line) in lines.iter().enumerate() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["action"], format!("step {}", i));
        }
    }

    #[tokio::test]
    async fn write_entry_isolates_eas() {
        // Two EAs with same agent short-name land in separate files.
        let tmp = tempfile::tempdir().unwrap();
        let omar_dir = tmp.path().to_path_buf();
        for ea in [0u32, 1u32] {
            let entry = LogEntry {
                timestamp: "2026-04-17T12:00:00Z".to_string(),
                ea_id: ea,
                agent_name: "api".to_string(),
                hierarchy_path: vec!["ea".to_string(), "api".to_string()],
                action: format!("action ea{}", ea),
                justification: "x".to_string(),
            };
            write_justification_entry(&omar_dir, "sid", ea, "api", &entry)
                .await
                .unwrap();
        }
        let ea0 = omar_dir.join("logs/sid/ea_0/api.jsonl");
        let ea1 = omar_dir.join("logs/sid/ea_1/api.jsonl");
        assert!(std::fs::read_to_string(&ea0)
            .unwrap()
            .contains("action ea0"));
        assert!(std::fs::read_to_string(&ea1)
            .unwrap()
            .contains("action ea1"));
        assert!(!std::fs::read_to_string(&ea0)
            .unwrap()
            .contains("action ea1"));
    }

    #[test]
    fn hierarchy_is_scoped_per_ea() {
        // Verify the same agent name in two EAs produces two isolated paths
        // because prefix and manager differ.
        let (prefix0, manager0) = ("omar-agent-0-".to_string(), "omar-agent-ea-0".to_string());
        let (prefix1, manager1) = ("omar-agent-1-".to_string(), "omar-agent-ea-1".to_string());

        let mut parents0 = HashMap::new();
        parents0.insert(format!("{}api", prefix0), manager0.clone());
        let mut parents1 = HashMap::new();
        parents1.insert(format!("{}api", prefix1), manager1.clone());

        assert_eq!(
            build_hierarchy_path(&parents0, &prefix0, &manager0, "api"),
            vec!["ea".to_string(), "api".to_string()]
        );
        assert_eq!(
            build_hierarchy_path(&parents1, &prefix1, &manager1, "api"),
            vec!["ea".to_string(), "api".to_string()]
        );
    }
}
