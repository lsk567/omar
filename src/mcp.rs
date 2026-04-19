//! OMAR MCP server.
//!
//! This replaces the legacy REST surface with a typed MCP tool interface.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::app::AgentInfo;
use crate::computer;
use crate::config;
use crate::ea::{self, EaId};
use crate::manager::{self, McpLaunchContext};
use crate::memory;
use crate::metrics;
use crate::projects;
use crate::scheduler::{self, ScheduledEvent};
use crate::tasks::{self, TaskRecord, TaskStatus};
use crate::tmux::{DeliveryOptions, HealthChecker, TmuxClient};

const JSONRPC_VERSION: &str = "2.0";
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_INSTRUCTIONS: &str = concat!(
    "OMAR provides orchestration tools for executive assistant and worker sessions. ",
    "Search OMAR tools when the task involves agents, tracked tasks, projects, scheduled events, ",
    "manager notes, action logs, or computer control. ",
    "Use create_task/check_task/complete_task for normal delegated work, ",
    "append_manager_note for persistent manager notes, ",
    "log_action before state-changing operations, ",
    "and schedule_event/list_events/cancel_event instead of sleep loops."
);

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallRequest {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageFraming {
    ContentLength,
    JsonLine,
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos() as u64
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn infer_backend_name(explicit_backend: Option<&str>, command: &str) -> String {
    fn normalize(s: &str) -> Option<&'static str> {
        match s.trim().to_ascii_lowercase().as_str() {
            "codex" => Some("codex"),
            "cursor" => Some("cursor"),
            "gemini" => Some("gemini"),
            "claude" | "claude-code" | "claude_code" => Some("claude"),
            "opencode" => Some("opencode"),
            _ => None,
        }
    }

    if let Some(name) = explicit_backend.and_then(normalize) {
        return name.to_string();
    }

    for token in command.split_whitespace() {
        let token = token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']'));
        if token.is_empty() {
            continue;
        }
        let executable = Path::new(token)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(token);
        if let Some(name) = normalize(executable) {
            return name.to_string();
        }
    }

    "unknown".to_string()
}

fn append_debug_log(context: &McpLaunchContext, line: &str) {
    let state_dir = ea::ea_state_dir(context.ea_id, &context.omar_dir);
    if std::fs::create_dir_all(&state_dir).is_err() {
        return;
    }
    let path = state_dir.join("mcp_server.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {}", now_rfc3339(), line);
    }
}

fn project_name_from_task(task: &str) -> String {
    let first = task.lines().next().unwrap_or("task").trim();
    let mut out: String = first.chars().take(80).collect();
    if out.is_empty() {
        out = "task".to_string();
    }
    out
}

fn last_output_line(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(120)
        .collect()
}

fn health_from_activity(activity: i64, idle_warning: i64) -> &'static str {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    if now.saturating_sub(activity) <= idle_warning {
        "running"
    } else {
        "idle"
    }
}

fn lock_path_for_state_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(".mcp-state.lock")
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        for _ in 0..500 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("Failed to acquire {:?}", path))
                }
            }
        }
        Err(anyhow!("Timed out waiting for lock {:?}", path))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn run_server_from_context_file(path: PathBuf) -> Result<()> {
    let context: McpLaunchContext = serde_json::from_str(
        &fs::read_to_string(&path)
            .with_context(|| format!("Failed to read MCP context file {}", path.display()))?,
    )
    .with_context(|| format!("Failed to parse MCP context file {}", path.display()))?;
    OmarMcpServer::new(context).run()
}

struct OmarMcpServer {
    context: McpLaunchContext,
}

#[derive(Debug)]
struct SpawnRequestInternal {
    name: Option<String>,
    task: Option<String>,
    workdir: Option<String>,
    command: Option<String>,
    backend: Option<String>,
    model: Option<String>,
    role: Option<String>,
    parent: Option<String>,
    spawn_lock_wait_ms: u64,
}

impl OmarMcpServer {
    fn new(context: McpLaunchContext) -> Self {
        Self { context }
    }

    fn run(&self) -> Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = BufReader::new(stdin.lock());
        let mut writer = stdout.lock();

        while let Some((request, framing)) = read_message(&mut reader)? {
            append_debug_log(
                &self.context,
                &format!("request method={} id={:?}", request.method, request.id),
            );
            let maybe_response = self.handle_request(request);
            if let Some(response) = maybe_response {
                append_debug_log(
                    &self.context,
                    &format!(
                        "response id={} has_error={}",
                        response.id,
                        response.error.is_some()
                    ),
                );
                write_message(&mut writer, &response, framing)?;
                writer.flush()?;
            }
        }

