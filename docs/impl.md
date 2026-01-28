# OMAR: Implementation Plan

## Tech Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust | Memory safety, concurrency, single binary |
| TUI Framework | ratatui | Active community, flexible, performant |
| Async Runtime | tokio | Industry standard, good ecosystem |
| CLI | clap | Derive macros, excellent UX |
| Config | toml | Native Rust support, human-readable |
| Error Handling | anyhow + thiserror | Ergonomic error handling |

## Project Structure

```
omar/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point, CLI parsing
│   ├── app.rs               # Application state machine
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── dashboard.rs     # Main dashboard layout
│   │   ├── agent_card.rs    # Individual agent widget
│   │   └── status_bar.rs    # Top status bar
│   ├── tmux/
│   │   ├── mod.rs
│   │   ├── client.rs        # tmux command wrapper
│   │   ├── session.rs       # Session types
│   │   └── health.rs        # Health checking logic
│   ├── config.rs            # Configuration loading
│   └── event.rs             # Input/tick event handling
├── tests/
│   ├── tmux_client_test.rs
│   └── health_test.rs
└── README.md
```

## Milestones

### M1: Core Infrastructure
- [ ] Project setup (Cargo.toml, dependencies)
- [ ] tmux client wrapper (`tmux/client.rs`)
  - `list_sessions()` → Vec<Session>
  - `capture_pane(target)` → String
  - `get_pane_activity(target)` → i64
  - `send_keys(target, keys)` → Result<()>
  - `kill_session(name)` → Result<()>
  - `new_session(name, command)` → Result<()>
- [ ] Basic config loading with serde

### M2: Health Monitoring
- [ ] Health checker (`tmux/health.rs`)
  - Idle time calculation
  - Error pattern scanning (regex)
  - HealthState enum (Ok, Idle, Stuck)
- [ ] Unit tests for health logic

### M3: TUI Dashboard
- [ ] Event loop (crossterm + tokio)
- [ ] Main app state machine (`app.rs`)
- [ ] Status bar widget (agent count, system stats)
- [ ] Agent card widget (name, status, idle time, preview)
- [ ] Grid layout (responsive columns)
- [ ] Keyboard navigation (j/k, arrows)
- [ ] Auto-refresh via tick events

### M4: Agent Interaction
- [ ] Attach via popup (`tmux display-popup`)
- [ ] Spawn new agent
- [ ] Kill agent (with confirmation modal)
- [ ] Filter/search agents

### M5: Polish
- [ ] Error handling (tmux not running, no sessions)
- [ ] Graceful shutdown (cleanup terminal state)
- [ ] CLI help and documentation
- [ ] Release builds and installation

## Key Implementation Details

### tmux Client

```rust
// src/tmux/client.rs
use std::process::Command;
use anyhow::{Result, Context};

pub struct Session {
    pub name: String,
    pub activity: i64,
    pub attached: bool,
    pub pane_pid: u32,
}

pub struct TmuxClient {
    prefix: String,
}

impl TmuxClient {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self { prefix: prefix.into() }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("tmux")
            .args(args)
            .output()
            .context("Failed to execute tmux")?;

        if !output.status.success() {
            anyhow::bail!("tmux error: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let output = self.run(&[
            "list-sessions", "-F",
            "#{session_name}|#{session_activity}|#{session_attached}|#{pane_pid}"
        ])?;

        let sessions = output
            .lines()
            .filter(|line| line.starts_with(&self.prefix))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() != 4 { return None; }
                Some(Session {
                    name: parts[0].to_string(),
                    activity: parts[1].parse().ok()?,
                    attached: parts[2] == "1",
                    pane_pid: parts[3].parse().ok()?,
                })
            })
            .collect();

        Ok(sessions)
    }

    pub fn capture_pane(&self, target: &str, lines: i32) -> Result<String> {
        self.run(&["capture-pane", "-t", target, "-p", "-S", &(-lines).to_string()])
    }

    pub fn attach_popup(&self, session: &str, width: &str, height: &str) -> Result<()> {
        Command::new("tmux")
            .args([
                "display-popup", "-E", "-w", width, "-h", height,
                &format!("tmux attach -t {}", session)
            ])
            .status()
            .context("Failed to open popup")?;
        Ok(())
    }

    pub fn new_session(&self, name: &str, command: &str, workdir: &str) -> Result<()> {
        self.run(&[
            "new-session", "-d", "-s", name, "-c", workdir, command
        ])?;
        Ok(())
    }

    pub fn kill_session(&self, name: &str) -> Result<()> {
        self.run(&["kill-session", "-t", name])?;
        Ok(())
    }
}
```

