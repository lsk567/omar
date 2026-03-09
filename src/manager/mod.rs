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

/// Escape a string for use in a sed replacement (with `|` as delimiter).
fn sed_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('&', "\\&")
        .replace('\n', "\\n")
}

/// Build a CLI command with system prompt loaded from a file via native flag.
///
/// - `prompt_file`: absolute path to the prompt .md file
/// - `substitutions`: `(pattern, replacement)` pairs for sed; empty = use `cat`
///
/// Detects backend from `base_command`:
///   - contains "claude" → `--system-prompt`
///   - contains "opencode" → `--prompt`
pub fn build_agent_command(
    base_command: &str,
    prompt_file: &Path,
    substitutions: &[(&str, &str)],
) -> String {
    let flag = if base_command.contains("claude") {
        "--system-prompt"
    } else if base_command.contains("opencode") {
        "--prompt"
    } else {
        return base_command.to_string();
    };

    let path_str = prompt_file.display();
    let shell_expr = if substitutions.is_empty() {
        format!("$(cat '{}')", path_str)
    } else {
        let sed_script: String = substitutions
            .iter()
            .map(|(pat, repl)| format!("s|{}|{}|g", pat, sed_escape(repl)))
            .collect::<Vec<_>>()
            .join("; ");
        format!("$(sed '{}' '{}')", sed_script, path_str)
    };

    format!("{} {} \"{}\"", base_command, flag, shell_expr)
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
        return build_agent_command(base_command, &prompt_file, &[]);
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

    build_agent_command(base_command, &combined_path, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_agent_command_claude() {
        let cmd = build_agent_command("claude --some-flag", Path::new("/tmp/prompts/ea.md"), &[]);
        assert_eq!(
            cmd,
            "claude --some-flag --system-prompt \"$(cat '/tmp/prompts/ea.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_opencode() {
        let cmd = build_agent_command("opencode", Path::new("/tmp/prompts/pm.md"), &[]);
        assert_eq!(cmd, "opencode --prompt \"$(cat '/tmp/prompts/pm.md')\"");
    }

    #[test]
    fn test_build_agent_command_unknown_backend() {
        let cmd = build_agent_command("vim", Path::new("/tmp/prompts/ea.md"), &[]);
        assert_eq!(cmd, "vim");
    }

    #[test]
    fn test_build_agent_command_with_substitutions() {
        let cmd = build_agent_command(
            "claude",
            Path::new("/prompts/worker.md"),
            &[("{{PARENT_NAME}}", "pm-api"), ("{{TASK}}", "build it")],
        );
        assert_eq!(
            cmd,
            "claude --system-prompt \"$(sed 's|{{PARENT_NAME}}|pm-api|g; s|{{TASK}}|build it|g' '/prompts/worker.md')\""
        );
    }

    #[test]
    fn test_sed_escape() {
        assert_eq!(sed_escape("hello"), "hello");
        assert_eq!(sed_escape("a\\b"), "a\\\\b");
        assert_eq!(sed_escape("a|b"), "a\\|b");
        assert_eq!(sed_escape("a&b"), "a\\&b");
        assert_eq!(sed_escape("a\nb"), "a\\nb");
        // Combined
        assert_eq!(sed_escape("a\\|&\nb"), "a\\\\\\|\\&\\nb");
    }
}
