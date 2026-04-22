# Flat 100-Agent Stress Test

**Purpose:** Validate OMAR can handle 100 concurrent agents spawned in parallel. Tests spawn throughput, concurrent execution, and cleanup.

## Setup

EA registers one project (via `add_project`) and spawns 100 agents named `exp-001` through `exp-100` in rapid succession via the `spawn_agent` MCP tool, all attached to that shared `project_id`.

## Agent Task Template

Each agent receives:

```
You are agent #N in a 100-agent experiment. Acknowledge your agent number, then output [TASK COMPLETE] and wake the EA with omar_wake_later({"receiver": "ea", "payload": "[CHILD COMPLETE] exp-N: Agent #N complete.", "delay_seconds": 0}).
```

## How to Run

EA first registers the project once, then issues 100 `spawn_agent` calls, one per agent:

```
add_project({"name": "flat-100-agents"})
# → {"project_id": 1, ...}

spawn_agent({
  "name": "exp-001",
  "project_id": 1,
  "task": "You are agent #1 in a 100-agent experiment. Acknowledge your agent number, then output [TASK COMPLETE] and wake the EA with omar_wake_later({\"receiver\": \"ea\", \"payload\": \"[CHILD COMPLETE] exp-001: Agent #1 complete.\", \"delay_seconds\": 0}).",
  "parent": "ea"
})
```

Repeat for `exp-002` through `exp-100`, incrementing both the `name` suffix and the `#N` number in the task text. The EA can fan these out without waiting between calls.

## Monitoring

Each agent sends a `omar_wake_later` to the EA on completion (delay_seconds: 0) with payload `[CHILD COMPLETE] exp-NNN: ...`, which the EA receives as a wake-up. The EA counts incoming completions — no polling needed. Use `list_agents` + `get_agent` only to investigate stragglers that haven't reported in after a reasonable timeout.

## Cleanup

Once all 100 agents report `[TASK COMPLETE]`, call `kill_agent` on each one:

```
kill_agent({"name": "exp-001"})
```

...through `exp-100`.

## Expected Behavior

1. All 100 agents spawn successfully.
2. Each agent acknowledges its number and outputs `[TASK COMPLETE]`.
3. EA detects completion from all 100.
4. EA kills all 100, leaving a clean dashboard.

## Success Criteria

- All 100 agents spawn without error.
- All 100 report `[TASK COMPLETE]`.
- All 100 are killed and cleaned up.
- No stragglers on the dashboard.

## Previous Result

- **Duration:** ~18 seconds total (spawn to full cleanup).
- **Result:** All 100 completed and cleaned up successfully.
