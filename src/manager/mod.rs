//! Manager agent — prompt embedding and command building

use std::path::{Path, PathBuf};

use crate::memory;

/// Manager session name (exported for use in app.rs)
pub const MANAGER_SESSION: &str = "omar-agent-ea";

// Embed prompt files at compile time so they work regardless of CWD.
const PROMPT_EA: &str = include_str!("../../prompts/executive-assistant.md");
const PROMPT_AGENT: &str = include_str!("../../prompts/agent.md");

/// Embedded prompt files, keyed by filename.
const EMBEDDED_PROMPTS: &[(&str, &str)] = &[
    ("executive-assistant.md", PROMPT_EA),
    ("agent.md", PROMPT_AGENT),
];

/// Return the `~/.omar/prompts/` directory, writing embedded prompts on first call.
pub fn prompts_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar")
        .join("prompts");
    std::fs::create_dir_all(&dir).ok();

    for (name, content) in EMBEDDED_PROMPTS {
        let path = dir.join(name);
        // Always overwrite so prompts stay in sync with the binary
        std::fs::write(&path, content).ok();
    }

    dir
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Claude,
    Codex,
    Cursor,
    Opencode,
}

fn detect_backend_token(token: &str) -> Option<BackendKind> {
    let token = token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')'));
    let executable = Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(token);

    match executable {
        "claude" => Some(BackendKind::Claude),
        "codex" => Some(BackendKind::Codex),
        "cursor" => Some(BackendKind::Cursor),
        "opencode" => Some(BackendKind::Opencode),
        _ => None,
    }
}

fn detect_backend(base_command: &str) -> Option<BackendKind> {
    base_command
        .split_whitespace()
        .find_map(detect_backend_token)
}

/// Build a CLI command with system prompt loaded from a file via native flag.
///
/// Detects backend from `base_command`:
///   - contains "claude" → `--system-prompt`
///   - contains "codex" → `developer_instructions`
///   - contains "cursor" → `"Load the <path>"` as positional prompt
///   - contains "opencode" → `--prompt`
pub fn build_agent_command(base_command: &str, prompt_file: &Path) -> String {
    let shell_expr = format!("$(cat '{}')", prompt_file.display());

    match detect_backend(base_command) {
        Some(BackendKind::Claude) => {
            format!("{} --system-prompt \"{}\"", base_command, shell_expr)
        }
        Some(BackendKind::Codex) => format!(
            "{} -c \"developer_instructions='''{}'''\"",
            base_command, shell_expr
        ),
        Some(BackendKind::Cursor) => {
            format!(
                "{} \"Load the {} file and follow the instructions.\"",
                base_command,
                prompt_file.display()
            )
        }
        Some(BackendKind::Opencode) => format!("{} --prompt \"{}\"", base_command, shell_expr),
        None => base_command.to_string(),
    }
}

/// Build an EA command with memory state baked into the system prompt.
///
/// Reads the EA prompt, appends the latest memory snapshot, writes a
/// combined file to `~/.omar/ea_prompt_combined.md`, and returns the
/// CLI command with the combined file as the system prompt.
pub fn build_ea_command(base_command: &str) -> String {
    let prompt_file = prompts_dir().join("executive-assistant.md");
    let mem = memory::load_memory();

    if mem.is_empty() {
        return build_agent_command(base_command, &prompt_file);
    }

    let prompt_content = std::fs::read_to_string(&prompt_file).unwrap_or_default();
    let combined = format!(
        "{}\n\n---\n\n## Current OMAR State (from previous session)\n\n{}",
        prompt_content, mem
    );

    let combined_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".omar")
        .join("ea_prompt_combined.md");
    std::fs::create_dir_all(combined_path.parent().unwrap()).ok();
    std::fs::write(&combined_path, &combined).ok();

    build_agent_command(base_command, &combined_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_agent_command_claude() {
        let cmd = build_agent_command("claude --some-flag", Path::new("/tmp/prompts/ea.md"));
        assert_eq!(
            cmd,
            "claude --some-flag --system-prompt \"$(cat '/tmp/prompts/ea.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_opencode() {
        let cmd = build_agent_command("opencode", Path::new("/tmp/prompts/pm.md"));
        assert_eq!(cmd, "opencode --prompt \"$(cat '/tmp/prompts/pm.md')\"");
    }

    #[test]
    fn test_build_agent_command_codex() {
        let cmd = build_agent_command(
            "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox",
            Path::new("/tmp/prompts/ea.md"),
        );
        assert_eq!(
            cmd,
            "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox -c \"developer_instructions='''$(cat '/tmp/prompts/ea.md')'''\""
        );
    }

    #[test]
    fn test_build_agent_command_wrapped_claude() {
        let cmd = build_agent_command(
            "env ANTHROPIC_API_KEY=test claude --some-flag",
            Path::new("/tmp/prompts/ea.md"),
        );
        assert_eq!(
            cmd,
            "env ANTHROPIC_API_KEY=test claude --some-flag --system-prompt \"$(cat '/tmp/prompts/ea.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_wrapped_opencode() {
        let cmd = build_agent_command(
            "npx opencode --model local",
            Path::new("/tmp/prompts/pm.md"),
        );
        assert_eq!(
            cmd,
            "npx opencode --model local --prompt \"$(cat '/tmp/prompts/pm.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_wrapped_codex() {
        let cmd = build_agent_command(
            "env OPENAI_API_KEY=test codex --no-alt-screen",
            Path::new("/tmp/prompts/ea.md"),
        );
        assert_eq!(
            cmd,
            "env OPENAI_API_KEY=test codex --no-alt-screen -c \"developer_instructions='''$(cat '/tmp/prompts/ea.md')'''\""
        );
    }

    #[test]
    fn test_build_agent_command_cursor() {
        let cmd = build_agent_command("cursor agent --yolo", Path::new("/tmp/prompts/ea.md"));
        assert_eq!(
            cmd,
            "cursor agent --yolo \"Load the /tmp/prompts/ea.md file and follow the instructions.\""
        );
    }

    #[test]
    fn test_build_agent_command_unknown_backend() {
        let cmd = build_agent_command("vim", Path::new("/tmp/prompts/ea.md"));
        assert_eq!(cmd, "vim");
    }
}
