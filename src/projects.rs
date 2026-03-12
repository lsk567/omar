//! Project management — CRUD on per-EA tasks.md
//!
//! File format: numbered lines like `1. Project name`
//! IDs are stable: deletion does NOT renumber remaining items.
//! New projects get max_existing_id + 1.

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Mutex to serialize concurrent read-modify-write on tasks.md.
/// Same pattern as WORKER_TASKS_LOCK in memory.rs.
static PROJECTS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct Project {
    pub id: usize,
    pub name: String,
}

/// Path to the projects file for an EA
pub fn projects_path_in(state_dir: &Path) -> PathBuf {
    fs::create_dir_all(state_dir).ok();
    state_dir.join("tasks.md")
}

/// Load projects from an EA's state directory
pub fn load_projects_from(state_dir: &Path) -> Vec<Project> {
    let path = projects_path_in(state_dir);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_projects(&content)
}

/// Parse project lines from content
fn parse_projects(content: &str) -> Vec<Project> {
    let mut projects = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Match lines like "1. Project name" — preserve the actual numeric ID
        if let Some(dot_pos) = trimmed.find(". ") {
            let num_str = &trimmed[..dot_pos];
            if num_str.chars().all(|c| c.is_ascii_digit()) && !num_str.is_empty() {
                if let Ok(id) = num_str.parse::<usize>() {
                    let name = trimmed[dot_pos + 2..].trim();
                    if !name.is_empty() {
                        projects.push(Project {
                            id,
                            name: name.to_string(),
                        });
                    }
                }
            }
        }
    }
    projects
}

/// Save projects to an EA's state directory (IDs are preserved, not renumbered)
pub fn save_projects_to(state_dir: &Path, projects: &[Project]) -> Result<()> {
    let path = projects_path_in(state_dir);
    let content: String = projects
        .iter()
        .map(|p| format!("{}. {}", p.id, p.name))
        .collect::<Vec<_>>()
        .join("\n");
    let content = if content.is_empty() {
        String::new()
    } else {
        format!("{}\n", content)
    };
    fs::write(&path, content)?;
    Ok(())
}

/// Add a project to an EA, returns new id
pub fn add_project_in(state_dir: &Path, name: &str) -> Result<usize> {
    let _guard = PROJECTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut projects = load_projects_from(state_dir);
    let id = projects.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    projects.push(Project {
        id,
        name: name.to_string(),
    });
    save_projects_to(state_dir, &projects)?;
    Ok(id)
}

/// Remove a project by id from an EA, returns whether it was found
pub fn remove_project_in(state_dir: &Path, id: usize) -> Result<bool> {
    let _guard = PROJECTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut projects = load_projects_from(state_dir);
    if let Some(pos) = projects.iter().position(|p| p.id == id) {
        projects.remove(pos);
        save_projects_to(state_dir, &projects)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_projects() {
        let content = "1. Build REST API\n2. Set up CI/CD\n3. Refactor DB\n";
        let projects = parse_projects(content);
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "Build REST API");
        assert_eq!(projects[1].name, "Set up CI/CD");
        assert_eq!(projects[2].name, "Refactor DB");
        assert_eq!(projects[0].id, 1);
        assert_eq!(projects[2].id, 3);
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_projects("").is_empty());
        assert!(parse_projects("   \n\n").is_empty());
    }

    #[test]
    fn test_parse_ignores_bad_lines() {
        let content = "1. Good line\nrandom text\n- bullet\n2. Another good line\n";
        let projects = parse_projects(content);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "Good line");
        assert_eq!(projects[1].name, "Another good line");
    }

    #[test]
    fn test_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        add_project_in(state_dir, "Alpha").unwrap();
        add_project_in(state_dir, "Beta").unwrap();

        let loaded = load_projects_from(state_dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Alpha");
        assert_eq!(loaded[1].name, "Beta");
    }

    #[test]
    fn test_remove_project() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        add_project_in(state_dir, "Alpha").unwrap();
        add_project_in(state_dir, "Beta").unwrap();
        add_project_in(state_dir, "Gamma").unwrap();

        assert!(remove_project_in(state_dir, 2).unwrap());
        let loaded = load_projects_from(state_dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Alpha");
        assert_eq!(loaded[1].name, "Gamma");
        // IDs must be stable after deletion — Gamma keeps id=3, not renumbered to 2
        assert_eq!(loaded[0].id, 1);
        assert_eq!(loaded[1].id, 3);
    }

    #[test]
    fn test_stable_ids_after_delete_and_add() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        let id1 = add_project_in(state_dir, "Alpha").unwrap();
        let id2 = add_project_in(state_dir, "Beta").unwrap();
        let id3 = add_project_in(state_dir, "Gamma").unwrap();
        assert_eq!((id1, id2, id3), (1, 2, 3));

        // Delete middle project
        assert!(remove_project_in(state_dir, id2).unwrap());

        // Remaining IDs unchanged
        let loaded = load_projects_from(state_dir);
        assert_eq!(loaded[0].id, 1);
        assert_eq!(loaded[1].id, 3);

        // New project gets max+1 = 4, not 2
        let id4 = add_project_in(state_dir, "Delta").unwrap();
        assert_eq!(id4, 4);

        let loaded = load_projects_from(state_dir);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[2].id, 4);
        assert_eq!(loaded[2].name, "Delta");
    }
}
