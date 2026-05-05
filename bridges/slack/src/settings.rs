//! Read the bridge's `[slack_bridge].active_ea` field from the shared
//! `~/.omar/config.toml`. The file is the single source of truth and is
//! only edited manually (or by the dashboard); the bridge never writes
//! it, to avoid any path that lets a Slack peer mutate the workspace
//! target.

use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

const SECTION: &str = "slack_bridge";
const FIELD: &str = "active_ea";

pub fn config_path(omar_dir: &Path) -> PathBuf {
    omar_dir.join("config.toml")
}

/// Read `[slack_bridge].active_ea` from `~/.omar/config.toml`. Returns
/// `None` when the file is missing, unparseable, or the field is unset.
pub fn load_active_ea(omar_dir: &Path) -> Option<String> {
    let path = config_path(omar_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    let doc: DocumentMut = content.parse().ok()?;
    doc.get(SECTION)?
        .get(FIELD)?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_active_ea(dir.path()), None);
    }

    #[test]
    fn load_reads_persisted_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(
            &path,
            "[dashboard]\nrefresh_interval = 5\n\n[slack_bridge]\nactive_ea = \"Research\"\n",
        )
        .unwrap();

        assert_eq!(load_active_ea(dir.path()), Some("Research".to_string()));
    }

    #[test]
    fn load_returns_none_when_section_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(&path, "[dashboard]\nrefresh_interval = 5\n").unwrap();

        assert_eq!(load_active_ea(dir.path()), None);
    }

    #[test]
    fn load_returns_none_when_field_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(&path, "[slack_bridge]\nother_field = 1\n").unwrap();

        assert_eq!(load_active_ea(dir.path()), None);
    }
}
