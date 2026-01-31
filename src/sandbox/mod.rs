pub mod config;
mod docker;
mod none;

use anyhow::Result;

pub use config::SandboxConfig;
use docker::DockerProvider;
use none::NoopProvider;

/// Trait for sandbox providers that wrap agent commands in isolated environments.
#[allow(dead_code)]
pub trait SandboxProvider: Send + Sync {
    /// Wrap a command for sandboxed execution.
    ///
    /// For Docker, this returns a `docker run ...` command string.
    /// For NoopProvider, this returns the command unchanged.
    ///
    /// `session_name` is used to derive the container name.
    /// `workdir` is the workspace directory to mount.
    fn wrap_command(&self, session_name: &str, command: &str, workdir: Option<&str>) -> String;

    /// Clean up resources for a single agent (e.g. remove its Docker container).
    fn cleanup(&self, session_name: &str) -> Result<()>;

    /// Clean up all sandbox resources (e.g. remove all omar-sandbox-* containers).
    fn cleanup_all(&self) -> Result<()>;

    /// Perform one-time setup (e.g. verify Docker is available, pull image).
    fn setup(&self) -> Result<()>;

    /// Whether sandboxing is active.
    fn is_enabled(&self) -> bool;
}

/// Create the appropriate sandbox provider based on configuration.
pub fn create_provider(config: &SandboxConfig) -> Box<dyn SandboxProvider> {
    if config.enabled {
        Box::new(DockerProvider::new(config.clone()))
    } else {
        Box::new(NoopProvider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_provider_disabled() {
        let config = SandboxConfig::default();
        let provider = create_provider(&config);
        assert!(!provider.is_enabled());
    }

    #[test]
    fn test_create_provider_enabled() {
        let config = SandboxConfig {
            enabled: true,
            ..Default::default()
        };
        let provider = create_provider(&config);
        assert!(provider.is_enabled());
    }

    #[test]
    fn test_disabled_provider_passthrough() {
        let config = SandboxConfig::default();
        let provider = create_provider(&config);
        let cmd = "claude --dangerously-skip-permissions";
        assert_eq!(provider.wrap_command("test", cmd, None), cmd);
    }

    #[test]
    fn test_enabled_provider_wraps_command() {
        let config = SandboxConfig {
            enabled: true,
            ..Default::default()
        };
        let provider = create_provider(&config);
        let cmd = provider.wrap_command("test", "claude", None);
        assert!(cmd.starts_with("docker run"));
        assert!(cmd.contains("omar-sandbox-test"));
    }
}
