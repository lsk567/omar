mod api;
mod app;
mod config;
mod event;
mod manager;
mod memory;
mod projects;
mod tmux;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::Mutex;

use app::App;
use config::Config;
use event::{AppEvent, EventHandler};
use tmux::TmuxClient;

/// Tmux session name used when omar auto-launches into tmux
pub const DASHBOARD_SESSION: &str = "omar-dashboard";

#[derive(Parser)]
#[command(name = "omar", about = "Agent dashboard for tmux", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file
    #[arg(short, long, default_value = "~/.config/omar/config.toml")]
    config: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn a new agent session
    Spawn {
        /// Name for the agent session
        #[arg(short, long)]
        name: String,

        /// Command to run in the session (defaults to configured default_command)
        #[arg(short, long)]
        command: Option<String>,

        /// Working directory
        #[arg(short, long)]
        workdir: Option<String>,
    },

    /// List all agent sessions
    List,

    /// Kill an agent session
    Kill {
        /// Name of the session to kill
        name: String,
    },

    /// Start or attach to the manager agent
    Manager {
        #[command(subcommand)]
        action: Option<ManagerAction>,
    },
}

#[derive(Subcommand)]
enum ManagerAction {
    /// Start the manager agent session
    Start,
    /// Run manager orchestration mode
    Orchestrate,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    let client = TmuxClient::new(&config.dashboard.session_prefix);

    match cli.command {
        Some(Commands::Spawn {
            name,
            command,
            workdir,
        }) => {
            let cmd = command.unwrap_or_else(|| config.agent.default_command.clone());
            spawn_agent(&client, &name, &cmd, workdir.as_deref())
        }
        Some(Commands::List) => list_agents(&client),
        Some(Commands::Kill { name }) => kill_agent(&client, &name),
        Some(Commands::Manager { action }) => match action {
            Some(ManagerAction::Start) | None => {
                manager::start_manager(&client, &config.agent.default_command)
            }
            Some(ManagerAction::Orchestrate) => {
                manager::run_manager_orchestration(&client, &config.agent.default_command)
            }
        },
        None => {
            if std::env::var("TMUX").is_err() {
                relaunch_in_tmux()
            } else {
                run_dashboard(config).await
            }
        }
    }
}

fn spawn_agent(
    client: &TmuxClient,
    name: &str,
    command: &str,
    workdir: Option<&str>,
) -> Result<()> {
    let full_name = format!("{}{}", client.prefix(), name);

    if client.has_session(&full_name)? {
        anyhow::bail!("Session '{}' already exists", name);
    }

    client.new_session(&full_name, command, workdir)?;
    println!("Spawned agent: {}", name);
    Ok(())
}

fn list_agents(client: &TmuxClient) -> Result<()> {
    let sessions = client.list_sessions()?;

    if sessions.is_empty() {
        println!("No agent sessions found");
        return Ok(());
    }

    println!("{:<20} {:<12} {:<10}", "NAME", "ATTACHED", "PID");
    println!("{}", "-".repeat(44));

    for session in sessions {
        let name = session
            .name
            .strip_prefix(client.prefix())
            .unwrap_or(&session.name);
        let attached = if session.attached { "yes" } else { "no" };
        println!("{:<20} {:<12} {:<10}", name, attached, session.pane_pid);
    }

    Ok(())
}

fn kill_agent(client: &TmuxClient, name: &str) -> Result<()> {
    let full_name = format!("{}{}", client.prefix(), name);

    if !client.has_session(&full_name)? {
        anyhow::bail!("Session '{}' not found", name);
    }

    client.kill_session(&full_name)?;
    println!("Killed agent: {}", name);
    Ok(())
}

/// Re-launch omar inside a tmux session.
/// Called when the dashboard is started outside of tmux so that popups,
/// attach, and other tmux-dependent features work correctly.
fn relaunch_in_tmux() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut cmd = std::process::Command::new("tmux");
    // -A: attach if session already exists, otherwise create it
    cmd.args(["new-session", "-A", "-s", DASHBOARD_SESSION]);
    cmd.arg(&exe);
    cmd.args(&args);

    // exec() replaces the current process; only returns on error
    let err = cmd.exec();
    anyhow::bail!("Failed to launch tmux: {}", err)
}