### Health Checker

```rust
// src/tmux/health.rs
use regex::Regex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    Ok,
    Idle,
    Stuck,
}

pub struct HealthChecker {
    client: TmuxClient,
    idle_warning: i64,
    idle_critical: i64,
    error_pattern: Regex,
}

impl HealthChecker {
    pub fn new(
        client: TmuxClient,
        idle_warning: i64,
        idle_critical: i64,
        error_patterns: &[&str],
    ) -> Self {
        let pattern = error_patterns.join("|");
        Self {
            client,
            idle_warning,
            idle_critical,
            error_pattern: Regex::new(&format!("(?i){}", pattern)).unwrap(),
        }
    }

    pub fn check(&self, session: &Session) -> HealthState {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let idle = now - session.activity;

        if idle > self.idle_critical {
            return HealthState::Stuck;
        }

        // Check for error patterns
        if let Ok(output) = self.client.capture_pane(&session.name, 20) {
            if self.error_pattern.is_match(&output) {
                return HealthState::Stuck;
            }
        }

        if idle > self.idle_warning {
            return HealthState::Idle;
        }

        HealthState::Ok
    }
}
```

### Application State

```rust
// src/app.rs
use crate::tmux::{TmuxClient, Session, HealthChecker, HealthState};

pub struct AgentInfo {
    pub session: Session,
    pub health: HealthState,
    pub last_output: String,
}

pub struct App {
    pub agents: Vec<AgentInfo>,
    pub selected: usize,
    pub should_quit: bool,
    pub show_confirm_kill: bool,
    client: TmuxClient,
    health_checker: HealthChecker,
}

impl App {
    pub fn new(client: TmuxClient, health_checker: HealthChecker) -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            should_quit: false,
            show_confirm_kill: false,
            client,
            health_checker,
        }
    }

    pub fn refresh(&mut self) -> anyhow::Result<()> {
        let sessions = self.client.list_sessions()?;
        self.agents = sessions
            .into_iter()
            .map(|session| {
                let health = self.health_checker.check(&session);
                let last_output = self.client
                    .capture_pane(&session.name, 1)
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(50)
                    .collect();
                AgentInfo { session, health, last_output }
            })
            .collect();

        // Keep selection in bounds
        if self.selected >= self.agents.len() && !self.agents.is_empty() {
            self.selected = self.agents.len() - 1;
        }
        Ok(())
    }

    pub fn next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.agents.is_empty() {
            self.selected = self.selected.checked_sub(1).unwrap_or(self.agents.len() - 1);
        }
    }

    pub fn attach_selected(&self) -> anyhow::Result<()> {
        if let Some(agent) = self.agents.get(self.selected) {
            self.client.attach_popup(&agent.session.name, "80%", "80%")?;
        }
        Ok(())
    }

    pub fn kill_selected(&mut self) -> anyhow::Result<()> {
        if let Some(agent) = self.agents.get(self.selected) {
            self.client.kill_session(&agent.session.name)?;
            self.refresh()?;
        }
        Ok(())
    }
}
```

### UI Rendering

