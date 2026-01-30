//! Project management — CRUD on ~/.omar/tasks.md
//!
//! File format: numbered lines like `1. Project name`
//! Renumbered sequentially on every save.

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Project {
    pub id: usize,
    pub name: String,
}

/// Path to the projects file (~/.omar/tasks.md)
pub fn projects_path() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar");
    fs::create_dir_all(&dir).ok();
    dir.join("tasks.md")
}

/// Load projects from file
pub fn load_projects() -> Vec<Project> {
    let path = projects_path();
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
        // Match lines like "1. Project name"
        if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
            // Consume remaining digits
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            if let Some(name) = rest.strip_prefix(". ") {
                let name = name.trim();
                if !name.is_empty() {
                    projects.push(Project {
                        id: projects.len() + 1,
                        name: name.to_string(),
                    });
                }
            }
        }
    }
    projects
}

/// Save projects to file (renumbered 1..n)
pub fn save_projects(projects: &[Project]) -> Result<()> {
    let path = projects_path();
    let content: String = projects
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{}. {}", i + 1, p.name))
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

/// Add a project, returns new id
pub fn add_project(name: &str) -> Result<usize> {
    let mut projects = load_projects();
    let id = projects.len() + 1;
    projects.push(Project {
        id,
        name: name.to_string(),
    });
    save_projects(&projects)?;
    Ok(id)
}

/// Remove a project by id (1-based), returns whether it was found
pub fn remove_project(id: usize) -> Result<bool> {
    let mut projects = load_projects();
    if id == 0 || id > projects.len() {
        return Ok(false);
    }
    projects.remove(id - 1);
    save_projects(&projects)?;
    Ok(true)
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
        let path = dir.path().join("tasks.md");

        // Write directly to test save/load
        let projects = [
            Project {
                id: 1,
                name: "Alpha".to_string(),
            },
            Project {
                id: 2,
                name: "Beta".to_string(),
            },
        ];
        let content: String = projects
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{}. {}", i + 1, p.name))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&path, &content).unwrap();

        let parsed = parse_projects(&content);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "Alpha");
        assert_eq!(parsed[1].name, "Beta");
    }
}
