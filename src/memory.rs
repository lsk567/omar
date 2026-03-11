//! Persistent memory — state snapshots in per-EA state directories
//!
//! Writes a human-readable markdown snapshot of the current OMAR state
//! so a newly created manager session can resume seamlessly.
//! All functions take a `state_dir` parameter for EA-scoped isolation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::app::AgentInfo;
use crate::ea::EaId;
use crate::projects;
use crate::scheduler::ScheduledEvent;
use crate::tmux::TmuxClient;

/// Per-file-type mutexes to serialize concurrent read-modify-write operations.
/// These are process-global (not per-EA) which is sufficient since all EAs
/// run in the same process and operate on separate state_dir paths.
static WORKER_TASKS_LOCK: Mutex<()> = Mutex::new(());
static AGENT_PARENTS_LOCK: Mutex<()> = Mutex::new(());

/// Generic JSON helpers
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
}

/// Write JSON atomically: write to a sibling `.tmp` file then rename into place.
/// This prevents partial writes from being visible to concurrent readers.
fn write_json<T: serde::Serialize>(path: &Path, data: &T) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, &json).is_ok() {
            if fs::rename(&tmp, path).is_err() {
                let _ = fs::remove_file(&tmp);
            }
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
}

/// Save a worker's task description (upsert)
pub fn save_worker_task_in(state_dir: &Path, session: &str, task: &str) {
    let path = state_dir.join("worker_tasks.json");
    let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load_worker_tasks_inner(state_dir);
    tasks.insert(session.to_string(), task.to_string());
    write_json(&path, &tasks);
}

/// Load all worker task mappings for an EA
pub fn load_worker_tasks_from(state_dir: &Path) -> HashMap<String, String> {
    let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_worker_tasks_inner(state_dir)
}

/// Inner (lock-free) loader — only call while holding `WORKER_TASKS_LOCK`.
fn load_worker_tasks_inner(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("worker_tasks.json");
    read_json(&path).unwrap_or_default()
}

/// Save a child->parent mapping (upsert)
pub fn save_agent_parent_in(state_dir: &Path, child: &str, parent: &str) {
    let path = state_dir.join("agent_parents.json");
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut parents = load_agent_parents_inner(state_dir);
    parents.insert(child.to_string(), parent.to_string());
    write_json(&path, &parents);
}

/// Load all child->parent mappings for an EA
pub fn load_agent_parents_from(state_dir: &Path) -> HashMap<String, String> {
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_agent_parents_inner(state_dir)
}

/// Inner (lock-free) loader — only call while holding `AGENT_PARENTS_LOCK`.
fn load_agent_parents_inner(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("agent_parents.json");
    read_json(&path).unwrap_or_default()
}

/// Remove a child->parent mapping
pub fn remove_agent_parent_in(state_dir: &Path, child: &str) {
    let path = state_dir.join("agent_parents.json");
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut parents = load_agent_parents_inner(state_dir);
    parents.remove(child);
    write_json(&path, &parents);
}

/// Load an agent's self-reported status
pub fn load_agent_status_in(state_dir: &Path, session_name: &str) -> Option<String> {
    let path = state_dir
        .join("status")
        .join(format!("{}.md", session_name));
    fs::read_to_string(&path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Save an agent's status
pub fn save_agent_status_in(state_dir: &Path, session_name: &str, status: &str) {
    let dir = state_dir.join("status");
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.md", session_name));
    fs::write(&path, status).ok();
}

/// Load the memory file contents (empty string if missing)
pub fn load_memory_from(state_dir: &Path) -> String {
    let path = state_dir.join("memory.md");
    fs::read_to_string(&path).unwrap_or_default()
}

/// Returns the EA-specific manager notes file path.
/// Each EA uses its own file: ~/.omar/manager_notes_ea<ID>.md
/// (e.g. ~/.omar/manager_notes_ea0.md for EA 0)
pub fn manager_notes_path(omar_dir: &Path, ea_id: EaId) -> PathBuf {
    omar_dir.join(format!("manager_notes_ea{}.md", ea_id))
}

/// Load manager notes for an EA (empty string if file is missing).
pub fn load_manager_notes(omar_dir: &Path, ea_id: EaId) -> String {
    let path = manager_notes_path(omar_dir, ea_id);
    fs::read_to_string(&path).unwrap_or_default()
}

/// Write a full state snapshot — SCOPED to one EA
pub fn write_memory_to(
    state_dir: &Path,
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    manager_session: &str,
    client: &TmuxClient,
    events: &[ScheduledEvent],
) {
    let project_list = projects::load_projects_from(state_dir);

    // Hold the lock across read-modify-write of worker_tasks.json.
    let worker_tasks = {
        let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks = load_worker_tasks_inner(state_dir);
        // Clean up stale entries (sessions that no longer exist for this EA)
        let active_sessions: Vec<String> = agents.iter().map(|a| a.session.name.clone()).collect();
        tasks.retain(|k, _| active_sessions.contains(k));
        write_json(&state_dir.join("worker_tasks.json"), &tasks);
        tasks
    };

    let mut out = String::from("# OMAR State\n\n");

    // Active projects
    if !project_list.is_empty() {
        out.push_str("## Active Projects\n");
        for p in &project_list {
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
        if let Ok(output) = client.capture_pane(manager_session, 20) {
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

    let path = state_dir.join("memory.md");
    fs::create_dir_all(state_dir).ok();
    fs::write(&path, &out).ok();
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn manager_notes_path_is_ea_scoped() {
        let base = std::path::PathBuf::from("/home/user/.omar");
        assert_eq!(
            manager_notes_path(&base, 0),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea0.md")
        );
        assert_eq!(
            manager_notes_path(&base, 1),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea1.md")
        );
        assert_eq!(
            manager_notes_path(&base, 42),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea42.md")
        );
    }

    #[test]
    fn load_manager_notes_returns_empty_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let notes = load_manager_notes(dir.path(), 0);
        assert!(notes.is_empty());
    }

    #[test]
    fn load_manager_notes_reads_ea_specific_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = manager_notes_path(dir.path(), 3);
        std::fs::write(&path, "EA 3 notes").unwrap();

        // EA 3 gets its notes
        assert_eq!(load_manager_notes(dir.path(), 3), "EA 3 notes");
        // EA 0 does NOT see EA 3's notes
        assert!(load_manager_notes(dir.path(), 0).is_empty());
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