async fn run_dashboard(config: Config) -> Result<()> {
    // Start API server if enabled
    if config.api.enabled {
        let api_config = config.api.clone();
        let api_app = Arc::new(Mutex::new(App::new(&config)));
        tokio::spawn(async move {
            if let Err(e) = api::start_server(api_app, &api_config).await {
                eprintln!("API server error: {}", e);
            }
        });
    }

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app for dashboard (separate instance)
    let mut app = App::new(&config);

    // Initial refresh
    if let Err(e) = app.refresh() {
        app.set_status(format!("Error: {}", e));
    }

    // Event loop
    let tick_rate = Duration::from_secs(config.dashboard.refresh_interval);
    let mut events = EventHandler::new(tick_rate);

    loop {
        // Draw UI
        terminal.draw(|f| ui::render(f, &app))?;

        // Handle events
        if let Some(event) = events.next().await {
            match event {
                AppEvent::Key(key) => {
                    // Handle project input mode
                    if app.project_input_mode {
                        match key.code {
                            KeyCode::Esc => {
                                app.project_input_mode = false;
                                app.project_input.clear();
                            }
                            KeyCode::Enter => {
                                let name = app.project_input.clone();
                                if !name.trim().is_empty() {
                                    app.add_project(name.trim());
                                    app.set_status("Project added");
                                }
                                app.project_input_mode = false;
                                app.project_input.clear();
                            }
                            KeyCode::Backspace => {
                                app.project_input.pop();
                            }
                            KeyCode::Char(c) => {
                                app.project_input.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle interactive mode - forward keys to selected agent
                    if app.interactive_mode {
                        match key.code {
                            KeyCode::Esc => {
                                app.exit_interactive();
                            }
                            KeyCode::Enter => {
                                let _ = app.send_key_to_selected("Enter");
                            }
                            KeyCode::Backspace => {
                                let _ = app.send_key_to_selected("BSpace");
                            }
                            KeyCode::Tab => {
                                let _ = app.send_key_to_selected("Tab");
                            }
                            KeyCode::Up => {
                                let _ = app.send_key_to_selected("Up");
                            }
                            KeyCode::Down => {
                                let _ = app.send_key_to_selected("Down");
                            }
                            KeyCode::Left => {
                                let _ = app.send_key_to_selected("Left");
                            }
                            KeyCode::Right => {
                                let _ = app.send_key_to_selected("Right");
                            }
                            KeyCode::Char(c) => {
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    // Send Ctrl+key
                                    let _ = app.send_key_to_selected(&format!("C-{}", c));
                                } else {
                                    let _ = app.send_text_to_selected(&c.to_string());
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle confirmation dialog
                    if app.show_confirm_kill {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                if let Err(e) = app.kill_selected() {
                                    app.set_status(format!("Error: {}", e));
                                }
                            }
                            _ => {
                                app.show_confirm_kill = false;
                            }
                        }
                        continue;
                    }

                    // Handle help popup
                    if app.show_help {
                        app.show_help = false;
                        continue;
                    }

                    // Normal key handling
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            app.next();
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            app.previous();
                        }
                        KeyCode::Char('i') => {
                            // Enter interactive mode for any selected agent
                            app.enter_interactive();
                        }
                        KeyCode::Enter => {
                            if std::env::var("TMUX").is_ok() {
                                // Inside tmux: use display-popup overlay (stays on top of dashboard)
                                if let Err(e) = app.attach_selected() {
                                    app.set_status(format!("Error: {}", e));
                                }
                                // Discard ticks that accumulated while popup was open
                                events.drain();
                                // Force redraw after popup closes
                                terminal.clear()?;
                            } else {
                                // Outside tmux: temporarily exit alternate screen
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

                                let result = app.attach_selected();

                                // Restore terminal
                                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                enable_raw_mode()?;
                                // Discard ticks that accumulated while popup was open
                                events.drain();
                                terminal.clear()?;

                                if let Err(e) = result {
                                    app.set_status(format!("Error: {}", e));
                                }
                            }
                        }
                        KeyCode::Char('n') => {
                            if let Err(e) = app.spawn_agent() {
                                app.set_status(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('d') => {
                            if app.selected_agent().is_some() {
                                app.show_confirm_kill = true;
                            }
                        }
                        KeyCode::Char('p') => {
                            app.project_input_mode = true;
                        }
                        KeyCode::Char('r') => {
                            if let Err(e) = app.refresh() {
                                app.set_status(format!("Error: {}", e));
                            } else {
                                app.set_status("Refreshed");
                            }
                        }
                        KeyCode::Char('?') => {
                            app.show_help = !app.show_help;
                        }
                        _ => {}
                    }
                }
                AppEvent::Tick => {
                    app.clear_status();
                    if let Err(e) = app.refresh() {
                        app.set_status(format!("Error: {}", e));
                    }
                }
                AppEvent::Resize(_, _) => {
                    // Terminal will handle resize automatically
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    // Kill the EA session on exit
    if app
        .client()
        .has_session(crate::manager::MANAGER_SESSION)
        .unwrap_or(false)
    {
        let _ = app.client().kill_session(crate::manager::MANAGER_SESSION);
    }

    Ok(())
}
