//! Read/write the bridge's `[slack_bridge].active_ea` field in the shared
//! `~/.omar/config.toml`. Uses `toml_edit` so other sections (and any
//! comments) are preserved verbatim on save.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item, Table};

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

/// Persist `[slack_bridge].active_ea = ea_name` in the shared config file,
/// preserving every other section. Creates the file if absent.
pub fn save_active_ea(omar_dir: &Path, ea_name: &str) -> Result<()> {
    let path = config_path(omar_dir);
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: DocumentMut = if content.is_empty() {
        DocumentMut::new()
    } else {
        content
            .parse()
            .with_context(|| format!("Failed to parse {}", path.display()))?
    };
    if !matches!(doc.get(SECTION), Some(Item::Table(_))) {
        doc[SECTION] = Item::Table(Table::new());
    }
    doc[SECTION][FIELD] = value(ea_name);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        save_active_ea(dir.path(), "Research").unwrap();
        assert_eq!(load_active_ea(dir.path()), Some("Research".to_string()));
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_active_ea(dir.path()), None);
    }

    #[test]
    fn save_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_path(dir.path());
        std::fs::write(
            &path,
            "# top comment\n[dashboard]\nrefresh_interval = 5\n\n[agent]\ndefault_command = \"claude\"\n",
        )
        .unwrap();

        save_active_ea(dir.path(), "Research").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# top comment"));
        assert!(content.contains("[dashboard]"));
        assert!(content.contains("refresh_interval = 5"));
        assert!(content.contains("[agent]"));
        assert!(content.contains("default_command = \"claude\""));
        assert!(content.contains("[slack_bridge]"));
        assert!(content.contains("active_ea = \"Research\""));
    }

    #[test]
    fn save_overwrites_existing_value() {
        let dir = tempfile::tempdir().unwrap();
        save_active_ea(dir.path(), "First").unwrap();
        save_active_ea(dir.path(), "Second").unwrap();
        assert_eq!(load_active_ea(dir.path()), Some("Second".to_string()));
    }
}
