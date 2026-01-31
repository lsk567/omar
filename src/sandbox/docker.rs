use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::config::SandboxConfig;
use super::SandboxProvider;

/// Docker-based sandbox provider.
///
/// Wraps agent commands in `docker run` with security hardening:
/// - Configurable network mode (default: "bridge")
/// - `--security-opt no-new-privileges --cap-drop ALL`
/// - `--read-only --tmpfs /tmp:rw,noexec,size=512m`
/// - Configurable memory, CPU, and PID limits
/// - Auto-detects and mounts the agent binary and config directory
pub struct DockerProvider {
    config: SandboxConfig,
}

impl DockerProvider {
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Build the Docker CLI arguments for a sandboxed container.
    fn docker_args(&self, session_name: &str, command: &str, workdir: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--name".to_string(),
            format!("omar-sandbox-{}", session_name),
            // Networking
            "--network".to_string(),
            self.config.network.clone(),
            // Security hardening
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            // Read-only root filesystem with writable /tmp
            "--read-only".to_string(),
            "--tmpfs".to_string(),
            "/tmp:rw,noexec,size=512m".to_string(),
            // Resource limits
            "--memory".to_string(),
            self.config.limits.memory.clone(),
            "--cpus".to_string(),
            self.config.limits.cpus.to_string(),
            "--pids-limit".to_string(),
            self.config.limits.pids_limit.to_string(),
        ];

        // Mount workspace
        if let Some(dir) = workdir {
            args.push("-v".to_string());
            args.push(format!(
                "{}:/workspace:{}",
                dir, self.config.filesystem.workspace_access
            ));
            args.push("-w".to_string());
            args.push("/workspace".to_string());
        }

        // Auto-detect and mount the agent binary (ro)
        let binary_name = command.split_whitespace().next().unwrap_or("");
        if let Some(real_path) = resolve_binary_path(binary_name) {
            // Mount the binary itself at its resolved path
            args.push("-v".to_string());
            args.push(format!(
                "{}:{}:ro",
                real_path.display(),
                real_path.display()
            ));

            // If the original `which` path differs (symlink), also mount it
            if let Some(which_path) = which_binary(binary_name) {
                if which_path != real_path {
                    args.push("-v".to_string());
                    args.push(format!(
                        "{}:{}:ro",
                        which_path.display(),
                        which_path.display()
                    ));
                }
            }

            // Mount shared libraries the binary might need (/lib, /lib64)
            // The base image has its own, but the binary may link against host-specific ones.
            // Only mount /lib64/ld-linux-x86-64.so.2 if it exists (dynamic linker).
            let ld_path = Path::new("/lib64/ld-linux-x86-64.so.2");
            if ld_path.exists() {
                args.push("-v".to_string());
                args.push(format!("{}:{}:ro", ld_path.display(), ld_path.display()));
            }
        }

        // Auto-detect and mount agent config directory (ro)
        if let Some(home) = dirs::home_dir() {
            // Claude config: ~/.claude/
            let claude_config = home.join(".claude");
            if claude_config.is_dir() {
                args.push("-v".to_string());
                args.push(format!(
                    "{}:{}:ro",
                    claude_config.display(),
                    claude_config.display()
                ));
                // Set HOME so the agent finds its config at the expected path
                args.push("-e".to_string());
                args.push(format!("HOME={}", home.display()));
            }

            // Opencode config: ~/.config/opencode/
            let opencode_config = home.join(".config").join("opencode");
            if opencode_config.is_dir() {
                args.push("-v".to_string());
                args.push(format!(
                    "{}:{}:ro",
                    opencode_config.display(),
                    opencode_config.display()
                ));
            }
        }

        // User-specified bind mounts
        for mount in &self.config.filesystem.bind_mounts {
            args.push("-v".to_string());
            // If the mount spec has no colons, mount at same path read-only
            if mount.contains(':') {
                args.push(mount.clone());
            } else {
                args.push(format!("{}:{}:ro", mount, mount));
            }
        }

