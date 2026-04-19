//! Manager agent — prompt embedding, command building, and orchestration

pub mod protocol;

use anyhow::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::ea::{self, EaId};
use crate::memory;
use crate::metrics;
use crate::tmux::{DeliveryOptions, TmuxClient};
use protocol::{parse_manager_message, ManagerMessage, ProposedAgent};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpLaunchContext {
    pub omar_dir: PathBuf,
    pub ea_id: EaId,
    pub session_prefix: String,
    pub default_command: String,
    pub default_workdir: String,
    pub health_idle_warning: i64,
}

#[derive(Debug, Clone)]
pub struct ManagerRuntimeOptions {
    pub default_workdir: String,
    pub health_idle_warning: i64,
}

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

fn backend_token(base_command: &str, kind: BackendKind) -> Option<String> {
    base_command.split_whitespace().find_map(|token| {
        if detect_backend_token(token) == Some(kind) {
            Some(
                token
                    .trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')'))
                    .to_string(),
            )
        } else {
            None
        }
    })
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

/// MCP state directory for a given EA. Stable per-EA path — avoids leaking
/// files into world-readable `/tmp` and prevents unbounded growth from
/// per-spawn UUID filenames.
fn mcp_ea_dir(context: &McpLaunchContext) -> Option<PathBuf> {
    let dir = context
        .omar_dir
        .join("mcp")
        .join(format!("ea-{}", context.ea_id));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Write `bytes` to `path` with mode 0600 on Unix. Caller-readable only,
/// because these files embed workdirs and the omar binary path which leak
/// detail about the user's environment to other accounts on shared hosts.
fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

fn materialize_mcp_context_file(context: &McpLaunchContext) -> Option<PathBuf> {
    let dir = mcp_ea_dir(context)?;
    let path = dir.join("context.json");
    let json = serde_json::to_vec(context).ok()?;
    write_private_file(&path, &json).ok()?;
    Some(path)
}

fn materialize_claude_mcp_config(context: &McpLaunchContext) -> Option<PathBuf> {
    let server_exe = std::env::current_exe().ok()?;
    let context_file = materialize_mcp_context_file(context)?;
    let json = serde_json::json!({
        "mcpServers": {
            "omar": {
                "type": "stdio",
                "command": server_exe,
                "args": ["mcp-server", "--context-file", context_file],
            }
        }
    });

    let dir = mcp_ea_dir(context)?;
    let path = dir.join("claude-mcp.json");
    write_private_file(&path, &serde_json::to_vec(&json).ok()?).ok()?;
    Some(path)
}

fn codex_mcp_overrides(context: &McpLaunchContext) -> Option<String> {
    let server_exe = std::env::current_exe().ok()?;
    let context_file = materialize_mcp_context_file(context)?;
    let command = serde_json::to_string(&server_exe.display().to_string()).ok()?;
    let args = serde_json::to_string(&vec![
        "mcp-server".to_string(),
        "--context-file".to_string(),
        context_file.display().to_string(),
    ])
    .ok()?;
    Some(format!(
        "-c 'mcp_servers.omar.command={}' -c 'mcp_servers.omar.args={}'",
        command, args
    ))
}

fn gemini_mcp_bootstrap(base_command: &str, context: &McpLaunchContext) -> Option<String> {
    let server_exe = std::env::current_exe().ok()?;
    let context_file = materialize_mcp_context_file(context)?;
    let gemini_exec =
        backend_token(base_command, BackendKind::Gemini).unwrap_or_else(|| "gemini".to_string());
    let server_exe = server_exe.display().to_string();
    let context_file = context_file.display().to_string();
    Some(format!(
        "({gemini} mcp remove omar >/dev/null 2>&1 || true; \
         {gemini} mcp add -s user omar '{server}' mcp-server --context-file '{context}' >/dev/null 2>&1 || true)",
        gemini = gemini_exec,
        server = server_exe,
        context = context_file
    ))
}

fn opencode_config_env(context: &McpLaunchContext) -> Option<String> {
    let server_exe = std::env::current_exe().ok()?;
    let context_file = materialize_mcp_context_file(context)?;
    let config = serde_json::json!({
        "mcp": {
            "omar": {
                "type": "local",
                "enabled": true,
                "command": [
                    server_exe.display().to_string(),
                    "mcp-server",
                    "--context-file",
                    context_file.display().to_string()
                ]
            }
        }
    });
    Some(config.to_string())
}

fn ensure_cursor_mcp_config(context: &McpLaunchContext) -> Option<()> {
    // Cursor only reads MCP servers from `~/.cursor/mcp.json`, so we have to
    // write there. Scope the key per-EA (`omar-ea-<id>`) so concurrent spawns
    // across EAs don't clobber each other, preserve every non-omar key the
    // user already has, and write via tmp+rename so partial writes under
    // concurrency can't corrupt the file.
    let server_exe = std::env::current_exe().ok()?;
    let context_file = materialize_mcp_context_file(context)?;
    let home = std::env::var("HOME").ok()?;
    let cursor_dir = PathBuf::from(home).join(".cursor");
    std::fs::create_dir_all(&cursor_dir).ok()?;
    let path = cursor_dir.join("mcp.json");

    let mut root = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    {
        Some(v) if v.is_object() => v,
        _ => serde_json::json!({}),
    };

    if !root
        .get("mcpServers")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        root["mcpServers"] = serde_json::json!({});
    }

    // Remove the legacy plain `omar` key and any stale `omar-ea-*` entries
    // whose context files no longer exist. Stale entries cause cursor to
    // fail MCP server startup, which can block loading of the fresh entry.
    if let Some(servers) = root["mcpServers"].as_object_mut() {
        let stale_keys: Vec<String> = servers
            .iter()
            .filter_map(|(k, v)| {
                if k == "omar" {
                    return Some(k.clone());
                }
                if k.starts_with("omar-ea-") {
                    let ctx_path = v
                        .get("args")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.last())
                        .and_then(|p| p.as_str())
                        .map(PathBuf::from);
                    if let Some(p) = ctx_path {
                        if !p.exists() {
                            return Some(k.clone());
                        }
                    }
                }
                None
            })
            .collect();
        for k in stale_keys {
            servers.remove(&k);
        }
    }

    let key = format!("omar-ea-{}", context.ea_id);
    root["mcpServers"][&key] = serde_json::json!({
        "command": server_exe.display().to_string(),
        "args": ["mcp-server", "--context-file", context_file.display().to_string()],
        "enabled": true,
    });

    // Best-effort cleanup of any `mcp.json.omar-*.tmp` leftovers from a
    // prior crash between write and rename. We own this naming scheme, so
    // it's safe to sweep on every successful call.
    if let Ok(entries) = std::fs::read_dir(&cursor_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("mcp.json.omar-") && name.ends_with(".tmp") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    let payload = serde_json::to_vec_pretty(&root).ok()?;
    let tmp = cursor_dir.join(format!("mcp.json.omar-{}.tmp", Uuid::new_v4()));
    std::fs::write(&tmp, &payload).ok()?;
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(())
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
    mcp_context: &McpLaunchContext,
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

    // Per-backend MCP wiring. Each helper returns None only on an IO-level
    // failure (omar_dir unwritable, current_exe missing, serde error) — in
    // that case we fall back to launching the agent without MCP so the
    // session can still come up and the human operator sees the problem
    // via a degraded but visible agent, rather than a launch failure.
    match detect_backend(base_command) {
        Some(BackendKind::Claude) => match materialize_claude_mcp_config(mcp_context) {
            Some(mcp_config) => format!(
                "{} --system-prompt \"{}\" --mcp-config '{}'",
                base_command,
                shell_expr,
                mcp_config.display()
            ),
            None => format!("{} --system-prompt \"{}\"", base_command, shell_expr),
        },
        Some(BackendKind::Codex) => {
            let mut cmd = format!(
                "{} -c \"developer_instructions='''{}'''\"",
                base_command, shell_expr
            );
            if let Some(overrides) = codex_mcp_overrides(mcp_context) {
                cmd.push(' ');
                cmd.push_str(&overrides);
            }
            cmd
        }
        Some(BackendKind::Cursor) => {
            let rendered = materialize_prompt_file(prompt_file, substitutions);
            let _ = ensure_cursor_mcp_config(mcp_context);
            format!(
                "{} --approve-mcps \"Load the '{}' file and follow the instructions.\"",
                base_command,
                rendered.display()
            )
        }
        Some(BackendKind::Gemini) => {
            let mut cmd = format!(
                "TERM=xterm-256color {} --allowed-mcp-server-names omar -i \"{}\"",
                base_command, shell_expr
            );
            if let Some(setup) = gemini_mcp_bootstrap(base_command, mcp_context) {
                cmd = format!("{}; {}", setup, cmd);
            }
            cmd
        }
        Some(BackendKind::Opencode) => match opencode_config_env(mcp_context) {
            Some(config) => format!(
                "OPENCODE_CONFIG_CONTENT='{}' {} --prompt \"{}\"",
                config.replace('\'', "'\\''"),
                base_command,
                shell_expr
            ),
            None => format!("{} --prompt \"{}\"", base_command, shell_expr),
        },
        None => base_command.to_string(),
    }
}

