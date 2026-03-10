//! Manager agent — prompt embedding, command building, and orchestration

pub mod protocol;

use anyhow::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::ea::{self, EaId};
use crate::memory;
use crate::tmux::TmuxClient;
use protocol::{parse_manager_message, ManagerMessage, ProposedAgent};

// Embed prompt files at compile time so they work regardless of CWD.
const PROMPT_EA: &str = include_str!("../../prompts/executive-assistant.md");
const PROMPT_AGENT: &str = include_str!("../../prompts/agent.md");

/// Embedded prompt files, keyed by filename.
const EMBEDDED_PROMPTS: &[(&str, &str)] = &[
    ("executive-assistant.md", PROMPT_EA),
    ("agent.md", PROMPT_AGENT),
];

/// Return the `{omar_dir}/prompts/` directory, writing embedded prompts into it.
///
/// Prompts are shared templates containing `{{EA_ID}}` placeholders.
/// Substitution happens at spawn time in `build_ea_command` / `build_agent_command`.
pub fn prompts_dir(omar_dir: &Path) -> PathBuf {
    let dir = omar_dir.join("prompts");
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

/// Build a CLI command with the system prompt injected via backend-specific mechanisms.
///
/// Detects backend from `base_command`:
///   - claude  → `--system-prompt "$(cat '<path>')"`
///   - codex   → `-c "developer_instructions='''$(cat '<path>')'''"`
///   - cursor  → positional arg `"Load the <path> file and follow the instructions."`
///   - opencode → `--prompt "$(cat '<path>')"`
///   - unknown → returns `base_command` unchanged
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
                "{} \"Load the '{}' file and follow the instructions.\"",
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
/// Reads the EA prompt template, appends the latest memory snapshot, writes a
/// combined file to `{omar_dir}/ea/{ea_id}/ea_prompt_combined.md` (per-EA scoped,
/// fixing Gotcha G8), and returns the CLI command with `{{EA_ID}}` and `{{EA_NAME}}`
/// substituted via sed.
pub fn build_ea_command(base_command: &str, ea_id: EaId, ea_name: &str, omar_dir: &Path) -> String {
    let prompt_file = prompts_dir(omar_dir).join("executive-assistant.md");
    let state_dir = ea::ea_state_dir(ea_id, omar_dir);
    let mem = memory::load_memory_from(&state_dir);

    let notes = memory::load_manager_notes(omar_dir, ea_id);
    let prompt_content = std::fs::read_to_string(&prompt_file).unwrap_or_default();

    // Write combined prompt (template + memory) to EA-scoped directory
    let combined_path = state_dir.join("ea_prompt_combined.md");
    std::fs::create_dir_all(&state_dir).ok();

    let combined = match (mem.is_empty(), notes.is_empty()) {
        (true, true) => prompt_content,
        (false, true) => format!(
            "{}\n\n---\n\n## Current OMAR State (from previous session)\n\n{}",
            prompt_content, mem
        ),
        (true, false) => format!(
            "{}\n\n---\n\n## Manager Notes (from previous session)\n\n{}",
            prompt_content, notes
        ),
        (false, false) => format!(
            "{}\n\n---\n\n## Current OMAR State (from previous session)\n\n{}\n\n## Manager Notes (from previous session)\n\n{}",
            prompt_content, mem, notes
        ),
    };
    std::fs::write(&combined_path, &combined).ok();

    // Substitute {{EA_ID}} and {{EA_NAME}} in the prompt
    build_agent_command(
        base_command,
        &combined_path,
        &[("{{EA_ID}}", &ea_id.to_string()), ("{{EA_NAME}}", ea_name)],
    )
}

/// Start the manager agent session for a specific EA.
pub fn start_manager(
    client: &TmuxClient,
    command: &str,
    ea_id: EaId,
    ea_name: &str,
    omar_dir: &Path,
    base_prefix: &str,
) -> Result<()> {
    let session = ea::ea_manager_session(ea_id, base_prefix);

    // Check if manager already exists
    if client.has_session(&session)? {
        println!("Manager session already exists. Attaching...");
        client.attach_session(&session)?;
        return Ok(());
    }

    // Build command with EA system prompt + memory baked in
    let cmd = build_ea_command(command, ea_id, ea_name, omar_dir);

    // Create manager session — system prompt set at process start
    println!("Starting manager agent (EA {})...", ea_id);
    client.new_session(
        &session,
        &cmd,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(2));

    // Attach to the session
    println!("Attaching to manager session...");
    client.attach_session(&session)?;

    Ok(())
}

/// Run the manager in orchestration mode (interactive)
pub fn run_manager_orchestration(
    client: &TmuxClient,
    command: &str,
    ea_id: EaId,
    ea_name: &str,
    omar_dir: &Path,
    base_prefix: &str,
) -> Result<()> {
    let session = ea::ea_manager_session(ea_id, base_prefix);

    println!("=== OMAR Manager Orchestration Mode (EA {}) ===\n", ea_id);

    // Check if manager exists
    if !client.has_session(&session)? {
        println!("No manager session found. Starting one...");
        start_manager(client, command, ea_id, ea_name, omar_dir, base_prefix)?;
        return Ok(());
    }

    loop {
        // Get user input
        print!("\n[OMAR] Enter command (or 'help'): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        match input {
            "help" | "h" => {
                print_help();
            }
            "status" | "s" => {
                show_status(client, &session)?;
            }
            "attach" | "a" => {
                client.attach_session(&session)?;
            }
            "check" | "c" => {
                check_manager_output(client, &session)?;
            }
            "approve" | "y" => {
                approve_plan(client, command, &session, ea_id, omar_dir, base_prefix)?;
            }
            "reject" | "n" => {
                reject_plan(client, &session)?;
            }
            "quit" | "q" => {
                println!("Exiting orchestration mode.");
                break;
            }
            _ if input.starts_with("send ") => {
                let rest = &input[5..];
                if let Some((target, msg)) = rest.split_once(' ') {
                    send_to_agent(client, target, msg)?;
                } else {
                    println!("Usage: send <agent-name> <message>");
                }
            }
            _ if !input.is_empty() => {
                // Send to manager as a request
                send_to_manager(client, &session, input)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"
OMAR Manager Commands:
  <text>        Send request to manager agent
  status (s)    Show all agent status
  attach (a)    Attach to manager session
  check (c)     Check manager's latest output for plans
  approve (y)   Approve the proposed plan
  reject (n)    Reject the proposed plan
  send <agent> <msg>  Send message to specific agent
  quit (q)      Exit orchestration mode
"#
    );
}

fn show_status(client: &TmuxClient, session: &str) -> Result<()> {
    println!("\n=== Agent Status ===");

    // Show manager
    if client.has_session(session)? {
        let output = client.capture_pane(session, 3)?;
        println!("Manager: Active");
        println!(
            "  Last output: {}",
            output.lines().last().unwrap_or("(none)")
        );
    } else {
        println!("Manager: Not running");
    }

    // Show workers
    let sessions = client.list_sessions()?;
    if sessions.is_empty() {
        println!("\nNo worker agents running.");
    } else {
        println!("\nWorkers:");
        for s in sessions {
            let output = client.capture_pane(&s.name, 1).unwrap_or_default();
            let short_name = s.name.strip_prefix(client.prefix()).unwrap_or(&s.name);
            println!("  {}: {}", short_name, output.trim());
        }
    }

    Ok(())
}

fn check_manager_output(client: &TmuxClient, session: &str) -> Result<()> {
    let output = client.capture_pane(session, 50)?;

    if let Some(msg) = parse_manager_message(&output) {
        match msg {
            ManagerMessage::Plan {
                description,
                agents,
            } => {
                println!("\n=== Proposed Plan ===");
                println!("Goal: {}\n", description);
                println!("Agents:");
                for (i, agent) in agents.iter().enumerate() {
                    println!("  {}. {} ({})", i + 1, agent.name, agent.role);
                    println!("     Task: {}", agent.task);
                    if !agent.depends_on.is_empty() {
                        println!("     Depends on: {}", agent.depends_on.join(", "));
                    }
                }
                println!("\nApprove this plan? Use 'approve' or 'reject'");
            }
            ManagerMessage::Send { target, message } => {
                println!("Manager wants to send to '{}': {}", target, message);
            }
            ManagerMessage::Query { target } => {
                println!("Manager querying status of: {}", target);
            }
            ManagerMessage::Complete { summary } => {
                println!("Manager reports completion: {}", summary);
            }
        }
    } else {
        println!("No structured plan found in manager output.");
        println!("Recent output:");
        for line in output.lines().rev().take(10) {
            println!("  {}", line);
        }
    }

    Ok(())
}

fn approve_plan(
    client: &TmuxClient,
    command: &str,
    session: &str,
    ea_id: EaId,
    omar_dir: &Path,
    base_prefix: &str,
) -> Result<()> {
    let output = client.capture_pane(session, 50)?;

    if let Some(ManagerMessage::Plan {
        description,
        agents,
    }) = parse_manager_message(&output)
    {
        println!("\nApproving plan: {}", description);
        println!("Spawning {} worker agents...\n", agents.len());

        for agent in &agents {
            spawn_worker(client, agent, command, ea_id, omar_dir, base_prefix)?;
        }

        // Notify manager that plan was approved
        let approval_msg = format!(
            "Plan approved. {} agents spawned: {}",
            agents.len(),
            agents
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        send_to_manager(client, session, &approval_msg)?;

        println!("\nAll agents spawned. Use 'status' to monitor progress.");
    } else {
        println!("No plan found to approve. Use 'check' to see manager output.");
    }

    Ok(())
}

fn reject_plan(client: &TmuxClient, session: &str) -> Result<()> {
    print!("Reason for rejection: ");
    io::stdout().flush()?;

    let mut reason = String::new();
    io::stdin().read_line(&mut reason)?;

    send_to_manager(
        client,
        session,
        &format!("Plan rejected. Reason: {}", reason.trim()),
    )?;
    println!("Rejection sent to manager.");

    Ok(())
}

fn send_to_manager(client: &TmuxClient, session: &str, message: &str) -> Result<()> {
    client.send_keys_literal(session, message)?;
    client.send_keys(session, "Enter")?;
    println!("Sent to manager: {}", message);
    Ok(())
}

fn send_to_agent(client: &TmuxClient, agent: &str, message: &str) -> Result<()> {
    let session_name = format!("{}{}", client.prefix(), agent);

    if !client.has_session(&session_name)? {
        println!("Agent '{}' not found.", agent);
        return Ok(());
    }

    client.send_keys_literal(&session_name, message)?;
    client.send_keys(&session_name, "Enter")?;
    println!("Sent to {}: {}", agent, message);
    Ok(())
}

fn spawn_worker(
    client: &TmuxClient,
    agent: &ProposedAgent,
    command: &str,
    ea_id: EaId,
    omar_dir: &Path,
    base_prefix: &str,
) -> Result<()> {
    let session_name = format!("{}{}", client.prefix(), agent.name);

    if client.has_session(&session_name)? {
        println!("  {} - already exists, skipping", agent.name);
        return Ok(());
    }

    // Build command with worker system prompt (template vars substituted via sed)
    let parent_name = ea::ea_manager_session(ea_id, base_prefix);
    let prompt_file = prompts_dir(omar_dir).join("agent.md");
    let cmd = build_agent_command(
        command,
        &prompt_file,
        &[
            ("{{PARENT_NAME}}", &parent_name),
            ("{{TASK}}", &agent.task),
            ("{{EA_ID}}", &ea_id.to_string()),
        ],
    );

    // Create worker session — system prompt set at process start
    client.new_session(
        &session_name,
        &cmd,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(1));

    // Send first user message to kick off work
    client.send_keys_literal(&session_name, "Start working on your assigned task now.")?;
    thread::sleep(Duration::from_millis(200));
    client.send_keys(&session_name, "Enter")?;

    // Persist worker task description to EA-scoped state dir
    let state_dir = ea::ea_state_dir(ea_id, omar_dir);
    memory::save_worker_task_in(&state_dir, &session_name, &agent.task);

    println!("  {} - spawned ({})", agent.name, agent.role);

    Ok(())
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
    fn test_build_agent_command_with_ea_id() {
        let cmd = build_agent_command(
            "claude",
            Path::new("/prompts/agent.md"),
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "do stuff"),
                ("{{EA_ID}}", "2"),
            ],
        );
        assert!(cmd.contains("s|{{EA_ID}}|2|g"));
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

    #[test]
    fn test_build_ea_command_substitutes_ea_id() {
        let dir = tempfile::tempdir().unwrap();
        let omar_dir = dir.path();

        // Create state dir for EA
        let state_dir = ea::ea_state_dir(0, omar_dir);
        std::fs::create_dir_all(&state_dir).unwrap();

        let cmd = build_ea_command("claude", 0, "Default", omar_dir);
        assert!(cmd.contains("s|{{EA_ID}}|0|g"));
        assert!(cmd.contains("s|{{EA_NAME}}|Default|g"));
    }

    #[test]
    fn test_build_ea_command_writes_to_ea_scoped_dir() {
        let dir = tempfile::tempdir().unwrap();
        let omar_dir = dir.path();

        let state_dir = ea::ea_state_dir(1, omar_dir);
        std::fs::create_dir_all(&state_dir).unwrap();

        build_ea_command("claude", 1, "Research", omar_dir);

        // Combined prompt should be in EA-scoped directory, not global
        let combined = state_dir.join("ea_prompt_combined.md");
        assert!(combined.exists());
        let content = std::fs::read_to_string(&combined).unwrap();
        assert!(content.contains("Executive Assistant"));
    }

    #[test]
    fn test_build_ea_command_includes_memory() {
        let dir = tempfile::tempdir().unwrap();
        let omar_dir = dir.path();

        let state_dir = ea::ea_state_dir(0, omar_dir);
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("memory.md"), "# Saved state\nSome memory").unwrap();

        build_ea_command("claude", 0, "Default", omar_dir);

        let combined = state_dir.join("ea_prompt_combined.md");
        let content = std::fs::read_to_string(&combined).unwrap();
        assert!(content.contains("Some memory"));
        assert!(content.contains("Current OMAR State"));
    }

    #[test]
    fn test_prompts_dir_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let pdir = prompts_dir(dir.path());
        assert!(pdir.join("executive-assistant.md").exists());
        assert!(pdir.join("agent.md").exists());
    }
}
