# OMAR HTTP API Design

## Overview

OMAR exposes a local HTTP API that allows any agent (Claude, opencode, custom tools) to orchestrate worker agents. This enables:
- Agent-agnostic orchestration
- Distributed deployment (agents on different machines)
- Easy integration with any tool that can make HTTP calls

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         OMAR Dashboard                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │   Agent 1   │  │   Agent 2   │  │   Agent 3   │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                      MANAGER                                 ││
│  └─────────────────────────────────────────────────────────────┘│
└──────────────────────────┬──────────────────────────────────────┘
                           │
                    ┌──────▼──────┐
                    │  HTTP API   │
                    │ :9876       │
                    └──────┬──────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
   ┌─────────┐       ┌─────────┐       ┌─────────┐
   │ Claude  │       │opencode │       │ Custom  │
   │ Manager │       │  Agent  │       │  Tool   │
   └─────────┘       └─────────┘       └─────────┘
```

## API Endpoints

### Agent Management

#### `POST /api/agents`
Spawn a new worker agent.

```json
Request:
{
  "name": "worker-1",           // Optional, auto-generated if not provided
  "task": "Implement feature X", // Task description for the agent
  "workdir": "/path/to/project", // Optional, defaults to OMAR's cwd
  "command": "claude",           // Optional, defaults to config
  "depends_on": ["worker-0"]     // Optional, wait for these agents
}

Response:
{
  "id": "worker-1",
  "status": "running",
  "session": "worker-1",
  "created_at": "2025-01-26T12:00:00Z"
}
```

#### `GET /api/agents`
List all agents.

```json
Response:
{
  "agents": [
    {
      "id": "worker-1",
      "status": "running",
      "health": "working",
      "idle_seconds": 5,
      "last_output": "Writing tests..."
    },
    {
      "id": "worker-2",
      "status": "running",
      "health": "waiting",
      "idle_seconds": 30,
      "last_output": "> "
    }
  ],
  "manager": {
    "id": "omar-manager",
    "status": "running",
    "health": "working"
  }
}
```

#### `GET /api/agents/{id}`
Get details for a specific agent.

```json
Response:
{
  "id": "worker-1",
  "status": "running",
  "health": "working",
  "idle_seconds": 5,
  "last_output": "Writing tests...",
  "created_at": "2025-01-26T12:00:00Z",
  "task": "Implement feature X",
  "output_tail": "... last 50 lines of output ..."
}
```

#### `DELETE /api/agents/{id}`
Kill an agent.

```json
Response:
{
  "id": "worker-1",
  "status": "killed"
}
```

#### `POST /api/agents/{id}/send`
Send input to an agent.

```json
Request:
{
  "text": "Yes, proceed with the implementation",
  "enter": true  // Whether to send Enter key after
}

Response:
{
  "status": "sent"
}
```

### Plan Management

#### `POST /api/plans`
Submit a plan for user approval.

```json
Request:
{
  "description": "Implement user authentication",
  "agents": [
    {
      "name": "auth-backend",
      "role": "Backend Developer",
      "task": "Implement JWT authentication in the API"
    },
    {
      "name": "auth-frontend",
      "role": "Frontend Developer",
      "task": "Add login/logout UI components",
      "depends_on": ["auth-backend"]
    }
  ]
}

Response:
{
  "plan_id": "plan-123",
  "status": "pending_approval"
}
```

#### `GET /api/plans/{id}`
Check plan status.

```json
Response:
{
  "plan_id": "plan-123",
  "status": "approved",  // pending_approval, approved, rejected
  "agents": [...]
}
```

### System

#### `GET /api/health`
Health check endpoint.

```json
Response:
{
  "status": "ok",
  "version": "0.1.0",
  "uptime_seconds": 3600
}
```

#### `GET /api/config`
Get current configuration.

```json
Response:
{
  "default_command": "claude --dangerously-skip-permissions",
  "workdir": "/home/user/project",
  "refresh_interval": 2
}
```

## Configuration

Add to `~/.config/omar/config.toml`:

```toml
[api]
enabled = true
port = 9876
host = "127.0.0.1"  # Use "0.0.0.0" for remote access
# auth_token = "secret"  # Optional, for remote deployments
```

## Usage Examples

### From Claude/Manager Agent

```bash
# Spawn a worker
curl -X POST http://localhost:9876/api/agents \
  -H "Content-Type: application/json" \
  -d '{"name": "worker-1", "task": "Write unit tests for auth module"}'

# Check agent status
curl http://localhost:9876/api/agents/worker-1

# Send input to agent
curl -X POST http://localhost:9876/api/agents/worker-1/send \
  -d '{"text": "y", "enter": true}'

# Kill agent when done
curl -X DELETE http://localhost:9876/api/agents/worker-1
```

### From Python Script

```python
import requests

# Spawn workers in parallel
for i in range(3):
    requests.post("http://localhost:9876/api/agents", json={
        "name": f"worker-{i}",
        "task": f"Implement feature {i}"
    })

# Wait for completion
while True:
    agents = requests.get("http://localhost:9876/api/agents").json()
    if all(a["health"] == "waiting" for a in agents["agents"]):
        break
    time.sleep(5)
```

## Security Considerations

1. **Local only by default**: Bind to 127.0.0.1
2. **Optional auth token**: For remote deployments
3. **No shell injection**: Validate all inputs
4. **Rate limiting**: Prevent abuse

## Implementation Plan

1. Add `axum` or `warp` HTTP server to OMAR
2. Run server in background tokio task
3. Share app state via `Arc<Mutex<App>>`
4. Add API endpoints incrementally
5. Update manager to use HTTP API instead of direct tmux

## Future Extensions

- WebSocket for real-time updates
- Agent output streaming
- Remote OMAR instances (distributed workers)
- Authentication for multi-user setups
