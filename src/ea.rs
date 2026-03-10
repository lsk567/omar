//! Multi-EA support — EaId type, EA registry, and resolution utilities.
//!
//! Each EA (Executive Assistant) owns a namespace of tmux sessions, a state
//! directory, and an isolated project board / memory / event scope.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// EA identifier. Simple integer. EA 0 always exists.
pub type EaId = u32;

/// Metadata for a registered EA, persisted in ~/.omar/eas.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EaInfo {
    pub id: EaId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: u64, // Unix timestamp
}

/// The tmux session prefix for an EA's worker agents.
/// EA 0: "omar-agent-0-"
/// EA 1: "omar-agent-1-"
///
/// IMPORTANT: base_prefix must end with '-' (e.g., "omar-agent-").
/// Do NOT trim it — the trailing '-' separates the base from the ea_id.
pub fn ea_prefix(ea_id: EaId, base_prefix: &str) -> String {
    format!("{}{}-", base_prefix, ea_id)
}

/// The tmux session name for an EA's manager (the EA itself).
/// EA 0: "omar-agent-ea-0"
/// EA 1: "omar-agent-ea-1"
pub fn ea_manager_session(ea_id: EaId, base_prefix: &str) -> String {
    format!("{}ea-{}", base_prefix, ea_id)
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
    let mut eas: Vec<EaInfo> = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    // Ensure EA 0 always exists
    if !eas.iter().any(|e| e.id == 0) {
        eas.insert(
            0,
            EaInfo {
                id: 0,
                name: "Default".to_string(),
                description: None,
                created_at: 0,
            },
        );
    }
    eas
}

/// Load the high-water mark counter for EA IDs.
/// Returns 0 if the counter file doesn't exist.
fn load_next_id_counter(base_dir: &Path) -> EaId {
    let path = base_dir.join("ea_next_id");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Save the high-water mark counter for EA IDs.
fn save_next_id_counter(base_dir: &Path, next_id: EaId) -> anyhow::Result<()> {
    let path = base_dir.join("ea_next_id");
    fs::create_dir_all(base_dir).ok();
    fs::write(&path, next_id.to_string())?;
    Ok(())
}

/// Validate an EA name: must be non-empty, at most 64 chars, and contain only
/// characters in [a-zA-Z0-9_-]. Returns an error describing the violation.
pub fn validate_ea_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("EA name must not be empty");
    }
    if name.len() > 64 {
        anyhow::bail!(
            "EA name must not exceed 64 characters (got {})",
            name.len()
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "EA name '{}' contains invalid characters; only [a-zA-Z0-9_-] are allowed",
            name
        );
    }
    Ok(())
}

