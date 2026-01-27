//! Manager agent orchestration

pub mod protocol;

use anyhow::Result;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crate::tmux::TmuxClient;
use protocol::{parse_manager_message, ManagerMessage, ProposedAgent};

/// Manager session name (exported for use in app.rs)
pub const MANAGER_SESSION: &str = "oma-manager";

/// System prompt for the manager agent
pub const MANAGER_SYSTEM_PROMPT: &str = r#"You are a Manager Agent in the OMA (One Man Army) system. Your role is to:

1. UNDERSTAND the user's high-level request
2. BREAK IT DOWN into parallel sub-tasks for worker agents
3. PROPOSE a plan in JSON format for user approval
4. COORDINATE workers once approved

## How to Propose a Plan

When you have a plan, output it in this EXACT JSON format:

```json
{
  "type": "plan",
  "description": "Brief description of the overall goal",
  "agents": [
    {
      "name": "short-name",
      "role": "Role Title",
      "task": "Specific task description",
      "depends_on": []
    }
  ]
}
```

## Guidelines

- Keep agent names short (e.g., "api", "auth", "db", "test")
- Be specific about each agent's task
- Use depends_on to specify dependencies (agent names)
- Aim for 2-5 agents for most tasks
- Each agent should have a focused, independent scope

## Example

User: "Build a web app with user authentication"

Your response:
I'll break this down into parallel workstreams:

```json
{
  "type": "plan",
  "description": "Web app with user authentication",
  "agents": [
    {"name": "backend", "role": "Backend Developer", "task": "Set up Express server with /api routes", "depends_on": []},
    {"name": "auth", "role": "Auth Specialist", "task": "Implement JWT authentication and login/register endpoints", "depends_on": ["backend"]},
    {"name": "frontend", "role": "Frontend Developer", "task": "Create React app with login/register forms", "depends_on": []},
    {"name": "integration", "role": "Integration Engineer", "task": "Connect frontend to backend, test auth flow end-to-end", "depends_on": ["auth", "frontend"]}
  ]
}
```

This creates 4 agents with clear dependencies. Agents without dependencies can work in parallel.

## Commands After Approval

Once the user approves, you can send messages to workers:

```json
{"type": "send", "target": "agent-name", "message": "Your instructions here"}
```

Query status:
```json
{"type": "query", "target": "all"}
```

Now, wait for the user's request.
"#;

/// Start the manager agent session
pub fn start_manager(client: &TmuxClient) -> Result<()> {
    // Check if manager already exists
    if client.has_session(MANAGER_SESSION)? {
        println!("Manager session already exists. Attaching...");
        client.attach_session(MANAGER_SESSION)?;
        return Ok(());
    }

    // Create manager session with Claude
    println!("Starting manager agent...");
    client.new_session(
        MANAGER_SESSION,
        "claude --dangerously-skip-permissions",
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(2));

    // Send the system prompt
    println!("Configuring manager with system prompt...");
    client.send_keys_literal(MANAGER_SESSION, MANAGER_SYSTEM_PROMPT)?;
    client.send_keys(MANAGER_SESSION, "Enter")?;

    thread::sleep(Duration::from_millis(500));

    // Attach to the session
    println!("Attaching to manager session...");
    client.attach_session(MANAGER_SESSION)?;

    Ok(())
}

/// Run the manager in orchestration mode (interactive)
pub fn run_manager_orchestration(client: &TmuxClient) -> Result<()> {
    println!("=== OMA Manager Orchestration Mode ===\n");

    // Check if manager exists
    if !client.has_session(MANAGER_SESSION)? {
        println!("No manager session found. Starting one...");
        start_manager(client)?;
        return Ok(());
    }

    loop {
        // Get user input
        print!("\n[OMA] Enter command (or 'help'): ");
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
                approve_plan(client)?;
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
OMA Manager Commands:
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

fn approve_plan(client: &TmuxClient) -> Result<()> {
    let output = client.capture_pane(MANAGER_SESSION, 50)?;

    if let Some(ManagerMessage::Plan {
        description,
        agents,
    }) = parse_manager_message(&output)
    {
        println!("\nApproving plan: {}", description);
        println!("Spawning {} worker agents...\n", agents.len());

        for agent in &agents {
            spawn_worker(client, agent)?;
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

fn spawn_worker(client: &TmuxClient, agent: &ProposedAgent) -> Result<()> {
    let session_name = format!("{}{}", client.prefix(), agent.name);

    if client.has_session(&session_name)? {
        println!("  {} - already exists, skipping", agent.name);
        return Ok(());
    }

    // Create the worker session
    client.new_session(
        &session_name,
        "claude --dangerously-skip-permissions",
        Some(&std::env::current_dir()?.to_string_lossy()),
    )?;

    // Give it time to start
    thread::sleep(Duration::from_secs(1));

    // Send worker context
    let context = format!(
        r#"You are a Worker Agent in the OMA system.

YOUR ROLE: {}
YOUR TASK: {}

INSTRUCTIONS:
- Focus ONLY on your assigned task
- Work independently but be aware others are working in parallel
- When you complete your task, say: [TASK COMPLETE]
- If you're blocked, say: [BLOCKED: reason]
- If you need clarification, say: [NEED INPUT: question]

Begin working on your task now.
"#,
        agent.role, agent.task
    );

    client.send_keys_literal(&session_name, &context)?;
    client.send_keys(&session_name, "Enter")?;

    println!("  {} - spawned ({})", agent.name, agent.role);

    Ok(())
}