/// Build an EA command with memory state baked into the system prompt.
///
/// Reads the EA prompt template, appends the latest memory snapshot, writes a
/// combined file to `{omar_dir}/ea/{ea_id}/ea_prompt_combined.md` (per-EA scoped,
/// fixing Gotcha G8), and returns the CLI command with `{{EA_ID}}` and `{{EA_NAME}}`
/// substituted via sed.
pub fn build_ea_command(
    base_command: &str,
    ea_id: EaId,
    ea_name: &str,
    omar_dir: &Path,
    mcp_context: &McpLaunchContext,
) -> String {
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
        mcp_context,
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
    options: &ManagerRuntimeOptions,
) -> Result<()> {
    let start = Instant::now();
    let session = ea::ea_manager_session(ea_id, base_prefix);

    // Check if manager already exists
    if client.has_session(&session)? {
        println!("Manager session already exists. Attaching...");
        client.attach_session(&session)?;
        return Ok(());
    }

    // Build command with EA system prompt + memory baked in
    let cmd = build_ea_command(
        command,
        ea_id,
        ea_name,
        omar_dir,
        &McpLaunchContext {
            omar_dir: omar_dir.to_path_buf(),
            ea_id,
            session_prefix: base_prefix.to_string(),
            default_command: command.to_string(),
            default_workdir: options.default_workdir.clone(),
            health_idle_warning: options.health_idle_warning,
        },
    );

    // Create manager session — system prompt set at process start
    println!("Starting manager agent (EA {})...", ea_id);
    client.new_session(
        &session,
        &cmd,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(2));
    metrics::record_manager_start(ea_id, &session, true, start.elapsed().as_millis() as u64);

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
    options: &ManagerRuntimeOptions,
) -> Result<()> {
    let session = ea::ea_manager_session(ea_id, base_prefix);

    println!("=== OMAR Manager Orchestration Mode (EA {}) ===\n", ea_id);

    // Check if manager exists
    if !client.has_session(&session)? {
        println!("No manager session found. Starting one...");
        start_manager(
            client,
            command,
            ea_id,
            ea_name,
            omar_dir,
            base_prefix,
            options,
        )?;
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
    client.deliver_prompt(session, message, &DeliveryOptions::default())?;
    println!("Sent to manager: {}", message);
    Ok(())
}

fn send_to_agent(client: &TmuxClient, agent: &str, message: &str) -> Result<()> {
    let session_name = format!("{}{}", client.prefix(), agent);

    if !client.has_session(&session_name)? {
        println!("Agent '{}' not found.", agent);
        return Ok(());
    }

    client.deliver_prompt(&session_name, message, &DeliveryOptions::default())?;
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
    let parent_name = "ea";
    let prompt_file = prompts_dir(omar_dir).join("agent.md");
    let cmd = build_agent_command(
        command,
        &prompt_file,
        &[
            ("{{PARENT_NAME}}", parent_name),
            ("{{TASK}}", &agent.task),
            ("{{EA_ID}}", &ea_id.to_string()),
        ],
        &McpLaunchContext {
            omar_dir: omar_dir.to_path_buf(),
            ea_id,
            session_prefix: base_prefix.to_string(),
            default_command: command.to_string(),
            default_workdir: ".".to_string(),
            health_idle_warning: 15,
        },
    );

    // Create worker session — system prompt set at process start
    client.new_session(
        &session_name,
        &cmd,
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(1));

    // Deliver initial task context so agent.md's "Task Header" section is satisfied
    let first_message = format!("YOUR NAME: {}\nYOUR TASK: {}", agent.name, agent.task);
    let _ = client.deliver_prompt(&session_name, &first_message, &DeliveryOptions::default());

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

    /// A minimal MCP context scoped to a caller-supplied temp dir. Tests that
    /// exercise only command-string shape (no filesystem assertions) can pass
    /// any path — the per-backend materializers return `None` silently on IO
    /// failure, which is part of what we're asserting on. Tests that also
    /// need the context files on disk must use a real `tempfile::tempdir()`.
    fn test_mcp_context(omar_dir: &Path) -> McpLaunchContext {
        McpLaunchContext {
            omar_dir: omar_dir.to_path_buf(),
            ea_id: 0,
            session_prefix: "omar-agent-".to_string(),
            default_command: "claude".to_string(),
            default_workdir: ".".to_string(),
            health_idle_warning: 15,
        }
    }

    #[test]
    fn test_build_agent_command_claude() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "claude --some-flag",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(
            cmd.starts_with("claude --some-flag --system-prompt \"$(cat '/tmp/prompts/ea.md')\""),
            "unexpected claude command: {cmd}"
        );
        assert!(cmd.contains("--mcp-config"));
    }

    #[test]
    fn test_build_agent_command_codex() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "codex --no-alt-screen",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.starts_with(
            "codex --no-alt-screen -c \"developer_instructions='''$(cat '/tmp/prompts/ea.md')'''\""
        ));
        assert!(cmd.contains("mcp_servers.omar.command"));
        assert!(cmd.contains("mcp_servers.omar.args"));
    }

    #[test]
    fn test_build_agent_command_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "cursor agent --yolo",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.contains("cursor agent --yolo --approve-mcps"));
        assert!(cmd.contains("Load the '/tmp/"));
    }

    #[test]
    fn test_build_agent_command_gemini() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "gemini --yolo",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.contains("gemini mcp remove omar"));
        assert!(cmd.contains("gemini mcp add -s user omar"));
        assert!(cmd.contains("mcp-server --context-file"));
        assert!(cmd.contains(
            "TERM=xterm-256color gemini --yolo --allowed-mcp-server-names omar -i \"$(cat '/tmp/prompts/ea.md')\""
        ));
    }

    #[test]
    fn test_build_agent_command_opencode() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "opencode",
            Path::new("/tmp/prompts/pm.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.contains("OPENCODE_CONFIG_CONTENT="));
        assert!(cmd.contains("\"mcp\""));
        assert!(cmd.contains("\"omar\""));
        assert!(cmd.contains("--prompt \"$(cat '/tmp/prompts/pm.md')\""));
    }

    #[test]
    fn test_build_agent_command_env_wrapper_preserved() {
        // Backend detection must look past shell-env prefixes like
        // `env FOO=bar <backend>` so per-backend flags still get added.
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "env ANTHROPIC_API_KEY=test claude --yolo",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.starts_with("env ANTHROPIC_API_KEY=test claude --yolo --system-prompt"));
    }

    #[test]
    fn test_build_agent_command_unknown_backend() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "vim",
            Path::new("/tmp/prompts/ea.md"),
            &[],
            &test_mcp_context(dir.path()),
        );
        assert_eq!(cmd, "vim");
    }

    #[test]
    fn test_build_agent_command_with_substitutions() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "claude",
            Path::new("/prompts/worker.md"),
            &[("{{PARENT_NAME}}", "pm-api"), ("{{TASK}}", "build it")],
            &test_mcp_context(dir.path()),
        );
        assert!(cmd.contains("s|{{PARENT_NAME}}|pm-api|g"));
        assert!(cmd.contains("s|{{TASK}}|build it|g"));
        assert!(cmd.contains("'/prompts/worker.md'"));
    }

    #[test]
    fn test_build_agent_command_with_ea_id() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = build_agent_command(
            "claude",
            Path::new("/prompts/agent.md"),
            &[
                ("{{PARENT_NAME}}", "ea"),
                ("{{TASK}}", "do stuff"),
                ("{{EA_ID}}", "2"),
            ],
            &test_mcp_context(dir.path()),
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

        let cmd = build_ea_command(
            "claude",
            0,
            "Default",
            omar_dir,
            &test_mcp_context(omar_dir),
        );
        assert!(cmd.contains("s|{{EA_ID}}|0|g"));
        assert!(cmd.contains("s|{{EA_NAME}}|Default|g"));
    }

    #[test]
    fn test_build_ea_command_writes_to_ea_scoped_dir() {
        let dir = tempfile::tempdir().unwrap();
        let omar_dir = dir.path();

        let state_dir = ea::ea_state_dir(1, omar_dir);
        std::fs::create_dir_all(&state_dir).unwrap();

        build_ea_command(
            "claude",
            1,
            "Research",
            omar_dir,
            &McpLaunchContext {
                omar_dir: omar_dir.to_path_buf(),
                ea_id: 1,
                session_prefix: "omar-agent-".to_string(),
                default_command: "claude".to_string(),
                default_workdir: ".".to_string(),
                health_idle_warning: 15,
            },
        );

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

        build_ea_command(
            "claude",
            0,
            "Default",
            omar_dir,
            &test_mcp_context(omar_dir),
        );

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
