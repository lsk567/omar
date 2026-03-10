# Edge Case Audit: EA Deletion Scenarios

> **Auditor**: w-edge (OMAR agent)
> **Date**: 2026-03-06
> **Branch**: feature/multi-ea
> **Spec**: ~/Documents/research/omar/docs/multi_ea_final_spec.md
>
> **Post-Merge Re-verification**: fm-edge (OMAR agent)
> **Date**: 2026-03-09
> **Status**: All original findings confirmed. 2 new merge-related issues found.

---

## Scenario Under Test

1. System starts fresh -> EA 0 exists (default)
2. User creates EA 1
3. User DELETES EA 0
4. What happens?

---

## Findings

### 1. Does `delete_ea` allow deleting EA 0? Is there a guard?

**PASS - Properly guarded at two levels.** *(Re-verified 2026-03-09)*

- **Level 1 (API handler)**: `src/api/handlers.rs:134-142` — The `delete_ea` handler checks `if ea_id == 0` and returns `403 Forbidden` with message "Cannot delete EA 0".
- **Level 2 (Core logic)**: `src/ea.rs:124-127` — `unregister_ea()` checks `if ea_id == 0` and returns `anyhow::bail!("Cannot delete EA 0")`.
- **Level 3 (Registry load)**: `src/ea.rs:49-67` — `load_registry()` always ensures EA 0 exists, even if `eas.json` is corrupted or missing. This provides defense-in-depth.

**Verdict**: EA 0 deletion is impossible through any code path. Triple-layered protection.

### 2. If EA 0 is deleted while active, does the dashboard crash?

