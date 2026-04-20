//! Manager agent — prompt embedding, command building, and orchestration

pub mod protocol;

use anyhow::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

use crate::ea::{self, EaId};
use crate::memory;
use crate::tmux::{DeliveryOptions, TmuxClient};
use protocol::{parse_manager_message, ManagerMessage, ProposedAgent};

// Embed prompt files at compile time so they work regardless of CWD.
const PROMPT_EA: &str = include_str!("../../prompts/executive-assistant.md");
const PROMPT_AGENT: &str = include_str!("../../prompts/agent.md");
const PROMPT_WATCHDOG: &str = include_str!("../../prompts/watchdog.md");

/// Embedded prompt files, keyed by filename.
const EMBEDDED_PROMPTS: &[(&str, &str)] = &[
    ("executive-assistant.md", PROMPT_EA),
    ("agent.md", PROMPT_AGENT),
    ("watchdog.md", PROMPT_WATCHDOG),
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

/// Escape a string for use in a sed replacement (with `|` as delimiter).
///
/// The sed expression is wrapped in single quotes in the generated shell command
/// (e.g. `sed 's|PAT|REPL|g' file`), so any single quote in the replacement
/// must be closed, escaped, and reopened: `'` → `'\''`.
fn sed_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('&', "\\&")
        .replace('\n', "\\n")
        .replace('\'', "'\\''")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Claude,
    Codex,
    Cursor,
    Gemini,
    Opencode,
}

/// How a backend consumes the agent.md prompt.
///
/// Group 1 backends accept a real system prompt (invisible to chat turns),
/// so the YOUR NAME / YOUR TASK header must arrive as a follow-up user
/// message via `TmuxClient::deliver_prompt`.
///
/// Group 2 backends receive agent.md as the first user message. To avoid
/// a wasted round-trip (and, for opencode, protocol drift) we materialize
/// a single combined prompt that already contains the task header and
/// skip the follow-up delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDeliveryMode {
    /// Task header needs a follow-up `deliver_prompt` call (claude, codex).
    SystemPrompt,
    /// Task header is already inline in the spawn command (cursor, gemini, opencode).
    InitialUserMessage,
}

impl PromptDeliveryMode {
    /// Whether the spawn command already contains the task header.
    ///
    /// Callers that would otherwise follow up with `deliver_prompt` must
    /// skip that step when this returns `true`.
    pub fn delivers_task_inline(self) -> bool {
        matches!(self, PromptDeliveryMode::InitialUserMessage)
    }
}

impl BackendKind {
    fn prompt_delivery_mode(self) -> PromptDeliveryMode {
        match self {
            BackendKind::Claude | BackendKind::Codex => PromptDeliveryMode::SystemPrompt,
            BackendKind::Cursor | BackendKind::Gemini | BackendKind::Opencode => {
                PromptDeliveryMode::InitialUserMessage
            }
        }
    }
}

/// Detect the prompt delivery mode for a base command.
///
/// Unknown backends fall back to `SystemPrompt` — the conservative choice,
/// since that keeps the existing two-step flow and does not suppress the
/// task delivery.
pub fn prompt_delivery_mode(base_command: &str) -> PromptDeliveryMode {
    detect_backend(base_command)
        .map(BackendKind::prompt_delivery_mode)
        .unwrap_or(PromptDeliveryMode::SystemPrompt)
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
        "gemini" => Some(BackendKind::Gemini),
        "opencode" => Some(BackendKind::Opencode),
        _ => None,
    }
}

fn detect_backend(base_command: &str) -> Option<BackendKind> {
    base_command
        .split_whitespace()
        .find_map(detect_backend_token)
}

impl BackendKind {
    fn canonical_name(self) -> &'static str {
        match self {
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Cursor => "cursor",
            BackendKind::Gemini => "gemini",
            BackendKind::Opencode => "opencode",
        }
    }
}

