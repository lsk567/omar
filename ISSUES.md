# Known Issues

## Open

### 1. Health detection incorrect on agent startup
**Status:** Open
**Severity:** Low
**Date:** 2025-01-25

**Description:**
When an agent first starts up, the health detector shows "Working" (green) even though the agent is actually waiting for user input or idle. This happens because:

1. The `session_activity` timestamp is recent (agent just started)
2. The health checker prioritizes recent activity over output patterns
3. The initial Claude prompt hasn't been captured yet

**Current behavior:**
New agent → Shows "Working" for ~60 seconds → Then correctly shows "Waiting" or "Idle"

**Expected behavior:**
New agent → Should immediately detect the Claude prompt (`>`) and show "WaitingForInput"

**Root cause:**
In `health.rs`, the check logic prioritizes idle time over pattern matching:
```rust
// Recent activity means working
if idle < self.idle_warning {
    return HealthState::Working;
}
```

This runs before the waiting pattern check has a chance to trigger for new sessions.

**Potential fix:**
Reorder the checks to prioritize `WaitingForInput` detection regardless of idle time:
1. Check for errors first (stuck)
2. Check for waiting patterns (waiting for input)
3. Check for working patterns (actively working)
4. Fall back to idle time-based detection

---

### 3. Agent status reporting incorrect
**Status:** Open
**Severity:** Medium
**Date:** 2026-01-27

**Description:**
The agent health status displayed in the dashboard does not accurately reflect the actual state of agents. The status indicators (Working/Waiting/Idle/Stuck) frequently show incorrect states.

**Current behavior:**
- Agents may show "Working" when they are actually idle or waiting for input
- Status updates may lag behind actual agent state
- Pattern matching for detecting agent state is unreliable

**Expected behavior:**
- Status should accurately reflect whether an agent is actively processing, waiting for input, idle, or stuck
- Status should update promptly when agent state changes

**Related:** See issue #1 for startup-specific health detection problems.

---

## Resolved

### 2. Pressing 'd' kills user's main tmux session
**Status:** Fixed (3e0b0b3)
**Severity:** Critical
**Date:** 2025-01-26

**Description:**
After removing the session prefix for direct tmux integration, `oma list` showed ALL tmux sessions including the user's attached terminal. If the user selected their main session and pressed 'd' to kill, it would exit their entire tmux environment.

**Fix:**
- Filter out attached sessions from the agent list
- Add safety check to block killing attached sessions
- Block killing manager with 'd' key