        Ok(())
    }

    fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = request.id.clone()?;
        if request.jsonrpc != JSONRPC_VERSION {
            return Some(error_response(id, -32600, "Unsupported jsonrpc version"));
        }

        let response = match request.method.as_str() {
            "initialize" => ok_response(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "omar",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": SERVER_INSTRUCTIONS,
                }),
            ),
            "tools/list" => ok_response(id, json!({ "tools": tool_definitions() })),
            "tools/call" => match serde_json::from_value::<ToolCallRequest>(request.params) {
                Ok(call) => ok_response(id, self.call_tool(call)),
                Err(err) => {
                    error_response(id, -32602, &format!("Invalid tool call params: {}", err))
                }
            },
            "resources/list" => ok_response(id, json!({ "resources": [] })),
            "ping" => ok_response(id, json!({})),
            _ => error_response(id, -32601, &format!("Unknown method '{}'", request.method)),
        };

        Some(response)
    }

    fn call_tool(&self, call: ToolCallRequest) -> Value {
        append_debug_log(
            &self.context,
            &format!("tool_call name={} args={}", call.name, call.arguments),
        );
        let result = match call.name.as_str() {
            "list_backends" => self.list_backends(),
            "list_eas" => self.list_eas(),
            "create_ea" => self.create_ea(call.arguments),
            "delete_ea" => self.delete_ea(call.arguments),
            "list_agents" => self.list_agents(),
            "get_agent" => self.get_agent(call.arguments),
            "get_agent_summary" => self.get_agent_summary(call.arguments),
            "update_agent_status" => self.update_agent_status(call.arguments),
            "spawn_agent_session" => self.spawn_agent_session(call.arguments),
            "kill_agent" => self.kill_agent(call.arguments),
            "send_input" => self.send_input(call.arguments),
            "list_projects" => self.list_projects(),
            "schedule_event" => self.schedule_event(call.arguments),
            "list_events" => self.list_events(),
            "cancel_event" => self.cancel_event(call.arguments),
            "create_task" => self.create_task(call.arguments),
            "check_task" => self.check_task(call.arguments),
            "complete_task" => self.complete_task(call.arguments),
            "replace_stuck_task_agent" => self.replace_stuck_task_agent(call.arguments),
            "append_manager_note" => self.append_manager_note(call.arguments),
            "log_action" => self.log_action(call.arguments),
            "computer_status" => self.computer_status(),
            "computer_lock_acquire" => self.computer_lock_acquire(call.arguments),
            "computer_lock_release" => self.computer_lock_release(call.arguments),
            "computer_screenshot" => self.computer_screenshot(call.arguments),
            "computer_mouse" => self.computer_mouse(call.arguments),
            "computer_keyboard" => self.computer_keyboard(call.arguments),
            "computer_screen_size" => self.computer_screen_size(),
            "computer_mouse_position" => self.computer_mouse_position(),
            other => Err(anyhow!("Unknown tool '{}'", other)),
        };

        match result {
            Ok(value) => {
                append_debug_log(&self.context, &format!("tool_ok name={}", call.name));
                tool_success(value)
            }
            Err(err) => {
                append_debug_log(
                    &self.context,
                    &format!("tool_err name={} err={}", call.name, err),
                );
                tool_error(err)
            }
        }
    }

    fn ea_id(&self) -> Result<EaId> {
        Ok(self.context.ea_id)
    }

    fn state_dir(&self) -> Result<PathBuf> {
        Ok(ea::ea_state_dir(self.ea_id()?, &self.context.omar_dir))
    }

    fn session_prefix(&self) -> Result<String> {
        Ok(ea::ea_prefix(self.ea_id()?, &self.context.session_prefix))
    }

    fn manager_session(&self) -> Result<String> {
        Ok(ea::ea_manager_session(
            self.ea_id()?,
            &self.context.session_prefix,
        ))
    }

    fn qualified_session_name(&self, short_or_full: &str) -> Result<String> {
        let prefix = self.session_prefix()?;
        let manager = self.manager_session()?;
        if short_or_full == manager || short_or_full.starts_with(&prefix) {
            Ok(short_or_full.to_string())
        } else {
            Ok(format!("{}{}", prefix, short_or_full))
        }
    }

    fn display_name<'a>(&self, session_name: &'a str) -> Result<&'a str> {
        let prefix = self.session_prefix()?;
        Ok(session_name.strip_prefix(&prefix).unwrap_or(session_name))
    }

    fn client(&self) -> Result<TmuxClient> {
        Ok(TmuxClient::new(self.session_prefix()?))
    }

    fn scheduler(&self) -> scheduler::Scheduler {
        scheduler::Scheduler::with_store(scheduler::events_store_path(&self.context.omar_dir))
    }

    fn refresh_memory(&self) -> Result<()> {
        let state_dir = self.state_dir()?;
        let prefix = self.session_prefix()?;
        let manager_session = self.manager_session()?;
        let client = TmuxClient::new(&prefix);
        let mut checker = HealthChecker::new(client.clone(), self.context.health_idle_warning);
        let sessions = client.list_sessions().unwrap_or_default();
        let mut manager = None;
        let mut agents = Vec::new();
        for session in sessions {
            let health_info = checker.check_detailed(&session.name);
            let info = AgentInfo {
                session: session.clone(),
                health: health_info.state,
                health_info,
            };
            if session.name == manager_session {
                manager = Some(info);
            } else {
                agents.push(info);
            }
        }
        memory::write_memory_to(
            &state_dir,
            &agents,
            manager.as_ref(),
            &manager_session,
            &client,
            &self.scheduler().list_by_ea(self.ea_id()?),
        );
        Ok(())
    }

    fn list_backends(&self) -> Result<Value> {
        let backends = ["claude", "codex", "cursor", "gemini", "opencode"];
        let infos: Vec<Value> = backends
            .iter()
            .filter_map(|name| {
                let command = config::resolve_backend(name).ok()?;
                let executable = command.split_whitespace().next().unwrap_or(name);
                let available = Command::new(executable)
                    .arg("--version")
                    .output()
                    .is_ok_and(|output| output.status.success());
                Some(json!({
                    "name": name,
                    "available": available,
                    "command": command,
                }))
            })
            .collect();
        Ok(json!({ "backends": infos }))
    }

    fn list_eas(&self) -> Result<Value> {
        let registry = ea::ensure_default_ea(&self.context.omar_dir)?;
        let active = ea::resolve_active_ea(&self.context.omar_dir, &registry);
        let eas: Vec<Value> = registry
            .into_iter()
            .map(|ea_info| {
                json!({
                    "id": ea_info.id,
                    "name": ea_info.name,
                    "description": ea_info.description,
                    "is_active": ea_info.id == active,
                })
            })
            .collect();
        Ok(json!({ "active": active, "eas": eas }))
    }

    fn create_ea(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
            description: Option<String>,
        }
        let args: Args = serde_json::from_value(args)?;
        ea::validate_ea_name(&args.name).map_err(|e| anyhow!(e))?;
        let id = ea::register_ea(
            &self.context.omar_dir,
            &args.name,
            args.description.as_deref(),
        )?;
        Ok(json!({
            "id": id,
            "name": args.name,
            "description": args.description,
        }))
    }

    fn delete_ea(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            ea_id: u32,
        }
        let args: Args = serde_json::from_value(args)?;
        let prefix = ea::ea_prefix(args.ea_id, &self.context.session_prefix);
        let manager_session = ea::ea_manager_session(args.ea_id, &self.context.session_prefix);
        let client = TmuxClient::new(&prefix);
        let sessions = client.list_sessions().unwrap_or_default();
        let mut killed = 0usize;
        for session in sessions {
            if session.name != manager_session {
                let _ = client.kill_session(&session.name);
                killed += 1;
            }
        }
        if client.has_session(&manager_session).unwrap_or(false) {
            let _ = client.kill_session(&manager_session);
            killed += 1;
        }
        let state_dir = ea::ea_state_dir(args.ea_id, &self.context.omar_dir);
        if state_dir.exists() {
            let _ = fs::remove_dir_all(&state_dir);
        }
        let notes_path = memory::manager_notes_path(&self.context.omar_dir, args.ea_id);
        if notes_path.exists() {
            let _ = fs::remove_file(notes_path);
        }
        let events_cancelled = self.scheduler().cancel_by_ea(args.ea_id);
        ea::unregister_ea(&self.context.omar_dir, args.ea_id)?;
        Ok(json!({
            "deleted_ea": args.ea_id,
            "agents_killed": killed,
            "events_cancelled": events_cancelled,
        }))
    }

    fn list_agents(&self) -> Result<Value> {
        let client = self.client()?;
        let manager_session = self.manager_session()?;
        let sessions = client.list_sessions()?;
        let agents: Vec<Value> = sessions
            .iter()
            .filter(|s| s.name != manager_session)
            .map(|s| {
                json!({
                    "id": self.display_name(&s.name).unwrap_or(&s.name),
                    "health": health_from_activity(s.activity, self.context.health_idle_warning),
                    "last_output": last_output_line(&client.capture_pane(&s.name, 50).unwrap_or_default()),
                })
            })
            .collect();
        Ok(json!({ "agents": agents }))
    }

    fn get_agent(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let client = self.client()?;
        let session_name = self.qualified_session_name(&args.name)?;
        let output_tail = client
            .capture_pane(&session_name, 200)
            .map_err(|_| anyhow!("Agent '{}' not found", args.name))?;
        let activity = client.get_pane_activity(&session_name).unwrap_or_default();
        Ok(json!({
            "id": self.display_name(&session_name)?,
            "health": health_from_activity(activity, self.context.health_idle_warning),
            "last_output": last_output_line(&output_tail),
            "output_tail": output_tail,
        }))
    }

    fn get_agent_summary(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let client = self.client()?;
        let session_name = self.qualified_session_name(&args.name)?;
        if !client.has_session(&session_name).unwrap_or(false) {
            return Err(anyhow!("Agent '{}' not found", args.name));
        }
        let short_name = self.display_name(&session_name)?.to_string();
        let task = tasks::find_task_by_agent_in(&state_dir, &short_name);
        let children: Vec<String> = tasks::load_tasks_from(&state_dir)
            .into_iter()
            .filter(|record| {
                record.parent_agent == short_name && record.status == TaskStatus::Running
            })
            .map(|record| record.agent_name)
            .collect();
        Ok(json!({
            "id": short_name,
            "task": task.as_ref().map(|record| record.task_text.clone()),
            "status": task.as_ref().and_then(|record| record.last_status.clone()),
            "children": children,
        }))
    }

    fn update_agent_status(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
            status: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let session_name = self.qualified_session_name(&args.name)?;
        let client = self.client()?;
        if !client.has_session(&session_name).unwrap_or(false) {
            return Err(anyhow!("Agent '{}' not found", args.name));
        }
        memory::save_agent_status_in(&state_dir, &session_name, &args.status);
        if let Some(task) =
            tasks::find_task_by_agent_in(&state_dir, self.display_name(&session_name)?)
        {
            let _ = tasks::update_task_in(&state_dir, &task.task_id, |record| {
                record.last_status = Some(args.status.clone());
                record.updated_at = now_ns();
            })?;
        }
        drop(_lock);
        self.refresh_memory()?;
        Ok(json!({ "status": "updated" }))
    }

    fn spawn_agent_session(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: Option<String>,
            task: Option<String>,
            workdir: Option<String>,
            command: Option<String>,
            backend: Option<String>,
            model: Option<String>,
            role: Option<String>,
            parent: Option<String>,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let lock_wait_start = std::time::Instant::now();
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let spawn_lock_wait_ms = lock_wait_start.elapsed().as_millis() as u64;
        let result = self.spawn_agent_internal(SpawnRequestInternal {
            name: args.name,
            task: args.task,
            workdir: args.workdir,
            command: args.command,
            backend: args.backend,
            model: args.model,
            role: args.role,
            parent: args.parent,
            spawn_lock_wait_ms,
        })?;
        drop(_lock);
        self.refresh_memory()?;
        Ok(result)
    }

    fn spawn_agent_internal(&self, request: SpawnRequestInternal) -> Result<Value> {
        let spawn_start = std::time::Instant::now();
        let ea_id = self.ea_id()?;
        let state_dir = self.state_dir()?;
        let prefix = self.session_prefix()?;
        let manager_session = self.manager_session()?;
        let client = self.client()?;

        let session_name = match request.name.as_deref() {
            Some(n) if !n.trim().is_empty() => {
                let stripped = n.strip_prefix(&prefix).unwrap_or(n);
                format!("{}{}", prefix, stripped)
            }
            _ => generate_agent_name_in_ea(&prefix),
        };
        let short_name = self.display_name(&session_name)?.to_string();
        let parent_session = if let Some(parent) = request.parent {
            if parent == "ea" {
                manager_session.clone()
            } else {
                self.qualified_session_name(&parent)?
            }
        } else {
            manager_session.clone()
        };
        let prompt_parent = if parent_session == manager_session {
            "ea".to_string()
        } else {
            self.display_name(&parent_session)?.to_string()
        };

        if request.backend.is_some() && request.command.is_some() {
            return Err(anyhow!("Cannot specify both 'backend' and 'command'"));
        }

        let mut base_command = if let Some(backend) = request.backend.clone() {
            config::resolve_backend(&backend).map_err(|err| anyhow!(err))?
        } else {
            request
                .command
                .unwrap_or_else(|| self.context.default_command.clone())
        };
        if let Some(model) = request.model.clone() {
            if !model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
            {
                return Err(anyhow!(
                    "Invalid model name. Only alphanumeric, '-', '_', '.', '/' allowed."
                ));
            }
            base_command = format!("{} --model {}", base_command, model);
        }

        let workdir = request
            .workdir
            .unwrap_or_else(|| self.context.default_workdir.clone());
        let has_agent_prompt = matches!(
            request.role.as_deref(),
            Some("project-manager") | Some("agent")
        ) || request.task.is_some();
        let command = if has_agent_prompt {
            let prompt_file = manager::prompts_dir(&self.context.omar_dir).join("agent.md");
            manager::build_agent_command_with_mcp(
                &base_command,
                &prompt_file,
                &[
                    ("{{PARENT_NAME}}", &prompt_parent),
                    ("{{TASK}}", request.task.as_deref().unwrap_or("")),
                    ("{{EA_ID}}", &ea_id.to_string()),
                ],
                Some(&self.context),
            )
        } else {
            base_command.clone()
        };

        if client.has_session(&session_name).unwrap_or(false) {
            return Err(anyhow!("Agent '{}' already exists", short_name));
        }
        let tmux_spawn_start = std::time::Instant::now();
        client.new_session(&session_name, &command, Some(&workdir))?;
        let tmux_spawn_ms = tmux_spawn_start.elapsed().as_millis() as u64;
        let backend_name = infer_backend_name(request.backend.as_deref(), &base_command);
        metrics::record_backend_bootstrap(&backend_name);

        memory::save_agent_parent_in(&state_dir, &session_name, &parent_session);
        if let Some(task_text) = request.task {
            memory::save_worker_task_in(&state_dir, &session_name, &task_text);
            let client2 = client.clone();
            let session2 = session_name.clone();
            let first_message = format!("YOUR NAME: {}\nYOUR TASK: {}", short_name, task_text);
            let backend_name2 = backend_name.clone();
            let readiness_markers = crate::tmux::backend_readiness_markers(&backend_name).to_vec();
            thread::spawn(move || {
                let delivery_start = std::time::Instant::now();
                let _ = if !readiness_markers.is_empty() {
                    client2.wait_for_markers(
                        &session2,
                        &readiness_markers,
                        Duration::from_secs(45),
                        Duration::from_millis(250),
                    );
                    Ok(())
                } else {
                    client2.wait_for_stable(
                        &session2,
                        Duration::from_millis(500),
                        Duration::from_secs(8),
                        Duration::from_millis(120),
                    )
                };
                let opts = DeliveryOptions::default();
                let delivery = client2.deliver_prompt(&session2, &first_message, &opts);
                metrics::record_prompt_delivery(
                    ea_id,
                    &session2,
                    &backend_name2,
                    delivery_start.elapsed().as_millis() as u64,
                    delivery.is_ok(),
                );
            });
        }

        metrics::record_agent_spawn(metrics::AgentSpawnMetric {
            ea_id,
            session: &session_name,
            short_name: &short_name,
            backend: &backend_name,
            has_task: has_agent_prompt,
            spawn_lock_wait_ms: request.spawn_lock_wait_ms,
            tmux_spawn_ms,
            total_spawn_ms: spawn_start.elapsed().as_millis() as u64,
        });

        Ok(json!({
            "id": short_name,
            "session": session_name,
            "status": "running",
        }))
    }

    fn kill_agent(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let client = self.client()?;
        let session_name = self.qualified_session_name(&args.name)?;
        let manager_session = self.manager_session()?;
        if session_name == manager_session {
            return Err(anyhow!("Cannot kill manager via MCP"));
        }
        if !client.has_session(&session_name).unwrap_or(false) {
            return Err(anyhow!("Agent '{}' not found", args.name));
        }
        client.kill_session(&session_name)?;
        memory::remove_agent_parent_in(&state_dir, &session_name);
        let short_name = self.display_name(&session_name)?.to_string();
        let events_cancelled = self
            .scheduler()
            .cancel_by_receiver_and_ea(&short_name, self.ea_id()?);
        drop(_lock);
        self.refresh_memory()?;
        Ok(json!({ "status": "killed", "events_cancelled": events_cancelled }))
    }

    fn send_input(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
            text: String,
            #[serde(default)]
            enter: bool,
        }
        let args: Args = serde_json::from_value(args)?;
        let client = self.client()?;
        let session_name = self.qualified_session_name(&args.name)?;
        if !client.has_session(&session_name).unwrap_or(false) {
            return Err(anyhow!("Agent '{}' not found", args.name));
        }
        client.send_keys_literal(&session_name, &args.text)?;
        if args.enter {
            thread::sleep(Duration::from_millis(100));
            client.send_keys(&session_name, "Enter")?;
        }
        Ok(json!({ "status": "sent" }))
    }

    fn schedule_event(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            receiver: String,
            payload: String,
            sender: Option<String>,
            timestamp_ns: Option<u64>,
            delay_seconds: Option<u64>,
            delay_ns: Option<u64>,
            recurring_seconds: Option<u64>,
            recurring_ns: Option<u64>,
        }
        let args: Args = serde_json::from_value(args)?;
        let base = now_ns();
        let delay_ns = match (args.delay_seconds, args.delay_ns) {
            (Some(seconds), Some(extra_ns)) => seconds
                .saturating_mul(1_000_000_000)
                .saturating_add(extra_ns),
            (Some(seconds), None) => seconds.saturating_mul(1_000_000_000),
            (None, Some(extra_ns)) => extra_ns,
            (None, None) => 0,
        };
        let recurring_ns = match (args.recurring_seconds, args.recurring_ns) {
            (Some(seconds), Some(extra_ns)) => Some(
                seconds
                    .saturating_mul(1_000_000_000)
                    .saturating_add(extra_ns),
            ),
            (Some(seconds), None) => Some(seconds.saturating_mul(1_000_000_000)),
            (None, Some(extra_ns)) => Some(extra_ns),
            (None, None) => None,
        };
        let event = ScheduledEvent {
            id: Uuid::new_v4().to_string(),
            sender: args.sender.unwrap_or_else(|| "ea".to_string()),
            receiver: args.receiver,
            timestamp: args
                .timestamp_ns
                .unwrap_or_else(|| base.saturating_add(delay_ns)),
            payload: args.payload,
            created_at: base,
            recurring_ns,
            ea_id: self.ea_id()?,
        };
        self.scheduler().insert(event.clone());
        self.refresh_memory()?;
        Ok(json!({
            "id": event.id,
            "sender": event.sender,
            "receiver": event.receiver,
            "timestamp_ns": event.timestamp,
            "recurring_ns": event.recurring_ns,
        }))
    }

    fn list_events(&self) -> Result<Value> {
        let mut events = self.scheduler().list_by_ea(self.ea_id()?);
        events.sort_by_key(|event| (event.timestamp, event.created_at));
        Ok(json!({
            "events": events.into_iter().map(|event| {
                json!({
                    "id": event.id,
                    "sender": event.sender,
                    "receiver": event.receiver,
                    "timestamp_ns": event.timestamp,
                    "payload": event.payload,
                    "created_at": event.created_at,
                    "recurring_ns": event.recurring_ns,
                    "ea_id": event.ea_id,
                })
            }).collect::<Vec<_>>()
        }))
    }

    fn cancel_event(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            event_id: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let scheduler = self.scheduler();
        match scheduler.cancel_if_ea(&args.event_id, self.ea_id()?) {
            Ok(event) => {
                self.refresh_memory()?;
                Ok(json!({
                    "id": event.id,
                    "status": "cancelled",
                }))
            }
            Err(true) => Err(anyhow!(
                "Event '{}' belongs to a different EA",
                args.event_id
            )),
            Err(false) => Err(anyhow!("Event '{}' not found", args.event_id)),
        }
    }

    fn list_projects(&self) -> Result<Value> {
        let state_dir = self.state_dir()?;
        let projects: Vec<Value> = projects::load_projects_from(&state_dir)
            .into_iter()
            .map(|project| json!({ "id": project.id, "name": project.name }))
            .collect();
        Ok(json!({ "projects": projects }))
    }

    fn create_task(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            task: String,
            name: Option<String>,
            project_name: Option<String>,
            parent: Option<String>,
            backend: Option<String>,
            model: Option<String>,
            workdir: Option<String>,
        }
        let args: Args = serde_json::from_value(args)?;
        let ea_id = self.ea_id()?;
        let state_dir = self.state_dir()?;
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let project_name = args
            .project_name
            .clone()
            .unwrap_or_else(|| project_name_from_task(&args.task));
        let project_id = projects::add_project_in(&state_dir, &project_name)?;
        let agent = self.spawn_agent_internal(SpawnRequestInternal {
            name: args.name,
            task: Some(args.task.clone()),
            workdir: args.workdir,
            command: None,
            backend: args.backend.clone(),
            model: args.model.clone(),
            role: Some("agent".to_string()),
            parent: args.parent.clone(),
            spawn_lock_wait_ms: 0,
        })?;
        let agent_name = agent
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("agent")
            .to_string();
        let task_id = Uuid::new_v4().to_string();
        let parent_agent = args.parent.unwrap_or_else(|| "ea".to_string());
        tasks::add_task_in(
            &state_dir,
            TaskRecord {
                task_id: task_id.clone(),
                ea_id,
                project_id,
                project_name: project_name.clone(),
                agent_name: agent_name.clone(),
                parent_agent,
                task_text: args.task,
                backend: args.backend,
                model: args.model,
                status: TaskStatus::Running,
                created_at: now_ns(),
                updated_at: now_ns(),
                replacement_count: 0,
                previous_agents: Vec::new(),
                summary: None,
                last_status: None,
            },
        )?;
        drop(_lock);
        self.refresh_memory()?;
        Ok(json!({
            "task_id": task_id,
            "project_id": project_id,
            "project_name": project_name,
            "agent_name": agent_name,
            "status": "running",
        }))
    }

    fn check_task(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            task_id: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let task = tasks::find_task_in(&state_dir, &args.task_id)
            .ok_or_else(|| anyhow!("Task '{}' not found", args.task_id))?;
        let client = self.client()?;
        let session_name = self.qualified_session_name(&task.agent_name)?;
        let agent_exists = client.has_session(&session_name).unwrap_or(false);
        let output_tail = if agent_exists {
            client.capture_pane(&session_name, 200).unwrap_or_default()
        } else {
            String::new()
        };
        let health = if agent_exists {
            let activity = client.get_pane_activity(&session_name).unwrap_or_default();
            Some(health_from_activity(
                activity,
                self.context.health_idle_warning,
            ))
        } else {
            None
        };
        Ok(json!({
            "task_id": task.task_id,
            "agent_name": task.agent_name,
            "project_id": task.project_id,
            "project_name": task.project_name,
            "status": task.status,
            "summary": task.summary,
            "last_status": task.last_status,
            "agent_exists": agent_exists,
            "health": health,
            "ready_to_complete": output_tail.contains("[TASK COMPLETE]"),
            "last_output": last_output_line(&output_tail),
            "output_tail": output_tail,
        }))
    }

    fn complete_task(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            task_id: String,
            summary: Option<String>,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let existing = tasks::find_task_in(&state_dir, &args.task_id)
            .ok_or_else(|| anyhow!("Task '{}' not found", args.task_id))?;
        if existing.status == TaskStatus::Completed {
            return Ok(json!({
                "task_id": existing.task_id,
                "status": "completed",
                "agent_name": existing.agent_name,
                "project_id": existing.project_id,
            }));
        }

        let client = self.client()?;
        let session_name = self.qualified_session_name(&existing.agent_name)?;
        if client.has_session(&session_name).unwrap_or(false) {
            let _ = client.kill_session(&session_name);
        }
        let events_cancelled = self
            .scheduler()
            .cancel_by_receiver_and_ea(&existing.agent_name, self.ea_id()?);
        let _ = projects::remove_project_in(&state_dir, existing.project_id)?;
        memory::remove_agent_parent_in(&state_dir, &session_name);
        let updated = tasks::update_task_in(&state_dir, &args.task_id, |record| {
            record.status = TaskStatus::Completed;
            record.summary = args.summary.clone().or_else(|| record.summary.clone());
            record.updated_at = now_ns();
        })?
        .ok_or_else(|| anyhow!("Task '{}' disappeared during completion", args.task_id))?;
        drop(_lock);
        self.refresh_memory()?;
        Ok(json!({
            "task_id": updated.task_id,
            "status": "completed",
            "agent_name": updated.agent_name,
            "project_id": updated.project_id,
            "summary": updated.summary,
            "events_cancelled": events_cancelled,
        }))
    }

    fn replace_stuck_task_agent(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            task_id: String,
            additional_context: Option<String>,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let _lock = FileLock::acquire(lock_path_for_state_dir(&state_dir))?;
        let task = tasks::find_task_in(&state_dir, &args.task_id)
            .ok_or_else(|| anyhow!("Task '{}' not found", args.task_id))?;
        if task.status == TaskStatus::Completed {
            return Err(anyhow!("Task '{}' is already completed", args.task_id));
        }
        let client = self.client()?;
        let session_name = self.qualified_session_name(&task.agent_name)?;
        if client.has_session(&session_name).unwrap_or(false) {
            let _ = client.kill_session(&session_name);
        }
        self.scheduler()
            .cancel_by_receiver_and_ea(&task.agent_name, self.ea_id()?);

        let new_task_text = match args.additional_context {
            Some(extra) if !extra.trim().is_empty() => {
                format!("{}\n\nAdditional context:\n{}", task.task_text, extra)
            }
            _ => task.task_text.clone(),
        };
        let _ = self.spawn_agent_internal(SpawnRequestInternal {
            name: Some(task.agent_name.clone()),
            task: Some(new_task_text.clone()),
            workdir: Some(self.context.default_workdir.clone()),
            command: None,
            backend: task.backend.clone(),
            model: task.model.clone(),
            role: Some("agent".to_string()),
            parent: Some(task.parent_agent.clone()),
            spawn_lock_wait_ms: 0,
        })?;

        let updated = tasks::update_task_in(&state_dir, &args.task_id, |record| {
            record.status = TaskStatus::Running;
            record.task_text = new_task_text.clone();
            record.replacement_count += 1;
            record.updated_at = now_ns();
        })?
        .ok_or_else(|| anyhow!("Task '{}' disappeared during replacement", args.task_id))?;
        drop(_lock);
        self.refresh_memory()?;
        Ok(json!({
            "task_id": updated.task_id,
            "status": "running",
            "agent_name": updated.agent_name,
            "replacement_count": updated.replacement_count,
        }))
    }

    fn append_manager_note(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            text: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let ea_id = self.ea_id()?;
        let path = memory::manager_notes_path(&self.context.omar_dir, ea_id);
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut out = existing;
        if !out.trim().is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("[{}]\n{}\n", now_rfc3339(), args.text.trim()));
        fs::write(&path, out)?;
        Ok(json!({ "status": "appended", "path": path }))
    }

    fn log_action(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent_name: String,
            action: String,
            justification: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let state_dir = self.state_dir()?;
        let path = state_dir.join("action_log.jsonl");
        fs::create_dir_all(&state_dir).ok();
        let line = serde_json::to_string(&json!({
            "timestamp": now_rfc3339(),
            "ea_id": self.ea_id()?,
            "agent_name": args.agent_name,
            "action": args.action,
            "justification": args.justification,
        }))?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{}", line)?;
        Ok(json!({ "status": "logged", "path": path }))
    }

    fn computer_lock_path(&self) -> PathBuf {
        self.context.omar_dir.join("computer.lock")
    }

    fn read_computer_holder(&self) -> Option<String> {
        fs::read_to_string(self.computer_lock_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn computer_status(&self) -> Result<Value> {
        let xdotool = computer::is_available();
        let screenshot = computer::is_screenshot_available();
        let screen_size = computer::get_screen_size().ok().map(|size| {
            json!({
                "width": size.width,
                "height": size.height,
            })
        });
        Ok(json!({
            "available": xdotool && screenshot && screen_size.is_some(),
            "xdotool": xdotool,
            "screenshot": screenshot,
            "display": screen_size.is_some(),
            "screenshot_ready": screenshot && screen_size.is_some(),
            "screen_size": screen_size,
            "held_by": self.read_computer_holder(),
        }))
    }

    fn computer_lock_acquire(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let owner = format!("{}:{}", self.ea_id()?, args.agent);
        let path = self.computer_lock_path();
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                write!(file, "{}", owner)?;
                Ok(json!({ "status": "acquired", "held_by": owner }))
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let held_by = self.read_computer_holder();
                if held_by.as_deref() == Some(owner.as_str()) {
                    Ok(json!({ "status": "already_held", "held_by": owner }))
                } else {
                    Err(anyhow!(
                        "Computer is locked by '{}'",
                        held_by.unwrap_or_else(|| "unknown".to_string())
                    ))
                }
            }
            Err(err) => Err(err).context("Failed to acquire computer lock"),
        }
    }

    fn computer_lock_release(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
        }
        let args: Args = serde_json::from_value(args)?;
        let owner = format!("{}:{}", self.ea_id()?, args.agent);
        let held_by = self.read_computer_holder();
        match held_by {
            Some(ref holder) if *holder == owner => {
                let _ = fs::remove_file(self.computer_lock_path());
                Ok(json!({ "status": "released" }))
            }
            Some(holder) => Err(anyhow!("Lock held by '{}', not '{}'", holder, owner)),
            None => Ok(json!({ "status": "not_held" })),
        }
    }

    fn verify_computer_lock(&self, agent: &str) -> Result<()> {
        let expected = format!("{}:{}", self.ea_id()?, agent);
        match self.read_computer_holder() {
            Some(holder) if holder == expected => Ok(()),
            Some(holder) => Err(anyhow!("Computer is locked by '{}'", holder)),
            None => Err(anyhow!("You must acquire the computer lock first")),
        }
    }

    fn computer_screenshot(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
            max_width: Option<u32>,
            max_height: Option<u32>,
        }
        let args: Args = serde_json::from_value(args)?;
        self.verify_computer_lock(&args.agent)?;
        let image_base64 = if let (Some(w), Some(h)) = (args.max_width, args.max_height) {
            computer::take_screenshot_resized(w, h)?
        } else {
            computer::take_screenshot()?
        };
        let size = computer::get_screen_size().unwrap_or(computer::ScreenSize {
            width: 0,
            height: 0,
        });
        Ok(json!({
            "image_base64": image_base64,
            "width": size.width,
            "height": size.height,
            "format": "png",
        }))
    }

    fn computer_mouse(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
            action: String,
            x: i32,
            y: i32,
            button: Option<u8>,
            to_x: Option<i32>,
            to_y: Option<i32>,
            scroll_direction: Option<String>,
            scroll_amount: Option<u32>,
        }
        let args: Args = serde_json::from_value(args)?;
        self.verify_computer_lock(&args.agent)?;
        let button = args.button.unwrap_or(1);
        match args.action.as_str() {
            "move" => computer::mouse_move(args.x, args.y)?,
            "click" => computer::mouse_click(args.x, args.y, button)?,
            "double_click" => computer::mouse_double_click(args.x, args.y, button)?,
            "drag" => computer::mouse_drag(
                args.x,
                args.y,
                args.to_x.ok_or_else(|| anyhow!("drag requires to_x"))?,
                args.to_y.ok_or_else(|| anyhow!("drag requires to_y"))?,
                button,
            )?,
            "scroll" => computer::mouse_scroll(
                args.x,
                args.y,
                args.scroll_direction
                    .as_deref()
                    .ok_or_else(|| anyhow!("scroll requires scroll_direction"))?,
                args.scroll_amount.unwrap_or(3),
            )?,
            other => {
                return Err(anyhow!(
                    "Unknown mouse action '{}'. Use move/click/double_click/drag/scroll",
                    other
                ));
            }
        }
        Ok(json!({ "status": "ok" }))
    }

    fn computer_keyboard(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
            action: String,
            text: String,
        }
        let args: Args = serde_json::from_value(args)?;
        self.verify_computer_lock(&args.agent)?;
        match args.action.as_str() {
            "type" => computer::type_text(&args.text)?,
            "key" => computer::key_press(&args.text)?,
            other => return Err(anyhow!("Unknown keyboard action '{}'. Use type/key", other)),
        }
        Ok(json!({ "status": "ok" }))
    }

    fn computer_screen_size(&self) -> Result<Value> {
        let size = computer::get_screen_size()?;
        Ok(json!({ "width": size.width, "height": size.height }))
    }

    fn computer_mouse_position(&self) -> Result<Value> {
        let (x, y) = computer::get_mouse_position()?;
        Ok(json!({ "x": x, "y": y }))
    }
}

