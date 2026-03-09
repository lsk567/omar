mod api;
mod app;
mod computer;
mod config;
mod event;
mod manager;
mod memory;
mod projects;
mod scheduler;
mod settings;
mod tmux;
mod ui;

use std::io;
use std::path::PathBuf;
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

    /// Agent backend to use (e.g., "claude", "opencode")
    #[arg(short, long)]
    agent: Option<String>,
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

    /// Configure tmux for optimal omar experience
    SetupTmux,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = Config::load(&cli.config)?;
    if let Some(ref agent) = cli.agent {
        config.agent.default_command = config::resolve_backend(agent);
    }
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
        Some(Commands::SetupTmux) => setup_tmux(),
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

    // Kill any stale dashboard session left behind by a previous crash.
    // Without this, `new-session` would fail because the session already exists
    // (but contains a dead shell instead of the dashboard).
    let client = TmuxClient::new("");
    let _ = client.kill_session(DASHBOARD_SESSION);

    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut cmd = std::process::Command::new("tmux");
    cmd.args(["new-session", "-s", DASHBOARD_SESSION]);
    cmd.arg(&exe);
    cmd.args(&args);

    // exec() replaces the current process; only returns on error
    let err = cmd.exec();
    anyhow::bail!("Failed to launch tmux: {}", err)
}

/// Recommended tmux settings for omar, keyed by option name.
const TMUX_RECOMMENDED: &[(&str, &str, &str)] = &[
    ("mouse", "set -g mouse on", "mouse scrolling and selection"),
    (
        "extended-keys",
        "set -g extended-keys on",
        "Shift+Enter in agents",
    ),
    (
        "set-clipboard",
        "set -g set-clipboard on",
        "clipboard integration",
    ),
];

/// Additional raw lines that need to appear in tmux.conf (checked by substring).
const TMUX_EXTRA_LINES: &[(&str, &str, &str)] = &[
    (
        "terminal-features',*:extkeys'",
        "set -as terminal-features ',*:extkeys'",
        "extended key passthrough",
    ),
    (
        "terminal-features',*:clipboard'",
        "set -as terminal-features ',*:clipboard'",
        "clipboard passthrough",
    ),
    (
        "bind-key-nC-\\\\",
        "bind-key -n C-\\\\ detach-client",
        "Ctrl+\\\\ to detach from popup",
    ),
];

/// Check if any recommended tmux settings are missing.
fn tmux_setup_needed() -> bool {
    for &(opt, cmd, _) in TMUX_RECOMMENDED {
        // Extract expected value from the command string (last word)
        let expected = cmd.split_whitespace().last().unwrap_or("on");
        if let Ok(out) = std::process::Command::new("tmux")
            .args(["show-options", "-gv", opt])
            .output()
        {
            let val = String::from_utf8_lossy(&out.stdout);
            if val.trim() != expected {
                return true;
            }
        }
    }
    false
}

/// Interactive tmux configuration setup.
fn setup_tmux() -> Result<()> {
    use std::io::Write;

    let conf_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tmux.conf");

    let existing = std::fs::read_to_string(&conf_path).unwrap_or_default();

    // Collect missing settings
    let mut to_add: Vec<(&str, &str)> = Vec::new();

    for &(opt, line, desc) in TMUX_RECOMMENDED {
        if !existing.contains(line) {
            // Also check if the option is already set at runtime
            let expected = line.split_whitespace().last().unwrap_or("on");
            let already_set = std::process::Command::new("tmux")
                .args(["show-options", "-gv", opt])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == expected)
                .unwrap_or(false);
            if !already_set {
                to_add.push((line, desc));
            }
        }
    }

    let normalized = existing.replace(' ', "");
    for &(needle, line, desc) in TMUX_EXTRA_LINES {
        if !normalized.contains(needle) {
            to_add.push((line, desc));
        }
    }

    if to_add.is_empty() {
        println!("✓ tmux is already configured for omar.");
        return Ok(());
    }

    println!(
        "The following settings will be added to {}:\n",
        conf_path.display()
    );
    for (line, desc) in &to_add {
        println!("  {}  # {}", line, desc);
    }

    print!("\nApply? [Y/n] ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if !input.is_empty() && input != "y" && input != "yes" {
        println!("Aborted.");
        return Ok(());
    }

    // Append to tmux.conf
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&conf_path)?;

    writeln!(file, "\n# omar recommended settings")?;
    for (line, _) in &to_add {
        writeln!(file, "{}", line)?;
    }

    // Apply to running tmux server
    if std::process::Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let _ = std::process::Command::new("tmux")
            .args(["source-file", &conf_path.to_string_lossy()])
            .status();
        println!("✓ Applied to ~/.tmux.conf and reloaded tmux.");
    } else {
        println!("✓ Applied to ~/.tmux.conf (tmux not running, will take effect next session).");
    }

    Ok(())
}

