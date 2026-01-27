mod api;
mod app;
mod config;
mod event;
mod manager;
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

#[derive(Parser)]
#[command(name = "oma", about = "Agent dashboard for tmux", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file
    #[arg(short, long, default_value = "~/.config/oma/config.toml")]
    config: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn a new agent session
    Spawn {
        /// Name for the agent session
        #[arg(short, long)]
        name: String,

        /// Command to run in the session
        #[arg(short, long, default_value = "claude")]
        command: String,

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
        }) => spawn_agent(&client, &name, &command, workdir.as_deref()),
        Some(Commands::List) => list_agents(&client),
        Some(Commands::Kill { name }) => kill_agent(&client, &name),
        Some(Commands::Manager { action }) => match action {
            Some(ManagerAction::Start) | None => manager::start_manager(&client),
            Some(ManagerAction::Orchestrate) => manager::run_manager_orchestration(&client),
        },
        None => run_dashboard(config).await,
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
                    // Handle confirmation dialog first
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
                        KeyCode::Enter => {
                            // Temporarily exit raw mode for popup
                            disable_raw_mode()?;
                            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

                            let result = app.attach_selected();

                            // Restore terminal
                            execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal.clear()?;

                            if let Err(e) = result {
                                app.set_status(format!("Error: {}", e));
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

    // Cleanup: kill manager session on exit
    let _ = app.client().kill_session(crate::manager::MANAGER_SESSION);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
