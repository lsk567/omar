use anyhow::Result;
use std::process::Command;

use super::config::SandboxConfig;
use super::SandboxProvider;

/// Docker-based sandbox provider.
///
/// Wraps agent commands in `docker run` with security hardening:
/// - `--network none` — no exfiltration, no API access
/// - `--security-opt no-new-privileges --cap-drop ALL`
/// - `--read-only --tmpfs /tmp:rw,noexec,size=512m`
/// - Configurable memory, CPU, and PID limits
/// - Workspace mounted with configurable access mode
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
            // Networking: block all
            "--network".to_string(),
            "none".to_string(),
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

        // Interactive mode for tmux compatibility
        args.push("-it".to_string());

        // Image
        args.push(self.config.image.clone());

        // Command to run inside container
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        args
    }
}

impl SandboxProvider for DockerProvider {
    fn wrap_command(&self, session_name: &str, command: &str, workdir: Option<&str>) -> String {
        let args = self.docker_args(session_name, command, workdir);
        // Build: docker run --rm --name ... <image> sh -c "<command>"
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
            limits: ResourceLimits {
                memory: "4g".to_string(),
                cpus: 2.0,
                pids_limit: 256,
            },
            filesystem: FilesystemPolicy {
                workspace_access: "rw".to_string(),
            },
        }
    }

    #[test]
    fn test_wrap_command_basic() {
        let provider = DockerProvider::new(test_config());
        let cmd = provider.wrap_command("test-agent", "some-agent --flag", None);

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
        assert!(cmd.contains("sh -c 'some-agent --flag'"));
    }

    #[test]
    fn test_wrap_command_with_workdir() {
        let provider = DockerProvider::new(test_config());
        let cmd =
            provider.wrap_command("worker-1", "some-agent --flag", Some("/home/user/project"));

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
    fn test_is_enabled() {
        let provider = DockerProvider::new(test_config());
        assert!(provider.is_enabled());
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