```rust
// src/ui/dashboard.rs
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Status bar
            Constraint::Min(0),     // Agent grid
            Constraint::Length(1),  // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_agent_grid(frame, app, chunks[1]);
    render_help_bar(frame, chunks[2]);
}

fn render_agent_grid(frame: &mut Frame, app: &App, area: Rect) {
    let cols = 3.min(app.agents.len().max(1));
    let rows = (app.agents.len() + cols - 1) / cols;

    let row_constraints: Vec<_> = (0..rows)
        .map(|_| Constraint::Length(6))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (i, agent) in app.agents.iter().enumerate() {
        let row = i / cols;
        let col = i % cols;

        let col_constraints: Vec<_> = (0..cols)
            .map(|_| Constraint::Ratio(1, cols as u32))
            .collect();

        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_chunks[row]);

        let is_selected = i == app.selected;
        render_agent_card(frame, agent, col_chunks[col], is_selected);
    }
}

fn render_agent_card(frame: &mut Frame, agent: &AgentInfo, area: Rect, selected: bool) {
    let (border_color, status_icon) = match agent.health {
        HealthState::Ok => (Color::Green, "🟢"),
        HealthState::Idle => (Color::Yellow, "🟡"),
        HealthState::Stuck => (Color::Red, "🔴"),
    };

    let border_style = if selected {
        Style::default().fg(border_color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(border_color)
    };

    let block = Block::default()
        .title(format!(" {} ", agent.session.name))
        .borders(Borders::ALL)
        .border_style(border_style);

    let idle_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64 - agent.session.activity;

    let content = format!(
        "{} {:?}\nIdle: {}s\n{}",
        status_icon,
        agent.health,
        idle_secs,
        agent.last_output
    );

    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}
```

### Event Loop

```rust
// src/event.rs
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use std::time::Duration;
use tokio::sync::mpsc;

pub enum AppEvent {
    Key(KeyEvent),
    Tick,
}

pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut tick_interval = tokio::time::interval(tick_rate);
            loop {
                let event = tokio::select! {
                    _ = tick_interval.tick() => AppEvent::Tick,
                    Ok(true) = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(100))) => {
                        if let Ok(Event::Key(key)) = event::read() {
                            AppEvent::Key(key)
                        } else {
                            continue;
                        }
                    }
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });

        Self { rx }
    }

    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }
}
```

### Main Entry

```rust
// src/main.rs
use clap::{Parser, Subcommand};
use anyhow::Result;

#[derive(Parser)]
#[command(name = "omar", about = "Agent dashboard for tmux")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, default_value = "~/.config/omar/config.toml")]
    config: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn a new agent
    Spawn {
        #[arg(short, long)]
        name: String,
        #[arg(short, long, default_value = "claude")]
        command: String,
    },
    /// List agents (non-TUI)
    List,
    /// Kill an agent
    Kill { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Spawn { name, command }) => spawn_agent(&name, &command),
        Some(Commands::List) => list_agents(),
        Some(Commands::Kill { name }) => kill_agent(&name),
        None => run_dashboard().await,
    }
}

async fn run_dashboard() -> Result<()> {
    // Initialize terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // Create app
    let client = TmuxClient::new("omar-agent-");
    let health_checker = HealthChecker::new(client.clone(), 60, 300, &["error", "failed"]);
    let mut app = App::new(client, health_checker);

    // Event loop
    let mut events = EventHandler::new(Duration::from_secs(2));
    app.refresh()?;

    loop {
        terminal.draw(|f| ui::dashboard::render(f, &app))?;

        match events.next().await {
            Some(AppEvent::Key(key)) => match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') | KeyCode::Down => app.next(),
                KeyCode::Char('k') | KeyCode::Up => app.previous(),
                KeyCode::Enter => {
                    // Temporarily exit raw mode for popup
                    crossterm::terminal::disable_raw_mode()?;
                    app.attach_selected()?;
                    crossterm::terminal::enable_raw_mode()?;
                }
                KeyCode::Char('r') => app.refresh()?,
                _ => {}
            },
            Some(AppEvent::Tick) => app.refresh()?,
            None => break,
        }
    }

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
}
```

## CLI Interface

```bash
# Start dashboard
omar

# Start with custom config
omar --config ~/.config/omar/config.toml

# Spawn a new agent
omar spawn --name my-agent --command "claude"

# List agents (non-TUI)
omar list

# Kill an agent
omar kill agent-1
```

## Dependencies

```toml
# Cargo.toml
[package]
name = "omar"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
regex = "1"
dirs = "5"

[dev-dependencies]
pretty_assertions = "1"
```

## Testing Strategy

1. **Unit tests**: tmux client (mock Command output), health checker
2. **Integration tests**: Real tmux in CI (`tmux new-session -d`)
3. **Manual testing**: Dogfood with actual Claude agents

## Open Questions

1. **Agent spawning**: Support templates for different agent types?
2. **Persistence**: Store agent configs to restore after reboot?
3. **Logging**: Capture agent output to files for debugging?
4. **Notifications**: Alert when agent gets stuck (desktop notification)?
