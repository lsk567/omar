//! Persistent memory — state snapshots in the EA's state directory.
//!
//! All path functions accept a `state_dir` parameter (the EA's root
//! state directory, e.g. `~/.omar/`) so multiple EAs can maintain
//! independent state.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::app::AgentInfo;
use crate::projects;
use crate::tmux::TmuxClient;

/// Ensure a directory exists, creating it if needed.
fn ensure_dir(state_dir: &Path) {
    fs::create_dir_all(state_dir).ok();
}

/// Path to the memory file
fn memory_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("memory.md")
}

/// Path to worker task descriptions
fn worker_tasks_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("worker_tasks.json")
}

/// Path to agent parent mappings (child → parent)
fn agent_parents_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("agent_parents.json")
}

/// Save a worker's task description (upsert)
pub fn save_worker_task(state_dir: &Path, session: &str, task: &str) {
    let mut tasks = load_worker_tasks(state_dir);
    tasks.insert(session.to_string(), task.to_string());
    write_worker_tasks(state_dir, &tasks);
}

/// Load all worker task mappings
pub fn load_worker_tasks(state_dir: &Path) -> HashMap<String, String> {
    let path = worker_tasks_path(state_dir);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write worker tasks to disk
fn write_worker_tasks(state_dir: &Path, tasks: &HashMap<String, String>) {
    ensure_dir(state_dir);
    let path = worker_tasks_path(state_dir);
    if let Ok(json) = serde_json::to_string_pretty(tasks) {
        fs::write(&path, json).ok();
    }
}

/// Save a child→parent mapping (upsert)
pub fn save_agent_parent(state_dir: &Path, child: &str, parent: &str) {
    let mut parents = load_agent_parents(state_dir);
    parents.insert(child.to_string(), parent.to_string());
    write_agent_parents(state_dir, &parents);
}

/// Load all child→parent mappings
pub fn load_agent_parents(state_dir: &Path) -> HashMap<String, String> {
    let path = agent_parents_path(state_dir);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write agent parents to disk
pub fn write_agent_parents(state_dir: &Path, parents: &HashMap<String, String>) {
    ensure_dir(state_dir);
    let path = agent_parents_path(state_dir);
    if let Ok(json) = serde_json::to_string_pretty(parents) {
        fs::write(&path, json).ok();
    }
}

/// Remove a child→parent mapping
pub fn remove_agent_parent(state_dir: &Path, child: &str) {
    let mut parents = load_agent_parents(state_dir);
    parents.remove(child);
    write_agent_parents(state_dir, &parents);
}

/// Directory for agent status files
fn status_dir(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("status")
}

/// Load an agent's self-reported status
pub fn load_agent_status(state_dir: &Path, session_name: &str) -> Option<String> {
    let path = status_dir(state_dir).join(format!("{}.md", session_name));
    fs::read_to_string(&path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Save an agent's status
pub fn save_agent_status(state_dir: &Path, session_name: &str, status: &str) {
    let dir = status_dir(state_dir);
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.md", session_name));
    fs::write(&path, status).ok();
}

/// Load the memory file contents (empty string if missing)
pub fn load_memory(state_dir: &Path) -> String {
    let path = memory_path(state_dir);
    fs::read_to_string(&path).unwrap_or_default()
}

/// Write a full state snapshot to the EA's state directory.
///
/// Captures: active projects, worker states + tasks, manager status,
/// and the manager's recent conversation context.
pub fn write_memory(
    state_dir: &Path,
    ea_session: &str,
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    client: &TmuxClient,
) {
    ensure_dir(state_dir);
    let projects = projects::load_projects(state_dir);
    let mut worker_tasks = load_worker_tasks(state_dir);

    // Clean up stale entries from worker_tasks (sessions that no longer exist)
    let active_sessions: Vec<String> = agents.iter().map(|a| a.session.name.clone()).collect();
    worker_tasks.retain(|k, _| active_sessions.contains(k));
    write_worker_tasks(state_dir, &worker_tasks);

    // Note: agent_parents cleanup is intentionally NOT done here.
    // write_memory() can be called with a stale agents list (e.g., from
    // add_project/complete_project) which would incorrectly delete parent
    // entries for recently API-spawned workers. Parent entries are cleaned
    // up explicitly via remove_agent_parent() when agents are killed.

    let mut out = String::from("# OMAR State\n\n");

    // Active projects
    if !projects.is_empty() {
        out.push_str("## Active Projects\n");
        for p in &projects {
            out.push_str(&format!("{}. {}\n", p.id, p.name));
        }
        out.push('\n');
    }

    // Active workers
    if !agents.is_empty() {
        out.push_str("## Active Agents\n");
        for agent in agents {
            let health = agent.health.as_str();
            let task_desc = worker_tasks
                .get(&agent.session.name)
                .map(|t| t.as_str())
                .unwrap_or("(no task assigned)");
            out.push_str(&format!(
                "- {} ({}): {}\n",
                agent.session.name, health, task_desc
            ));
        }
        out.push('\n');
    }

    // Manager status
    out.push_str("## Manager\n");
    match manager {
        Some(_) => out.push_str("- Status: Running\n"),
        None => out.push_str("- Status: Not running\n"),
    }
    out.push('\n');

    // Manager's recent context (last ~20 lines of pane output)
    if manager.is_some() {
        if let Ok(output) = client.capture_pane(ea_session, 20) {
            let trimmed: Vec<&str> = output
                .lines()
                .map(|l| l.trim_end())
                .filter(|l| !l.is_empty())
                .collect();
            if !trimmed.is_empty() {
                out.push_str("## Manager's Recent Context\n");
                for line in &trimmed {
                    out.push_str(&format!("> {}\n", line));
                }
                out.push('\n');
            }
        }
    }

    let path = memory_path(state_dir);
    fs::write(&path, &out).ok();
}

// ── Shared paths ──

/// Base directory for all OMAR state (`~/.omar/`).
pub fn omar_base_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".omar")
}

// ── EA list persistence ──

/// Path to the global EA list (lives in the base ~/.omar/ directory, not per-EA)
fn eas_path() -> std::path::PathBuf {
    omar_base_dir().join("eas.json")
}

/// Load the list of EA IDs from ~/.omar/eas.json
pub fn load_ea_ids() -> Vec<String> {
    let path = eas_path();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save the list of EA IDs to ~/.omar/eas.json
pub fn save_ea_ids(ids: &[String]) {
    ensure_dir(&omar_base_dir());
    let path = eas_path();
    if let Ok(json) = serde_json::to_string_pretty(ids) {
        fs::write(&path, json).ok();
    }
}