/// Locate the `omar-slack` binary. Checks next to the current executable
/// first, then falls back to a PATH lookup.
fn find_slack_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("omar-slack");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Fall back to PATH lookup
    Some(PathBuf::from("omar-slack"))
}

/// Spawn the Slack bridge binary if SLACK_BOT_TOKEN and SLACK_APP_TOKEN are set.
fn spawn_slack_bridge() -> Option<std::process::Child> {
    if std::env::var("SLACK_BOT_TOKEN").is_err() || std::env::var("SLACK_APP_TOKEN").is_err() {
        return None;
    }

    let binary = find_slack_binary()?;
    match std::process::Command::new(&binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            eprintln!("[omar] Slack bridge started (pid {})", child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[omar] Failed to start Slack bridge: {}", e);
            None
        }
    }
}

/// Locate the `omar-computer` binary. Checks next to the current executable
/// first, then falls back to a PATH lookup.
fn find_computer_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("omar-computer");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Fall back to PATH lookup
    Some(PathBuf::from("omar-computer"))
}

/// Spawn the computer-use bridge binary if DISPLAY is set (X11 available).
fn spawn_computer_bridge() -> Option<std::process::Child> {
    if std::env::var("DISPLAY").is_err() {
        return None;
    }

    let binary = find_computer_binary()?;
    match std::process::Command::new(&binary)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            eprintln!("[omar] Computer bridge started (pid {})", child.id());
            Some(child)
        }
        Err(e) => {
            eprintln!("[omar] Failed to start computer bridge: {}", e);
            None
        }
    }
}

