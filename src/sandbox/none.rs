use anyhow::Result;

use super::SandboxProvider;

/// No-op sandbox provider (passthrough when sandbox is disabled).
///
/// Returns commands unchanged and performs no cleanup.
pub struct NoopProvider;

impl SandboxProvider for NoopProvider {
    fn wrap_command(&self, _session_name: &str, command: &str, _workdir: Option<&str>) -> String {
        command.to_string()
    }

    fn cleanup(&self, _session_name: &str) -> Result<()> {
        Ok(())
    }

    fn cleanup_all(&self) -> Result<()> {
        Ok(())
    }

    fn setup(&self) -> Result<()> {
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_returns_command_unchanged() {
        let provider = NoopProvider;
        let cmd = "some-agent --flag";
        assert_eq!(provider.wrap_command("test", cmd, None), cmd);
    }

    #[test]
    fn test_noop_returns_command_unchanged_with_workdir() {
        let provider = NoopProvider;
        let cmd = "opencode";
        assert_eq!(provider.wrap_command("test", cmd, Some("/home/user")), cmd);
    }

    #[test]
    fn test_noop_is_not_enabled() {
        let provider = NoopProvider;
        assert!(!provider.is_enabled());
    }

    #[test]
    fn test_noop_cleanup_succeeds() {
        let provider = NoopProvider;
        assert!(provider.cleanup("test").is_ok());
    }

    #[test]
    fn test_noop_cleanup_all_succeeds() {
        let provider = NoopProvider;
        assert!(provider.cleanup_all().is_ok());
    }

    #[test]
    fn test_noop_setup_succeeds() {
        let provider = NoopProvider;
        assert!(provider.setup().is_ok());
    }
}