/// Register a new EA. Returns the assigned ID.
/// IDs are monotonically increasing and never reused, even after deletion.
pub fn register_ea(
    base_dir: &Path,
    name: &str,
    description: Option<&str>,
) -> anyhow::Result<EaId> {
    validate_ea_name(name)?;
    let mut eas = load_registry(base_dir);
    let max_existing = eas.iter().map(|e| e.id).max().unwrap_or(0);
    let counter = load_next_id_counter(base_dir);
    // Use whichever is higher to ensure monotonicity even after deletions.
    // Fix V8: Use checked_add to prevent u32 overflow wrapping to 0 (which
    // would collide with EA 0 and violate INV5's uniqueness guarantee).
    let next_id = max_existing.max(counter).checked_add(1).ok_or_else(|| {
        anyhow::anyhow!("EA ID space exhausted (u32::MAX reached). Cannot create more EAs.")
    })?;
    let ea = EaInfo {
        id: next_id,
        name: name.to_string(),
        description: description.map(String::from),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    eas.push(ea);
    save_registry(base_dir, &eas)?;
    // Persist the high-water mark so IDs are never reused after deletion
    save_next_id_counter(base_dir, next_id)?;
    // Create state directory
    let state_dir = ea_state_dir(next_id, base_dir);
    fs::create_dir_all(state_dir.join("status"))?;
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
    fs::create_dir_all(base_dir).ok();
    let json = serde_json::to_string_pretty(eas)?;
    // Atomic write: write to temp file, then rename
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Migrate legacy state files from ~/.omar/ to ~/.omar/ea/0/
pub fn migrate_legacy_state(omar_dir: &Path) {
    let ea0_dir = ea_state_dir(0, omar_dir);
    if ea0_dir.join("memory.md").exists() {
        return; // Already migrated
    }
    fs::create_dir_all(ea0_dir.join("status")).ok();

    let files = [
        "tasks.md",
        "memory.md",
        "worker_tasks.json",
        "agent_parents.json",
        "ea_prompt_combined.md",
    ];
    for file in &files {
        let old = omar_dir.join(file);
        let new_path = ea0_dir.join(file);
        if old.exists() && !new_path.exists() {
            fs::rename(&old, &new_path).ok();
        }
    }

    // Move status directory
    let old_status = omar_dir.join("status");
    let new_status = ea0_dir.join("status");
    if old_status.exists() {
        // Copy files from old status to new status (dir may already exist)
        if let Ok(entries) = fs::read_dir(&old_status) {
            for entry in entries.flatten() {
                let dest = new_status.join(entry.file_name());
                if !dest.exists() {
                    fs::rename(entry.path(), dest).ok();
                }
            }
        }
        fs::remove_dir_all(&old_status).ok();
    }
}

/// Migrate legacy tmux sessions to EA 0 naming.
pub fn migrate_legacy_sessions(base_prefix: &str) {
    use std::process::Command;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ea_prefix() {
        assert_eq!(ea_prefix(0, "omar-agent-"), "omar-agent-0-");
        assert_eq!(ea_prefix(1, "omar-agent-"), "omar-agent-1-");
        assert_eq!(ea_prefix(42, "omar-agent-"), "omar-agent-42-");
    }

    #[test]
    fn test_ea_manager_session() {
        assert_eq!(ea_manager_session(0, "omar-agent-"), "omar-agent-ea-0");
        assert_eq!(ea_manager_session(1, "omar-agent-"), "omar-agent-ea-1");
    }

    #[test]
    fn test_ea_state_dir() {
        let base = PathBuf::from("/home/user/.omar");
        assert_eq!(ea_state_dir(0, &base), PathBuf::from("/home/user/.omar/ea/0"));
        assert_eq!(ea_state_dir(1, &base), PathBuf::from("/home/user/.omar/ea/1"));
    }

    #[test]
    fn test_load_registry_empty() {
        let dir = tempfile::tempdir().unwrap();
        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 1);
        assert_eq!(eas[0].id, 0);
        assert_eq!(eas[0].name, "Default");
    }

    #[test]
    fn test_register_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let id = register_ea(dir.path(), "Research", Some("R&D")).unwrap();
        assert_eq!(id, 1);

        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 2);
        assert!(eas.iter().any(|ea| ea.id == 1 && ea.name == "Research"));
    }

    #[test]
    fn test_unregister() {
        let dir = tempfile::tempdir().unwrap();
        register_ea(dir.path(), "Research", None).unwrap();
        unregister_ea(dir.path(), 1).unwrap();
        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 1); // only EA 0 remains
    }

    #[test]
    fn test_unregister_ea0_fails() {
        let dir = tempfile::tempdir().unwrap();
        let result = unregister_ea(dir.path(), 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_manager_not_in_worker_prefix() {
        // Manager session "omar-agent-ea-0" should NOT start with worker prefix "omar-agent-0-"
        let manager = ea_manager_session(0, "omar-agent-");
        let prefix = ea_prefix(0, "omar-agent-");
        assert!(!manager.starts_with(&prefix));
    }

    #[test]
    fn test_ids_monotonic_after_deletion() {
        // IDs should never be reused, even when the highest-ID EA is deleted.
        let dir = tempfile::tempdir().unwrap();

        // Create EA 1 and EA 2
        let id1 = register_ea(dir.path(), "Alpha", None).unwrap();
        assert_eq!(id1, 1);
        let id2 = register_ea(dir.path(), "Beta", None).unwrap();
        assert_eq!(id2, 2);

        // Delete EA 2 (the highest)
        unregister_ea(dir.path(), 2).unwrap();

        // Create a new EA — should get ID 3, NOT 2
        let id3 = register_ea(dir.path(), "Gamma", None).unwrap();
        assert_eq!(id3, 3, "ID should be 3 (monotonic), not 2 (reused)");

        // Delete both non-zero EAs
        unregister_ea(dir.path(), 1).unwrap();
        unregister_ea(dir.path(), 3).unwrap();

        // Create another — should get ID 4, NOT 1
        let id4 = register_ea(dir.path(), "Delta", None).unwrap();
        assert_eq!(id4, 4, "ID should be 4 (monotonic), not 1 (reused)");
    }

    #[test]
    fn test_ids_monotonic_without_counter_file() {
        // If the counter file is missing (e.g., upgraded from old version),
        // IDs should still work correctly based on max existing ID.
        let dir = tempfile::tempdir().unwrap();

        let id1 = register_ea(dir.path(), "First", None).unwrap();
        assert_eq!(id1, 1);

        // Manually delete the counter file to simulate upgrade scenario
        let counter_path = dir.path().join("ea_next_id");
        if counter_path.exists() {
            fs::remove_file(&counter_path).unwrap();
        }

        // Should still use max(existing) + 1 = 2
        let id2 = register_ea(dir.path(), "Second", None).unwrap();
        assert_eq!(id2, 2);
    }
}