/// Kill a child process gracefully: SIGTERM first, then SIGKILL after timeout.
fn kill_child_gracefully(child: &mut std::process::Child, timeout: Duration) {
    // Send SIGTERM
    let _ = std::process::Command::new("kill")
        .arg(child.id().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Wait for the process to exit
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => {
                if start.elapsed() >= timeout {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // Force kill if still running
    let _ = child.kill();
    let _ = child.wait();
}

async fn run_dashboard(config: Config) -> Result<()> {
    // Create the ticker buffer and scheduler, then spawn the event loop
    let ticker = scheduler::TickerBuffer::new();
    let scheduler = Arc::new(scheduler::Scheduler::new());
    let popup_receiver = scheduler::new_popup_receiver();
    tokio::spawn(scheduler::run_event_loop(
        scheduler.clone(),
        ticker.clone(),
        popup_receiver.clone(),
    ));

    // Start API server if enabled
    if config.api.enabled {
        let api_config = config.api.clone();
        let api_state = Arc::new(api::handlers::ApiState {
            app: Arc::new(Mutex::new(App::new(&config, ticker.clone()))),
            scheduler: scheduler.clone(),
            computer_lock: computer::new_lock(),
        });
        tokio::spawn(async move {
            if let Err(e) = api::start_server(api_state, &api_config).await {
                eprintln!("API server error: {}", e);
            }
        });
    }

    // Spawn Slack bridge if configured
    let mut slack_bridge = spawn_slack_bridge();

    // Spawn computer-use bridge if X11 is available
    let mut computer_bridge = spawn_computer_bridge();

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app for dashboard (separate instance)
    let mut app = App::new(&config, ticker);

    // Show bridge status
    match (slack_bridge.is_some(), computer_bridge.is_some()) {
        (true, true) => app.set_status("Slack & computer bridges started"),
        (true, false) => app.set_status("Slack bridge started"),
        (false, true) => app.set_status("Computer bridge started"),
        _ => {}
    }

    // Warn if tmux config is missing recommended settings
    if tmux_setup_needed() {
        app.set_persistent_warning("⚠ tmux not configured for omar — run 'omar setup-tmux' to fix");
    }

    // Initial refresh
    if let Err(e) = app.refresh() {
        app.set_status(format!("Error: {}", e));
    }

    // Event loop
    let tick_rate = Duration::from_secs(config.dashboard.refresh_interval);
    let mut events = EventHandler::new(tick_rate);
    let mut tick_count: u64 = 0;

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

                    // Handle confirmation dialog (kill or quit)
                    if let Some(action) = app.pending_confirm {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => match action {
                                app::ConfirmAction::Kill => {
                                    if let Err(e) = app.kill_selected() {
                                        app.set_status(format!("Error: {}", e));
                                    }
                                }
                                app::ConfirmAction::Quit => {
                                    app.should_quit = true;
                                }
                            },
                            _ => {
                                app.pending_confirm = None;
                            }
                        }
                        continue;
                    }

                    // Handle help popup
                    if app.show_help {
                        app.show_help = false;
                        continue;
                    }

                    // Handle events popup
                    if app.show_events {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('e') | KeyCode::Enter => {
                                app.show_events = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle debug console popup
                    if app.show_debug_console {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('D') => {
                                app.show_debug_console = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle settings popup
                    if app.show_settings {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('S') => {
                                app.show_settings = false;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if app.settings_selected > 0 {
                                    app.settings_selected -= 1;
                                }
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if app.settings_selected + 1 < app.settings.count() {
                                    app.settings_selected += 1;
                                }
                            }
                            KeyCode::Enter => {
                                app.settings.toggle(app.settings_selected);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle sidebar enlarged popup
                    if app.sidebar_popup.is_some() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                app.sidebar_popup = None;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Normal key handling
                    match key.code {
                        KeyCode::Char('Q') => {
                            app.pending_confirm = Some(app::ConfirmAction::Quit);
                        }
                        KeyCode::Esc => {
                            app.drill_up();
                        }
                        KeyCode::Tab | KeyCode::Right => {
                            if app.sidebar_focused {
                                app.sidebar_focused = false;
                            } else {
                                app.drill_down();
                            }
                        }
                        KeyCode::Left => {
                            if !app.sidebar_focused {
                                app.sidebar_focused = true;
                            } else {
                                app.drill_up();
                            }
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.pending_confirm = Some(app::ConfirmAction::Quit);
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if app.sidebar_focused {
                                app.sidebar_next();
                            } else {
                                app.next();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if app.sidebar_focused {
                                app.sidebar_previous();
                            } else {
                                app.previous();
                            }
                        }
                        KeyCode::Char('h') => {
                            app.sidebar_focused = true;
                        }
                        KeyCode::Char('l') => {
                            app.sidebar_focused = false;
                        }
                        KeyCode::Enter => {
                            if app.sidebar_focused {
                                if app.sidebar_panel == app::SidebarPanel::Events {
                                    app.scheduled_events = scheduler.list();
                                    app.scheduled_events.sort_by_key(|e| e.timestamp);
                                    app.show_events = true;
                                } else {
                                    app.sidebar_popup = Some(app.sidebar_panel);
                                }
                                continue;
                            }
                            // Tell the scheduler which agent popup is open so it
                            // defers events for that receiver until the popup closes.
                            *popup_receiver.lock().unwrap() = app.selected_agent_short_name();

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

                            // Popup closed — clear so events resume delivery
                            *popup_receiver.lock().unwrap() = None;
                        }
                        KeyCode::Char('n') => {
                            if let Err(e) = app.spawn_agent() {
                                app.set_status(format!("Error: {}", e));
                            }
                        }
                        KeyCode::Char('d') => {
                            if app.selected_agent().is_some() {
                                app.pending_confirm = Some(app::ConfirmAction::Kill);
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
                        KeyCode::Char('e') => {
                            app.scheduled_events = scheduler.list();
                            app.scheduled_events.sort_by_key(|e| e.timestamp);
                            app.show_events = true;
                        }
                        KeyCode::Char('D') => {
                            app.show_debug_console = true;
                        }
                        KeyCode::Char('z') => {
                            // Detach from tmux — dashboard + agents keep running
                            if std::env::var("TMUX").is_ok() {
                                let _ = std::process::Command::new("tmux")
                                    .args(["detach-client"])
                                    .status();
                            }
                        }
                        KeyCode::Char('S') => {
                            app.show_settings = true;
                        }
                        KeyCode::Char('?') => {
                            app.show_help = !app.show_help;
                        }
                        _ => {}
                    }
                }
                AppEvent::Tick => {
                    // Rotate quotes every ~30 ticks
                    tick_count += 1;
                    if tick_count.is_multiple_of(30) {
                        app.quote_index = app.quote_index.wrapping_add(1);
                    }

                    // Keep event snapshots fresh for status-bar countdowns and events popup.
                    app.scheduled_events = scheduler.list();
                    app.scheduled_events.sort_by_key(|e| e.timestamp);

                    // Skip refresh while a popup/input overlay is active
                    // to avoid interrupting user input.
                    if !app.has_popup() {
                        app.clear_status();
                        if let Err(e) = app.refresh() {
                            app.set_status(format!("Error: {}", e));
                        }
                    }

                    // Keep system_state.md in sync with live state
                    memory::write_memory(
                        &app.agents,
                        app.manager.as_ref(),
                        app.client(),
                        &app.scheduled_events,
                    );
                }
                AppEvent::TickerScroll => {
                    app.ticker_offset = app.ticker_offset.wrapping_add(1);
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

    // Kill Slack bridge on exit
    if let Some(ref mut child) = slack_bridge {
        kill_child_gracefully(child, Duration::from_secs(3));
    }

    // Kill computer bridge on exit
    if let Some(ref mut child) = computer_bridge {
        kill_child_gracefully(child, Duration::from_secs(3));
    }

    Ok(())
}