fn materialize_prompt_file(prompt_file: &Path, substitutions: &[(&str, &str)]) -> PathBuf {
    if substitutions.is_empty() {
        return prompt_file.to_path_buf();
    }

    let mut content = std::fs::read_to_string(prompt_file).unwrap_or_default();
    for (pattern, replacement) in substitutions {
        content = content.replace(pattern, replacement);
    }

    let dir = std::env::temp_dir().join("omar-prompts");
    std::fs::create_dir_all(&dir).ok();

    let stem = prompt_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prompt");
    let ext = prompt_file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");
    let rendered = dir.join(format!("{}-{}.{}", stem, Uuid::new_v4(), ext));

    if std::fs::write(&rendered, content).is_ok() {
        rendered
    } else {
        prompt_file.to_path_buf()
    }
}

/// Render agent.md (with substitutions applied) plus a trailing
/// `YOUR NAME` / `YOUR TASK` header into a fresh temp file and return the
/// path. Used for group-2 backends so the entire first user message fits in
/// one round-trip.
///
/// Returns `None` on any I/O failure (unreadable source, failed write) so
/// the caller can fall back to the two-step flow rather than silently
/// spawning a worker that will never receive the task header.
fn materialize_combined_prompt(
    prompt_file: &Path,
    substitutions: &[(&str, &str)],
    short_name: &str,
    task: &str,
) -> Option<PathBuf> {
    let mut content = std::fs::read_to_string(prompt_file).ok()?;
    for (pattern, replacement) in substitutions {
        content = content.replace(pattern, replacement);
    }
    content.push_str(&format!(
        "\n\n---\n\nYOUR NAME: {}\nYOUR TASK: {}\n",
        short_name, task
    ));

    let dir = std::env::temp_dir().join("omar-prompts");
    std::fs::create_dir_all(&dir).ok()?;

    let stem = prompt_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prompt");
    let ext = prompt_file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");
    let rendered = dir.join(format!("{}-combined-{}.{}", stem, Uuid::new_v4(), ext));

    std::fs::write(&rendered, &content).ok()?;
    Some(rendered)
}

/// Command and delivery mode returned by `build_worker_agent_command`.
#[derive(Debug, Clone)]
pub struct AgentSpawnCommand {
    /// The shell command to spawn the backend.
    pub command: String,
    /// How the task header reaches the agent — used by callers to decide
    /// whether a follow-up `deliver_prompt` call is needed.
    pub delivery_mode: PromptDeliveryMode,
}

/// Build a CLI command with system prompt loaded from a file via native flag.
///
/// - `prompt_file`: absolute path to the prompt .md file
/// - `substitutions`: `(pattern, replacement)` pairs for sed; empty = use `cat`
///
/// Detects backend from `base_command`:
///   - claude  → `--system-prompt "$(cat '<path>')"`
///   - codex   → `-c "developer_instructions='''$(cat '<path>')'''"`
///   - cursor  → positional arg `"Load the <path> file and follow the instructions."`
///   - gemini  → `-i "$(cat '<path>')"`
///   - opencode → `--prompt "$(cat '<path>')"`
///   - unknown → returns `base_command` unchanged
pub fn build_agent_command(
    base_command: &str,
    prompt_file: &Path,
    substitutions: &[(&str, &str)],
) -> String {
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

    match detect_backend(base_command) {
        Some(BackendKind::Claude) => {
            format!("{} --system-prompt \"{}\"", base_command, shell_expr)
        }
        Some(BackendKind::Codex) => format!(
            "{} -c \"developer_instructions='''{}'''\"",
            base_command, shell_expr
        ),
        Some(BackendKind::Cursor) => {
            let rendered = materialize_prompt_file(prompt_file, substitutions);
            format!(
                "{} \"Load the '{}' file and follow the instructions.\"",
                base_command,
                rendered.display()
            )
        }
        Some(BackendKind::Gemini) => {
            format!("TERM=xterm-256color {} -i \"{}\"", base_command, shell_expr)
        }
        Some(BackendKind::Opencode) => format!("{} --prompt \"{}\"", base_command, shell_expr),
        None => base_command.to_string(),
    }
}

