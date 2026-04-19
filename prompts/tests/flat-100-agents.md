# Flat 100-Agent Stress Test

> Legacy note: this stress test still documents the removed HTTP API. Re-run it through OMAR MCP tools such as `spawn_agent_session`, `get_agent`, `list_agents`, and `kill_agent` instead of `curl http://localhost:9876/...`.

**Purpose:** Validate OMAR can handle 100 concurrent agents spawned in parallel. Tests spawn throughput, concurrent execution, and cleanup.

## Setup

EA spawns 100 agents named `exp-001` through `exp-100` in rapid succession using the OMAR API.

## Agent Task Template

Each agent receives:

```
You are agent #N in a 100-agent experiment. Report [TASK COMPLETE] immediately after acknowledging your agent number.
```

## How to Run

EA issues 100 POST requests to spawn agents:

```bash
# Replace {{EA_ID}} with the target EA id (e.g. 0 for the default EA).
for i in $(seq -w 1 100); do
  curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
    -H "Content-Type: application/json" \
    -d "{\"name\": \"exp-$i\", \"task\": \"You are agent #$i in a 100-agent experiment. Report [TASK COMPLETE] immediately after acknowledging your agent number.\", \"parent\": \"ea\"}"
done
```

Then monitor all agents, polling for `[TASK COMPLETE]` in each agent's output:

```bash
# Check a single agent
curl http://localhost:9876/api/ea/{{EA_ID}}/agents/exp-001

# List all agents
curl http://localhost:9876/api/ea/{{EA_ID}}/agents
```

Once all 100 report `[TASK COMPLETE]`, kill them all:

```bash
for i in $(seq -w 1 100); do
  curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/exp-$i
done
```

## Expected Behavior

1. All 100 agents spawn successfully
2. Each agent acknowledges its number and outputs `[TASK COMPLETE]`
3. EA detects completion from all 100
4. EA kills all 100 agents, leaving a clean dashboard

## Success Criteria

- All 100 agents spawn without error
- All 100 agents report `[TASK COMPLETE]`
- All 100 agents are killed and cleaned up
- No stragglers remain on the dashboard

## Previous Result

- **Duration:** ~18 seconds total (spawn to full cleanup)
- **Result:** All 100 completed and cleaned up successfully
