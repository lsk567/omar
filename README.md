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

## Supported Agent Backends

Omar auto-detects which agent backend is available on your system:

| Backend | Command | Auto-detected |
|---------|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `claude --dangerously-skip-permissions` | Yes (first priority) |
| [Opencode](https://github.com/nichochar/opencode) | `opencode` | Yes (second priority) |
| Custom | Any command | Via config |

If both are installed, `claude` takes priority. Override with the `default_command` config option.

## Configuration

Create `~/.config/omar/config.toml`:

```toml
[dashboard]
refresh_interval = 2
session_prefix = "omar-agent-"

[health]
idle_warning = 15

[agent]
default_command = "claude --dangerously-skip-permissions"  # or "opencode", or any command
default_workdir = "."
```

## Requirements

- tmux 3.0+
- Rust 1.70+
- At least one agent backend (claude, opencode, or custom)

## License

MIT
