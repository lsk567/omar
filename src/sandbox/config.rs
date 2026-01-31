use serde::{Deserialize, Serialize};

/// Top-level sandbox configuration.
///
/// When `enabled` is false (the default), all commands are executed without
/// sandboxing. When true, worker agents are launched inside Docker containers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Whether sandboxing is enabled (opt-in, off by default)
    #[serde(default)]
    pub enabled: bool,

    /// Docker image to use for sandboxed workers
    #[serde(default = "default_image")]
    pub image: String,

    /// Docker network mode for sandboxed containers.
    /// "none" = no network (strictest, but agent can't call LLM API);
    /// "bridge" = default Docker network (allows outbound connections);
    /// "host" = share host network namespace.
    #[serde(default = "default_network")]
    pub network: String,

    /// Resource limits for sandboxed containers
    #[serde(default)]
    pub limits: ResourceLimits,

    /// Filesystem access policy
    #[serde(default)]
    pub filesystem: FilesystemPolicy,
}

/// Resource limits applied to each sandboxed container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Memory limit (Docker format, e.g. "4g")
    #[serde(default = "default_memory")]
    pub memory: String,

    /// CPU quota (number of CPUs, e.g. 2.0)
    #[serde(default = "default_cpus")]
    pub cpus: f64,

    /// Maximum number of PIDs inside the container
    #[serde(default = "default_pids_limit")]
    pub pids_limit: u32,
}

/// Filesystem access policy for workspace mounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemPolicy {
    /// Workspace mount mode: "rw" (read-write) or "ro" (read-only)
    #[serde(default = "default_workspace_access")]
    pub workspace_access: String,

    /// Additional host paths to bind-mount into the container.
    /// Format: "host_path:container_path:mode" (e.g. "/opt/tools:/opt/tools:ro")
    /// or just "path" to mount at the same path read-only.
    #[serde(default)]
    pub bind_mounts: Vec<String>,
}

fn default_image() -> String {
    "ubuntu:22.04".to_string()
}

fn default_network() -> String {
    "bridge".to_string()
}

fn default_memory() -> String {
    "4g".to_string()
}

fn default_cpus() -> f64 {
    2.0
}

fn default_pids_limit() -> u32 {
    256
}

fn default_workspace_access() -> String {
    "rw".to_string()
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            image: default_image(),
            network: default_network(),
            limits: ResourceLimits::default(),
            filesystem: FilesystemPolicy::default(),
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory: default_memory(),
            cpus: default_cpus(),
            pids_limit: default_pids_limit(),
        }
    }
}

impl Default for FilesystemPolicy {
    fn default() -> Self {
        Self {
            workspace_access: default_workspace_access(),
            bind_mounts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.image, "ubuntu:22.04");
        assert_eq!(config.network, "bridge");
        assert_eq!(config.limits.memory, "4g");
        assert_eq!(config.limits.cpus, 2.0);
        assert_eq!(config.limits.pids_limit, 256);
        assert_eq!(config.filesystem.workspace_access, "rw");
        assert!(config.filesystem.bind_mounts.is_empty());
    }

    #[test]
    fn test_sandbox_config_absent_section() {
        // When [sandbox] section is entirely absent, Config should get defaults
        let toml = "";
        let config: SandboxConfig = toml::from_str(toml).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.image, "ubuntu:22.04");
    }

    #[test]
    fn test_sandbox_config_parse_enabled() {
        let toml = r#"
enabled = true
image = "node:20"
network = "none"

[limits]
memory = "8g"
cpus = 4.0
pids_limit = 512

[filesystem]
workspace_access = "ro"
bind_mounts = ["/opt/tools:/opt/tools:ro"]
"#;
        let config: SandboxConfig = toml::from_str(toml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.image, "node:20");
        assert_eq!(config.network, "none");
        assert_eq!(config.limits.memory, "8g");
        assert_eq!(config.limits.cpus, 4.0);
        assert_eq!(config.limits.pids_limit, 512);
        assert_eq!(config.filesystem.workspace_access, "ro");
        assert_eq!(
            config.filesystem.bind_mounts,
            vec!["/opt/tools:/opt/tools:ro"]
        );
    }

    #[test]
    fn test_sandbox_config_partial_parse() {
        let toml = r#"
enabled = true
"#;
        let config: SandboxConfig = toml::from_str(toml).unwrap();
        assert!(config.enabled);
        // Everything else should be defaults
        assert_eq!(config.image, "ubuntu:22.04");
        assert_eq!(config.limits.memory, "4g");
        assert_eq!(config.limits.cpus, 2.0);
    }
}
