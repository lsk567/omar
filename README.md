# OMAR

Make everyone a one-man army by commanding agents non-stop.

A TUI dashboard for monitoring and managing multiple AI coding agents running in tmux sessions.

## Features

- Real-time monitoring of agent sessions
- Health status tracking (OK, Idle, Stuck)
- Error pattern detection in agent output
- Quick attach/detach via tmux popups
- Session management (spawn, list, kill)

## Installation

```bash
cargo install --path .
```

## Usage

### Dashboard Mode

```bash
omar
```

### CLI Commands

```bash
# Spawn a new agent
omar spawn -n my-agent -c "claude"

# List all agents
omar list

# Kill an agent
omar kill my-agent
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `q` / `Esc` | Quit |
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `Enter` | Attach to agent |
| `n` | Spawn new agent |
| `d` | Kill agent |
| `r` | Refresh |
| `?` | Help |

## Configuration

Create `~/.config/omar/config.toml`:

```toml
[dashboard]
refresh_interval = 2
session_prefix = "omar-agent-"

[health]
idle_warning = 60
idle_critical = 300
error_patterns = ["error", "failed", "rate limit"]

[agent]
default_command = "claude"
default_workdir = "."
```

## Requirements

- tmux 3.0+
- Rust 1.70+

## License

MIT