        // Interactive mode for tmux compatibility
        args.push("-it".to_string());

        // Image
        args.push(self.config.image.clone());

        // Command to run inside container (use the original command as-is,
        // since we've mounted the binary at its host path)
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        args
    }
}

impl SandboxProvider for DockerProvider {
    fn wrap_command(&self, session_name: &str, command: &str, workdir: Option<&str>) -> String {
        let args = self.docker_args(session_name, command, workdir);
        let mut parts = vec!["docker".to_string()];
        parts.extend(args.into_iter().map(|a| shell_escape(&a)));
        parts.join(" ")
    }

    fn cleanup(&self, session_name: &str) -> Result<()> {
        let container_name = format!("omar-sandbox-{}", session_name);
        // Try to stop and remove the container (ignore errors if already gone)
        let _ = Command::new("docker")
            .args(["rm", "-f", &container_name])
            .output();
        Ok(())
    }

    fn cleanup_all(&self) -> Result<()> {
        // Remove all containers matching our naming convention
        let output = Command::new("docker")
            .args([
                "ps",
                "-a",
                "--filter",
                "name=omar-sandbox-",
                "--format",
                "{{.Names}}",
            ])
            .output()?;

        let names = String::from_utf8_lossy(&output.stdout);
        for name in names.lines() {
            if !name.is_empty() {
                let _ = Command::new("docker").args(["rm", "-f", name]).output();
            }
        }
        Ok(())
    }

    fn setup(&self) -> Result<()> {
        // Verify Docker is available
        let output = Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Docker not found: {}. Install Docker to use sandbox mode.",
                    e
                )
            })?;

        if !output.status.success() {
            anyhow::bail!("Docker daemon not running. Start Docker to use sandbox mode.");
        }

        // Pull the image if not present
        let image = &self.config.image;
        let check = Command::new("docker")
            .args(["image", "inspect", image])
            .output()?;

        if !check.status.success() {
            eprintln!("Pulling sandbox image: {}", image);
            let pull = Command::new("docker").args(["pull", image]).status()?;
            if !pull.success() {
                anyhow::bail!("Failed to pull Docker image: {}", image);
            }
        }

        Ok(())
    }

    fn is_enabled(&self) -> bool {
        true
    }
}

/// Resolve a binary name to its real filesystem path (following symlinks).
fn resolve_binary_path(binary_name: &str) -> Option<PathBuf> {
    let which_path = which_binary(binary_name)?;
    // Follow symlinks to get the real path
    std::fs::canonicalize(&which_path).ok()
}

/// Find a binary's path via `which`.
fn which_binary(binary_name: &str) -> Option<PathBuf> {
    if binary_name.is_empty() {
        return None;
    }
    let output = Command::new("which").arg(binary_name).output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    } else {
        None
    }
}

