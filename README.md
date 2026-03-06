<div align="center">

# one-man army

<img src="docs/img/thermopylae.png" alt="thermopylae" width="450" />

Lead an army of 300 agents to solve humanity's biggest problems.

**`omar` is a TUI for creating powerful agentic organizations.**

<a href="https://opensource.org/licenses/BSD-2-Clause"><img src="https://img.shields.io/badge/License-BSD_2--Clause-blue.svg" alt="License"/></a>&nbsp;<a href="https://github.com/lsk567/omar/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/lsk567/omar/ci.yml?label=CI&logo=github" alt="CI Status"/></a>&nbsp;<a href="https://discord.gg/X76PSzmfWr"><img src="https://img.shields.io/discord/1467663881588572182?label=Discord&logo=discord&logoColor=white&color=5865F2&cacheSeconds=60" alt="Discord"/></a>

</div>

## Features

- Professional TUI dashboard for all your agents in one place
- Deep hierarchy of parallel agents, just like a company
- Talk to any agent - you are in control
- Messaging systems integration (e.g., Slack, etc.)
- Computer use (Linux)
- Highly customizable, supporting all `tmux` commands

## Installation

#### Install Dependencies

- tmux 3.0+
- Rust 1.70+
- GNU Make
- [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) or [Opencode](https://github.com/anomalyco/opencode)

#### Build from source

```bash
make install
```

## Quick Start

#### Step 1: Launch `omar`

```bash
$ omar
```

https://github.com/user-attachments/assets/b720eb41-1d97-4331-9c2c-10a0e4580286

Go [here](#supported-agent-backends) to see how to launch with other agent backends.

#### Step 2: Tell your executive assistent (EA) to run a test prompt.

Copy the following into your EA window:
```
Load and run <omar-root>/prompts/tests/project-factory.md
```

https://github.com/user-attachments/assets/3dfe5bd3-9b9f-474c-a036-a1058413935d

You should see agents being spawned by the EA.

Tip: Use ↑↓ to cycle through agents at the current level. Use → to drill into a deeper level. Use ← to back out.

https://github.com/user-attachments/assets/dc94edb4-24ea-4e7e-aa8c-f0bc31d09d3f

#### Step 3: Shutdown the project.

Go back to the EA and type in:
```
Shutdown the test project and its agents. Delete <omar-root>/junk/ folder.
```

https://github.com/user-attachments/assets/94b9a78f-5eb2-4557-9932-f17fed536ba5

## Supported Agent Backends

Omar auto-detects which agent backend is available on your system:

| Backend | How to launch |
|---------|---------------|
| [Claude Code](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview) | `omar` (default) |
| [Opencode](https://github.com/anomalyco/opencode) | `omar --agent opencode` |

## License

BSD 2-Clause

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=lsk567/omar&type=date&legend=top-left)](https://www.star-history.com/#lsk567/omar&type=date&legend=top-left)
