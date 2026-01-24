# OMA: Agent Dashboard Design

## Overview

OMA is a TUI dashboard for monitoring and managing multiple AI coding agents. It runs inside tmux and leverages tmux's native features for session management.

## Architecture

```
┌─ tmux server ───────────────────────────────────────────────┐
│                                                             │
│  ┌─ oma-dashboard (session) ─────────────────────────────┐  │
│  │                                                       │  │
│  │  ┌─ TUI Dashboard ─────────────────────────────────┐  │  │
│  │  │ Agents: 3/5 active    CPU: 12%    Mem: 4.2GB    │  │  │
│  │  │ ┌─────────┐ ┌─────────┐ ┌─────────┐            │  │  │
│  │  │ │ agent-1 │ │ agent-2 │ │ agent-3 │            │  │  │
│  │  │ │ 🟢 OK   │ │ 🟡 IDLE │ │ 🔴 STUCK│            │  │  │
│  │  │ │ 2m ago  │ │ 5m ago  │ │ 15m ago │            │  │  │
│  │  │ └─────────┘ └─────────┘ └─────────┘            │  │  │
│  │  │                                                 │  │  │
│  │  │ [Enter: Attach] [n: New] [k: Kill] [q: Quit]   │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  │                                                       │  │
│  │     ┌─ tmux popup ──────────────────────┐            │  │
│  │     │ $ claude                          │            │  │
│  │     │ > Analyzing src/auth.py...        │            │  │
│  │     │ > Found 3 issues...               │            │  │
│  │     │                                   │            │  │
│  │     └───────────────────────────────────┘            │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌─ agent-1 (session) ───┐  ┌─ agent-2 (session) ───┐      │
│  │ claude working...     │  │ claude thinking...    │      │
│  └───────────────────────┘  └───────────────────────┘      │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Sessions
- Each agent runs in its own tmux session (`oma-agent-{id}`)
- Dashboard runs in `oma-dashboard` session
- Sessions are prefixed with `oma-` for easy filtering

### Health States
| State | Condition | Indicator |
|-------|-----------|-----------|
| OK | Activity within 60s | 🟢 |
| IDLE | No activity 1-5 min | 🟡 |
| STUCK | No activity >5 min or error pattern detected | 🔴 |

### Health Detection
1. **Idle time**: `#{pane_activity}` timestamp from tmux
2. **Error patterns**: Scan recent output for `error`, `failed`, `rate limit`
3. **Process state**: Check if pane PID is still alive

## Data Flow

```
┌─────────────┐     tmux list-sessions      ┌─────────────┐
│   tmux      │ ◄─────────────────────────► │  Dashboard  │
│   server    │     tmux capture-pane       │    TUI      │
│             │     tmux display-message    │             │
└─────────────┘                             └─────────────┘
       │                                           │
       │ manages                                   │ user input
       ▼                                           ▼
┌─────────────┐                             ┌─────────────┐
│   Agent     │ ◄── tmux send-keys ──────── │  Commands   │
│  Sessions   │ ◄── tmux display-popup ──── │  (attach,   │
│             │ ◄── tmux kill-session ───── │   kill,     │
└─────────────┘                             │   spawn)    │
                                            └─────────────┘
```

## Key Commands

| Key | Action |
|-----|--------|
| `j/k` or `↑/↓` | Navigate agents |
| `Enter` | Attach to agent (popup) |
| `n` | Spawn new agent |
| `k` | Kill selected agent |
| `r` | Refresh status |
| `q` | Quit dashboard |
| `/` | Filter agents |

## Configuration

```toml
# ~/.config/oma/config.toml

[dashboard]
refresh_interval = 2  # seconds
session_prefix = "oma-agent-"

[health]
idle_warning = 60     # seconds
idle_critical = 300   # seconds
error_patterns = ["error", "failed", "rate limit", "exception"]

[agent]
default_command = "claude"
default_workdir = "."
```

## Non-Goals (v1)
- Agent-to-agent communication
- Automatic agent spawning/scaling
- Persistent agent state across restarts
- Remote tmux server support