/// Escape a string for safe inclusion in a shell command.
fn shell_escape(s: &str) -> String {
    if s.chars().all(|c| {
        c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == ':' || c == '='
    }) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::config::{FilesystemPolicy, ResourceLimits, SandboxConfig};

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            enabled: true,
            image: "ubuntu:22.04".to_string(),
            network: "none".to_string(),
            limits: ResourceLimits {
                memory: "4g".to_string(),
                cpus: 2.0,
                pids_limit: 256,
            },
            filesystem: FilesystemPolicy {
                workspace_access: "rw".to_string(),
                bind_mounts: Vec::new(),
            },
        }
    }

    #[test]
    fn test_wrap_command_basic() {
        let provider = DockerProvider::new(test_config());
        // Use a binary that won't be found by `which` so we can test the base structure
        let cmd = provider.wrap_command("test-agent", "nonexistent-agent --flag", None);

        assert!(cmd.starts_with("docker run"));
        assert!(cmd.contains("--rm"));
        assert!(cmd.contains("--name omar-sandbox-test-agent"));
        assert!(cmd.contains("--network none"));
        assert!(cmd.contains("--security-opt no-new-privileges"));
        assert!(cmd.contains("--cap-drop ALL"));
        assert!(cmd.contains("--read-only"));
        assert!(cmd.contains("--tmpfs '/tmp:rw,noexec,size=512m'"));
        assert!(cmd.contains("--memory 4g"));
        assert!(cmd.contains("--cpus 2"));
        assert!(cmd.contains("--pids-limit 256"));
        assert!(cmd.contains("ubuntu:22.04"));
        assert!(cmd.contains("sh -c 'nonexistent-agent --flag'"));
    }

    #[test]
    fn test_wrap_command_with_workdir() {
        let provider = DockerProvider::new(test_config());
        let cmd = provider.wrap_command(
            "worker-1",
            "nonexistent-agent --flag",
            Some("/home/user/project"),
        );

        assert!(cmd.contains("-v /home/user/project:/workspace:rw"));
        assert!(cmd.contains("-w /workspace"));
    }

    #[test]
    fn test_wrap_command_ro_workspace() {
        let mut config = test_config();
        config.filesystem.workspace_access = "ro".to_string();
        let provider = DockerProvider::new(config);
        let cmd = provider.wrap_command("worker-1", "opencode", Some("/home/user/project"));

        assert!(cmd.contains("-v /home/user/project:/workspace:ro"));
    }

    #[test]
    fn test_wrap_command_custom_limits() {
        let mut config = test_config();
        config.limits.memory = "8g".to_string();
        config.limits.cpus = 4.0;
        config.limits.pids_limit = 512;
        let provider = DockerProvider::new(config);
        let cmd = provider.wrap_command("test", "bash", None);

        assert!(cmd.contains("--memory 8g"));
        assert!(cmd.contains("--cpus 4"));
        assert!(cmd.contains("--pids-limit 512"));
    }

    #[test]
    fn test_wrap_command_configurable_network() {
        let mut config = test_config();
        config.network = "bridge".to_string();
        let provider = DockerProvider::new(config);
        let cmd = provider.wrap_command("test", "bash", None);

        assert!(cmd.contains("--network bridge"));
        assert!(!cmd.contains("--network none"));
    }

    #[test]
    fn test_wrap_command_with_bind_mounts() {
        let mut config = test_config();
        config.filesystem.bind_mounts =
            vec!["/opt/tools:/opt/tools:ro".to_string(), "/data".to_string()];
        let provider = DockerProvider::new(config);
        let cmd = provider.wrap_command("test", "bash", None);

        assert!(cmd.contains("-v /opt/tools:/opt/tools:ro"));
        assert!(cmd.contains("-v /data:/data:ro"));
    }

    #[test]
    fn test_wrap_command_mounts_known_binary() {
        // `sh` should exist on every system
        let provider = DockerProvider::new(test_config());
        let cmd = provider.wrap_command("test", "sh -c 'echo hello'", None);

        // The resolved path of `sh` should appear as a -v mount
        if let Some(real_path) = resolve_binary_path("sh") {
            let mount = format!("-v {}:{}:ro", real_path.display(), real_path.display());
            assert!(
                cmd.contains(&mount),
                "Expected mount: {}\nIn: {}",
                mount,
                cmd
            );
        }
    }

    #[test]
    fn test_is_enabled() {
        let provider = DockerProvider::new(test_config());
        assert!(provider.is_enabled());
    }

    #[test]
    fn test_resolve_binary_path_sh() {
        // sh should always exist
        let path = resolve_binary_path("sh");
        assert!(path.is_some());
    }

    #[test]
    fn test_resolve_binary_path_nonexistent() {
        let path = resolve_binary_path("this-binary-definitely-does-not-exist-12345");
        assert!(path.is_none());
    }

    #[test]
    fn test_resolve_binary_path_empty() {
        let path = resolve_binary_path("");
        assert!(path.is_none());
    }

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("ubuntu:22.04"), "ubuntu:22.04");
        assert_eq!(shell_escape("--network"), "--network");
    }

    #[test]
    fn test_shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_escape_with_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }
}