**N/A - EA 0 cannot be deleted** (see finding #1).

For non-zero EAs deleted while active: the `delete_ea` handler (handlers.rs:181-184) checks `if app.active_ea == ea_id` and calls `app.switch_ea(0)`. The dashboard gracefully switches to EA 0. The error from `switch_ea` is ignored with `let _ =`, but even if it fails, `active_ea` is already set to 0 (switch_ea sets it immediately at app.rs:915), so the next dashboard tick will self-correct. *(Re-verified 2026-03-09)*

**Verdict**: No crash. Dashboard switches to EA 0 gracefully.

### 3. If EA 0 is deleted, what is the active EA? Does it auto-switch to EA 1?

**N/A - EA 0 cannot be deleted.**

For non-zero EA deletion: the handler always switches to EA 0, NOT to "the next available EA". This is intentional — EA 0 is the guaranteed safe harbor and always exists. There is no "auto-switch to another non-zero EA" logic.

### 4. Can the system still function with only EA 1 (no EA 0)?

**Impossible state - EA 0 always exists.**

`load_registry()` (ea.rs:49-68) guarantees EA 0 is always present. Even if someone manually edits `eas.json` to remove EA 0, `load_registry()` will re-insert it on the next read. The system structurally cannot have "no EA 0".

### 5. What if ALL EAs are deleted?

**Impossible - EA 0 cannot be deleted.**

Since EA 0 has a hard guard against deletion, at minimum EA 0 will always exist. The "delete all EAs" scenario cannot occur through any API or TUI code path.

### 6. What if the deleted EA had running agents? Are they killed first?

**PASS - Proper ordered teardown.**

The `delete_ea` handler (handlers.rs:130-191) performs a 6-step ordered teardown: *(Re-verified 2026-03-09)*

1. **Unregister from registry** — Blocks future API calls to this EA (resolve_ea returns 404)
2. **Cancel all events** — `scheduler.cancel_by_ea(ea_id)` removes all pending events
3. **Kill all worker agents** — Iterates tmux sessions matching the EA prefix, kills each
4. **Kill the manager session** — Killed AFTER workers so the manager doesn't try to respawn them
5. **Remove state directory** — Cleans up `~/.omar/ea/{id}/` and all files within
6. **Update dashboard** — If the deleted EA was active, switches to EA 0; reloads registry

**Race condition safety**: Between step 1 (unregister) and step 3 (kill agents), running agents may try to make API calls. These calls fail with 404 because `resolve_ea` checks the registry. This is the correct behavior — the agents are about to be killed anyway.

**Verdict**: Agents are properly killed during EA deletion. Events are cancelled. State is cleaned up.

### 7. Are IDs reused or monotonic?

**BUG FOUND AND FIXED** *(Re-verified 2026-03-09: fix confirmed in ea.rs:90-121)*

Previously `register_ea()` computed `next_id = max(existing_ids) + 1`.

This means:
- Create EA 1, EA 2 -> IDs: [0, 1, 2]
- Delete EA 2 -> IDs: [0, 1], max = 1
- Create new EA -> `next_id = 1 + 1 = 2` -> **ID 2 is reused!**

More critically:
- Create EA 1, EA 2 -> Delete EA 1 and EA 2 -> IDs: [0], max = 0
- Create new EA -> `next_id = 0 + 1 = 1` -> **ID 1 is reused!**

**Impact**: Reused IDs could cause confusion in logs, event histories, or audit trails. While not a data integrity issue (state directories are cleaned up on deletion), it violates the principle of least surprise.

**FIX APPLIED**: See "Fixes" section below. Changed to use a persistent high-water mark counter in `~/.omar/ea_next_id`. Also includes `checked_add` overflow protection (Fix V8, ea.rs:101-103).

### 8. Additional Edge Cases Checked

#### 8a. Dashboard refresh during EA deletion

Between steps 5 (remove state dir) and 6 (update dashboard) of `delete_ea`, the App mutex is NOT held. If the dashboard refreshes during this window while showing the deleted EA, it reads from a deleted state directory. All file-reading functions (`load_projects_from`, `load_agent_parents_from`, etc.) return empty/default values on I/O errors, so this is safe — no crash, just temporarily empty data for one refresh cycle.

**Verdict**: Safe. Self-corrects on next tick.

#### 8b. `cycle_next_ea` (F2 key) after EA deletion

`cycle_next_ea()` (app.rs:943-957) uses `self.registered_eas` which is shared via `Arc<Mutex<App>>`. The `delete_ea` handler updates `app.registered_eas` in step 6. Since both API and TUI share the same App instance, the TUI always has the up-to-date registry.

If `registered_eas.len() <= 1` (only EA 0), `cycle_next_ea` returns early — no crash.

**Verdict**: Function is safe, BUT **see BUG-2 below** — the F1/F2 keybindings are missing from the key handler, so this function is never called from TUI.

#### 8c. Event delivery to killed EA agent

If an event fires for an agent whose EA was just deleted (between the event being popped from the scheduler and the tmux send-keys), `deliver_to_tmux` (scheduler/mod.rs:244-277) will fail silently (tmux returns an error for a non-existent session) and log to the ticker. The scheduler mutex serializes queue operations, so `cancel_by_ea` and event delivery cannot race on the same event.

**Verdict**: Safe. Fails silently and logs.

#### 8d. Concurrent EA deletion attempts

Two API calls to `DELETE /api/eas/1` simultaneously:
- First call: `resolve_ea` finds EA 1, `unregister_ea` removes it from registry
- Second call: `resolve_ea` re-loads registry from disk, EA 1 is gone -> returns 404

This is correct because `unregister_ea` does atomic write (write tmp + rename).

**Verdict**: Safe. Second call gets 404.

#### 8e. Creating an EA immediately after deleting one

After `DELETE /api/eas/2`, creating a new EA via `POST /api/eas` with the same name works correctly. The old state directory was removed, and (after the fix) a fresh ID is assigned. No state leakage between the old and new EA.

**Verdict**: Safe.

---

## Bugs Found and Fixed

### BUG-1: EA ID Reuse After Highest-ID Deletion

**Location**: `src/ea.rs` (originally line 77, now lines 90-121)

**Problem**: `register_ea()` uses `max(existing_ids) + 1` for the next EA ID. If the EA with the highest ID is deleted, its ID will be reused for the next creation. This violates monotonicity.

**Example**:
```
Create EA 1 -> ID 1
Create EA 2 -> ID 2
Delete EA 2
Create EA 3 -> ID 2 (REUSED! Should be 3)
```

**Fix**: Added a persistent high-water mark counter stored in `~/.omar/ea_next_id`. The counter is only incremented, never decremented. On each `register_ea` call, the new ID is `max(counter, max_existing_ids) + 1`, and the counter is updated. Also uses `checked_add` to prevent u32 overflow (Fix V8).

**Files changed**: `src/ea.rs`
**Status**: ✅ Confirmed fixed in post-merge code (2026-03-09). Tests pass (`test_ids_monotonic_after_deletion`, `test_ids_monotonic_without_counter_file`).

### BUG-2: F1/F2 Keybindings Missing (Merge Conflict Casualty)

> **Found by**: fm-edge (post-merge re-verification, 2026-03-09)

**Location**: `src/main.rs` key event handler (lines 687-873)

**Problem**: The dashboard help text (`src/ui/dashboard.rs:1318-1319`) and status bar (`dashboard.rs:575-580`) advertise:
- **F1**: Create new EA
- **F2**: Cycle to next EA

The app methods exist: `app.cycle_next_ea()` (app.rs:943) and `app.create_ea()` (app.rs:960). However, the key event handler in `main.rs` has **NO** `KeyCode::F(1)` or `KeyCode::F(2)` arms. The `_ => {}` catch-all silently discards these key events.

**Impact**: Users cannot create or switch EAs via the TUI. Multi-EA management is only possible through the HTTP API (`POST /api/eas`, `PUT /api/eas/active`). The feature is *functionally correct* but *inaccessible from the keyboard*.

**Root cause**: Likely lost during merge conflict resolution in main.rs.

**Fix needed**: Add to the key handler in `main.rs` (in the normal key handling `match` block):
```rust
KeyCode::F(1) => {
    // TODO: prompt for name via input mode, then call app.create_ea(...)
}
KeyCode::F(2) => {
    app.cycle_next_ea();
}
```

### BUG-3: Sidebar Enter Shows Global Events (Not EA-Scoped)

> **Found by**: fm-edge (post-merge re-verification, 2026-03-09)

**Location**: `src/main.rs:781`

**Problem**: When opening the Events popup via **Enter** on the sidebar, the code uses `scheduler.list()` (global, all EAs). But pressing **'e'** uses `scheduler.list_by_ea(app.active_ea)` (correctly EA-scoped). The sidebar path leaks events from other EAs.

**Fix needed**: Change `main.rs:781` from:
```rust
app.scheduled_events = scheduler.list();
```
to:
```rust
app.scheduled_events = scheduler.list_by_ea(app.active_ea);
```

### NOTE-1: EA Prompt Has One Stale Global URL

> **Found by**: fm-edge (post-merge re-verification, 2026-03-09)

**Location**: `prompts/executive-assistant.md:285`

**Issue**: The cron recovery section references `curl http://localhost:9876/api/events` (global, no such route exists). Should be `curl http://localhost:9876/api/ea/{{EA_ID}}/events`.

**Impact**: Low — EA agent will get a 404, which is confusing but not dangerous. The agent can self-correct since other event URLs in the same prompt are correct.

---

## Post-Merge Re-verification Summary (2026-03-09)

All 10 original edge cases were re-verified against the post-merge code. Line numbers shifted but all safety invariants are preserved:

| Edge Case | Status | Notes |
|-----------|--------|-------|
| Delete EA 0 | ✅ GUARDED | Triple protection: handler, core logic, registry load |
| Dashboard crash on active EA deletion | ✅ SAFE | Switches to EA 0 gracefully |
| Active EA auto-switch target | ✅ CORRECT | Always switches to EA 0 (guaranteed safe harbor) |
| System without EA 0 | ✅ IMPOSSIBLE | Registry enforces EA 0 existence |
| All EAs deleted | ✅ IMPOSSIBLE | EA 0 cannot be deleted |
| Running agents on EA deletion | ✅ HANDLED | Ordered 6-step teardown |
| ID monotonicity | ✅ BUG FIXED | Persistent counter + checked_add overflow guard |
| Concurrent deletion | ✅ SAFE | Atomic registry writes + resolve_ea re-reads |
| Dashboard refresh during deletion | ✅ SAFE | File reads return defaults on I/O error |
| F2 cycling after deletion | ✅ SAFE | Function safe, but **keybinding missing (BUG-2)** |
| **F1/F2 keybindings** | 🐛 **MISSING** | Merge dropped key handler arms; API still works |
| **Sidebar events scope** | 🐛 **WRONG** | Enter path uses global list, 'e' key uses EA-scoped |
| **EA prompt stale URL** | ⚠️ **MINOR** | One global `/api/events` ref should be EA-scoped |
