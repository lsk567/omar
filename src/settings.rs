//! Dashboard settings persisted to ~/.omar/settings.json

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

fn settings_path() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar");
    fs::create_dir_all(&dir).ok();
    dir.join("settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSettings {
    #[serde(default = "default_true")]
    pub show_event_queue: bool,
    #[serde(default)]
    pub sidebar_right: bool,
}

fn default_true() -> bool {
    true
}

impl Default for DashboardSettings {
    fn default() -> Self {
        Self {
            show_event_queue: true,
            sidebar_right: false,
        }
    }
}

impl DashboardSettings {
    pub fn load() -> Self {
        let path = settings_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            fs::write(&path, json).ok();
        }
    }

    /// Number of toggleable settings
    pub fn count(&self) -> usize {
        2
    }

    /// Get label and current value for a setting by index
    pub fn item(&self, index: usize) -> Option<(&str, bool)> {
        match index {
            0 => Some(("Show event queue in sidebar", self.show_event_queue)),
            1 => Some(("Sidebar on right side", self.sidebar_right)),
            _ => None,
        }
    }

    /// Toggle a setting by index
    pub fn toggle(&mut self, index: usize) {
        match index {
            0 => self.show_event_queue = !self.show_event_queue,
            1 => self.sidebar_right = !self.sidebar_right,
            _ => {}
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings() {
        let s = DashboardSettings::default();
        assert!(s.show_event_queue);
        assert!(!s.sidebar_right);
    }

    #[test]
    fn toggle_setting() {
        let mut s = DashboardSettings::default();
        assert!(s.show_event_queue);
        s.show_event_queue = !s.show_event_queue;
        assert!(!s.show_event_queue);
        assert!(!s.sidebar_right);
        s.sidebar_right = !s.sidebar_right;
        assert!(s.sidebar_right);
    }

    #[test]
    fn item_and_count() {
        let s = DashboardSettings::default();
        assert_eq!(s.count(), 2);
        let (label, val) = s.item(0).unwrap();
        assert_eq!(label, "Show event queue in sidebar");
        assert!(val);
        let (label, val) = s.item(1).unwrap();
        assert_eq!(label, "Sidebar on right side");
        assert!(!val);
        assert!(s.item(2).is_none());
    }

    #[test]
    fn roundtrip_json() {
        let s = DashboardSettings {
            show_event_queue: false,
            sidebar_right: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let s2: DashboardSettings = serde_json::from_str(&json).unwrap();
        assert!(!s2.show_event_queue);
        assert!(s2.sidebar_right);
    }
}