fn generate_agent_name_in_ea(prefix: &str) -> String {
    for i in 1..1000 {
        let name = format!("{}{}", prefix, i);
        let result = Command::new("tmux")
            .args(["has-session", "-t", &name])
            .output();
        match result {
            Ok(output) if !output.status.success() => return name,
            _ => continue,
        }
    }
    format!("{}{}", prefix, &Uuid::new_v4().to_string()[..8])
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<(JsonRpcRequest, MessageFraming)>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed_line = line.trim();
        // Some clients (including current Claude CLI) send line-delimited JSON-RPC
        // over stdio instead of Content-Length framing.
        if trimmed_line.starts_with('{') {
            let request = serde_json::from_str::<JsonRpcRequest>(trimmed_line)
                .context("Invalid JSON line MCP request")?;
            return Ok(Some((request, MessageFraming::JsonLine)));
        }
        if line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("Invalid Content-Length header")?,
                );
            }
        }
    }

    let length = content_length.ok_or_else(|| anyhow!("Missing Content-Length header"))?;
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok(Some((
        serde_json::from_slice(&payload)?,
        MessageFraming::ContentLength,
    )))
}

fn write_message(
    writer: &mut impl Write,
    response: &JsonRpcResponse,
    framing: MessageFraming,
) -> Result<()> {
    let payload = serde_json::to_vec(response)?;
    match framing {
        MessageFraming::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", payload.len())?;
            writer.write_all(&payload)?;
        }
        MessageFraming::JsonLine => {
            writer.write_all(&payload)?;
            writer.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Value, code: i64, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION,
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

fn tool_success(value: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        }],
        "structuredContent": value,
        "isError": false,
    })
}

