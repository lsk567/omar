# One-Man Army

Be a one-man army with non-stop agents tackling the biggest problems.

A TUI dashboard for managing multiple AI coding agents based on `tmux`.

<img src="docs/img/thermopylae.png" alt="thermopylae" width="450" />

## Features

- Real-time monitoring of agent sessions
- Health status tracking (Running, Idle)
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

# Start or attach to the manager agent
omar manager
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `q` / `Esc` | Quit |
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `Enter` | Attach to agent |
| `i` | Interactive mode (type directly to agent) |
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
idle_warning = 15

[agent]
default_command = "claude"
default_workdir = "."
```

## Requirements

- tmux 3.0+
- Rust 1.70+

## License

MIT
