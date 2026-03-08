//! Persistent memory — split into two files under ~/.omar/
//!
//! - `system_state.md`  — written exclusively by the Rust backend (authoritative
//!   system state: projects, agents, scheduled events, manager status).
//! - `manager_notes.md` — written exclusively by the manager agent (semantic
//!   context: task summaries, user preferences, completed work notes).
//!
//! `load_memory()` combines both files so the manager gets the full picture on
//! restart without either side clobbering the other.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::app::AgentInfo;
use crate::projects;
use crate::scheduler::ScheduledEvent;
use crate::tmux::TmuxClient;

/// Ensure ~/.omar/ exists and return it
fn omar_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar");
    fs::create_dir_all(&dir).ok();
    dir
}

/// Path to the system state file (Rust-owned)
fn system_state_path() -> PathBuf {
    omar_dir().join("system_state.md")
}

/// Path to the manager notes file (manager-agent-owned)
fn manager_notes_path() -> PathBuf {
    omar_dir().join("manager_notes.md")
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

/// Directory for agent status files
fn status_dir() -> PathBuf {
    let dir = omar_dir().join("status");
    fs::create_dir_all(&dir).ok();
    dir
}

/// Load an agent's self-reported status from ~/.omar/status/<session>.md
pub fn load_agent_status(session_name: &str) -> Option<String> {
    let path = status_dir().join(format!("{}.md", session_name));
    fs::read_to_string(&path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Save an agent's status to ~/.omar/status/<session>.md
pub fn save_agent_status(session_name: &str, status: &str) {
    let path = status_dir().join(format!("{}.md", session_name));
    fs::write(&path, status).ok();
}

/// Combine system state and manager notes into a single memory string.
fn combine_memory(system: &str, notes: &str) -> String {
    match (system.is_empty(), notes.is_empty()) {
        (true, true) => String::new(),
        (false, true) => system.to_string(),
        (true, false) => notes.to_string(),
        (false, false) => format!("{}\n---\n\n{}", system, notes),
    }
}

/// Load combined memory: system_state.md + manager_notes.md
pub fn load_memory() -> String {
    let system = fs::read_to_string(system_state_path()).unwrap_or_default();
    let notes = fs::read_to_string(manager_notes_path()).unwrap_or_default();
    combine_memory(&system, &notes)
}

/// Write system state snapshot to ~/.omar/system_state.md
///
/// Captures: active projects, worker states + tasks, scheduled events
/// (with exact periods and payloads for recovery), manager status,
/// and the manager's recent conversation context.
pub fn write_memory(
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    client: &TmuxClient,
    events: &[ScheduledEvent],
) {
    let projects = projects::load_projects();
    let mut worker_tasks = load_worker_tasks();

    // Clean up stale entries from worker_tasks (sessions that no longer exist)
    let active_sessions: Vec<String> = agents.iter().map(|a| a.session.name.clone()).collect();
    worker_tasks.retain(|k, _| active_sessions.contains(k));
    write_worker_tasks(&worker_tasks);

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

    // Scheduled events — include exact periods and full payloads for recovery
    if !events.is_empty() {
        out.push_str("## Scheduled Events\n");
        for ev in events {
            let type_label = match ev.recurring_ns {
                Some(ns) => {
                    let secs = ns / 1_000_000_000;
                    format!("cron every {}s (period_ns={})", secs, ns)
                }
                None => "once".to_string(),
            };
            out.push_str(&format!(
                "- id={} [{}] {} -> {}\n  payload: {}\n",
                ev.id, type_label, ev.sender, ev.receiver, ev.payload
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

    let path = system_state_path();
    fs::write(&path, &out).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_both_empty() {
        assert_eq!(combine_memory("", ""), "");
    }

    #[test]
    fn combine_system_only() {
        let result = combine_memory("# OMAR State\n", "");
        assert_eq!(result, "# OMAR State\n");
    }

    #[test]
    fn combine_notes_only() {
        let result = combine_memory("", "# Manager Notes\n");
        assert_eq!(result, "# Manager Notes\n");
    }

    #[test]
    fn combine_both_present() {
        let result = combine_memory("# OMAR State\n", "# Manager Notes\n");
        assert!(result.contains("# OMAR State\n"));
        assert!(result.contains("---"));
        assert!(result.contains("# Manager Notes\n"));
        // System state comes first, then separator, then notes
        let sep_pos = result.find("---").unwrap();
        let state_pos = result.find("# OMAR State").unwrap();
        let notes_pos = result.find("# Manager Notes").unwrap();
        assert!(state_pos < sep_pos);
        assert!(sep_pos < notes_pos);
    }

    #[test]
    fn scheduled_event_format_includes_period_and_payload() {
        // Verify the format string used in write_memory includes exact details
        let ns: u64 = 300_000_000_000; // 300s = 5min
        let secs = ns / 1_000_000_000;
        let label = format!("cron every {}s (period_ns={})", secs, ns);
        assert_eq!(label, "cron every 300s (period_ns=300000000000)");

        let line = format!(
            "- id={} [{}] {} -> {}\n  payload: {}\n",
            "abc-123", label, "manager", "worker-1", "check deployment status"
        );
        assert!(line.contains("period_ns=300000000000"));
        assert!(line.contains("payload: check deployment status"));
        assert!(line.contains("id=abc-123"));
    }
}