fn tool_error(err: anyhow::Error) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": err.to_string(),
        }],
        "isError": true,
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "list_backends",
            "List installed OMAR backends.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "list_eas",
            "List registered EAs.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "create_ea",
            "Create a new EA.",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string"},
                    "description":{"type":"string"}
                },
                "required":["name"]
            }),
        ),
        tool(
            "delete_ea",
            "Delete an EA and its sessions.",
            json!({
                "type":"object",
                "properties":{"ea_id":{"type":"integer"}},
                "required":["ea_id"]
            }),
        ),
        tool(
            "list_agents",
            "List agents in the current EA.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "get_agent",
            "Get detailed agent output.",
            json!({
                "type":"object",
                "properties":{"name":{"type":"string"}},
                "required":["name"]
            }),
        ),
        tool(
            "get_agent_summary",
            "Get agent task and child summary.",
            json!({
                "type":"object",
                "properties":{"name":{"type":"string"}},
                "required":["name"]
            }),
        ),
        tool(
            "update_agent_status",
            "Update self-reported agent status.",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string"},
                    "status":{"type":"string"}
                },
                "required":["name","status"]
            }),
        ),
        tool(
            "spawn_agent_session",
            "Spawn a raw agent/demo session. Prefer create_task for tracked work.",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string"},
                    "task":{"type":"string"},
                    "workdir":{"type":"string"},
                    "command":{"type":"string"},
                    "backend":{"type":"string"},
                    "model":{"type":"string"},
                    "role":{"type":"string"},
                    "parent":{"type":"string"}
                }
            }),
        ),
        tool(
            "kill_agent",
            "Kill an agent session.",
            json!({
                "type":"object",
                "properties":{"name":{"type":"string"}},
                "required":["name"]
            }),
        ),
        tool(
            "send_input",
            "Send input to an agent/demo session.",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string"},
                    "text":{"type":"string"},
                    "enter":{"type":"boolean"}
                },
                "required":["name","text"]
            }),
        ),
        tool(
            "list_projects",
            "List projects in the current EA.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "schedule_event",
            "Schedule a wake-up or message for an agent or the EA.",
            json!({
                "type":"object",
                "properties":{
                    "receiver":{"type":"string"},
                    "payload":{"type":"string"},
                    "sender":{"type":"string"},
                    "timestamp_ns":{"type":"integer"},
                    "delay_seconds":{"type":"integer"},
                    "delay_ns":{"type":"integer"},
                    "recurring_seconds":{"type":"integer"},
                    "recurring_ns":{"type":"integer"}
                },
                "required":["receiver","payload"]
            }),
        ),
        tool(
            "list_events",
            "List scheduled events in the current EA.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "cancel_event",
            "Cancel a scheduled event by id.",
            json!({
                "type":"object",
                "properties":{"event_id":{"type":"string"}},
                "required":["event_id"]
            }),
        ),
        tool(
            "create_task",
            "Create a tracked task: add project, spawn worker, and persist lifecycle state.",
            json!({
                "type":"object",
                "properties":{
                    "task":{"type":"string"},
                    "name":{"type":"string"},
                    "project_name":{"type":"string"},
                    "parent":{"type":"string"},
                    "backend":{"type":"string"},
                    "model":{"type":"string"},
                    "workdir":{"type":"string"}
                },
                "required":["task"]
            }),
        ),
        tool(
            "check_task",
            "Inspect tracked task state.",
            json!({
                "type":"object",
                "properties":{"task_id":{"type":"string"}},
                "required":["task_id"]
            }),
        ),
        tool(
            "complete_task",
            "Complete a tracked task atomically.",
            json!({
                "type":"object",
                "properties":{
                    "task_id":{"type":"string"},
                    "summary":{"type":"string"}
                },
                "required":["task_id"]
            }),
        ),
        tool(
            "replace_stuck_task_agent",
            "Replace the worker behind a tracked task.",
            json!({
                "type":"object",
                "properties":{
                    "task_id":{"type":"string"},
                    "additional_context":{"type":"string"}
                },
                "required":["task_id"]
            }),
        ),
        tool(
            "append_manager_note",
            "Append text to the current EA manager notes file.",
            json!({
                "type":"object",
                "properties":{"text":{"type":"string"}},
                "required":["text"]
            }),
        ),
        tool(
            "log_action",
            "Write a structured action log entry.",
            json!({
                "type":"object",
                "properties":{
                    "agent_name":{"type":"string"},
                    "action":{"type":"string"},
                    "justification":{"type":"string"}
                },
                "required":["agent_name","action","justification"]
            }),
        ),
        tool(
            "computer_status",
            "Check desktop control availability.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "computer_lock_acquire",
            "Acquire the computer control lock.",
            json!({
                "type":"object",
                "properties":{"agent":{"type":"string"}},
                "required":["agent"]
            }),
        ),
        tool(
            "computer_lock_release",
            "Release the computer control lock.",
            json!({
                "type":"object",
                "properties":{"agent":{"type":"string"}},
                "required":["agent"]
            }),
        ),
        tool(
            "computer_screenshot",
            "Take a screenshot while holding the lock.",
            json!({
                "type":"object",
                "properties":{
                    "agent":{"type":"string"},
                    "max_width":{"type":"integer"},
                    "max_height":{"type":"integer"}
                },
                "required":["agent"]
            }),
        ),
        tool(
            "computer_mouse",
            "Perform a mouse action while holding the lock.",
            json!({
                "type":"object",
                "properties":{
                    "agent":{"type":"string"},
                    "action":{"type":"string"},
                    "x":{"type":"integer"},
                    "y":{"type":"integer"},
                    "button":{"type":"integer"},
                    "to_x":{"type":"integer"},
                    "to_y":{"type":"integer"},
                    "scroll_direction":{"type":"string"},
                    "scroll_amount":{"type":"integer"}
                },
                "required":["agent","action","x","y"]
            }),
        ),
        tool(
            "computer_keyboard",
            "Perform a keyboard action while holding the lock.",
            json!({
                "type":"object",
                "properties":{
                    "agent":{"type":"string"},
                    "action":{"type":"string"},
                    "text":{"type":"string"}
                },
                "required":["agent","action","text"]
            }),
        ),
        tool(
            "computer_screen_size",
            "Read screen size.",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "computer_mouse_position",
            "Read mouse position.",
            json!({"type":"object","properties":{}}),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_context() -> McpLaunchContext {
        McpLaunchContext {
            omar_dir: std::env::temp_dir().join(format!("omar-mcp-test-{}", Uuid::new_v4())),
            ea_id: 0,
            session_prefix: "omar-test-".to_string(),
            default_command: "claude --dangerously-skip-permissions".to_string(),
            default_workdir: ".".to_string(),
            health_idle_warning: 15,
        }
    }

    #[test]
    fn initialize_response_includes_server_instructions() {
        let server = OmarMcpServer::new(test_context());
        let response = server
            .handle_request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: Some(json!(1)),
                method: "initialize".to_string(),
                params: json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1.0"},
                }),
            })
            .expect("initialize should produce a response");

        let result = response.result.expect("initialize should return a result");
        assert_eq!(result["instructions"], json!(SERVER_INSTRUCTIONS));
        assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
    }

    #[test]
    fn read_message_accepts_lf_terminated_lowercase_content_length() {
        let payload = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "params": {}
        }))
        .unwrap();
        let input = format!(
            "content-length: {}\ncontent-type: application/json\n\n{}",
            payload.len(),
            String::from_utf8(payload).unwrap()
        );
        let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
        let request = read_message(&mut reader).expect("message should parse");
        let (request, framing) = request.expect("message should be present");
        assert_eq!(request.method, "ping");
        assert_eq!(request.id, Some(json!(1)));
        assert_eq!(framing, MessageFraming::ContentLength);
    }

    #[test]
    fn read_message_accepts_single_line_json_request() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{}}\n";
        let mut reader = BufReader::new(Cursor::new(input.as_slice()));
        let request = read_message(&mut reader).expect("message should parse");
        let (request, framing) = request.expect("message should be present");
        assert_eq!(request.method, "initialize");
        assert_eq!(request.id, Some(json!(0)));
        assert_eq!(framing, MessageFraming::JsonLine);
    }
}