/// Build a spawn command for a worker agent that also takes ownership of the
/// `YOUR NAME` / `YOUR TASK` header.
///
/// - Group 1 backends (claude, codex): returns the same command as
///   `build_agent_command` with `delivery_mode = SystemPrompt`. The caller
///   must follow up with a `deliver_prompt` carrying the task header.
/// - Group 2 backends (cursor, gemini, opencode): materializes a combined
///   prompt file (agent.md with substitutions applied + the task header
///   appended) and feeds it to the backend as a single initial user
///   message. `delivery_mode = InitialUserMessage` tells the caller to
///   skip any follow-up `deliver_prompt` — the task is already inline.
/// - Unknown backends: returns `base_command` unchanged with
///   `SystemPrompt`, so an upstream `deliver_prompt` still runs and the
///   task arrives.
pub fn build_worker_agent_command(
    base_command: &str,
    prompt_file: &Path,
    substitutions: &[(&str, &str)],
    short_name: &str,
    task: &str,
) -> AgentSpawnCommand {
    let mode = prompt_delivery_mode(base_command);
    match mode {
        PromptDeliveryMode::SystemPrompt => AgentSpawnCommand {
            command: build_agent_command(base_command, prompt_file, substitutions),
            delivery_mode: mode,
        },
        PromptDeliveryMode::InitialUserMessage => {
            let Some(combined) =
                materialize_combined_prompt(prompt_file, substitutions, short_name, task)
            else {
                // Falling back to the legacy two-step flow: render the
                // agent.md prompt without the task header and tell the
                // caller to deliver the task via a follow-up
                // `deliver_prompt`. This preserves correctness (the agent
                // still gets its task) at the cost of the extra round-trip
                // this PR is otherwise trying to eliminate.
                eprintln!(
                    "warning: failed to materialize combined prompt for {}; \
                     falling back to two-step delivery",
                    short_name
                );
                return AgentSpawnCommand {
                    command: build_agent_command(base_command, prompt_file, substitutions),
                    delivery_mode: PromptDeliveryMode::SystemPrompt,
                };
            };
            let path_str = combined.display();
            let shell_expr = format!("$(cat '{}')", path_str);
            let command = match detect_backend(base_command) {
                Some(BackendKind::Cursor) => format!(
                    "{} \"Load the '{}' file and follow the instructions.\"",
                    base_command, path_str
                ),
                Some(BackendKind::Gemini) => {
                    format!("TERM=xterm-256color {} -i \"{}\"", base_command, shell_expr)
                }
                Some(BackendKind::Opencode) => {
                    format!("{} --prompt \"{}\"", base_command, shell_expr)
                }
                // Unreachable: prompt_delivery_mode only returns
                // InitialUserMessage for the three backends above.
                _ => base_command.to_string(),
            };
            AgentSpawnCommand {
                command,
                delivery_mode: mode,
            }
        }
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

    // Build command with worker system prompt. For group-2 backends
    // (cursor/gemini/opencode) the task header is folded into the initial
    // prompt; for group-1 backends (claude/codex) a follow-up
    // `deliver_prompt` carries the task header.
    let parent_name = "ea";
    let prompt_file = prompts_dir(omar_dir).join("agent.md");
    let spawn_cmd = build_worker_agent_command(
        command,
        &prompt_file,
        &[
            ("{{PARENT_NAME}}", parent_name),
            ("{{TASK}}", &agent.task),
            ("{{EA_ID}}", &ea_id.to_string()),
        ],
        &agent.name,
        &agent.task,
    );

    // Create worker session — system prompt set at process start
    client.new_session(
        &session_name,
        &spawn_cmd.command,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Group-2 backends already received the task header in the spawn
    // command; skip the follow-up delivery so we don't duplicate the turn.
    if !spawn_cmd.delivery_mode.delivers_task_inline() {
        // Wait for backend readiness when possible, then deliver an explicit
        // first task message so workers begin execution deterministically.
        // If markers succeed, the TUI is proven ready; skip
        // require_initial_change (a fresh Claude Code banner stays
        // pixel-stable after drawing, so any extra "wait for a change" would
        // time out).
        let markers_proved_ready = if let Some(kind) = detect_backend(command) {
            let markers = crate::tmux::backend_readiness_markers(kind.canonical_name());
            if markers.is_empty() {
                false
            } else {
                let detected = client.wait_for_markers(
                    &session_name,
                    markers,
                    Duration::from_secs(60),
                    Duration::from_millis(250),
                );
                if !detected {
                    println!(
                        "  {} - readiness markers timed out; attempting delivery anyway",
                        agent.name
                    );
                }
                detected
            }
        } else {
            false
        };

        let initial_msg = format!("YOUR NAME: {}\nYOUR TASK: {}", agent.name, agent.task);
        let opts = DeliveryOptions {
            startup_timeout: Duration::from_secs(45),
            stable_quiet: Duration::from_millis(800),
            verify_timeout: Duration::from_secs(6),
            max_retries: 4,
            poll_interval: Duration::from_millis(120),
            retry_delay: Duration::from_millis(250),
            require_initial_change: !markers_proved_ready,
        };
        client
            .deliver_prompt(&session_name, &initial_msg, &opts)
            .map_err(|e| {
                anyhow::anyhow!("failed to deliver initial task to {}: {}", agent.name, e)
            })?;
    }

    // Persist worker task description to EA-scoped state dir
    let state_dir = ea::ea_state_dir(ea_id, omar_dir);
    memory::save_worker_task_in(&state_dir, &session_name, &agent.task);
    memory::save_agent_parent_in(
        &state_dir,
        &session_name,
        &ea::ea_manager_session(ea_id, base_prefix),
    );

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
    fn test_build_agent_command_codex() {
        let cmd = build_agent_command(
            "codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox",
            Path::new("/tmp/prompts/ea.md"),
            &[],
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
            &[],
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
            &[],
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
            &[],
        );
        assert_eq!(
            cmd,
            "env OPENAI_API_KEY=test codex --no-alt-screen -c \"developer_instructions='''$(cat '/tmp/prompts/ea.md')'''\""
        );
    }

    #[test]
    fn test_build_agent_command_cursor() {
        let cmd = build_agent_command("cursor agent --yolo", Path::new("/tmp/prompts/ea.md"), &[]);
        assert_eq!(
            cmd,
            "cursor agent --yolo \"Load the '/tmp/prompts/ea.md' file and follow the instructions.\""
        );
    }

    #[test]
    fn test_build_agent_command_gemini() {
        let cmd = build_agent_command("gemini --yolo", Path::new("/tmp/prompts/ea.md"), &[]);
        assert_eq!(
            cmd,
            "TERM=xterm-256color gemini --yolo -i \"$(cat '/tmp/prompts/ea.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_wrapped_gemini() {
        let cmd = build_agent_command(
            "env FOO=bar gemini --yolo",
            Path::new("/tmp/prompts/ea.md"),
            &[],
        );
        assert_eq!(
            cmd,
            "TERM=xterm-256color env FOO=bar gemini --yolo -i \"$(cat '/tmp/prompts/ea.md')\""
        );
    }

    #[test]
    fn test_build_agent_command_wrapped_cursor() {
        let cmd = build_agent_command(
            "env FOO=bar cursor agent --yolo",
            Path::new("/tmp/prompts/ea.md"),
            &[],
        );
        assert_eq!(
            cmd,
            "env FOO=bar cursor agent --yolo \"Load the '/tmp/prompts/ea.md' file and follow the instructions.\""
        );
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
        // Single quotes must be escaped so they don't terminate the surrounding
        // shell single-quoted sed expression (BUG B fix).
        assert_eq!(sed_escape("it's"), "it'\\''s");
        assert_eq!(sed_escape("don't stop"), "don'\\''t stop");
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

    // ── PromptDeliveryMode classification ──
    //
    // Group-1 backends (claude, codex) install agent.md as a real system
    // prompt, so the YOUR NAME/YOUR TASK header must be delivered as a
    // follow-up user message. Group-2 backends (cursor, gemini, opencode)
    // receive agent.md as the first user turn; the fix folds the task
    // header into that same turn, so callers must NOT re-deliver it.

    #[test]
    fn test_prompt_delivery_mode_group1_claude() {
        assert_eq!(
            prompt_delivery_mode("claude --yolo"),
            PromptDeliveryMode::SystemPrompt
        );
        assert!(!prompt_delivery_mode("claude").delivers_task_inline());
    }

    #[test]
    fn test_prompt_delivery_mode_group1_codex() {
        assert_eq!(
            prompt_delivery_mode("codex --no-alt-screen"),
            PromptDeliveryMode::SystemPrompt
        );
        assert!(!prompt_delivery_mode("env FOO=bar codex").delivers_task_inline());
    }

    #[test]
    fn test_prompt_delivery_mode_group2_cursor() {
        assert_eq!(
            prompt_delivery_mode("cursor agent --yolo"),
            PromptDeliveryMode::InitialUserMessage
        );
        assert!(prompt_delivery_mode("cursor agent --yolo").delivers_task_inline());
    }

    #[test]
    fn test_prompt_delivery_mode_group2_gemini() {
        assert_eq!(
            prompt_delivery_mode("gemini --yolo"),
            PromptDeliveryMode::InitialUserMessage
        );
        assert!(prompt_delivery_mode("gemini").delivers_task_inline());
    }

    #[test]
    fn test_prompt_delivery_mode_group2_opencode() {
        assert_eq!(
            prompt_delivery_mode("opencode"),
            PromptDeliveryMode::InitialUserMessage
        );
        assert!(prompt_delivery_mode("npx opencode --model local").delivers_task_inline());
    }

    #[test]
    fn test_prompt_delivery_mode_unknown_falls_back_to_system_prompt() {
        // Unknown backend: conservative fallback keeps the follow-up
        // deliver_prompt step (otherwise the task would never reach it).
        assert_eq!(
            prompt_delivery_mode("vim"),
            PromptDeliveryMode::SystemPrompt
        );
        assert!(!prompt_delivery_mode("vim").delivers_task_inline());
    }

    // ── build_worker_agent_command behaviour ──

    /// Helper: render a real agent.md template to a temp file (mirroring
    /// what `prompts_dir` would install) so tests can read the combined
    /// output back from disk.
    fn write_real_agent_prompt(dir: &Path) -> PathBuf {
        let pdir = prompts_dir(dir);
        pdir.join("agent.md")
    }

    fn read_combined_file_from_command(cmd: &str) -> String {
        // Commands for group-2 backends reference a rendered file either
        // directly (cursor: "Load the 'PATH' file...") or via `$(cat 'PATH')`
        // (gemini, opencode). Extract the first quoted path and read it.
        let start = cmd.find('\'').expect("expected quoted path in command");
        let rest = &cmd[start + 1..];
        let end = rest.find('\'').expect("expected closing quote");
        let path = &rest[..end];
        std::fs::read_to_string(path).expect("combined prompt file should exist")
    }

    #[test]
    fn test_build_worker_agent_command_group1_claude_uses_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "claude",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "do a thing"),
                ("{{EA_ID}}", "0"),
            ],
            "worker-42",
            "do a thing",
        );
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::SystemPrompt);
        assert!(spawn.command.contains("--system-prompt"));
        // Group-1 commands do NOT embed the YOUR NAME header — the
        // follow-up deliver_prompt carries it.
        assert!(!spawn.command.contains("YOUR NAME: worker-42"));
    }

    #[test]
    fn test_build_worker_agent_command_group1_codex_uses_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "codex --no-alt-screen",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "do a thing"),
                ("{{EA_ID}}", "0"),
            ],
            "codex-worker",
            "do a thing",
        );
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::SystemPrompt);
        assert!(spawn.command.contains("developer_instructions"));
        // Group-1 commands leave task delivery to the follow-up deliver_prompt.
        assert!(!spawn.command.contains("YOUR NAME: codex-worker"));
    }

    #[test]
    fn test_build_worker_agent_command_cursor_combines_prompt_and_task() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "cursor agent --yolo",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "ship it"),
                ("{{EA_ID}}", "0"),
            ],
            "cursor-worker",
            "ship it",
        );
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::InitialUserMessage);
        assert!(spawn.command.contains("Load the "));
        assert!(spawn.command.contains("file and follow the instructions."));

        let combined = read_combined_file_from_command(&spawn.command);
        assert!(
            combined.contains("You operate in one of two distinct roles"),
            "combined prompt should include agent.md body text"
        );
        assert!(combined.contains("YOUR NAME: cursor-worker"));
        assert!(combined.contains("YOUR TASK: ship it"));
    }

    #[test]
    fn test_build_worker_agent_command_gemini_combines_prompt_and_task() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "gemini --yolo",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "summarize logs"),
                ("{{EA_ID}}", "0"),
            ],
            "gem-worker",
            "summarize logs",
        );
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::InitialUserMessage);
        assert!(spawn.command.starts_with("TERM=xterm-256color "));
        assert!(spawn.command.contains(" -i "));

        let combined = read_combined_file_from_command(&spawn.command);
        assert!(combined.contains("You operate in one of two distinct roles"));
        assert!(combined.contains("YOUR NAME: gem-worker"));
        assert!(combined.contains("YOUR TASK: summarize logs"));
    }

    #[test]
    fn test_build_worker_agent_command_opencode_combines_prompt_and_task() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "opencode",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "refactor foo.rs"),
                ("{{EA_ID}}", "0"),
            ],
            "oc-worker",
            "refactor foo.rs",
        );
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::InitialUserMessage);
        assert!(spawn.command.contains("--prompt "));

        let combined = read_combined_file_from_command(&spawn.command);
        assert!(combined.contains("You operate in one of two distinct roles"));
        assert!(combined.contains("YOUR NAME: oc-worker"));
        assert!(combined.contains("YOUR TASK: refactor foo.rs"));
    }

    #[test]
    fn test_build_worker_agent_command_group2_falls_back_on_materialize_failure() {
        // If the combined-prompt file can't be written (here: non-existent
        // source file that can't be read), `build_worker_agent_command` must
        // fall back to the legacy two-step flow so the task is still
        // delivered via the follow-up `deliver_prompt`. Silently returning
        // InitialUserMessage with no task embedded would leave the worker
        // waiting forever.
        let missing = Path::new("/nonexistent/does-not-exist.md");
        let spawn =
            build_worker_agent_command("opencode", missing, &[("{{TASK}}", "x")], "oc", "x");
        assert_eq!(spawn.delivery_mode, PromptDeliveryMode::SystemPrompt);
        assert!(!spawn.delivery_mode.delivers_task_inline());
        // Command should still be valid opencode syntax (built by the
        // legacy `build_agent_command` path).
        assert!(spawn.command.contains("opencode"));
        assert!(spawn.command.contains("--prompt"));
    }

    #[test]
    fn test_build_worker_agent_command_group2_substitutions_rendered() {
        // Substitutions inside agent.md (e.g. {{EA_ID}}) must be expanded
        // in the combined file because group-2 backends read the file
        // content directly — there is no sed layer between us and the
        // backend as there is for claude/codex.
        let dir = tempfile::tempdir().unwrap();
        let prompt_file = write_real_agent_prompt(dir.path());
        let spawn = build_worker_agent_command(
            "opencode",
            &prompt_file,
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "t"),
                ("{{EA_ID}}", "7"),
            ],
            "oc",
            "t",
        );
        let combined = read_combined_file_from_command(&spawn.command);
        // agent.md references {{EA_ID}} in the curl examples; after
        // substitution the literal placeholder must be gone.
        assert!(
            !combined.contains("{{EA_ID}}"),
            "combined prompt should have {{{{EA_ID}}}} substituted"
        );
        assert!(combined.contains("ea_7") || combined.contains("/ea/7"));
    }
}
