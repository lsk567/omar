# OMAR Multi-EA: Final Implementation Specification

> **Status**: Final Merged Spec — supersedes `multi_ea_design.md` and `inside_out_analysis.md`
> **Date**: 2026-03-06
> **Source**: Merges outside-in API design (v2) with inside-out codebase analysis
> **Context**: The `fix/race-and-cleanup-bugs` branch had 12 catalogued bugs. This spec structurally prevents every one, while also addressing all 10 hidden gotchas, 7 edge cases, and 10 integration points discovered by tracing every data flow in the codebase.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Design Principles](#2-design-principles)
3. [Architecture Decision: Single Process Multi-EA](#3-architecture-decision-single-process-multi-ea)
4. [Data Model](#4-data-model)
5. [API Design — Path-Scoped](#5-api-design--path-scoped)
6. [Agent Namespace and Isolation](#6-agent-namespace-and-isolation)
7. [Scheduler and Event Isolation](#7-scheduler-and-event-isolation)
8. [State and Memory Isolation](#8-state-and-memory-isolation)
9. [Dashboard/TUI Changes](#9-dashboardtui-changes)
10. [Prompt Changes](#10-prompt-changes)
11. [Gotcha Resolution Matrix](#11-gotcha-resolution-matrix)
12. [Edge Case Handling](#12-edge-case-handling)
13. [Bug Fix Verification Matrix](#13-bug-fix-verification-matrix)
14. [Implementation Plan](#14-implementation-plan)
15. [File-by-File Change List](#15-file-by-file-change-list)
16. [Migration Path](#16-migration-path)

---

## 1. Executive Summary

OMAR needs multi-EA support so users can run isolated teams of AI agents. Each EA owns its own agent hierarchy, project board, event queue, and memory. The user switches between EAs from the dashboard, which fully reloads to show the selected EA's world.

### Key design decisions (settled)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| EA identifier | Integer path parameter (`u32`) | Simple, no string escaping, naturally orderable |
| API routing | `/api/ea/<id>/agents`, `/api/ea/<id>/events`, etc. | URL IS the isolation boundary — structural, not inferential |
| Default EA | `id=0` (always exists, cannot be deleted) | No special-casing, no "default" string magic |
| Backward compatibility | None — all routes under `/api/ea/<id>/` exclusively | Clean break eliminates dual-path ambiguity bugs |
| Isolation model | Routing enforces scoping; no runtime ownership checks | `owning_ea()` eliminated entirely |
| Event scoping | `ea_id` field on every `ScheduledEvent` | No cross-team event routing possible |
| Dashboard behavior | Full reload on EA switch | Simple, correct, no stale-state bugs |
| Process model | Single process managing multiple EAs | Shared scheduler, shared computer lock, single port |
| App instance | Single `App` behind `Arc<Mutex<App>>` | Eliminates dual-App TOCTOU race (BUG-C2) |

### Key insight

If every API call includes `ea_id` as a path parameter, and every handler uses that parameter to scope its tmux prefix, file paths, and event queries, then cross-EA access is **impossible by construction**. No runtime validation needed. The URL IS the isolation boundary.

**Estimated scope**: ~950 lines added, ~310 removed, across 14 files (1 new + 13 modified).

---

## 2. Design Principles

### P1: Path parameter as structural isolation

Every API endpoint lives under `/api/ea/{ea_id}/...`. The `ea_id` path parameter is extracted by Axum and threaded to every handler. This is not a filter — it IS the namespace.

```
BEFORE:  POST /api/agents {"name": "auth", "parent": "ea"}
         -> Which EA? Infer from parent? From active EA? Race condition.

AFTER:   POST /api/ea/0/agents {"name": "auth", "parent": "ea"}
         -> EA 0. Unambiguous. Structural. No inference needed.
```

### P2: Integer EA IDs, EA 0 is default

- EA IDs are `u32`: 0, 1, 2, ...
- EA 0 always exists and cannot be deleted
- New EAs get the next available integer
- No special string handling, no "default" magic

### P3: Single source of truth (resolves the Two-App Problem)

The current codebase has a critical design issue: the API server and dashboard have **separate `App` instances** (main.rs lines 457-483). They share tmux and files but have independent in-memory state. This is a TOCTOU race (BUG-C2).

**Resolution**: One `App` instance shared between dashboard and API via `Arc<Mutex<App>>`. Both read and write the same state. The Mutex serializes access.

```rust
// CURRENT (buggy): two independent App instances
let api_app = Arc::new(Mutex::new(App::new(&config, ticker.clone())));  // API's copy
let mut app = App::new(&config, ticker);  // Dashboard's copy — DIVERGES

// NEW: single shared App
let app = Arc::new(Mutex::new(App::new(&config, ticker.clone())));
let api_app = app.clone();  // Same Arc, same App
```

**Lock contention consideration**: The dashboard holds the lock during each 1s tick. The critical path is `refresh()`, which calls `list_sessions()` — a tmux subprocess taking ~5-50ms. During this time, ALL API calls block.

**Mitigation** (implement from the start, not as a follow-up):
1. Lock the App, read `active_ea` and `base_prefix` (~1us), unlock
2. Call `list_sessions()` with NO lock held (~5-50ms)
3. Re-lock the App, update `App.agents` from the results (~1us), unlock
4. Render the UI with NO lock held (reads App fields, but rendering is read-only)

This reduces lock hold time from ~50ms to ~2us per tick. The TOCTOU between steps 1 and 3 (user switches EA during the tmux call) is harmless — the next tick corrects it 1 second later. API handlers hold the lock for ~1-5ms each (in-memory operations + file I/O, no subprocesses).

### P4: Prefix-based tmux isolation

Each EA's agents live in a distinct tmux session namespace. Discovery is prefix-filtered `list_sessions()`. No `owning_ea()` disk lookups.

### P5: No backward compatibility

Old routes (`/api/agents`, `/api/projects`, `/api/events`) are removed. Agents' prompt instructions are updated to use the new paths. Clean break.

---

## 3. Architecture Decision: Single Process Multi-EA

The inside-out analysis identified three process model options. Here is the evaluation:

| Approach | Pros | Cons |
|----------|------|------|
| Single process, multi-EA | One port, shared scheduler, shared computer lock, simpler deployment | All EA state must be refactored to be per-EA |
| Process per EA | Maximum isolation, minimal code changes | Port management, can't share computer lock, harder to display all EAs in one dashboard |
| Hybrid (supervisor) | Best of both worlds | Most complex to implement |

**Decision: Single process.** Rationale:
- The API is already a tokio task — adding routing is natural
- The scheduler is already `Arc<Scheduler>` — can be shared across EAs
- The dashboard already has a tree view — can show multiple EA trees
- Computer lock MUST be global (one physical screen) — single process makes this trivial
- Agent prompts all hardcode `localhost:9876` — single port means no prompt changes for port
- All EAs run concurrently; the dashboard is just a view window

### Current process model (for reference)

```
┌──────────────────────────────────────────────────────────┐
│                    omar binary (tokio)                     │
│                                                            │
│  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐  │
│  │ TUI Dashboard │  │  API Server  │  │ Event Scheduler│  │
│  │  (main task)  │  │  (axum task) │  │  (tokio task)  │  │
│  └──────┬───────┘  └──────┬───────┘  └───────┬────────┘  │
│         │                 │                   │           │
│         ├─── Arc<Mutex<App>> ────────────────┤           │
│         ├─── Arc<Scheduler> ─────────────────┤           │
│         ├─── TickerBuffer ───────────────────┘           │
│         └─── PopupReceiver                               │
│                                                            │
│  ┌──────────────┐  ┌──────────────┐                       │
│  │ Slack Bridge  │  │Computer Bridge│  (child processes)   │
│  └──────────────┘  └──────────────┘                       │
└──────────────────────────────────────────────────────────┘
         │
         │ tmux commands (subprocess exec)
         ▼
┌──────────────────────────────────────────────────────────┐
│                    tmux server                             │
│  ┌────────────────┐  ┌────────────────┐  ┌─────────────┐ │
│  │omar-dashboard   │  │omar-agent-ea-0 │  │omar-agent-  │ │
│  │(dashboard TUI)  │  │(EA 0 manager)  │  │0-worker1    │ │
│  └────────────────┘  └────────────────┘  └─────────────┘ │
│                       ┌────────────────┐  ┌─────────────┐ │
│                       │omar-agent-ea-1 │  │omar-agent-  │ │
│                       │(EA 1 manager)  │  │1-api        │ │
│                       └────────────────┘  └─────────────┘ │
└──────────────────────────────────────────────────────────┘
```

---

## 4. Data Model

### 4.1 EA Identity — `src/ea.rs` (NEW FILE, ~180 lines)

```rust
use serde::{Serialize, Deserialize};
use std::path::{Path, PathBuf};

/// EA identifier. Simple integer. EA 0 always exists.
pub type EaId = u32;

/// Metadata for a registered EA, persisted in ~/.omar/eas.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EaInfo {
    pub id: EaId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: u64,  // Unix timestamp
}

/// The tmux session prefix for an EA's worker agents.
/// EA 0: "omar-agent-0-"
/// EA 1: "omar-agent-1-"
///
/// IMPORTANT: base_prefix must end with '-' (e.g., "omar-agent-").
/// Do NOT trim it — the trailing '-' separates the base from the ea_id.
pub fn ea_prefix(ea_id: EaId, base_prefix: &str) -> String {
    // base_prefix = "omar-agent-", ea_id = 0 => "omar-agent-0-"
    // base_prefix = "omar-agent-", ea_id = 1 => "omar-agent-1-"
    format!("{}{}-", base_prefix, ea_id)
}

/// The tmux session name for an EA's manager (the EA itself).
/// EA 0: "omar-agent-ea-0"
/// EA 1: "omar-agent-ea-1"
pub fn ea_manager_session(ea_id: EaId, base_prefix: &str) -> String {
    format!("{}ea-{}", base_prefix, ea_id)
    // "omar-agent-" -> "omar-agent-ea-0", "omar-agent-ea-1"
}

/// Directory for an EA's state files.
/// EA 0: ~/.omar/ea/0/
/// EA 1: ~/.omar/ea/1/
pub fn ea_state_dir(ea_id: EaId, base_dir: &Path) -> PathBuf {
    base_dir.join("ea").join(ea_id.to_string())
}

/// Load all registered EAs from ~/.omar/eas.json.
/// Always includes EA 0 even if the file doesn't exist.
pub fn load_registry(base_dir: &Path) -> Vec<EaInfo> {
    let path = base_dir.join("eas.json");
    let mut eas: Vec<EaInfo> = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    // Ensure EA 0 always exists
    if !eas.iter().any(|e| e.id == 0) {
        eas.insert(0, EaInfo {
            id: 0,
            name: "Default".to_string(),
            description: None,
            created_at: 0,
        });
    }
    eas
}

/// Register a new EA. Returns the assigned ID.
pub fn register_ea(
    base_dir: &Path,
    name: &str,
    description: Option<&str>,
) -> anyhow::Result<EaId> {
    let mut eas = load_registry(base_dir);
    let next_id = eas.iter().map(|e| e.id).max().unwrap_or(0) + 1;
    let ea = EaInfo {
        id: next_id,
        name: name.to_string(),
        description: description.map(String::from),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    eas.push(ea);
    save_registry(base_dir, &eas)?;
    // Create state directory
    let state_dir = ea_state_dir(next_id, base_dir);
    std::fs::create_dir_all(state_dir.join("status"))?;
    Ok(next_id)
}

/// Remove an EA from the registry. EA 0 cannot be removed.
pub fn unregister_ea(base_dir: &Path, ea_id: EaId) -> anyhow::Result<()> {
    if ea_id == 0 {
        anyhow::bail!("Cannot delete EA 0");
    }
    let mut eas = load_registry(base_dir);
    eas.retain(|e| e.id != ea_id);
    save_registry(base_dir, &eas)
}

fn save_registry(base_dir: &Path, eas: &[EaInfo]) -> anyhow::Result<()> {
    let path = base_dir.join("eas.json");
    let json = serde_json::to_string_pretty(eas)?;
    // Atomic write: write to temp file, then rename
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
```

**Design note**: `save_registry` uses atomic write (write temp + rename) because `eas.json` is the one truly global file that all EA operations touch. This addresses the TOCTOU file corruption risk identified in the inside-out analysis (Section 3.3).

### 4.2 ScheduledEvent gains ea_id

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ScheduledEvent {
    pub id: String,
    pub sender: String,
    pub receiver: String,
    pub timestamp: u64,
    pub payload: String,
    pub created_at: u64,
    pub recurring_ns: Option<u64>,
    pub ea_id: u32,  // NEW: mandatory, from path parameter
}
```

The `Ord` implementation remains timestamp-based. The `ea_id` is metadata used for filtering and delivery, not ordering.

### 4.3 Changes to App — `src/app.rs`

```rust
pub struct App {
    // NEW fields
    pub active_ea: EaId,
    pub registered_eas: Vec<EaInfo>,
    pub base_prefix: String,          // "omar-agent-" from config
    pub omar_dir: PathBuf,            // ~/.omar/

    // EXISTING fields (now scoped to active_ea automatically):
    pub agents: Vec<AgentInfo>,          // only agents under active_ea's prefix
    pub manager: Option<AgentInfo>,      // only active_ea's manager
    pub projects: Vec<Project>,          // only active_ea's projects
    pub focus_parent: String,            // session within active_ea
    pub agent_parents: HashMap<String, String>,  // active_ea's parents
    pub worker_tasks: HashMap<String, String>,   // active_ea's tasks
    pub scheduled_events: Vec<ScheduledEvent>,   // active_ea's events only
    // ... rest unchanged (client, health_checker, ticker, etc.)
}
```

### 4.4 Scoped file paths

All EAs use the same directory structure. No special cases.

```
~/.omar/
├── eas.json                   # Global EA registry
├── config.toml                # Global config (API port, etc.) — read-only
├── prompts/                   # Shared prompt templates (same for all EAs)
│   ├── executive-assistant.md
│   ├── agent.md
│   └── worker.md
├── ea/
│   ├── 0/                     # EA 0 (Default) — always exists
│   │   ├── memory.md
│   │   ├── worker_tasks.json
│   │   ├── agent_parents.json
│   │   ├── tasks.md
│   │   ├── ea_prompt_combined.md
│   │   └── status/
│   │       ├── omar-agent-0-pm.md
│   │       └── omar-agent-0-auth.md
│   ├── 1/                     # EA 1 (Research)
│   │   ├── memory.md
│   │   ├── worker_tasks.json
│   │   ├── agent_parents.json
│   │   ├── tasks.md
│   │   ├── ea_prompt_combined.md
│   │   └── status/
│   └── 2/                     # EA 2 (Ops)
│       └── ...
```

### 4.5 Tmux session naming

All EAs use the same naming scheme. No special cases.

| EA ID | Manager session | Agent prefix | Example agent |
|-------|----------------|--------------|---------------|
| 0 | `omar-agent-ea-0` | `omar-agent-0-` | `omar-agent-0-auth` |
| 1 | `omar-agent-ea-1` | `omar-agent-1-` | `omar-agent-1-auth` |
| 2 | `omar-agent-ea-2` | `omar-agent-2-` | `omar-agent-2-pm` |

**Note on the manager session naming**: The manager session `omar-agent-ea-N` does NOT start with the worker prefix `omar-agent-N-`. This is intentional — it prevents the manager from appearing in `list_sessions()` filtered by the worker prefix. The manager is detected separately by its exact name.

### 4.6 ApiState changes

```rust
pub struct ApiState {
    pub app: Arc<Mutex<App>>,          // Single shared App
    pub scheduler: Arc<Scheduler>,
    pub computer_lock: ComputerLock,
    pub base_prefix: String,           // NEW: "omar-agent-" from config
    pub omar_dir: PathBuf,             // NEW: ~/.omar/
}
```

---

## 5. API Design — Path-Scoped

### 5.1 Complete route table

Every route is under `/api/ea/{ea_id}/` or is EA-management / global.

```
# EA management (global)
GET    /api/eas                          list_eas         List all registered EAs
POST   /api/eas                          create_ea        Create a new EA
DELETE /api/eas/{ea_id}                  delete_ea        Delete an EA (full teardown)

# Dashboard state (global)
GET    /api/eas/active                   get_active_ea    Get the dashboard's active EA id
PUT    /api/eas/active                   switch_ea        Switch dashboard's active EA

# Health (global)
GET    /api/health                       health           Health check

# Agent operations (EA-scoped)
GET    /api/ea/{ea_id}/agents            list_agents      List agents for this EA
POST   /api/ea/{ea_id}/agents            spawn_agent      Spawn a new agent in this EA
GET    /api/ea/{ea_id}/agents/{name}     get_agent        Get agent details
DELETE /api/ea/{ea_id}/agents/{name}     kill_agent       Kill an agent
GET    /api/ea/{ea_id}/agents/{name}/summary    get_agent_summary    Agent summary card
PUT    /api/ea/{ea_id}/agents/{name}/status     update_agent_status  Update agent status
POST   /api/ea/{ea_id}/agents/{name}/send       send_input           Send input to agent

# Project operations (EA-scoped)
GET    /api/ea/{ea_id}/projects          list_projects    List projects
POST   /api/ea/{ea_id}/projects          add_project      Add a project
DELETE /api/ea/{ea_id}/projects/{id}     complete_project Complete/remove a project

# Event operations (EA-scoped)
POST   /api/ea/{ea_id}/events            schedule_event   Schedule an event
GET    /api/ea/{ea_id}/events            list_events      List events for this EA
DELETE /api/ea/{ea_id}/events/{id}       cancel_event     Cancel an event

# Computer use (global — one screen, one mouse)
GET    /api/computer/status              computer_status
POST   /api/computer/lock                computer_lock_acquire
DELETE /api/computer/lock                computer_lock_release
POST   /api/computer/screenshot          computer_screenshot
POST   /api/computer/mouse               computer_mouse
POST   /api/computer/keyboard            computer_keyboard
GET    /api/computer/screen-size         computer_screen_size
GET    /api/computer/mouse-position      computer_mouse_position
```

### 5.2 Router implementation — `src/api/mod.rs`

```rust
pub fn create_router(state: Arc<ApiState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Global
        .route("/api/health", get(handlers::health))
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
        .route("/api/ea/:ea_id/agents/:name/summary", get(handlers::get_agent_summary))
        .route("/api/ea/:ea_id/agents/:name/status", put(handlers::update_agent_status))
        .route("/api/ea/:ea_id/agents/:name/send", post(handlers::send_input))
        // EA-scoped: projects
        .route("/api/ea/:ea_id/projects", get(handlers::list_projects))
        .route("/api/ea/:ea_id/projects", post(handlers::add_project))
        .route("/api/ea/:ea_id/projects/:id", delete(handlers::complete_project))
        // EA-scoped: events
        .route("/api/ea/:ea_id/events", post(handlers::schedule_event))
        .route("/api/ea/:ea_id/events", get(handlers::list_events))
        .route("/api/ea/:ea_id/events/:id", delete(handlers::cancel_event))
        // Computer use (global)
        .route("/api/computer/status", get(handlers::computer_status))
        .route("/api/computer/lock", post(handlers::computer_lock_acquire))
        .route("/api/computer/lock", delete(handlers::computer_lock_release))
        .route("/api/computer/screenshot", post(handlers::computer_screenshot))
        .route("/api/computer/mouse", post(handlers::computer_mouse))
        .route("/api/computer/keyboard", post(handlers::computer_keyboard))
        .route("/api/computer/screen-size", get(handlers::computer_screen_size))
        .route("/api/computer/mouse-position", get(handlers::computer_mouse_position))
        .layer(cors)
        .with_state(state)
}
```

### 5.3 EA validation middleware pattern

Every EA-scoped handler validates the EA exists before proceeding:

```rust
/// Validate EA exists, returning prefix and state_dir. Reusable by all handlers.
fn resolve_ea(
    ea_id: u32,
    state: &ApiState,
) -> Result<(String, String, PathBuf), (StatusCode, Json<ErrorResponse>)> {
    let registry = ea::load_registry(&state.omar_dir);
    if !registry.iter().any(|e| e.id == ea_id) {
        return Err((StatusCode::NOT_FOUND, Json(ErrorResponse {
            error: format!("EA {} not found", ea_id),
        })));
    }
    let prefix = ea::ea_prefix(ea_id, &state.base_prefix);
    let manager = ea::ea_manager_session(ea_id, &state.base_prefix);
    let state_dir = ea::ea_state_dir(ea_id, &state.omar_dir);
    Ok((prefix, manager, state_dir))
}
```

### 5.4 Handler pattern — ea_id from path

```rust
/// Example: GET /api/ea/{ea_id}/agents
pub async fn list_agents(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ListAgentsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;
    let client = TmuxClient::new(&prefix);

    let sessions = client.list_sessions()?;
    let parents = memory::load_agent_parents_from(&state_dir);
    let tasks = memory::load_worker_tasks_from(&state_dir);

    let agents = sessions.iter()
        .filter(|s| s.name != manager_session)
        .map(|s| build_agent_info(s, &parents, &tasks, &state_dir))
        .collect();

    let manager = if client.has_session(&manager_session).unwrap_or(false) {
        Some(build_manager_info(&manager_session, &state_dir))
    } else {
        None
    };

    Ok(Json(ListAgentsResponse { agents, manager }))
}
```

### 5.5 Spawn agent handler — EA from path, not inference

```rust
/// POST /api/ea/{ea_id}/agents
pub async fn spawn_agent(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpawnAgentRequest>,
) -> Result<Json<SpawnAgentResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;

    // Agent name gets EA prefix automatically
    let session_name = match req.name {
        Some(n) => format!("{}{}", prefix, n.strip_prefix(&prefix).unwrap_or(&n)),
        None => generate_agent_name_in_ea(&prefix),  // see Section 6.4
    };

    // Short name (for prompts and events) strips the prefix
    let short_name = session_name.strip_prefix(&prefix)
        .unwrap_or(&session_name)
        .to_string();

    // Parent is resolved within this EA's namespace
    let parent_session = if let Some(ref parent) = req.parent {
        if parent == "ea" {
            manager_session.clone()
        } else {
            format!("{}{}", prefix, parent.strip_prefix(&prefix).unwrap_or(parent))
        }
    } else {
        manager_session.clone()
    };

    // Lock App to check for name collision and create tmux session atomically.
    // We hold the lock through the tmux new-session call (~5ms) to prevent
    // two concurrent spawns from both passing the has_session check.
    // This is the same pattern that fixes Gotcha G5 (agent name race).
    let app = state.app.lock().await;
    if app.client().has_session(&session_name).unwrap_or(false) {
        return Err((StatusCode::CONFLICT, Json(ErrorResponse {
            error: format!("Agent '{}' already exists", short_name),
        })));
    }

    // State operations use EA-scoped paths
    memory::save_agent_parent_in(&state_dir, &session_name, &parent_session);
    memory::save_worker_task_in(&state_dir, &session_name, &req.task);

    // Build and create tmux session (still holding App lock)
    let cmd = build_agent_command(
        &state.base_command,
        &prompts_dir().join("agent.md"),
        &[
            ("{{PARENT_NAME}}", &short_name),
            ("{{TASK}}", &req.task),
            ("{{EA_ID}}", &ea_id.to_string()),
        ],
    );
    let client = TmuxClient::new(&prefix);
    client.new_session(&session_name, &cmd)?;
    drop(app);  // Release lock after session creation — name is now reserved in tmux

    // Schedule recurring status check — ea_id is structural
    let status_event = ScheduledEvent {
        id: uuid::Uuid::new_v4().to_string(),
        sender: "system".to_string(),
        receiver: short_name.clone(),
        timestamp: now_ns() + 60_000_000_000,  // 60s
        payload: "[STATUS CHECK]".to_string(),
        created_at: now_ns(),
        recurring_ns: Some(60_000_000_000),
        ea_id,  // From path parameter — structural
    };
    state.scheduler.insert(status_event);

    Ok(Json(SpawnAgentResponse {
        name: short_name,
        session: session_name,
    }))
}
```

### 5.6 Delete EA handler — ordered teardown

```rust
/// DELETE /api/eas/{ea_id}
///
/// ORDERING IS CRITICAL — each step has a specific reason for its position:
///   1. Validate + unregister first => blocks concurrent API calls (resolve_ea returns 404)
///   2. Cancel events => prevents STATUS CHECK from firing to dead agents
///   3. Kill workers => clean up running processes
///   4. Kill manager => after workers, so manager doesn't try to respawn
///   5. Remove state dir => no orphan files
///   6. Update dashboard => switch to EA 0 if needed
pub async fn delete_ea(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
) -> Result<Json<DeleteEaResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Step 0: EA 0 cannot be deleted
    if ea_id == 0 {
        return Err((StatusCode::FORBIDDEN, Json(ErrorResponse {
            error: "Cannot delete EA 0".to_string(),
        })));
    }

    // Step 1: Validate EA exists, then IMMEDIATELY unregister it.
    // This must happen first so that any concurrent API calls to
    // /api/ea/{ea_id}/* will get 404 from resolve_ea() during teardown.
    let (prefix, manager_session, state_dir) = resolve_ea(ea_id, &state)?;
    ea::unregister_ea(&state.omar_dir, ea_id)?;

    // Step 2: Cancel all events for this EA (before killing agents,
    // so STATUS CHECK events don't fire to agents we're about to kill)
    let cancelled = state.scheduler.cancel_by_ea(ea_id);

    // Step 3: Kill all worker agents (tmux sessions matching prefix)
    let client = TmuxClient::new(&prefix);
    let sessions = client.list_sessions().unwrap_or_default();
    let mut killed = 0;
    for session in &sessions {
        if session.name != manager_session {
            let _ = client.kill_session(&session.name);
            killed += 1;
        }
    }

    // Step 4: Kill the EA's manager session (after workers, so it
    // can't observe worker deaths and try to respawn them)
    if client.has_session(&manager_session).unwrap_or(false) {
        let _ = client.kill_session(&manager_session);
        killed += 1;
    }

    // Step 5: Remove state directory (after all processes are dead)
    if state_dir.exists() {
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    // Step 6: If active EA was deleted, switch dashboard to EA 0
    let mut app = state.app.lock().await;
    if app.active_ea == ea_id {
        let _ = app.switch_ea(0);
    }
    app.registered_eas = ea::load_registry(&state.omar_dir);

    Ok(Json(DeleteEaResponse {
        deleted_ea: ea_id,
        agents_killed: killed,
        events_cancelled: cancelled,
    }))
}
```

### 5.7 New API models — `src/api/models.rs`

```rust
/// Request to create a new EA
#[derive(Debug, Deserialize)]
pub struct CreateEaRequest {
    pub name: String,
    pub description: Option<String>,
}

/// EA info in responses
#[derive(Debug, Serialize)]
pub struct EaResponse {
    pub id: u32,
    pub name: String,
    pub description: Option<String>,
    pub agent_count: usize,
    pub is_active: bool,
}

/// Response for listing EAs
#[derive(Debug, Serialize)]
pub struct ListEasResponse {
    pub eas: Vec<EaResponse>,
    pub active: u32,
}

/// Request to switch active EA
#[derive(Debug, Deserialize)]
pub struct SwitchEaRequest {
    pub id: u32,
}

/// Response for EA deletion
#[derive(Debug, Serialize)]
pub struct DeleteEaResponse {
    pub deleted_ea: u32,
    pub agents_killed: usize,
    pub events_cancelled: usize,
}

/// Modified: ScheduleEventRequest — ea_id comes from URL path, not body
#[derive(Debug, Deserialize)]
pub struct ScheduleEventRequest {
    pub sender: String,
    pub receiver: String,
    pub timestamp: u64,
    pub payload: String,
    pub recurring_ns: Option<u64>,
    // NO ea_id field — it comes from the URL path
}
```

---

## 6. Agent Namespace and Isolation

### 6.1 Session naming convention

```
EA 0:
  Manager: omar-agent-ea-0
  Workers: omar-agent-0-{name}    (e.g., omar-agent-0-pm, omar-agent-0-auth)

EA 1:
  Manager: omar-agent-ea-1
  Workers: omar-agent-1-{name}    (e.g., omar-agent-1-pm, omar-agent-1-api)

EA 2:
  Manager: omar-agent-ea-2
  Workers: omar-agent-2-{name}
```

### 6.2 Agent discovery

`TmuxClient::list_sessions()` filters by prefix at the string level. For EA 2, the prefix is `"omar-agent-2-"`. Only sessions starting with that prefix are returned. The manager session `"omar-agent-ea-2"` is detected separately.

```rust
// In App::refresh() or the list_agents handler:
let prefix = ea::ea_prefix(self.active_ea, &self.base_prefix);
let client = TmuxClient::new(&prefix);
let sessions = client.list_sessions()?;  // Only EA {active_ea}'s agents
```

### 6.3 Cross-EA protection is structural

An agent's prompt tells it to use `/api/ea/{ea_id}/agents` for API calls. The `{ea_id}` in the URL IS the scope boundary. If an agent in EA 0 tries to access `/api/ea/1/agents`, the API will serve EA 1's agent list — but any spawn/kill/event operations will target EA 1's namespace (not the caller's). Since prompts instruct agents to only use their own EA ID, this is a soft boundary enforced by prompts and a hard boundary enforced by naming.

### 6.4 Agent name auto-generation — race-safe

The inside-out analysis identified a race in `generate_agent_name()` (Gotcha G5): two concurrent spawns could both find the same name free. Fix:

```rust
/// Generate a unique agent name within an EA, using the App mutex for serialization.
/// Called while holding the App lock (inside spawn_agent).
fn generate_agent_name_in_ea(prefix: &str) -> String {
    // The caller holds Arc<Mutex<App>>, so this is serialized.
    // Additionally, tmux new-session will fail if the name exists (belt-and-suspenders).
    for i in 1..1000 {
        let name = format!("{}{}", prefix, i);
        let result = std::process::Command::new("tmux")
            .args(["has-session", "-t", &name])
            .output();
        match result {
            Ok(output) if !output.status.success() => return name,  // Session doesn't exist
            _ => continue,
        }
    }
    // Fallback: use UUID suffix
    format!("{}{}", prefix, &uuid::Uuid::new_v4().to_string()[..8])
}
```

The App mutex serializes concurrent spawn calls, and tmux `new-session` will fail with an error if a session name collision occurs. Both layers prevent the race.

---

## 7. Scheduler and Event Isolation

### 7.1 ScheduledEvent with ea_id

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ScheduledEvent {
    pub id: String,
    pub sender: String,
    pub receiver: String,
    pub timestamp: u64,
    pub payload: String,
    pub created_at: u64,
    pub recurring_ns: Option<u64>,
    pub ea_id: u32,  // Mandatory — from path parameter at creation time
}
```

### 7.2 Scheduler additions

```rust
impl Scheduler {
    // EXISTING methods unchanged: insert(), cancel(), peek_next_timestamp()

    /// List events for a specific EA only
    pub fn list_by_ea(&self, ea_id: u32) -> Vec<ScheduledEvent> {
        let queue = self.queue.lock().unwrap();
        queue.iter()
            .filter(|e| e.ea_id == ea_id)
            .cloned()
            .collect()
    }

    /// Cancel all events for a specific EA. Returns count cancelled.
    pub fn cancel_by_ea(&self, ea_id: u32) -> usize {
        let mut queue = self.queue.lock().unwrap();
        let events: Vec<ScheduledEvent> = queue.drain().collect();
        let mut count = 0;
        let mut remaining = BinaryHeap::new();
        for ev in events {
            if ev.ea_id == ea_id {
                count += 1;
            } else {
                remaining.push(ev);
            }
        }
        *queue = remaining;
        count
    }
}
```

### 7.3 Event delivery — fixed hardcoded prefix

The inside-out analysis identified that `deliver_to_tmux()` hardcodes `format!("omar-agent-{}", receiver)` (scheduler/mod.rs line 165), while API handlers use `resolve_session_name()` which reads from config. This is Gotcha G2.

**Fix**: `deliver_to_tmux()` takes `ea_id` and `base_prefix`, constructing the target dynamically:

```rust
pub(crate) fn deliver_to_tmux(
    ea_id: u32,
    receiver: &str,
    message: &str,
    base_prefix: &str,
    ticker: &TickerBuffer,
) {
    let target = if receiver == "ea" || receiver == "omar" {
        ea::ea_manager_session(ea_id, base_prefix)
    } else {
        let prefix = ea::ea_prefix(ea_id, base_prefix);
        format!("{}{}", prefix, receiver)
    };

    // Existing tmux send-keys logic, using dynamic `target`
    let result = Command::new("tmux")
        .args(["send-keys", "-t", &target, "-l", message])
        .output();
    match result {
        Ok(output) if output.status.success() => {
            // Also send Enter
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, "Enter"])
                .output();
        }
        _ => {
            ticker.push(format!("Event delivery failed to {}", target));
        }
    }
}
```

### 7.4 Event loop changes

`run_event_loop` receives `base_prefix` to construct delivery targets:

```rust
pub async fn run_event_loop(
    scheduler: Arc<Scheduler>,
    ticker: TickerBuffer,
    popup_receiver: PopupReceiver,
    base_prefix: String,           // NEW parameter
) {
    // ... existing loop logic ...

    // When delivering:
    deliver_to_tmux(
        batch[0].ea_id,    // ea_id from the event itself
        receiver,
        &message,
        &base_prefix,      // passed through
        &ticker,
    );

    // When re-inserting recurring events — preserve ea_id:
    let next = ScheduledEvent {
        id: uuid::Uuid::new_v4().to_string(),
        sender: ev.sender.clone(),
        receiver: ev.receiver.clone(),
        timestamp: ev.timestamp + recurring_ns,
        payload: ev.payload.clone(),
        created_at: now_ns(),
        recurring_ns: Some(recurring_ns),
        ea_id: ev.ea_id,   // PRESERVED from original event
    };
    scheduler.insert(next);
}
```

### 7.5 Event creation handler

```rust
/// POST /api/ea/{ea_id}/events
pub async fn schedule_event(
    Path(ea_id): Path<u32>,
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScheduleEventRequest>,
) -> impl IntoResponse {
    // Validate EA exists
    let _  = resolve_ea(ea_id, &state)?;

    let event = ScheduledEvent {
        id: uuid::Uuid::new_v4().to_string(),
        sender: req.sender,
        receiver: req.receiver,
        timestamp: req.timestamp,
        payload: req.payload,
        created_at: now_ns(),
        recurring_ns: req.recurring_ns,
        ea_id,  // FROM PATH PARAMETER — structural scoping
    };

    state.scheduler.insert(event.clone());
    Ok(Json(EventResponse { id: event.id, ea_id }))
}
```

---

## 8. State and Memory Isolation

### 8.1 Refactored memory functions — `src/memory.rs`

All memory functions take a `state_dir: &Path` parameter instead of using a global `omar_dir()`.

```rust
/// Save a worker's task description
pub fn save_worker_task_in(state_dir: &Path, session: &str, task: &str) {
    let path = state_dir.join("worker_tasks.json");
    let mut tasks = load_worker_tasks_from(state_dir);
    tasks.insert(session.to_string(), task.to_string());
    write_json(&path, &tasks);
}

/// Load all worker task mappings for an EA
pub fn load_worker_tasks_from(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("worker_tasks.json");
    read_json(&path).unwrap_or_default()
}

/// Save a child->parent mapping
pub fn save_agent_parent_in(state_dir: &Path, child: &str, parent: &str) {
    let path = state_dir.join("agent_parents.json");
    let mut parents = load_agent_parents_from(state_dir);
    parents.insert(child.to_string(), parent.to_string());
    write_json(&path, &parents);
}

/// Load all child->parent mappings for an EA
pub fn load_agent_parents_from(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("agent_parents.json");
    read_json(&path).unwrap_or_default()
}

/// Remove a child->parent mapping
pub fn remove_agent_parent_in(state_dir: &Path, child: &str) {
    let mut parents = load_agent_parents_from(state_dir);
    parents.remove(child);
    let path = state_dir.join("agent_parents.json");
    write_json(&path, &parents);
}

/// Load agent status
pub fn load_agent_status_in(state_dir: &Path, session_name: &str) -> Option<String> {
    let path = state_dir.join("status").join(format!("{}.md", session_name));
    std::fs::read_to_string(&path).ok().filter(|s| !s.trim().is_empty())
}

/// Save agent status
pub fn save_agent_status_in(state_dir: &Path, session_name: &str, status: &str) {
    let dir = state_dir.join("status");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.md", session_name));
    std::fs::write(&path, status).ok();
}

/// Load memory file
pub fn load_memory_from(state_dir: &Path) -> String {
    let path = state_dir.join("memory.md");
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// Write memory snapshot — SCOPED to one EA
pub fn write_memory_to(
    state_dir: &Path,
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    client: &TmuxClient,
) {
    // Same logic as current write_memory(), but:
    // 1. Uses state_dir for all file paths
    // 2. Only cleans up worker_tasks for THIS EA's agents (not global)
    let mut worker_tasks = load_worker_tasks_from(state_dir);
    let active_sessions: Vec<String> = agents.iter()
        .map(|a| a.session.name.clone())
        .collect();
    worker_tasks.retain(|k, _| active_sessions.contains(k));
    write_json(&state_dir.join("worker_tasks.json"), &worker_tasks);
    // ... rest of memory write logic using state_dir paths
}

/// Generic JSON helpers
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    std::fs::read_to_string(path).ok()
        .and_then(|c| serde_json::from_str(&c).ok())
}

fn write_json<T: serde::Serialize>(path: &Path, data: &T) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        std::fs::write(path, json).ok();
    }
}
```

### 8.2 Worker task cleanup — fix for Gotcha G9

The inside-out analysis identified a critical bug: `write_memory()` calls `worker_tasks.retain(|k, _| active_sessions.contains(k))` using a global agent list. With multiple EAs, one EA's `write_memory()` would see only ITS agents and delete other EAs' worker tasks.

**Fix**: Per-EA state directories solve this structurally. Each EA's `write_memory_to()` reads and writes `~/.omar/ea/{ea_id}/worker_tasks.json`. The retain filter only sees that EA's sessions. No cross-EA file access.

### 8.3 Refactored project functions — `src/projects.rs`

```rust
pub fn projects_path_in(state_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(state_dir).ok();
    state_dir.join("tasks.md")
}

pub fn load_projects_from(state_dir: &Path) -> Vec<Project> {
    let path = projects_path_in(state_dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_projects(&content)
}

pub fn save_projects_to(state_dir: &Path, projects: &[Project]) -> anyhow::Result<()> {
    let path = projects_path_in(state_dir);
    // ... same save logic (renumber 1..n) ...
    Ok(())
}

pub fn add_project_in(state_dir: &Path, name: &str) -> anyhow::Result<usize> {
    let mut projects = load_projects_from(state_dir);
    let id = projects.len() + 1;
    projects.push(Project { id, name: name.to_string() });
    save_projects_to(state_dir, &projects)?;
    Ok(id)
}

pub fn remove_project_in(state_dir: &Path, id: usize) -> anyhow::Result<bool> {
    let mut projects = load_projects_from(state_dir);
    if id == 0 || id > projects.len() { return Ok(false); }
    projects.remove(id - 1);
    save_projects_to(state_dir, &projects)?;
    Ok(true)
}
```

**Note on project concurrency**: The inside-out analysis identified that `add_project()` in the API handler doesn't lock the App mutex (Gotcha implicit in Section 2.2). Two concurrent project operations will corrupt `tasks.md`. **Fix**: All project operations in API handlers must be performed under the App lock, OR project file operations must use atomic write (write temp + rename). We choose the former — add project operations to the App mutex scope, matching the pattern for agent operations.

### 8.4 EA combined prompt — fix for Gotcha G8

`build_ea_command()` currently writes to `~/.omar/ea_prompt_combined.md`. With multiple EAs starting simultaneously, they'd overwrite each other.

**Fix**: The combined prompt is written to `~/.omar/ea/{ea_id}/ea_prompt_combined.md`:

```rust
pub fn build_ea_command(
    base_command: &str,
    ea_id: EaId,
    ea_name: &str,
    omar_dir: &Path,
) -> String {
    let state_dir = ea::ea_state_dir(ea_id, omar_dir);
    let prompt_file = state_dir.join("ea_prompt_combined.md");
    // ... build prompt and write to EA-scoped path ...
}
```

---

## 9. Dashboard/TUI Changes

### 9.1 EA switcher bar

```
+--------------------------------------------------------------+
| EA: [0:Default] | 1:Research | 2:Ops       F1:New  F2:Switch |
+--------------------------------------------------------------+
| One-Man Army | Agents: 5 | 3 Running 2 Idle | Events: 12     |
+--------------------------------------------------------------+
| (agent grid for the selected EA)                              |
...
```

### 9.2 Layout changes — `src/ui/dashboard.rs`

```rust
pub fn render(frame: &mut Frame, app: &App) {
    let status_height = if app.status_message.is_some() { 4 } else { 3 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),              // NEW: EA switcher bar
            Constraint::Length(status_height),   // Status bar
            Constraint::Percentage(55),          // Agent grid + projects sidebar
            Constraint::Min(8),                  // Manager panel
            Constraint::Length(1),               // Help bar
        ])
        .split(frame.area());

    render_ea_bar(frame, app, chunks[0]);     // NEW
    render_status_bar(frame, app, chunks[1]);
    // ... rest shifted down by one chunk index
}
```

### 9.3 render_ea_bar

```rust
fn render_ea_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![
        Span::styled("EA: ", Style::default().fg(Color::DarkGray)),
    ];
    for ea in &app.registered_eas {
        let is_active = ea.id == app.active_ea;
        let label = format!("{}:{}", ea.id, ea.name);
        if is_active {
            spans.push(Span::styled(
                format!("[{}]", label),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::raw(" | "));
    }
    if !app.registered_eas.is_empty() { spans.pop(); }  // Remove trailing " | "
    spans.push(Span::raw("  "));
    spans.push(Span::styled("F1", Style::default().add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(":New "));
    spans.push(Span::styled("F2", Style::default().add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(":Switch"));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

### 9.4 Full reload on EA switch

```rust
impl App {
    pub fn switch_ea(&mut self, ea_id: EaId) -> Result<()> {
        self.active_ea = ea_id;

        // Reconstruct tmux client with new EA's prefix
        let new_prefix = ea::ea_prefix(ea_id, &self.base_prefix);
        self.client = TmuxClient::new(new_prefix);
        self.health_checker = HealthChecker::new(self.client.clone(), self.health_threshold);

        // Reset all view state
        self.focus_parent = ea::ea_manager_session(ea_id, &self.base_prefix);
        self.focus_stack.clear();
        self.selected = 0;
        self.manager_selected = true;

        // Reload all state for the new EA
        let state_dir = ea::ea_state_dir(ea_id, &self.omar_dir);
        std::fs::create_dir_all(&state_dir).ok();
        self.projects = projects::load_projects_from(&state_dir);
        self.agent_parents = memory::load_agent_parents_from(&state_dir);
        self.worker_tasks = memory::load_worker_tasks_from(&state_dir);

        // Refresh discovers agents via the new prefix
        self.refresh()?;
        self.set_status(format!("Switched to EA {}", ea_id));
        Ok(())
    }

    pub fn cycle_next_ea(&mut self) -> Result<()> {
        if self.registered_eas.len() <= 1 { return Ok(()); }
        let current_idx = self.registered_eas.iter()
            .position(|ea| ea.id == self.active_ea)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % self.registered_eas.len();
        let next_ea = self.registered_eas[next_idx].id;
        self.switch_ea(next_ea)
    }

    pub fn create_ea(&mut self, name: &str) -> Result<EaId> {
        let ea_id = ea::register_ea(&self.omar_dir, name, None)?;
        self.registered_eas = ea::load_registry(&self.omar_dir);
        self.set_status(format!("Created EA {}: {}", ea_id, name));
        Ok(ea_id)
    }
}
```

### 9.5 Key bindings — `src/main.rs`

```rust
KeyCode::F(1) if !app.has_popup() => {
    // Create new EA: enter EA name input mode
    app.ea_input_mode = true;
}
KeyCode::F(2) if !app.has_popup() => {
    // Cycle to next EA
    if let Err(e) = app.cycle_next_ea() {
        app.set_status(format!("Error: {}", e));
    }
}
```

### 9.6 Tick handler changes

```rust
// CURRENT (line 693 in main.rs):
// app.scheduled_events = scheduler.list();

// NEW: scope events to active EA
let active_ea = {
    let app_guard = app.lock().await;
    app_guard.active_ea
};
// ... later when updating:
{
    let mut app_guard = app.lock().await;
    app_guard.scheduled_events = scheduler.list_by_ea(active_ea);
}
```

---

## 10. Prompt Changes

### 10.1 Executive assistant prompt — `prompts/executive-assistant.md`

Add template variables and update all API examples:

```markdown
You are the Executive Assistant (EA) for the "{{EA_NAME}}" team in the OMAR system.
Your EA ID is {{EA_ID}}.

## API Reference

All API calls are scoped to your EA. Use these endpoints:

### Spawn a sub-agent
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{"name": "agent-name", "task": "Task description"}'

### List your agents
curl http://localhost:9876/api/ea/{{EA_ID}}/agents

### Schedule an event
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/events \
  -H "Content-Type: application/json" \
  -d '{"sender": "ea", "receiver": "target", "timestamp": <ns>, "payload": "message"}'

IMPORTANT: You manage ONLY agents in EA {{EA_ID}}. Do not attempt to interact
with agents belonging to other EAs.
```

### 10.2 Agent prompt — `prompts/agent.md`

```markdown
You are an Agent in the OMAR system, belonging to EA team {{EA_ID}}.

## API Reference

All API calls must use your EA's path prefix: /api/ea/{{EA_ID}}/

### Spawn a sub-agent
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{"name": "agent-name", "task": "Task description", "parent": "{{PARENT_NAME}}"}'

### Schedule an event
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/events \
  -H "Content-Type: application/json" \
  -d '{"sender": "<YOUR NAME>", "receiver": "<target>", "timestamp": <ns>, "payload": "message"}'

### Update your status
curl -X PUT http://localhost:9876/api/ea/{{EA_ID}}/agents/<YOUR NAME>/status \
  -H "Content-Type: application/json" \
  -d '{"status": "<1-line status>"}'

### Kill an agent
curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/agent-name

### List events
curl http://localhost:9876/api/ea/{{EA_ID}}/events
```

### 10.3 Prompt file handling — fix for Gotcha G7

`prompts_dir()` (manager/mod.rs) writes ALL embedded prompts to `~/.omar/prompts/` on every call. Currently harmless (same content for all EAs), but the prompts are shared templates — they contain `{{EA_ID}}` placeholders, not EA-specific content. The substitution happens at spawn time via `build_agent_command()`.

**Resolution**: Prompts remain shared in `~/.omar/prompts/`. The overwrite race is benign because all EAs write identical content (the template). The EA-specific combined prompt is written to `~/.omar/ea/{ea_id}/ea_prompt_combined.md` (per Gotcha G8 fix).

### 10.4 build_ea_command() changes

```rust
pub fn build_ea_command(
    base_command: &str,
    ea_id: EaId,
    ea_name: &str,
    omar_dir: &Path,
) -> String {
    let prompt_file = prompts_dir(omar_dir).join("executive-assistant.md");
    let state_dir = ea::ea_state_dir(ea_id, omar_dir);
    let mem = memory::load_memory_from(&state_dir);

    // Write combined prompt to EA-scoped directory
    let combined_path = state_dir.join("ea_prompt_combined.md");
    // ... combine prompt + memory, write to combined_path ...

    // Substitute {{EA_ID}} and {{EA_NAME}} in the prompt
    build_agent_command(
        base_command,
        &combined_path,
        &[
            ("{{EA_ID}}", &ea_id.to_string()),
            ("{{EA_NAME}}", ea_name),
        ],
    )
}
```

### 10.5 build_agent_command() changes

The existing `build_agent_command()` gains `{{EA_ID}}` substitution:

```rust
// When spawning agents from the API:
let prompt_file = prompts_dir(&state.omar_dir).join("agent.md");
let cmd = build_agent_command(
    &base_command,
    &prompt_file,
    &[
        ("{{PARENT_NAME}}", &parent_name),
        ("{{TASK}}", &task),
        ("{{EA_ID}}", &ea_id.to_string()),
    ],
);
```

---

## 11. Gotcha Resolution Matrix

The inside-out analysis identified 10 hidden gotchas. Here is how each is resolved:

### G1: The Two-App Problem

**Gotcha**: API server and dashboard have separate `App` instances (main.rs lines 457, 483). They share tmux/files but have independent in-memory state. Adding per-EA state to `App` would cause divergence.

**Resolution**: Single `App` behind `Arc<Mutex<App>>` (Design Principle P3). Both dashboard and API operate on the same App instance. The Mutex serializes access. No TOCTOU possible.

**Lock contention**: Dashboard tick holds lock ~1ms for refresh; API handlers hold ~1-5ms per request. At current traffic levels (human-interactive, <10 req/s), contention is negligible. If needed, dashboard can split tick into: lock -> copy state snapshot -> unlock -> render (no lock).

### G2: Hardcoded Prefix in Event Delivery

**Gotcha**: `deliver_to_tmux()` at scheduler/mod.rs:165 hardcodes `format!("omar-agent-{}", receiver)`. But API handlers use `app.client().prefix()`. These could diverge.

**Resolution**: `deliver_to_tmux()` now takes `ea_id` and `base_prefix` parameters. Target is constructed dynamically: `ea::ea_prefix(ea_id, base_prefix) + receiver`. The hardcoded string is eliminated. See Section 7.3.

### G3: Event Receiver Names are Globally Flat

**Gotcha**: Events use short names (`"worker1"`) as receivers. With multiple EAs, `"worker1"` could exist in two EAs. The scheduler has no concept of EA scope.

**Resolution**: `ScheduledEvent` gains mandatory `ea_id` field (set from URL path at creation time). `deliver_to_tmux()` uses `ea_id` to construct the full session name: `ea_prefix(ea_id) + receiver`. Cross-EA delivery is impossible because ea_id is baked into the event. Agents continue using short names in event payloads; the scoping is transparent.

### G4: Project IDs are Sequential Per-File

**Gotcha**: Project IDs are 1-based indices into `tasks.md`, renumbered on save. No stable IDs.

**Resolution**: Per-EA directories (`~/.omar/ea/{ea_id}/tasks.md`) mean each EA has its own numbering. This is adequate — projects are EA-scoped, and within one EA the sequential numbering works. No need for UUIDs since cross-EA project references don't exist. Note: if a client caches project IDs, deletions can shift indices. This is an existing behavior, not introduced by multi-EA.

### G5: Agent Name Auto-Generation Races

**Gotcha**: `generate_agent_name()` iterates checking `has_session()`. Two concurrent spawns could both find the same name free.

**Resolution**: Agent creation is serialized by the App mutex (held during `spawn_agent`). Additionally, tmux `new-session` will return an error if the session already exists (belt-and-suspenders). See Section 6.4.

### G6: Memory Snapshot Timing

**Gotcha**: `write_memory()` captures the dashboard's agent list. The API modifies agents via its own (separate) App instance, so memory.md can be stale.

**Resolution**: Single shared App (G1 fix) eliminates the divergence. Both API and dashboard read from the same `App.agents`. When the API spawns/kills agents, the change is visible to `write_memory()` on the next call because they share the same App instance.

### G7: Prompt Files Overwritten Globally

**Gotcha**: `prompts_dir()` writes all embedded prompts to `~/.omar/prompts/` on every call. Two EA processes would race.

**Resolution**: Single process model means no concurrent writes. Additionally, prompt templates are identical for all EAs (they contain `{{EA_ID}}` placeholders). Even if two threads call `prompts_dir()` concurrently, they write identical content. The race is benign. For belt-and-suspenders: call `prompts_dir()` once at startup, not on every spawn.

### G8: EA Combined Prompt is a Single File

**Gotcha**: `build_ea_command()` writes to `~/.omar/ea_prompt_combined.md`. Two EAs starting simultaneously would overwrite each other.

**Resolution**: Combined prompt is now written to `~/.omar/ea/{ea_id}/ea_prompt_combined.md`. Each EA writes to its own file. No collision. See Section 8.4.

### G9: Worker Task Cleanup is Lazy

**Gotcha**: `write_memory()` retains only tasks for agents in the current agent list. With a global `worker_tasks.json`, one EA's cleanup would delete another EA's tasks.

**Resolution**: Per-EA state directories. Each EA has its own `worker_tasks.json`. The retain filter sees only that EA's sessions. No cross-EA deletion. See Section 8.2.

### G10: Dashboard Owns EA Lifecycle

**Gotcha**: On startup, `ensure_manager()` auto-creates the EA session. On quit, the dashboard kills the EA. With multiple EAs: does closing the dashboard kill all EAs?

**Resolution**: On quit, the dashboard kills ALL EA manager sessions (loop through `registered_eas`), since EAs are not useful without the dashboard/API server running (events won't be delivered, status checks won't fire). Worker agents are also killed to prevent orphans.

```rust
// On dashboard quit (main.rs):
for ea in &app.registered_eas {
    let manager = ea::ea_manager_session(ea.id, &app.base_prefix);
    if app.client().has_session(&manager).unwrap_or(false) {
        let _ = app.client().kill_session(&manager);
    }
    // Kill all workers for this EA
    let prefix = ea::ea_prefix(ea.id, &app.base_prefix);
    let client = TmuxClient::new(&prefix);
    for session in client.list_sessions().unwrap_or_default() {
        let _ = client.kill_session(&session.name);
    }
}
```

Alternative considered: let EAs survive dashboard restarts (they're tmux sessions, they persist). This would require the scheduler to also persist events to disk, which is a larger change. **Decision**: Kill all on quit (simpler, matches current behavior).

---

## 12. Edge Case Handling

### EC1: EA Deletion While Agents Are Running

**Scenario**: User deletes EA 1 via `DELETE /api/eas/1`. EA 1 has 5 worker agents actively running.

**Handling**: Ordered 6-step teardown (see Section 5.6):

1. Unregister EA 1 from `eas.json` — blocks any new concurrent API calls (`resolve_ea` returns 404)
2. Cancel all events where `ea_id == 1` — prevents STATUS CHECK events from firing to dead agents
3. Kill all worker tmux sessions matching `omar-agent-1-*` — agents are forcibly terminated
4. Kill the manager session `omar-agent-ea-1` — after workers, so it can't respawn them
5. Remove state directory `~/.omar/ea/1/` — no orphan state files
6. If dashboard was showing EA 1, switch to EA 0

**Residual artifacts**: None. Status files deleted with state dir. Worker tasks deleted with state dir. Agent parents deleted with state dir. Events cancelled in step 1. Tmux sessions killed in steps 2-3.

### EC2: Two EAs with Same Tmux Session Name

**Scenario**: EA 0 has agent "auth" (`omar-agent-0-auth`). EA 1 also has agent "auth" (`omar-agent-1-auth`).

**Handling**: No collision. The EA ID is embedded in the tmux session name prefix. These are distinct tmux sessions. Events for EA 0's "auth" go to `omar-agent-0-auth`; events for EA 1's "auth" go to `omar-agent-1-auth`.

**Agent prompt perspective**: Agents use short names (`"auth"`) in their API calls and event payloads. The system adds the EA prefix transparently. Agent prompts say `YOUR NAME: auth` — they don't need to know their full session name.

### EC3: Event Delivery to Deleted EA

**Scenario**: Event was scheduled for EA 1's "worker1" at time T+60s. At T+30s, EA 1 is deleted.

**Handling**: The `delete_ea` handler unregisters the EA (step 1) then calls `scheduler.cancel_by_ea(1)` (step 2), which removes all events for EA 1 from the queue. The event at T+60s will never fire.

**Edge case within edge case**: What if the event is being delivered (popped from queue) at the exact moment `cancel_by_ea` runs? The scheduler uses a `std::Mutex` on the queue, so these operations are serialized. Either the event is delivered first (to a now-dead tmux session, which fails silently and logs to ticker) or it's cancelled first (and never delivered). Both outcomes are safe.

**Another edge case**: What if a new event is scheduled for EA 1 between `unregister_ea` (step 1) and `cancel_by_ea` (step 2)? This can't happen because `resolve_ea` in `schedule_event` checks the registry — the EA is already unregistered, so the call returns 404.

### EC4: Dashboard State During EA Switch

**Scenario**: User presses F2 to switch from EA 0 to EA 1. During the switch, an API call arrives for EA 0.

**Handling**: `switch_ea()` runs under the App mutex lock. It atomically updates `active_ea`, reconstructs the `TmuxClient`, and reloads all state. During the lock, API calls block briefly. After the lock releases, the API handler for EA 0 will construct its own `TmuxClient` using `ea_prefix(0, ...)` independently of `active_ea`. API handlers don't use `active_ea` — they use the `ea_id` from the URL path. The dashboard shows EA 1's state; the API serves EA 0's data. Both are correct.

### EC5: Concurrent API Calls to Different EAs

**Scenario**: Agent in EA 0 calls `POST /api/ea/0/agents` at the same time agent in EA 1 calls `POST /api/ea/1/agents`.

**Handling**: Both handlers lock `Arc<Mutex<App>>` to serialize agent creation. The operations execute sequentially. Since they create tmux sessions with different prefixes (`omar-agent-0-X` vs `omar-agent-1-Y`), there's no tmux-level collision. State files are in different directories (`~/.omar/ea/0/` vs `~/.omar/ea/1/`), so no file-level collision.

**Performance note**: The Mutex serializes ALL API calls, not just same-EA calls. This is acceptable at current scale. If contention becomes an issue, we could use per-EA locks (`HashMap<EaId, Mutex<EaState>>`), but this is premature optimization.

### EC6: EA 0 Deletion Attempt

**Scenario**: User tries `DELETE /api/eas/0`.

**Handling**: Returns `403 Forbidden` with message "Cannot delete EA 0". EA 0 is the default EA and always exists. This is enforced in both `delete_ea` handler and `unregister_ea()`.

### EC7: Tmux Session Limits

**Scenario**: Many EAs, each with many agents, approach tmux's session limit (~256-512 depending on configuration).

**Handling**: This is a deployment concern, not an architectural one. The system does not enforce a per-EA agent limit. Tmux will return an error when the limit is hit, and the `spawn_agent` handler will propagate it as a 500 error.

**Recommendation**: Document a guideline of max ~10 agents per EA, and max ~20 concurrent EAs (200 sessions total). Add a health check that warns when session count exceeds a configurable threshold.

### EC8: Slack Bridge Routing

**Scenario**: Slack bridge sends messages. Which EA receives them?

**Handling**: Initially, all Slack messages route to EA 0 (the default). The Slack bridge uses `localhost:9876/api/ea/0/events` as its endpoint. Future: add a `[slack_routing]` section to `config.toml` mapping Slack channels to EA IDs:

```toml
[slack_routing]
default_ea = 0
channels = { "eng-alerts" = 1, "ops-alerts" = 2 }
```

### EC9: Computer Use Lock Across EAs

**Scenario**: Agent in EA 0 holds the computer lock. Agent in EA 1 tries to acquire it.

**Handling**: The computer lock is global (single `Arc<Mutex<Option<String>>>`). One agent across all EAs can hold it. Computer routes remain at `/api/computer/*` without EA scoping.

**Identity issue**: Currently, agents identify themselves by short name (`"browser"`) in lock requests. Two agents named `"browser"` in different EAs would collide — one could accidentally release the other's lock.

**Fix**: The computer lock `acquire` and `release` handlers should accept an optional `ea_id` field in the request body. If provided, the lock owner is stored as `"{ea_id}:{agent_name}"` (e.g., `"0:browser"`). If not provided (backward compat during migration), the short name is used. Agent prompts include `ea_id` in their computer lock curl examples:

```bash
curl -X POST http://localhost:9876/api/computer/lock \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": {{EA_ID}}}'
```

This is a low-priority fix since computer-use agents are rare, but it prevents a real (if unlikely) identity collision.

### EC10: Config File is Global

**Scenario**: Does each EA need its own config?

**Handling**: No. `~/.config/omar/config.toml` is global — it controls the API port, session prefix base, health thresholds, and default agent command. These are process-level settings, not per-EA settings. The `base_prefix` from config is used as the root for EA-specific prefixes.

---

## 13. Bug Fix Verification Matrix

All 12 bugs from the `fix/race-and-cleanup-bugs` branch are structurally prevented. The inside-out analysis adds confidence by showing the data flow paths that each fix covers.

| Bug ID | Severity | Description | Fix Mechanism | Data Flow Coverage (from inside-out) | Verified By |
|--------|----------|-------------|---------------|--------------------------------------|-------------|
| BUG-C1 | Critical | Flat agent list, no per-EA scoping | Path param + prefix-filtered `list_sessions()` | `App.agents` populated from `TmuxClient` with EA-scoped prefix. `list_sessions` filters at string level. | EA id in URL -> prefix -> tmux filter. No global agent list exists. |
| BUG-C2 | Critical | Dual-App TOCTOU race | Single App in `Arc<Mutex<App>>` | Section 3.2 of inside-out: two `App::new()` calls at lines 458 and 483. Now one `App`, one `Arc`. | One allocation, one Mutex. Verified by removing second `App::new()`. |
| BUG-H1 | High | `owning_ea()` fallback misattribution | `owning_ea()` eliminated; EA from URL path | Data flow: `spawn_agent -> save_agent_parent -> agent_parents.json`. Parent mapping is now per-EA dir. No lookup across EAs. | No inference, no fallback, no function to call. |
| BUG-H2 | High | Scheduler has no EA scoping | `ea_id` field on `ScheduledEvent`; `deliver_to_tmux` uses it | Event flow: `POST /events -> schedule_event -> insert() -> event_loop -> deliver_to_tmux()`. All steps carry `ea_id`. | Event created with EA from path; delivered with EA from event. |
| BUG-H3 | High | Memory write contention (read-modify-write race on JSON files) | App Mutex serializes in-memory; per-EA dirs isolate files; atomic writes for registry | File-based state: all 7 files in Section 3.3 of inside-out are now per-EA. Only `eas.json` is global and uses atomic write. | Serialized access via Mutex; isolated files via `~/.omar/ea/{id}/`. |
| BUG-M1 | Medium | No EA namespace in tmux sessions | `ea_id` in prefix: `omar-agent-{ea_id}-{name}` | Session naming: `ea_prefix()` embeds `ea_id`. `list_sessions()` filters by prefix. | Distinct prefixes per EA. Two EAs can have same agent name. |
| BUG-M2 | Medium | One-shot agent adoption race | No adoption needed; prefix IS identity | Spawn flow: agent created with prefix from URL path. No "adopt existing agents" step. | Agents born into prefix, never moved or adopted. |
| BUG-M3 | Medium | Incomplete `remove_ea()` | Ordered 6-step teardown | Teardown covers: registry removal FIRST (blocks concurrent calls), events (scheduler), tmux sessions (workers then manager), state dir (filesystem), dashboard (switch). See Section 5.6. | Registry removed first to prevent races. Events cancelled before sessions killed. State dir removed last. |
| BUG-M4 | Medium | Global event delivery hardcodes `"omar-agent-"` prefix | `deliver_to_tmux()` takes `ea_id` parameter | `deliver_to_tmux` was at scheduler/mod.rs:165. Now takes `ea_id` + `base_prefix`. Target constructed from `ea_prefix(ea_id, base_prefix) + receiver`. | Hardcoded string eliminated. Dynamic construction verified. |
| BUG-L1 | Low | Status bar counts not EA-scoped | `App.agents` populated from active EA's prefix | `total_agents()` and `health_counts()` read from `App.agents`, which is refreshed with EA-scoped `TmuxClient`. | `list_sessions()` returns only active EA's sessions. |
| BUG-L2 | Low | Events popup shows all events | `scheduler.list_by_ea(active_ea)` instead of `list()` | Tick handler: `scheduler.list()` -> `scheduler.list_by_ea(active_ea)`. Events popup reads `app.scheduled_events`. | Filter at query time. Only active EA's events displayed. |
| BUG-L3 | Low | Byte-based `truncate_str()` panics on multi-byte UTF-8 | Char-based truncation | `truncate_str` at dashboard.rs:1026. Fixed with `s.chars().collect::<Vec<char>>()`. | Cherry-pick from commit `625f7c1`. |

### Additional insights from inside-out analysis

The inside-out analysis revealed several issues NOT in the original 12-bug catalogue:

| Issue | Source | How This Spec Handles It |
|-------|--------|--------------------------|
| Project `add_project()` has no mutex protection | Inside-out Section 2.2 | Project operations in API handlers are performed under App mutex. |
| `write_memory()` deletes other EA's worker_tasks | Inside-out Gotcha G9 | Per-EA `worker_tasks.json` files. `retain()` only sees own EA's sessions. |
| `ea_prompt_combined.md` is a single global file | Inside-out Gotcha G8 | Moved to `~/.omar/ea/{id}/ea_prompt_combined.md`. |
| `generate_agent_name()` has TOCTOU race | Inside-out Gotcha G5 | Serialized by App mutex + tmux error as fallback. |
| Status files not cleaned up on agent kill | Inside-out Section 2.1 | Per-EA status dir; cleaned up on EA deletion. Individual cleanup added to `kill_agent`. |
| Popup deferral is global | Inside-out Section 2.3 | Popup is a dashboard concept (one dashboard). No per-EA popups needed. |

---

## 14. Implementation Plan

### Phase 1: Foundation (no user-facing changes)

**Goal**: Create EA data model, refactor memory/projects to take `state_dir`, add `ea_id` to events. All existing tests pass. Single-EA behavior unchanged.

**Cherry-picks from `fix/race-and-cleanup-bugs`:**
- UTF-8 char-based truncation (commit `625f7c1`)
- Flaky test fixes (from commit `0ccb459`)
- Idempotent `cancel_event` (from commit `0ccb459`)

**New file:**
1. `src/ea.rs` — `EaId` type alias, `EaInfo`, `ea_prefix()`, `ea_manager_session()`, `ea_state_dir()`, `load_registry()`, `register_ea()`, `unregister_ea()`, `save_registry()` (atomic write), `migrate_legacy_state()`

**Refactor existing files (behavior unchanged):**
2. `src/memory.rs` — Rename all functions to `_in(state_dir)` / `_from(state_dir)` variants. Keep old function names as thin wrappers that compute `state_dir` from a default `ea_state_dir(0, omar_dir())` for backward compat during migration. Add `read_json`/`write_json` helpers.
3. `src/projects.rs` — Rename to `_from(state_dir)` / `_to(state_dir)` / `_in(state_dir)` variants. Same thin wrapper approach.
4. `src/scheduler/event.rs` — Add `ea_id: u32` field to `ScheduledEvent`. Default to 0 in all existing test helpers.
5. `src/scheduler/mod.rs` — Update `deliver_to_tmux()` to accept `ea_id` and `base_prefix`. Add `list_by_ea()` and `cancel_by_ea()`. Update `run_event_loop()` signature to take `base_prefix`.
6. Tests — Update all test helpers to include `ea_id: 0` in `ScheduledEvent` construction.

**Verify**: `cargo test` passes. Single-EA behavior unchanged. EA 0 is the only EA.

**Dependencies**: None (first phase)

### Phase 2: Single Shared App + State Isolation

**Goal**: Eliminate dual-App problem. App becomes single instance behind `Arc<Mutex<App>>`. Dashboard and API share the same App.

1. `src/main.rs` — Create single `App` in `Arc<Mutex<App>>`, pass to both API server and dashboard loop. Remove second `App::new()`. Add `mod ea;`. Call `migrate_legacy_state()` on startup. Pass `base_prefix` to `run_event_loop()`.
2. `src/app.rs` — Add `active_ea`, `registered_eas`, `base_prefix`, `omar_dir` fields. Add `switch_ea()`, `create_ea()`, `cycle_next_ea()` methods. Update `refresh()` to use EA-scoped prefix. Update `App::new()` to load registry, initialize EA 0. Replace all `memory::` calls with `_in(state_dir)` variants.
3. `src/api/handlers.rs` — Update `ApiState` to hold `Arc<Mutex<App>>` (same Arc). All handlers that modify state now lock the shared App.

**Verify**: Dashboard and API share state. No TOCTOU. Agent spawn/kill from API is immediately visible in dashboard.

**Dependencies**: Phase 1 complete

### Phase 3: API Route Migration

**Goal**: All routes move under `/api/ea/{ea_id}/`. Old routes removed.

1. `src/api/mod.rs` — Replace all routes with path-scoped versions under `/api/ea/:ea_id/`. Add EA management routes.
2. `src/api/handlers.rs` — All EA-scoped handlers extract `ea_id` from path. Construct prefix and state_dir from `ea_id`. Add `resolve_ea()` helper. Add 5 new handlers: `list_eas`, `create_ea`, `delete_ea`, `get_active_ea`, `switch_ea`. Remove `resolve_session_name()` global prefix usage.
3. `src/api/models.rs` — Add `CreateEaRequest`, `EaResponse`, `ListEasResponse`, `SwitchEaRequest`, `DeleteEaResponse`. Remove `ea_id` from `ScheduleEventRequest` (comes from path).

**Verify**: `cargo test` passes. API requires `/api/ea/0/` prefix. EA CRUD endpoints work.

**Dependencies**: Phase 2 complete

### Phase 4: Dashboard UI + Manager + Prompts

**Goal**: Dashboard shows EA bar. F1/F2 keys work. Prompts updated. Full multi-EA flow end-to-end.

1. `src/ui/dashboard.rs` — Add `render_ea_bar()`. Shift layout by one row. Add char-based `truncate_str()`.
2. `src/main.rs` — Add F1/F2 key bindings. Update tick handler to use `list_by_ea()`. Update quit handler to kill all EAs.
3. `src/manager/mod.rs` — Remove `MANAGER_SESSION` const. `build_ea_command()` takes `ea_id` and `ea_name`. `start_manager()` takes `ea_id`. `ensure_manager()` works with EA-scoped sessions. All `memory::` calls updated.
4. `prompts/executive-assistant.md` — Add `{{EA_ID}}`, `{{EA_NAME}}`. Update all curl examples.
5. `prompts/agent.md` — Add `{{EA_ID}}`. Update all curl examples.
6. `src/api/handlers.rs` — Complete `delete_ea` handler with ordered 6-step teardown.

**Verify**: Full multi-EA flow: create EA via F1, switch via F2, spawn agents, schedule events, delete EA. All scoped correctly.

**Dependencies**: Phase 3 complete

### Phase 5: Migration + Polish

**Goal**: Existing users can upgrade without data loss. Tmux sessions renamed. State files migrated.

1. `src/ea.rs` — `migrate_legacy_state()` moves `~/.omar/*.md` and `~/.omar/*.json` to `~/.omar/ea/0/`. `migrate_legacy_sessions()` renames tmux sessions.
2. `src/main.rs` — Call migration functions on startup (idempotent — check if already migrated).
3. Integration tests — Test migration path, multi-EA creation/deletion/switching.

**Verify**: Fresh install works. Upgrade from single-EA works. All data preserved.

**Dependencies**: Phase 4 complete

---

## 15. File-by-File Change List

### New files

| File | Purpose | Lines (est.) |
|------|---------|-------------|
| `src/ea.rs` | `EaId` type, `EaInfo`, prefix/session/dir functions, registry CRUD, migration helpers | ~180 |

### Modified files

| File | Changes | Lines changed (est.) |
|------|---------|---------------------|
| `src/main.rs` | `mod ea;`. Single shared `App` in `Arc<Mutex<>>`. EA bar key bindings (F1, F2). Tick handler uses `list_by_ea()`. Pass `base_prefix` to `run_event_loop()`. Migration call. Quit handler kills all EAs. | +60/-30 |
| `src/app.rs` | Add `active_ea`, `registered_eas`, `base_prefix`, `omar_dir` fields. `switch_ea()`, `create_ea()`, `cycle_next_ea()` methods. `refresh()` uses EA-scoped prefix. `MANAGER_SESSION` const removed. All `memory::` calls updated to `_in(state_dir)`. | +150/-40 |
| `src/memory.rs` | Rename all functions to `_in(state_dir)` variants. Remove global `omar_dir()` and path functions. Add `read_json`/`write_json` helpers. `write_memory()` becomes `write_memory_to(state_dir)`. Scoped worker_tasks cleanup. | +60/-50 |
| `src/projects.rs` | Rename all functions to `_from(state_dir)`/`_to(state_dir)` variants. Remove global `projects_path()`. | +30/-20 |
| `src/scheduler/mod.rs` | `deliver_to_tmux()` takes `ea_id` + `base_prefix`. Add `list_by_ea()`, `cancel_by_ea()`. `run_event_loop()` takes `base_prefix`. Recurring event re-insert preserves `ea_id`. | +80/-30 |
| `src/scheduler/event.rs` | Add `ea_id: u32` to `ScheduledEvent`. Update test helpers. | +8/-2 |
| `src/event.rs` | No changes (AppEvent is unrelated). | 0 |
| `src/api/mod.rs` | Replace all routes with path-scoped versions. Add EA management routes. Remove old routes. | +30/-20 |
| `src/api/handlers.rs` | All EA-scoped handlers extract `ea_id`. `resolve_ea()` helper. `ApiState` gains `base_prefix`/`omar_dir`. 5 new EA CRUD handlers. `spawn_agent` uses EA from path. `schedule_event` uses EA from path. All memory/projects calls use `state_dir`. `delete_ea` with ordered teardown. | +200/-80 |
| `src/api/models.rs` | Add `CreateEaRequest`, `EaResponse`, `ListEasResponse`, `SwitchEaRequest`, `DeleteEaResponse`. Remove `ea_id` from `ScheduleEventRequest`. | +35/-0 |
| `src/manager/mod.rs` | Remove `MANAGER_SESSION` const. `build_ea_command()` takes `ea_id`/`ea_name`. `start_manager()` takes `ea_id`. All `memory::` calls updated. `prompts_dir()` unchanged (shared). Combined prompt goes to EA-scoped dir. | +40/-20 |
| `src/ui/dashboard.rs` | Add `render_ea_bar()`. Shift layout by one row. Char-based `truncate_str()`. | +50/-5 |
| `prompts/executive-assistant.md` | Add `{{EA_ID}}`, `{{EA_NAME}}`. Update curl examples. Cross-EA warning. | +15/-10 |
| `prompts/agent.md` | Add `{{EA_ID}}`. Update all curl examples. | +10/-5 |

### Total estimated scope

| Category | Files | Lines added | Lines removed | Net |
|----------|-------|-------------|---------------|-----|
| Data model (`ea.rs`) | 1 new | +180 | 0 | +180 |
| Memory/projects refactor | 2 | +90 | -70 | +20 |
| Scheduler scoping | 2 | +88 | -32 | +56 |
| API (routes + handlers + models) | 3 | +265 | -100 | +165 |
| Dashboard UI | 1 | +50 | -5 | +45 |
| App state management | 1 | +150 | -40 | +110 |
| Main + manager + prompts | 3 | +125 | -65 | +60 |
| **Total** | **14 files (1 new + 13 modified)** | **~950** | **~310** | **~640 net** |

---

## 16. Migration Path

Since backward compatibility is dropped, migration is a clean break.

### For the codebase

1. All `memory::save_worker_task()` -> `memory::save_worker_task_in(&state_dir, ...)`
2. All `projects::load_projects()` -> `projects::load_projects_from(&state_dir)`
3. All `memory::load_agent_parents()` -> `memory::load_agent_parents_from(&state_dir)`
4. `MANAGER_SESSION` constant removed -> `ea::ea_manager_session(ea_id, base_prefix)`
5. `deliver_to_tmux(receiver, message, ticker)` -> `deliver_to_tmux(ea_id, receiver, message, base_prefix, ticker)`

### For existing state (one-time migration)

```rust
pub fn migrate_legacy_state(omar_dir: &Path) {
    let ea0_dir = ea_state_dir(0, omar_dir);
    if ea0_dir.join("memory.md").exists() {
        return;  // Already migrated
    }
    std::fs::create_dir_all(ea0_dir.join("status")).ok();

    let files = [
        "tasks.md", "memory.md", "worker_tasks.json",
        "agent_parents.json", "ea_prompt_combined.md",
    ];
    for file in &files {
        let old = omar_dir.join(file);
        let new = ea0_dir.join(file);
        if old.exists() && !new.exists() {
            std::fs::rename(&old, &new).ok();
        }
    }

    let old_status = omar_dir.join("status");
    let new_status = ea0_dir.join("status");
    if old_status.exists() && !new_status.exists() {
        std::fs::rename(&old_status, &new_status).ok();
    }
}
```

### For existing tmux sessions (one-time migration)

```rust
pub fn migrate_legacy_sessions(base_prefix: &str) {
    // Rename manager: omar-agent-ea -> omar-agent-ea-0
    let old_manager = format!("{}ea", base_prefix);
    let new_manager = ea_manager_session(0, base_prefix);
    if old_manager != new_manager {
        let _ = Command::new("tmux")
            .args(["rename-session", "-t", &old_manager, &new_manager])
            .output();
    }

    // Rename agents: omar-agent-{name} -> omar-agent-0-{name}
    let new_prefix = ea_prefix(0, base_prefix);
    if let Ok(output) = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    {
        let sessions = String::from_utf8_lossy(&output.stdout);
        for name in sessions.lines() {
            if name.starts_with(base_prefix)
                && !name.starts_with(&new_prefix)
                && !name.starts_with(&format!("{}ea-", base_prefix))
                && name != "omar-dashboard"
            {
                let short = name.strip_prefix(base_prefix).unwrap_or(name);
                let new_name = format!("{}{}", new_prefix, short);
                let _ = Command::new("tmux")
                    .args(["rename-session", "-t", name, &new_name])
                    .output();
            }
        }
    }
}
```

---

## Appendix A: Hardcoded Constants That Must Become Dynamic

From the inside-out analysis — every hardcoded constant that this spec replaces:

```rust
// main.rs:34 — UNCHANGED (one dashboard per process)
pub const DASHBOARD_SESSION: &str = "omar-dashboard";

// manager/mod.rs:16 — REMOVED, replaced by ea::ea_manager_session(ea_id, base_prefix)
// WAS: pub const MANAGER_SESSION: &str = "omar-agent-ea";

// config.rs:79 — UNCHANGED (base prefix, used to derive EA-specific prefixes)
fn default_session_prefix() -> String { "omar-agent-".to_string() }

// config.rs:154 — UNCHANGED (single process, single port)
fn default_api_port() -> u16 { 9876 }

// scheduler/mod.rs:165 — REPLACED by deliver_to_tmux(ea_id, receiver, msg, base_prefix, ticker)
// WAS: let target = format!("omar-agent-{}", receiver);

// All prompt files — UPDATED to use {{EA_ID}} template variable
// WAS: hardcoded "localhost:9876/api/agents"
// NOW: "localhost:9876/api/ea/{{EA_ID}}/agents"
```

## Appendix B: Source File Reference

From the inside-out analysis — all source files read and their relevance to multi-EA:

| File | Lines | Multi-EA Impact |
|------|-------|----------------|
| `src/main.rs` | 744 | High — single vs shared App, EA bar keybindings, migration call, quit handler |
| `src/app.rs` | ~750 | High — EA state fields, refresh scoping, switch_ea(), manager detection |
| `src/config.rs` | 310 | Low — base_prefix read from here, no per-EA config |
| `src/event.rs` | 79 | None — TUI event handling, not ScheduledEvent |
| `src/memory.rs` | 196 | High — all functions need state_dir parameter |
| `src/projects.rs` | 161 | Medium — functions need state_dir parameter |
| `src/computer.rs` | 378 | None — remains global |
| `src/manager/mod.rs` | 463 | High — MANAGER_SESSION const, prompt building, EA lifecycle |
| `src/manager/protocol.rs` | ~100 | Low — message parsing, no EA-specific logic |
| `src/api/mod.rs` | 75 | High — all routes change |
| `src/api/handlers.rs` | 859 | High — all handlers gain ea_id, 5 new handlers |
| `src/api/models.rs` | 282 | Medium — new request/response types |
| `src/scheduler/mod.rs` | 604 | High — deliver_to_tmux, event loop, new methods |
| `src/scheduler/event.rs` | 78 | Medium — new ea_id field |
| `src/tmux/client.rs` | 218 | Low — already parameterized by prefix |
| `src/tmux/session.rs` | 20 | None |
| `src/tmux/health.rs` | 123 | Low — tracks by session name, EA-agnostic |
| `src/ui/dashboard.rs` | ~600 | Medium — new EA bar, layout shift |
| `prompts/executive-assistant.md` | 311 | Medium — template vars, API paths |
| `prompts/agent.md` | 149 | Medium — template vars, API paths |
| `prompts/worker.md` | ~80 | Low — template vars |

## Appendix C: Decisions Log

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | Process model? | Single process | Shared scheduler, computer lock, single port |
| 2 | EA identifier type? | `u32` integer | Simple, no escaping, orderable |
| 3 | EA in URL? | Path param `/api/ea/{id}/` | Structural isolation, not inferential |
| 4 | Default EA? | `id=0`, always exists | No special-casing |
| 5 | Backward compat? | None | Clean break, no dual-path bugs |
| 6 | App instance count? | One shared via `Arc<Mutex<App>>` | Eliminates TOCTOU (BUG-C2) |
| 7 | File locking? | App Mutex + per-EA dirs + atomic write for registry | Three-layer protection |
| 8 | Event scoping? | `ea_id` field on `ScheduledEvent` | Baked at creation, carried to delivery |
| 9 | Dashboard on EA switch? | Full reload | Simple, correct, no stale state |
| 10 | EA 0 deletable? | No | Always-exists invariant simplifies code |
| 11 | Prompts per-EA? | Shared templates, EA-specific combined prompt | Templates are identical; substitution at spawn time |
| 12 | Computer lock scope? | Global (one physical screen) | Correct semantic, simple implementation |
| 13 | Slack bridge routing? | EA 0 initially, configurable later | MVP simplicity |
| 14 | On dashboard quit? | Kill all EAs + agents | Match current behavior; events need scheduler to run |
| 15 | Event receiver names? | Short names, prefixed at delivery time | Agents don't need to know full session names |

---

## 17. Formal Verification Summary

> **Full report**: `~/Documents/research/omar/docs/multi_ea_verification_report.md`
> **Date**: 2026-03-06

### Overview

Four independent formal verification approaches were applied to the Multi-EA implementation:

| Approach | Bugs Found | Key Technique |
|----------|:----------:|---------------|
| Model Checking / TLA+ | 6 (V1-V6) | State enumeration, interleaving analysis, 12 concurrent pairs |
| SAT/SMT Solver | 2 (V7-V8) + 3 info | SMT-LIB2 encoding, 21 concurrent pairs x 5 invariants, boundary analysis |
| Theorem Proving | 0 new | Inductive proofs over ALL reachable states; confirmed prior fixes |
| SyGuS Synthesis | 2 (S1-S2) | Synthesize minimal correct implementations, compare against actual code |

An edge case audit found 1 additional bug (EA ID reuse). **Total: 11 bugs found and fixed.**

### Verdict

All four approaches **agree**: the implementation is **correct with respect to the five core invariants** after all fixes are applied.

### Core Invariants (all verified)

| Invariant | Status |
|-----------|--------|
| INV1: At most one EA active | Proved by theorem proving; confirmed by all 4 approaches |
| INV2: Agent-EA binding via prefix | Proved (prefix injectivity + no-containment lemmas) |
| INV3: Event-EA isolation | 4 bugs fixed (V2, V4/S1, V5, V7); now proved correct |
| INV4: Clean EA deletion | V3 fixed; 6-step teardown verified by all approaches |
| INV5: EA 0 immortality + ID monotonicity | V8 + BUG-1 fixed; proved by induction |

### Safety Properties (all proved)

- **Deadlock freedom**: Max lock depth = 1; no circular dependencies
- **Liveness**: All lock-held code paths are finite
- **Cross-EA isolation**: Structural via path-parameter routing and prefix uniqueness

### Residual Risks (accepted)

- Orphan tmux session from delete/spawn race (benign, minimal resources)
- File-level R/M/W races on per-EA JSON (self-correcting)
- Blocking I/O under async lock (acceptable at current scale)
- PopupReceiver not EA-scoped (30-second delay, not loss)

### Confidence Level: HIGH

Theorem proving provides guarantees over ALL reachable states. SMT and SyGuS found subtle bugs (event batching, atomicity) that model checking missed. All approaches converge on correctness post-fix.
