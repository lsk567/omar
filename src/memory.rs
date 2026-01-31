//! Persistent memory — state snapshots in ~/.omar/memory.md
//!
//! Writes a human-readable markdown snapshot of the current OMAR state
//! so a newly created manager session can resume seamlessly.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::app::AgentInfo;
use crate::projects;
use crate::tmux::TmuxClient;

/// Ensure ~/.omar/ exists and return it
fn omar_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar");
    fs::create_dir_all(&dir).ok();
    dir
}

/// Path to the memory file
fn memory_path() -> PathBuf {
    omar_dir().join("memory.md")
}

/// Path to worker task descriptions
fn worker_tasks_path() -> PathBuf {
    omar_dir().join("worker_tasks.json")
}

/// Path to agent parent mappings (child → parent)
fn agent_parents_path() -> PathBuf {
    omar_dir().join("agent_parents.json")
}

/// Save a worker's task description (upsert)
pub fn save_worker_task(session: &str, task: &str) {
    let mut tasks = load_worker_tasks();
    tasks.insert(session.to_string(), task.to_string());
    write_worker_tasks(&tasks);
}

/// Load all worker task mappings
pub fn load_worker_tasks() -> HashMap<String, String> {
    let path = worker_tasks_path();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write worker tasks to disk
fn write_worker_tasks(tasks: &HashMap<String, String>) {
    let path = worker_tasks_path();
    if let Ok(json) = serde_json::to_string_pretty(tasks) {
        fs::write(&path, json).ok();
    }
}

/// Save a child→parent mapping (upsert)
pub fn save_agent_parent(child: &str, parent: &str) {
    let mut parents = load_agent_parents();
    parents.insert(child.to_string(), parent.to_string());
    write_agent_parents(&parents);
}

/// Load all child→parent mappings
pub fn load_agent_parents() -> HashMap<String, String> {
    let path = agent_parents_path();
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write agent parents to disk
fn write_agent_parents(parents: &HashMap<String, String>) {
    let path = agent_parents_path();
    if let Ok(json) = serde_json::to_string_pretty(parents) {
        fs::write(&path, json).ok();
    }
}

/// Remove a child→parent mapping
pub fn remove_agent_parent(child: &str) {
    let mut parents = load_agent_parents();
    parents.remove(child);
    write_agent_parents(&parents);
}

/// Load the memory file contents (empty string if missing)
pub fn load_memory() -> String {
    let path = memory_path();
    fs::read_to_string(&path).unwrap_or_default()
}

/// Write a full state snapshot to ~/.omar/memory.md
///
/// Captures: active projects, worker states + tasks, manager status,
/// and the manager's recent conversation context.
pub fn write_memory(agents: &[AgentInfo], manager: Option<&AgentInfo>, client: &TmuxClient) {
    let projects = projects::load_projects();
    let mut worker_tasks = load_worker_tasks();

    // Clean up stale entries from worker_tasks (sessions that no longer exist)
    let active_sessions: Vec<String> = agents.iter().map(|a| a.session.name.clone()).collect();
    worker_tasks.retain(|k, _| active_sessions.contains(k));
    write_worker_tasks(&worker_tasks);

    // Clean up stale entries from agent_parents
    let mut agent_parents = load_agent_parents();
    agent_parents.retain(|k, _| active_sessions.contains(k));
    write_agent_parents(&agent_parents);

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
        out.push_str("## Active Workers\n");
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
        if let Ok(output) = client.capture_pane(crate::manager::MANAGER_SESSION, 20) {
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

    let path = memory_path();
    fs::write(&path, &out).ok();
}
