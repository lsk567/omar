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
- Visualize agents' chain of command
- Health status tracking
- Support all the familiar `tmux` commands you love!

<img src="docs/img/demo3.png" alt="demo"/>
☝️ See all your agents at once. Nagivate using arrow keys.

<img src="docs/img/demo4.png" alt="demo"/>
☝️ Talk to any agent in a pop-up window.

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

## Supported Agent Backends

Omar auto-detects which agent backend is available on your system:

| Backend | Command | Auto-detected |
|---------|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `claude --dangerously-skip-permissions` | Yes (first priority) |
| [Opencode](https://github.com/nichochar/opencode) | `opencode` | Yes (second priority) |
| Custom | Any command | Via config |

If both are installed, `claude` takes priority. Override with the `default_command` config option.

## License

BSD 2-Clause

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=lsk567/omar&type=date&legend=top-left)](https://www.star-history.com/#lsk567/omar&type=date&legend=top-left)
