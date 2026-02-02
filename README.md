<div align="center">

# one-man army

<img src="docs/img/thermopylae.png" alt="thermopylae" width="450" />

Be a one-man army with non-stop agents tackling the biggest problems.

**`omar` is a TUI dashboard for managing AI agents based on `tmux`.**

<p>
  <a href="https://opensource.org/licenses/BSD-2-Clause">
    <img src="https://img.shields.io/badge/License-BSD_2--Clause-blue.svg" alt="License"/>
  </a>
  <a href="https://github.com/lsk567/omar/actions/workflows/ci.yml">
    <img src="https://github.com/lsk567/omar/actions/workflows/ci.yml/badge.svg" alt="CI Status"/>
  </a>
  <a href="https://discord.gg/X76PSzmfWr">
    <img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord"/>
  </a>
</p>

</div>

## Features

- A TUI dashboard for all your agents in one place
- Spawn workers in parallel
- An executive assistant that manages agents for you
- Visualize a chain of command
- Health status tracking
- Easily talk to each agent via tmux popups
- Support all the familiar `tmux` commands you love

<img src="docs/img/demo2.png" alt="demo"/>

## Requirements

- tmux 3.0+
- Rust 1.70+
- At least one agent backend (claude, opencode, or custom)

## Installation

```bash
cargo install --path .
```

## Usage

### Dashboard Mode

```bash
omar
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

## License

MIT

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=lsk567/omar&type=date&legend=top-left)](https://www.star-history.com/#lsk567/omar&type=date&legend=top-left)
