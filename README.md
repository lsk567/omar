<div align="center">

# Open Multi-Agent Runtime

**`omar` is a TUI for creating powerful agentic organizations.**

Lead a team of 100 agents to solve humanity's biggest problems.


<p align="center">
  <a href="https://omar.tech">omar.tech</a>&nbsp; • &nbsp;
  <a href="https://omar.tech/zh/">中文</a>&nbsp; • &nbsp;
  <a href="https://opensource.org/licenses/BSD-3-Clause"><img src="https://img.shields.io/badge/License-BSD_3--Clause-blue.svg" alt="License" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://github.com/lsk567/omar/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/lsk567/omar/ci.yml?label=CI&logo=github" alt="CI Status" valign="middle"/></a>&nbsp; • &nbsp;
  <a href="https://discord.gg/X76PSzmfWr"><img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord" valign="middle"/></a>
</p>

</div>

![Demo](img/demo.gif)

## Features

- **Deep hierarchies**: Agents managing agents, just like a company.
- **Heterogeneity**: Let `claude`, `codex`, and more collaborate as a team.
- **Full control**: Talk to and control any subagent you want.
- **Life span**: Long-running or ephemeral agents, your choice.
- **Customization**: Support all `tmux` commands you love.

Other features include messaging systems integration (e.g., Slack), computer use, and more.

## Installation

### Prerequisites

- tmux 3.0+
- Rust 1.70+
- GNU Make
- One or more coding agents: [Claude](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview), [Codex](https://developers.openai.com/codex/cli), [Cursor](https://cursor.com/cli), [Gemini](https://geminicli.com), [Opencode](https://github.com/anomalyco/opencode)

### One-liner (recommended)

```bash
curl -fsSL https://omar.tech/install.sh | sh
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

Go [here](#supported-agent-backends) to see how to launch with specific agent backends.

#### Step 2: Tell your Executive Assistant (EA) to run a test prompt.

Copy the following into your EA window:
```
Run https://github.com/lsk567/omar/blob/main/prompts/tests/project-factory.md
```

You should see agents being spawned by the EA.

Tip: Use `↑↓←→` to cycle through agents at the current level. Use `Tab` to drill into a deeper level. Use `Shift+Tab` to back out.

#### Step 3: Shutdown the project.

Go back to the EA and type in:
```
Shutdown the test project and its agents.
```

## Supported Agent Backends

| Backend | How to launch |
|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `omar -a claude` (default) |
| [Codex CLI](https://developers.openai.com/codex/cli) | `omar -a codex` |
| [Cursor CLI](https://cursor.com/cli) | `omar -a cursor` |
| [Gemini CLI](https://geminicli.com) | `omar -a gemini` |
| [Opencode](https://github.com/anomalyco/opencode) | `omar -a opencode` |

## License

BSD 3-Clause

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=lsk567/omar&type=date&legend=top-left)](https://www.star-history.com/#lsk567/omar&type=date&legend=top-left)

---

OMAR is made with ❤️ in Berkeley, CA.