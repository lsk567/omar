//! Manager agent orchestration

pub mod protocol;

use anyhow::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::memory;
use crate::tmux::TmuxClient;
use protocol::{parse_manager_message, ManagerMessage, ProposedAgent};

/// Manager session name (exported for use in app.rs)
pub const MANAGER_SESSION: &str = "omar-agent-ea";

// Embed prompt files at compile time so they work regardless of CWD.
const PROMPT_EA: &str = include_str!("../../prompts/executive-assistant.md");
const PROMPT_PM: &str = include_str!("../../prompts/project-manager.md");
const PROMPT_WORKER: &str = include_str!("../../prompts/worker.md");

/// Embedded prompt files, keyed by filename.
const EMBEDDED_PROMPTS: &[(&str, &str)] = &[
    ("executive-assistant.md", PROMPT_EA),
    ("project-manager.md", PROMPT_PM),
    ("worker.md", PROMPT_WORKER),
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

/// Start the manager agent session
pub fn start_manager(client: &TmuxClient, command: &str) -> Result<()> {
    // Check if manager already exists
    if client.has_session(MANAGER_SESSION)? {
        println!("Manager session already exists. Attaching...");
        client.attach_session(MANAGER_SESSION)?;
        return Ok(());
    }

    // Build command with EA system prompt + memory baked in
    let cmd = build_ea_command(command);

    // Create manager session — system prompt set at process start
    println!("Starting manager agent...");
    client.new_session(
        MANAGER_SESSION,
        &cmd,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(2));

    // Attach to the session
    println!("Attaching to manager session...");
    client.attach_session(MANAGER_SESSION)?;

    Ok(())
}

/// Run the manager in orchestration mode (interactive)
pub fn run_manager_orchestration(client: &TmuxClient, command: &str) -> Result<()> {
    println!("=== OMAR Manager Orchestration Mode ===\n");

    // Check if manager exists
    if !client.has_session(MANAGER_SESSION)? {
        println!("No manager session found. Starting one...");
        start_manager(client, command)?;
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
                show_status(client)?;
            }
            "attach" | "a" => {
                client.attach_session(MANAGER_SESSION)?;
            }
            "check" | "c" => {
                check_manager_output(client)?;
            }
            "approve" | "y" => {
                approve_plan(client, command)?;
            }
            "reject" | "n" => {
                reject_plan(client)?;
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
                send_to_manager(client, input)?;
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

fn show_status(client: &TmuxClient) -> Result<()> {
    println!("\n=== Agent Status ===");

    // Show manager
    if client.has_session(MANAGER_SESSION)? {
        let output = client.capture_pane(MANAGER_SESSION, 3)?;
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
        for session in sessions {
            let output = client.capture_pane(&session.name, 1).unwrap_or_default();
            let short_name = session
                .name
                .strip_prefix(client.prefix())
                .unwrap_or(&session.name);
            println!("  {}: {}", short_name, output.trim());
        }
    }

    Ok(())
}

fn check_manager_output(client: &TmuxClient) -> Result<()> {
    let output = client.capture_pane(MANAGER_SESSION, 50)?;

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

fn approve_plan(client: &TmuxClient, command: &str) -> Result<()> {
    let output = client.capture_pane(MANAGER_SESSION, 50)?;

    if let Some(ManagerMessage::Plan {
        description,
        agents,
    }) = parse_manager_message(&output)
    {
        println!("\nApproving plan: {}", description);
        println!("Spawning {} worker agents...\n", agents.len());

        for agent in &agents {
            spawn_worker(client, agent, command)?;
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
        send_to_manager(client, &approval_msg)?;

        println!("\nAll agents spawned. Use 'status' to monitor progress.");
    } else {
        println!("No plan found to approve. Use 'check' to see manager output.");
    }

    Ok(())
}

fn reject_plan(client: &TmuxClient) -> Result<()> {
    print!("Reason for rejection: ");
    io::stdout().flush()?;

    let mut reason = String::new();
    io::stdin().read_line(&mut reason)?;

    send_to_manager(client, &format!("Plan rejected. Reason: {}", reason.trim()))?;
    println!("Rejection sent to manager.");

    Ok(())
}

fn send_to_manager(client: &TmuxClient, message: &str) -> Result<()> {
    client.send_keys_literal(MANAGER_SESSION, message)?;
    client.send_keys(MANAGER_SESSION, "Enter")?;
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

fn spawn_worker(client: &TmuxClient, agent: &ProposedAgent, command: &str) -> Result<()> {
    let session_name = format!("{}{}", client.prefix(), agent.name);

    if client.has_session(&session_name)? {
        println!("  {} - already exists, skipping", agent.name);
        return Ok(());
    }

    // Build command with worker system prompt (template vars substituted via sed)
    let parent_name = client.prefix().to_string() + "manager";
    let prompt_file = prompts_dir().join("worker.md");
    let cmd = build_agent_command(
        command,
        &prompt_file,
        &[("{{PARENT_NAME}}", &parent_name), ("{{TASK}}", &agent.task)],
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

    // Persist worker task description
    memory::save_worker_task(&session_name, &agent.task);

    println!("  {} - spawned ({})", agent.name, agent.role);

    Ok(())
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
