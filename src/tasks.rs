//! Persistent task registry for MCP-managed orchestration.
//!
//! Unlike the legacy worker/project tracking files, this registry is intended
//! to be the authoritative record for task lifecycle state.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static TASKS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed,
    Replaced,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub ea_id: u32,
    pub project_id: usize,
    pub project_name: String,
    pub agent_name: String,
    pub parent_agent: String,
    pub task_text: String,
    pub backend: Option<String>,
    pub model: Option<String>,
    pub status: TaskStatus,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub replacement_count: usize,
    #[serde(default)]
    pub previous_agents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
}

fn tasks_path_in(state_dir: &Path) -> PathBuf {
    fs::create_dir_all(state_dir).ok();
    state_dir.join("task_registry.json")
}

fn read_tasks_unlocked(state_dir: &Path) -> Vec<TaskRecord> {
    let path = tasks_path_in(state_dir);
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_tasks_unlocked(state_dir: &Path, tasks: &[TaskRecord]) -> Result<()> {
    let path = tasks_path_in(state_dir);
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(tasks)?;
    fs::write(&tmp, json).context("Failed to write temporary task registry")?;
    fs::rename(&tmp, &path).context("Failed to replace task registry")?;
    Ok(())
}

pub fn load_tasks_from(state_dir: &Path) -> Vec<TaskRecord> {
    let _guard = TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_tasks_unlocked(state_dir)
}

pub fn add_task_in(state_dir: &Path, record: TaskRecord) -> Result<()> {
    let _guard = TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = read_tasks_unlocked(state_dir);
    tasks.push(record);
    save_tasks_unlocked(state_dir, &tasks)
}

pub fn update_task_in<F>(state_dir: &Path, task_id: &str, mutator: F) -> Result<Option<TaskRecord>>
where
    F: FnOnce(&mut TaskRecord),
{
    let _guard = TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = read_tasks_unlocked(state_dir);
    let mut updated = None;
    for task in &mut tasks {
        if task.task_id == task_id {
            mutator(task);
            updated = Some(task.clone());
            break;
        }
    }
    save_tasks_unlocked(state_dir, &tasks)?;
    Ok(updated)
}

pub fn find_task_in(state_dir: &Path, task_id: &str) -> Option<TaskRecord> {
    let _guard = TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_tasks_unlocked(state_dir)
        .into_iter()
        .find(|task| task.task_id == task_id)
}

pub fn find_task_by_agent_in(state_dir: &Path, agent_name: &str) -> Option<TaskRecord> {
    let _guard = TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_tasks_unlocked(state_dir)
        .into_iter()
        .find(|task| task.agent_name == agent_name)
}
