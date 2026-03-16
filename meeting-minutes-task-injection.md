# Meeting Minutes: Task Injection Reliability Bug

**Date:** 2026-03-15
**Attendees:** Claude (claude backend, lead), inject-opencode (opencode backend, reviewer)

## Problem Statement

When spawning agents via the API (`POST /api/agents` with a `task` field), the task
sometimes fails to be injected into the agent session. The agent starts at the prompt
but never receives its task, sitting idle. Observed with `claude` and `cursor` backends.
Persistent agents that receive tasks via the `send` endpoint work fine.

## Root Cause Analysis

**File:** `src/api/handlers.rs`, lines 407-414 (spawn_agent handler)

The task injection used a **fixed 2-second `tokio::time::sleep`** before sending keys
to the newly created tmux session:

```rust
tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let _ = client.send_keys_literal(&session, &user_msg);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let _ = client.send_keys(&session, "Enter");
});
```

**Three issues identified:**

1. **Race condition (primary):** 2 seconds is not enough for some backends. `claude`
   can take 3-5 seconds to initialize; `cursor` even longer. If the backend hasn't
   rendered its prompt, `send_keys_literal` fires into a terminal that isn't ready —
   the keys are silently lost.

2. **Silent error suppression:** `let _ = client.send_keys_literal(...)` discards
   errors. If the tmux send fails (session gone, buffer issue), nobody is notified.

3. **No retry mechanism:** A single failed send permanently loses the task. The
   `send` endpoint works reliably because agents are already running when it's called.

## Why the `send` endpoint works

`POST /api/agents/:id/send` sends to an already-running agent. No startup race.
The task injection bug is specific to the spawn-time `task` field because it must
coordinate with backend startup timing.

## Fix Implemented

**Approach:** Replace fixed delay with **readiness polling + retries**.

### 1. `wait_for_pane_ready()` — new async helper

Polls `capture_pane` until the tmux pane has non-whitespace content, meaning the
backend has started and rendered its UI/prompt. Uses exponential backoff
(250ms → 500ms → 1s → 2s cap) with a 30-second timeout.

```rust
async fn wait_for_pane_ready(client, session, timeout_secs) -> bool
```

### 2. Retry loop for `send_keys_literal`

After readiness is confirmed, sends the task text with up to 3 retry attempts
(1-second delay between retries) to handle transient tmux errors.

### 3. Error reporting

Replaced `let _ = ...` with `eprintln!` warnings/errors so failures are visible
in logs instead of silently dropped.

### 4. Integration test

Added `test_task_injection_waits_for_readiness` that simulates a slow-starting
backend (`sleep 2 && exec bash`) and verifies the polling pattern works.

## Test Results

- 75 unit tests: all pass
- New integration test: passes
- 2 pre-existing flaky integration tests (`test_capture_pane`, `test_send_keys`):
  fail intermittently due to test session cleanup races (unrelated to this change)

## Files Changed

- `src/api/handlers.rs` — readiness polling + retry logic in `spawn_agent`, new `wait_for_pane_ready` fn
- `tests/integration_test.rs` — new `test_task_injection_waits_for_readiness` test
- `meeting-minutes-task-injection.md` — this document

## Discussion with inject-opencode

- Spawned `inject-opencode` agent via OMAR API with opencode backend to review the fix
- Event sent requesting review of the diff

## Decision

Ship the fix as a PR. The polling approach is robust across all backends because it
adapts to actual startup time rather than guessing with a fixed delay.
