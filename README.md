<div align="center">

# one-man army

<img src="docs/img/thermopylae.png" alt="thermopylae" width="450" />

Lead an army of 300 agents to solve humanity's biggest problems.

**`omar` is a TUI for creating powerful agentic organizations.**

<p align="center">
  <a href="https://omarmy.ai">omarmy.ai</a>&nbsp; • &nbsp;
  <a href="https://omarmy.ai/zh/">中文</a>&nbsp; • &nbsp;
  <a href="https://opensource.org/licenses/BSD-2-Clause"><img src="https://img.shields.io/badge/License-BSD_2--Clause-blue.svg" alt="License" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://github.com/lsk567/omar/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/lsk567/omar/ci.yml?label=CI&logo=github" alt="CI Status" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://discord.gg/X76PSzmfWr"><img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord" valign="middle"/></a>
</p>

</div>

## Features

- **Deep hierarchies**: Agents managing agents, just like a company.
- **Heterogeneity**: Let `claude`, `codex`, and more collaborate as a team.
- **Full control**: Talk to and control any subagent you want.
- **Life span**: Long-running or ephemeral agents, your choice.
- **Customization**: Support all `tmux` commands you love.

Other features include messaging systems integration (e.g., Slack), computer use, and more.

## Installation

### Prerequisites

- **tmux 3.0+** — `brew install tmux` (macOS) or `apt install tmux` (Debian/Ubuntu)
- At least one agent backend: [Claude](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview), [Codex](https://developers.openai.com/codex/cli), [Opencode](https://github.com/anomalyco/opencode), or [Cursor](https://cursor.com/cli).

### One-liner (recommended)

```bash
curl -fsSL https://omarmy.ai/install.sh | sh
```

Installs all binaries to `/usr/local/bin`.

### Homebrew

```bash
brew install lsk567/omar/omar
```

### Build from source

Requires Rust 1.70+ and GNU Make.

```bash
git clone https://github.com/lsk567/omar.git
cd omar && make install
```

## Quick Start

#### Step 1: Launch `omar`

```bash
$ omar
```

https://github.com/user-attachments/assets/b720eb41-1d97-4331-9c2c-10a0e4580286

Go [here](#supported-agent-backends) to see how to launch with other agent backends.

#### Step 2: Tell your Executive Assistant (EA) to run a test prompt.

Copy the following into your EA window:
```
Load and run https://github.com/lsk567/omar/blob/main/prompts/tests/project-factory.md
```

https://github.com/user-attachments/assets/3dfe5bd3-9b9f-474c-a036-a1058413935d

You should see agents being spawned by the EA.

Tip: Use ↑↓ to cycle through agents at the current level. Use → to drill into a deeper level. Use ← to back out.

https://github.com/user-attachments/assets/dc94edb4-24ea-4e7e-aa8c-f0bc31d09d3f

#### Step 3: Shutdown the project.

Go back to the EA and type in:
```
Shutdown the test project and its agents.
```

https://github.com/user-attachments/assets/94b9a78f-5eb2-4557-9932-f17fed536ba5

## Supported Agent Backends

| Backend | How to launch |
|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `omar` or `omar --agent claude` |
| [Codex CLI](https://developers.openai.com/codex/cli) | `omar --agent codex` |
| [Opencode](https://github.com/anomalyco/opencode) | `omar --agent opencode` |
| [Cursor CLI](https://cursor.com/cli) | `omar --agent cursor` |

## License

BSD 2-Clause

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=lsk567/omar&type=date&legend=top-left)](https://www.star-history.com/#lsk567/omar&type=date&legend=top-left)
