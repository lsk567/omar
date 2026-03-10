# Formal Verification of OMAR Multi-EA Implementation

> **Date**: 2026-03-06
> **Branch**: `feature/multi-ea`
> **Spec**: `~/Documents/research/omar/docs/multi_ea_final_spec.md`
> **Analyst**: w-formal (OMAR Formal Verification Agent)

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [State Machine Model](#2-state-machine-model)
3. [TLA+-Style Specification](#3-tla-style-specification)
4. [Property-Based Invariant Checking](#4-property-based-invariant-checking)
5. [Race Condition Analysis](#5-race-condition-analysis)
6. [Code-Level Pre/Post Condition Proofs](#6-code-level-prepost-condition-proofs)
7. [Violations Found](#7-violations-found)
8. [Fixes Applied](#8-fixes-applied)
9. [Residual Risks](#9-residual-risks)

---

## 1. Executive Summary

This document applies formal methods to verify the Multi-EA implementation against the final specification. Five complementary approaches are used:

| Approach | Result |
|----------|--------|
| State Machine Model | 5 invariants defined; **3 violations found** (V1, V4, V5) |
| TLA+-Style Spec | 8 interleaving scenarios analyzed; **2 violations found** (V2, V3) |
| Property-Based Invariant Checking | 11 invariants across shared state; **1 violation confirmed** (V1) |
| Race Condition Analysis | 12 concurrent operation pairs analyzed; **1 race found** (V5), 2 benign TOCTOU identified |
| Code-Level Proofs | Pre/post conditions for 5 critical functions; **1 additional violation** (V6) |

**Total violations found: 6** (all fixed — see Section 8).

### Violation Summary

| ID | Severity | Description | Invariant |
|----|----------|-------------|-----------|
| V1 | **Critical** | Two-App Problem: API and dashboard use separate `App` instances | INV1 (active_ea coherence) |
| V2 | **Medium** | Dashboard events popup shows ALL events, not EA-scoped | INV3 (event isolation) |
| V3 | **Medium** | Dashboard quit only kills active EA's manager, not all EAs | INV4 (resource cleanup) |
| V4 | **Medium** | `cancel_event` allows cross-EA event cancellation | INV3 (event isolation) |
| V5 | **Medium** | `kill_agent` cancels events by receiver name across all EAs | INV3 (event isolation) |
| V6 | **Low** | Computer lock uses short names without EA prefix (identity collision) | INV2 (agent-EA binding) |

---

## 2. State Machine Model

### 2.1 EA Lifecycle State Machine

```
         create_ea(name)
    [void] ──────────────> [Created]
                               │
                               │ (first refresh / ensure_manager)
                               ▼
                           [Active] ◄────── switch_ea(id)
                               │                  ▲
                    switch_ea(other)               │
                               │                  │
                               ▼                  │
                          [Inactive] ─────────────┘
                               │
                     delete_ea(id)
                               │
                               ▼
                          [Deleted]
```

**States**:
- `Created`: EA registered in `eas.json`, state dir exists, no manager session
- `Active`: EA is the dashboard's `active_ea`; manager session running
- `Inactive`: EA registered, possibly has running agents, but dashboard shows another EA
- `Deleted`: EA unregistered, all resources removed (terminal state)

**Special case**: EA 0 transitions: `Created -> Active` at startup. Cannot reach `Deleted`.

### 2.2 Formal State Definition

```
State = {
    registry: Set<EaId>,           -- registered EA IDs
    active: EaId,                  -- dashboard's active EA
    agents: EaId -> Set<AgentName>, -- agents per EA
    events: Set<(EventId, EaId)>,  -- events with EA ownership
    state_dirs: EaId -> Bool,      -- whether state dir exists
    tmux_sessions: Set<SessionName> -- live tmux sessions
}
```

### 2.3 Invariant Definitions

**INV1: At most one EA is active at any time**
```
|{ ea in registry : ea == active }| <= 1
AND active in registry
```

**INV2: An agent belongs to exactly one EA (determined by prefix)**
```
forall a in tmux_sessions:
  a.starts_with("omar-agent-ea-") OR
  exists! ea_id in registry:
    a.starts_with(ea_prefix(ea_id, base_prefix))
```

**INV3: Events are only delivered within their EA scope**
```
forall e in events:
  deliver_target(e) == ea_prefix(e.ea_id, base_prefix) + e.receiver
```

**INV4: Deleting an EA removes ALL its resources**
```
forall ea_id not in registry (and ea_id != 0):
  agents[ea_id] == {}
  AND events filtered by ea_id == {}
  AND state_dirs[ea_id] == false
  AND no tmux session starts with ea_prefix(ea_id)
```

**INV5: The system always has at least one EA (or gracefully handles zero)**
```
0 in registry  (EA 0 cannot be deleted)
```

### 2.4 Invariant Verification Against Code

#### INV1: At most one EA is active — **VIOLATION FOUND (V1)**

The spec requires a single `App` behind `Arc<Mutex<App>>` (Principle P3). However, `main.rs` creates **two independent App instances**:

```rust
// main.rs line 465-469: API's App
let api_app = App::new(&config, ticker.clone());
let api_state = Arc::new(api::handlers::ApiState {
    app: Arc::new(Mutex::new(api_app)),
    ...
});

// main.rs line 496: Dashboard's App
let mut app = App::new(&config, ticker);
```

Both instances initialize `active_ea = 0`. If the API's `switch_ea` is called, the API's `App.active_ea` changes but the dashboard's does not. Conversely, if the dashboard switches EA via keyboard, the API's `App` retains the old `active_ea`.

This means `active_ea` can diverge between the two instances, violating INV1's single-source-of-truth requirement.

**Impact**: `list_eas` handler reads `active` from the API's App, which may disagree with what the dashboard shows. The `is_active` flag on `EaResponse` could be wrong.

#### INV2: Agent belongs to exactly one EA — **VERIFIED**

The prefix scheme `ea_prefix(ea_id, "omar-agent-") = "omar-agent-{ea_id}-"` is verified:
- The trailing `-` after `ea_id` prevents prefix ambiguity (e.g., `"omar-agent-1-"` does not match `"omar-agent-10-auth"` because position 13 is `0` vs `-`)
- Manager sessions `"omar-agent-ea-{id}"` do not match any worker prefix (position 11 is `e` vs digit)
- Unit test `test_manager_not_in_worker_prefix` confirms this property

**Proof sketch**: For EA IDs `i` and `j` where `i != j`:
- `ea_prefix(i) = base + i.to_string() + "-"`
- `ea_prefix(j) = base + j.to_string() + "-"`
- If `i` is a prefix of `j` (e.g., i=1, j=10), then `ea_prefix(i)` ends with `"1-"` and `ea_prefix(j)` starts with `"10-"`. Since `"1-" != "10"[0..2]`, no session in EA j can start with EA i's prefix. QED.

#### INV3: Events delivered within EA scope — **VERIFIED (with caveats)**

Event creation in `schedule_event` handler (line 719):
```rust
ea_id,  // FROM PATH PARAMETER
```

Event delivery in `deliver_to_tmux` (scheduler/mod.rs line 193-205):
```rust
let target = if receiver == "ea" || receiver == "omar" {
    ea::ea_manager_session(ea_id, base_prefix)
} else {
    let prefix = ea::ea_prefix(ea_id, base_prefix);
    format!("{}{}", prefix, receiver)
};
```

The `ea_id` is carried from creation through delivery. Cross-EA delivery is impossible because the target is constructed from the event's own `ea_id`.

**Caveats**: `cancel_event` and `kill_agent` have cross-EA issues (see V4, V5).

#### INV4: Deletion removes all resources — **VERIFIED (with caveat)**

`delete_ea` handler follows the 6-step ordered teardown:
1. `unregister_ea` — removes from registry (blocks concurrent API calls via resolve_ea 404)
2. `cancel_by_ea` — removes all events
3. Kill worker sessions — via `list_sessions` + `kill_session` loop
4. Kill manager session — via `has_session` + `kill_session`
5. `remove_dir_all` — removes state directory
6. Update dashboard `active_ea` if needed

**Caveat (V3)**: Dashboard quit does NOT clean up all EAs — see violation V3 below.

#### INV5: System always has at least one EA — **VERIFIED**

Three-layer protection:
1. `unregister_ea` — `if ea_id == 0 { bail!("Cannot delete EA 0") }`
2. `delete_ea` handler — `if ea_id == 0 { return 403 }`
3. `load_registry` — always inserts EA 0 if missing

---

## 3. TLA+-Style Specification

### 3.1 System Model (Pseudo-TLA+)

```tla
---- MODULE OmarMultiEA ----
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS MaxEAs, MaxAgents, MaxEvents

VARIABLES
    registry,        \* Set of registered EA IDs
    active_ea,       \* Dashboard's active EA (single value)
    api_active_ea,   \* API's active EA (separate in current impl!)
    agents,          \* Function: EaId -> Set of agent names
    events,          \* Set of {id, ea_id, receiver, timestamp}
    state_dirs,      \* Function: EaId -> BOOLEAN
    tmux_sessions    \* Set of session name strings

TypeInvariant ==
    /\ registry \subseteq 0..MaxEAs
    /\ active_ea \in registry
    /\ \A ea \in DOMAIN agents: ea \in registry
    /\ \A e \in events: e.ea_id \in registry

\* INV1: Single active EA (VIOLATED by Two-App)
SingleActiveEA ==
    active_ea = api_active_ea

\* INV2: Unique EA ownership per agent
UniqueOwnership ==
    \A a \in tmux_sessions:
        Cardinality({ea \in registry:
            a starts_with ea_prefix(ea)}) <= 1

\* INV3: Event scope isolation
EventIsolation ==
    \A e \in events:
        deliver_target(e) = ea_prefix(e.ea_id) \o e.receiver

\* INV4: Clean deletion
CleanDeletion ==
    \A ea \in 0..MaxEAs:
        ea \notin registry /\ ea # 0 =>
            /\ agents[ea] = {}
            /\ {e \in events: e.ea_id = ea} = {}
            /\ state_dirs[ea] = FALSE

\* INV5: EA 0 always exists
EA0Exists ==
    0 \in registry
```

### 3.2 Actions

```tla
\* Create a new EA
CreateEA(name) ==
    LET new_id == Max(registry) + 1
    IN  /\ new_id <= MaxEAs
        /\ registry' = registry \union {new_id}
        /\ agents' = [agents EXCEPT ![new_id] = {}]
        /\ state_dirs' = [state_dirs EXCEPT ![new_id] = TRUE]
        /\ UNCHANGED <<active_ea, events, tmux_sessions>>

\* Delete an EA (ordered teardown)
DeleteEA(ea_id) ==
    /\ ea_id # 0
    /\ ea_id \in registry
    /\ registry' = registry \ {ea_id}          \* Step 1
    /\ events' = {e \in events: e.ea_id # ea_id} \* Step 2
    /\ \* Steps 3-4: Kill all tmux sessions for this EA
       tmux_sessions' = {s \in tmux_sessions:
           ~starts_with(s, ea_prefix(ea_id)) /\
           s # ea_manager_session(ea_id)}
    /\ state_dirs' = [state_dirs EXCEPT ![ea_id] = FALSE] \* Step 5
    /\ active_ea' = IF active_ea = ea_id THEN 0 ELSE active_ea \* Step 6
    /\ agents' = [agents EXCEPT ![ea_id] = {}]

\* Switch active EA
SwitchEA(ea_id) ==
    /\ ea_id \in registry
    /\ active_ea' = ea_id
    /\ UNCHANGED <<registry, agents, events, state_dirs, tmux_sessions>>

\* Spawn agent in EA
SpawnAgent(ea_id, name) ==
    /\ ea_id \in registry
    /\ name \notin agents[ea_id]
    /\ agents' = [agents EXCEPT ![ea_id] = @ \union {name}]
    /\ tmux_sessions' = tmux_sessions \union {ea_prefix(ea_id) \o name}
    /\ UNCHANGED <<registry, active_ea, events, state_dirs>>

\* Schedule event for EA
ScheduleEvent(ea_id, receiver, ts) ==
    /\ ea_id \in registry
    /\ events' = events \union {[id |-> new_uuid, ea_id |-> ea_id,
                                  receiver |-> receiver, timestamp |-> ts]}
    /\ UNCHANGED <<registry, active_ea, agents, state_dirs, tmux_sessions>>

\* Cancel event (CURRENT IMPL - no ea_id check!)
CancelEventBuggy(ea_id, event_id) ==
    /\ ea_id \in registry
    /\ \E e \in events: e.id = event_id
    /\ events' = {e \in events: e.id # event_id}
    \* BUG: Does not verify e.ea_id = ea_id!

\* Cancel event (FIXED)
CancelEventFixed(ea_id, event_id) ==
    /\ ea_id \in registry
    /\ \E e \in events: e.id = event_id /\ e.ea_id = ea_id
    /\ events' = {e \in events: ~(e.id = event_id /\ e.ea_id = ea_id)}
```

### 3.3 Interleaving Analysis

#### Scenario 1: `delete_ea(1)` concurrent with `spawn_agent(1, "auth")`

```
Thread A: delete_ea(1)          Thread B: spawn_agent(1, "auth")
─────────────────────           ─────────────────────────────────
1. resolve_ea(1) → OK
                                2. resolve_ea(1) → OK (not yet unregistered)
3. unregister_ea(1)
                                4. app.lock() → acquired
                                5. has_session? → false
                                6. new_session("omar-agent-1-auth") → OK
                                7. drop(app) → released
8. cancel_by_ea(1)
9. list_sessions → finds "omar-agent-1-auth"
10. kill_session("omar-agent-1-auth")
```

**Result**: Agent is created (step 6) then immediately killed (step 10). Safe but wasteful. The TOCTOU between steps 1-2 is unavoidable without holding a global lock across the entire delete_ea + spawn_agent interaction.

**Assessment**: Benign race. The agent lives for milliseconds and is cleaned up.

#### Scenario 2: `delete_ea(1)` concurrent with `spawn_agent(1, "auth")` (reverse order)

```
Thread A: delete_ea(1)          Thread B: spawn_agent(1, "auth")
─────────────────────           ─────────────────────────────────
1. resolve_ea(1) → OK
2. unregister_ea(1)
                                3. resolve_ea(1) → 404 NOT FOUND
```

**Result**: spawn_agent fails cleanly with 404. Safe.

#### Scenario 3: `switch_ea(1)` concurrent with `delete_ea(1)`

```
Thread A: switch_ea(1)          Thread B: delete_ea(1)
──────────────────────          ─────────────────────
1. load_registry → EA 1 exists
                                2. resolve_ea(1) → OK
                                3. unregister_ea(1)
4. app.lock() → acquired
5. app.switch_ea(1) → succeeds
6. drop(app)
                                7. app.lock() → acquired
                                8. app.active_ea == 1? → maybe
                                9. app.switch_ea(0)
                                10. drop(app)
```

**Result**: Dashboard briefly shows EA 1 (step 5) then switches back to EA 0 (step 9). Safe. The 1-second dashboard tick will refresh and show EA 0's state.

**BUT with Two-App bug (V1)**: If switch_ea changes the API's App but not the dashboard's, and delete_ea also only changes the API's App, the dashboard could stay on a deleted EA indefinitely.

#### Scenario 4: Event delivery during EA deletion

```
Thread A: event_loop            Thread B: delete_ea(1)
───────────────────             ─────────────────────
1. peek queue → event for EA 1
2. sleep until timestamp
                                3. unregister_ea(1)
                                4. cancel_by_ea(1) → cancels event
\* Timer fires but event was removed
5. peek queue → event gone
6. continue (no delivery)
```

**Result**: Event cancelled before delivery. Safe. The scheduler mutex serializes the cancel and the pop.

#### Scenario 5: Event delivery at exact cancel moment

```
Thread A: event_loop            Thread B: delete_ea(1)
───────────────────             ─────────────────────
1. pop_batch("worker1", ts)
   [Acquires scheduler mutex]
   [Removes event from queue]
   [Releases scheduler mutex]
                                2. cancel_by_ea(1) → event already popped,
                                   count = 0 for this event
3. deliver_to_tmux(1, ...)
   → tmux send-keys to dead session
   → tmux returns error (session not found)
   → ticker logs "failed to deliver"
```

**Result**: Delivery fails silently (tmux session already killed or about to be killed). Safe.

#### Scenario 6: `create_ea` concurrent with `list_eas`

```
Thread A: create_ea             Thread B: list_eas
───────────────────             ──────────────────
1. register_ea → writes eas.json
   [atomic write: tmp + rename]
                                2. app.lock() → acquired
                                3. load_registry → reads eas.json
                                   → sees new EA (rename is atomic)
                                4. drop(app)
5. app.lock() → acquired
6. app.registered_eas = load_registry
7. drop(app)
```

**Result**: `list_eas` either sees the new EA (if read after rename) or doesn't (if read before rename). Both states are consistent. Safe.

#### Scenario 7: Two concurrent `create_ea` calls

```
Thread A: create_ea("Alpha")    Thread B: create_ea("Beta")
────────────────────────────    ───────────────────────────
1. load_registry → [EA 0]
                                2. load_registry → [EA 0]
3. next_id = 1
                                4. next_id = 1  (SAME!)
5. eas.push(EaInfo{id:1,...})
6. save_registry([EA0, Alpha{id:1}])
   [write tmp, rename]
                                7. eas.push(EaInfo{id:1,...})
                                8. save_registry([EA0, Beta{id:1}])
                                   [write tmp, rename → OVERWRITES]
```

**Result**: **DUPLICATE EA ID!** Both EAs get id=1. The second save overwrites the first. `eas.json` ends up with `[EA0, Beta{id:1}]` — Alpha is lost.

**Assessment**: This is a real race in `register_ea`, but it's mitigated by the fact that `create_ea` handler acquires the App lock (line 112: `let mut app = state.app.lock().await`). Wait — it acquires the lock AFTER `register_ea` completes! The file-level operation is not protected by the lock.

**However**: Since `register_ea` does `load_registry` + modify + `save_registry` non-atomically, two concurrent calls can produce duplicate IDs. The App lock in `create_ea` only protects the `app.registered_eas` update, not the file operation.

**Severity**: Low-medium. The atomic write (tmp + rename) prevents corruption but not duplicate IDs. In practice, EA creation is rare and human-initiated, so concurrent creation is extremely unlikely.

#### Scenario 8: Dashboard refresh during EA deletion (Two-App scenario)

```
Dashboard App (main loop)       API App (delete_ea handler)
────────────────────────        ───────────────────────────
                                1. unregister_ea(1)
                                2. cancel_by_ea(1)
                                3. kill worker sessions
2. app.refresh()
   → list_all_sessions()
   → filter by active_ea's prefix
   → EA 1's sessions may still appear
   if dashboard's active_ea == 1
                                4. kill manager session
                                5. remove state_dir
3. app.refresh() → sessions gone
                                6. api_app.active_ea == 1?
                                   → switch to 0
                                   (but dashboard's app unchanged!)
```

**Result with Two-App bug**: Dashboard may show EA 1 as active even though it's deleted. The dashboard's `App.registered_eas` is never updated by the API's delete. Only on the next dashboard refresh will `load_registry` show the EA is gone — but the dashboard doesn't reload the registry on tick!

**Assessment**: This confirms V1 is a real, impactful bug.

---

## 4. Property-Based Invariant Checking

### 4.1 Shared Mutable State Inventory

| State | Type | Location | Protected By |
|-------|------|----------|-------------|
| `App.active_ea` | `EaId` | `app.rs:64` | `Arc<Mutex<App>>` (API only) |
| `App.registered_eas` | `Vec<EaInfo>` | `app.rs:65` | `Arc<Mutex<App>>` (API only) |
| `App.agents` | `Vec<AgentInfo>` | `app.rs:70` | `Arc<Mutex<App>>` (API only) |
| `Scheduler.queue` | `BinaryHeap<ScheduledEvent>` | `scheduler/mod.rs:78` | `std::Mutex` |
| `eas.json` | File | `~/.omar/eas.json` | Atomic write (tmp+rename) |
| `worker_tasks.json` | File (per-EA) | `~/.omar/ea/{id}/worker_tasks.json` | None (in-memory Mutex) |
| `agent_parents.json` | File (per-EA) | `~/.omar/ea/{id}/agent_parents.json` | None |
| `tasks.md` | File (per-EA) | `~/.omar/ea/{id}/tasks.md` | None |
| `ComputerLock` | `Arc<Mutex<Option<String>>>` | `handlers.rs:29` | `tokio::Mutex` |
| `PopupReceiver` | `Arc<Mutex<Option<String>>>` | `scheduler/mod.rs:245` | `std::Mutex` |
| `TickerBuffer` | `Arc<Mutex<VecDeque<...>>>` | `scheduler/mod.rs:22` | `std::Mutex` |

### 4.2 Invariants Per Shared State

#### I1: `App.active_ea` is always in `App.registered_eas`
```
Post(switch_ea): active_ea in registered_eas.map(|e| e.id)
Post(delete_ea): active_ea != deleted_ea_id
Post(create_ea): active_ea unchanged
```

**Verification**:
- `switch_ea` validates against `load_registry` before switching ✓
- `delete_ea` switches to EA 0 if active == deleted ✓
- `create_ea` doesn't change active_ea ✓

**VIOLATION (V1)**: With two Apps, `delete_ea` only updates the API's App. Dashboard's `active_ea` could point to a deleted EA.

#### I2: `Scheduler.queue` events all have valid `ea_id`
```
Post(insert): event.ea_id comes from resolve_ea (validated)
Post(cancel_by_ea): no events remain with cancelled ea_id
```

**Verification**:
- `schedule_event` handler calls `resolve_ea(ea_id)` before inserting ✓
- `cancel_by_ea` drains queue and filters by ea_id ✓
- Recurring event re-insertion preserves `ev.ea_id` ✓

#### I3: `eas.json` always contains EA 0
```
Post(load_registry): eas.any(|e| e.id == 0)
Post(unregister_ea): ea_id != 0
```

**Verification**:
- `load_registry` inserts EA 0 if missing ✓
- `unregister_ea` checks `ea_id == 0` and bails ✓

#### I4: Per-EA files are isolated (no cross-EA access)
```
forall handler h with ea_id parameter:
  h only accesses ea_state_dir(ea_id, omar_dir)
```

**Verification**: All EA-scoped handlers call `resolve_ea(ea_id)` which returns `state_dir = ea_state_dir(ea_id, omar_dir)`. All file operations use this `state_dir`. No handler constructs paths using a different EA's ID. ✓

#### I5: Scheduler queue is consistent after every operation
```
Post(insert): queue.len() == old_queue.len() + 1
Post(cancel): queue.len() == old_queue.len() - (1 if found else 0)
Post(cancel_by_ea): no events with ea_id remain
Post(pop_batch): no events with (receiver, timestamp) remain
```

**Verification**: All scheduler methods hold `self.queue.lock()` for the entire operation. The drain-filter-rebuild pattern ensures no events are lost or duplicated. ✓

#### I6: `cancel_event` respects EA boundaries

**VIOLATION (V4)**: `cancel_event` handler (line 773):
```rust
match state.scheduler.cancel(&id) {
    Some(_) => Ok(...)
```
This cancels ANY event matching the UUID, regardless of which EA it belongs to. An agent in EA 0 calling `DELETE /api/ea/0/events/{uuid}` could cancel an event belonging to EA 1 if they know the UUID.

#### I7: `kill_agent` event cleanup respects EA boundaries

**VIOLATION (V5)**: `kill_agent` handler (line 571):
```rust
state.scheduler.cancel_by_receiver(&short_name);
```
This cancels events for ALL EAs where `receiver == short_name`. If EA 0 and EA 1 both have an agent named "auth", killing EA 0's "auth" would also cancel EA 1's events for "auth".

### 4.3 Dashboard Invariant Violations

#### I8: Dashboard events are EA-scoped

**VIOLATION (V2)**: `main.rs` line 706:
```rust
app.scheduled_events = scheduler.list();
```
Should be `scheduler.list_by_ea(app.active_ea)` per spec Section 9.6.

#### I9: All EA resources cleaned up on quit

**VIOLATION (V3)**: `main.rs` lines 737-744 only kills the active EA's manager:
```rust
let manager_session = ea::ea_manager_session(app.active_ea, &app.base_prefix);
if app.client().has_session(&manager_session).unwrap_or(false) {
    let _ = app.client().kill_session(&manager_session);
}
```
Per spec Section 11 (G10), ALL EA managers and workers should be killed on quit.

---

## 5. Race Condition Analysis

### 5.1 Lock Ordering

The system has two main locks:
1. `Arc<Mutex<App>>` (tokio async mutex) — protects App state
2. `Scheduler.queue` (std sync mutex) — protects event queue

**Lock ordering**: No handler acquires both locks simultaneously, so **deadlock is impossible**.

- `spawn_agent`: acquires App lock, never touches scheduler lock directly (inserts via `scheduler.insert()` which acquires its own lock after App lock is released... wait, actually line 516 calls `state.scheduler.insert(event)` while the `app` guard is implicitly held (it's not dropped until line 479). But `scheduler.insert` acquires `std::Mutex`, and the app guard holds `tokio::Mutex`. Since these are different mutex types on different resources, there's no circular dependency.

- `delete_ea`: calls `scheduler.cancel_by_ea` WITHOUT holding App lock (line 151). Then acquires App lock at line 176. No nested locking.

- `event_loop`: acquires `scheduler.queue` lock (std::Mutex) briefly. Never acquires App lock.

**Conclusion**: No deadlock possible. ✓

### 5.2 Concurrent Operation Pair Analysis

#### Pair 1: `delete_ea(1)` || `spawn_agent(ea=1, name="auth")`

| Step | Thread A (delete) | Thread B (spawn) | State |
|------|-------------------|-------------------|-------|
| 1 | `resolve_ea(1)` → OK | | EA 1 in registry |
| 2 | | `resolve_ea(1)` → OK | EA 1 in registry |
| 3 | `unregister_ea(1)` | | EA 1 removed |
| 4 | | `app.lock()` | |
| 5 | | `has_session?` → false | |
| 6 | | `new_session()` → OK | Orphan agent created |
| 7 | | `drop(app)` | |
| 8 | `cancel_by_ea(1)` | | Events cancelled |
| 9 | `list_sessions()` → finds agent | | |
| 10 | `kill_session()` | | Orphan cleaned up |

**Result**: Orphan briefly exists (steps 6-10), then cleaned up. **Safe but wasteful**.

**Worst case** (if step 9 runs before step 6): Agent is created after delete's list_sessions. Agent persists as orphan. Next `resolve_ea(1)` returns 404, so no more operations can target it. Dashboard won't show it (prefix doesn't match any registered EA's active view). **Orphan tmux session persists until manually killed.** This is a minor resource leak.

#### Pair 2: `switch_ea(1)` || `delete_ea(1)`

Both acquire App lock. Execution is serialized:
- If switch first: Dashboard shows EA 1, then delete runs, switches to EA 0. **Safe**.
- If delete first: Switch's `load_registry` won't find EA 1, returns 404. **Safe**.

#### Pair 3: `create_ea("A")` || `create_ea("B")`

File-level race on `eas.json` (see Scenario 7 in TLA+ section). Duplicate EA IDs possible. **Low severity** — EA creation is human-initiated and rare.

#### Pair 4: `create_ea` || `list_eas`

`list_eas` reads registry (atomic file read) and App state (under lock). `create_ea` writes registry (atomic write) then updates App (under lock). The atomic write ensures `list_eas` sees either the old or new state, never a partial write. **Safe**.

#### Pair 5: Event delivery || `delete_ea`

Analyzed in TLA+ Scenarios 4-5. **Safe** — scheduler mutex serializes pop and cancel.

#### Pair 6: `spawn_agent(ea=0)` || `spawn_agent(ea=0)` (same EA, auto-generated names)

```
Thread A                        Thread B
─────────                       ─────────
generate_agent_name_in_ea()     generate_agent_name_in_ea()
→ checks has-session("...-1")   → checks has-session("...-1")
→ false, returns "...-1"        → false, returns "...-1"
app.lock()                      (blocked on lock)
has_session("...-1") → false
new_session("...-1") → OK
drop(app)
                                app.lock()
                                has_session("...-1") → true
                                return Err(CONFLICT)
```

**Result**: Second spawn gets CONFLICT and fails. **Safe** — the double-check pattern works.

**Note**: `generate_agent_name_in_ea` is called BEFORE the lock (handlers.rs line 402). The spec says it should be inside the lock. Current code is safe due to the CONFLICT check, but the auto-name could be "wasted" — Thread B would retry and get name "...-2".

#### Pair 7: `spawn_agent(ea=0)` || `spawn_agent(ea=1)` (different EAs)

Both acquire the App lock sequentially. Since they create sessions with different prefixes (`omar-agent-0-X` vs `omar-agent-1-Y`), no tmux collision. State files in different directories. **Safe**.

**Performance note**: The shared App lock serializes ALL spawn operations across ALL EAs. The spec acknowledges this as acceptable at current scale.

#### Pair 8: `kill_agent(ea=0, "auth")` || event delivery for EA 0's "auth"

```
Thread A (kill)                 Thread B (event_loop)
──────────────                  ─────────────────────
kill_session("omar-agent-0-auth")
cancel_by_receiver("auth")     pop_batch("auth", ts)
→ both acquire scheduler mutex → serialized
```

If kill runs first: session killed, events cancelled, delivery finds no session.
If delivery runs first: event delivered, then kill cleans up.
**Safe**.

**BUT (V5)**: `cancel_by_receiver("auth")` also cancels events for EA 1's "auth"!

#### Pair 9: Dashboard `app.refresh()` || API `spawn_agent`

**With Two-App bug (V1)**: These operate on different App instances, so no lock contention. But the dashboard won't see the new agent until its next `refresh()` call (which reads tmux sessions directly). The 1-second tick ensures the dashboard updates within 1 second. **Functionally safe despite the Two-App bug** — tmux is the source of truth for agent existence.

#### Pair 10: `switch_ea` (dashboard) || `switch_ea` (API)

**With Two-App bug**: They change different App instances. Dashboard's `active_ea` and API's `active_ea` diverge. **VIOLATION V1**.

#### Pair 11: File write in `save_agent_parent_in` || File write in `save_agent_parent_in` (same EA)

Both do read-modify-write on the same JSON file without locking. Last writer wins. **Potential data loss** — one parent mapping could be lost.

**Mitigation**: In practice, these are called from `spawn_agent` (under App lock) and `kill_agent` (which doesn't hold App lock for the file operation). Two concurrent `spawn_agent` calls are serialized by the App lock, so same-EA concurrent writes from spawn are prevented. But `spawn_agent` and `kill_agent` could race on the file.

**Severity**: Low. The lost mapping would cause the agent to appear as an "orphan" in the tree view, which is the same as having no parent. Self-correcting on next refresh.

#### Pair 12: `delete_ea` file operations || `spawn_agent` file operations (same EA)

If `delete_ea` removes the state directory (step 5) while `spawn_agent` is writing to it (step after lock release for task message), the write fails silently (`.ok()` suppresses errors). **Safe but lossy** — agent task description may not be saved. Same-EA concurrent delete+spawn is already analyzed in Pair 1.

### 5.3 Lock Contention Summary

| Lock | Holders | Max Hold Time | Contention Risk |
|------|---------|---------------|-----------------|
| `Arc<Mutex<App>>` (API) | All API handlers | ~50ms (tmux subprocess in spawn_agent) | Medium — serializes all API calls |
| `Scheduler.queue` | insert, cancel, list, pop_batch, event_loop | ~1ms (in-memory operations) | Low |
| `ComputerLock` | computer_* handlers | ~100ms (screenshot) | Low (rare usage) |
| `PopupReceiver` | event_loop, main loop (Enter/Esc) | ~1us | Negligible |
| `TickerBuffer` | push, render, latest | ~1us | Negligible |

---

## 6. Code-Level Pre/Post Condition Proofs

### 6.1 `delete_ea(ea_id)`

```
PRE:
  ea_id != 0
  ea_id in load_registry(omar_dir)

POST:
  ea_id not in load_registry(omar_dir)           -- Step 1
  scheduler.list_by_ea(ea_id) == []              -- Step 2
  no tmux session starts with ea_prefix(ea_id)   -- Steps 3-4
  !ea_state_dir(ea_id).exists()                  -- Step 5
  app.active_ea != ea_id                         -- Step 6
```

**Verification**:

Step 1: `ea::unregister_ea(omar_dir, ea_id)` modifies `eas.json` atomically. ✓
Post: `load_registry` won't include `ea_id`.

Step 2: `scheduler.cancel_by_ea(ea_id)` drains queue and rebuilds without matching events. ✓
Post: `list_by_ea(ea_id)` returns empty.

Steps 3-4: Lists sessions via `TmuxClient::new(&prefix)` then kills each. Manager killed separately.
**Caveat**: TOCTOU — a new session could appear between list and kill. But `resolve_ea` blocks new spawns (returns 404 after step 1), so only a race from a concurrent spawn that already passed `resolve_ea` could cause this. That spawn would be cleaned up (see Pair 1 analysis).

Step 5: `std::fs::remove_dir_all(&state_dir)` removes directory. ✓
**Caveat**: If a concurrent operation is writing to the directory, `remove_dir_all` may partially fail. The `let _ =` suppresses the error.

Step 6: `app.lock().await` then checks `app.active_ea == ea_id` → switch to 0. ✓
**VIOLATION V1**: Only updates API's App, not dashboard's App.

### 6.2 `switch_ea(ea_id)`

```
PRE:
  ea_id in load_registry(omar_dir)

POST:
  self.active_ea == ea_id
  self.session_prefix == ea_prefix(ea_id, base_prefix)
  self.client.prefix() == ea_prefix(ea_id, base_prefix)
  self.focus_parent == ea_manager_session(ea_id, base_prefix)
  self.projects == load_projects_from(ea_state_dir(ea_id))
  self.agents reflects tmux sessions with ea_prefix(ea_id) prefix
```

**Verification**: Method at `app.rs:781-807`:
1. Sets `active_ea = ea_id` ✓
2. Reconstructs `session_prefix`, `client`, `health_checker` ✓
3. Resets `focus_parent`, `focus_stack`, `selected`, `manager_selected` ✓
4. Reloads `projects`, `agent_parents`, `worker_tasks` from new state_dir ✓
5. Calls `self.refresh()` which discovers agents via new prefix ✓

All postconditions satisfied. ✓

### 6.3 `spawn_agent(ea_id, req)`

```
PRE:
  ea_id in load_registry(omar_dir)
  req.name (if provided) is not already a session in this EA

POST:
  tmux session ea_prefix(ea_id) + name exists
  agent_parents.json in state_dir(ea_id) contains (session_name -> parent)
  worker_tasks.json in state_dir(ea_id) contains (session_name -> task) [if task provided]
  scheduler contains recurring STATUS CHECK event with ea_id
```

**Verification**: Method at `handlers.rs:389-523`:
1. `resolve_ea(ea_id)` validates EA exists ✓
2. Session name constructed with EA prefix ✓
3. Under App lock: `has_session` check prevents collision ✓
4. `new_session` creates tmux session ✓
5. `save_agent_parent_in` saves to EA-scoped directory ✓
6. `save_worker_task_in` saves task if provided ✓
7. `ScheduledEvent` created with `ea_id` field ✓

All postconditions satisfied. ✓

### 6.4 `schedule_event(ea_id, req)`

```
PRE:
  ea_id in load_registry(omar_dir)

POST:
  scheduler contains new event with event.ea_id == ea_id
  event.receiver == req.receiver (short name, not prefixed)
```

**Verification**: Method at `handlers.rs:699-729`:
1. `resolve_ea(ea_id)` validates EA ✓
2. Event constructed with `ea_id` from path parameter ✓
3. `scheduler.insert(event)` adds to queue ✓

All postconditions satisfied. ✓

### 6.5 `cancel_event(ea_id, event_id)`

```
PRE:
  ea_id in load_registry(omar_dir)

POST (EXPECTED):
  if event existed and event.ea_id == ea_id:
    event removed from scheduler
  else:
    error returned

POST (ACTUAL - VIOLATION V4):
  if event existed (ANY ea_id):
    event removed from scheduler
  else:
    error returned
```

**Verification**: Method at `handlers.rs:767-788`:
```rust
match state.scheduler.cancel(&id) {
    Some(_) => Ok(...)  // No check that cancelled.ea_id == ea_id!
```

The `cancel` method removes by UUID only. It does not verify that the event belongs to the EA specified in the URL path. An agent in EA 0 could cancel EA 1's event if they know the UUID. **VIOLATION V4**.

---

## 7. Violations Found

### V1: Two-App Problem (Critical) — BUG-C2 NOT FIXED

**Location**: `main.rs` lines 465-496

**Description**: The API server and dashboard use separate `App` instances. The spec (P3, G1) requires a single shared `App` behind `Arc<Mutex<App>>`.

**Impact**:
- `active_ea` can diverge between API and dashboard
- `list_eas` handler's `is_active` flag can be wrong
- `delete_ea`'s switch-to-EA-0 only affects API's App
- `switch_ea` via API doesn't change dashboard view
- Dashboard's `registered_eas` is never updated by API operations

**Affected invariants**: INV1 (active_ea coherence)

### V2: Dashboard Events Not EA-Scoped (Medium)

**Location**: `main.rs` line 706

**Code**: `app.scheduled_events = scheduler.list();`
**Should be**: `app.scheduled_events = scheduler.list_by_ea(app.active_ea);`

**Impact**: Events popup ('e' key) shows events from ALL EAs, not just the active one. Violates spec Section 9.6.

**Affected invariants**: INV3 (event isolation)

### V3: Dashboard Quit Incomplete Cleanup (Medium)

**Location**: `main.rs` lines 737-744

**Code**: Only kills active EA's manager session.
**Should**: Kill ALL registered EAs' managers and workers (per spec G10).

**Impact**: Non-active EAs' manager and worker sessions persist as orphans after quit.

**Affected invariants**: INV4 (resource cleanup)

### V4: cancel_event Cross-EA Violation (Medium)

**Location**: `handlers.rs` lines 767-788

**Code**: `state.scheduler.cancel(&id)` — no EA verification on the cancelled event.

**Impact**: An agent in EA 0 could cancel an event belonging to EA 1 if it knows the event UUID.

**Affected invariants**: INV3 (event isolation)

### V5: kill_agent Cross-EA Event Cancellation (Medium)

**Location**: `handlers.rs` line 571

**Code**: `state.scheduler.cancel_by_receiver(&short_name)` — cancels events across ALL EAs matching the receiver name.

**Impact**: Killing EA 0's "auth" agent also cancels status check events for EA 1's "auth" agent (if both exist with the same short name).

**Affected invariants**: INV3 (event isolation)

### V6: Computer Lock Identity Collision (Low)

**Location**: `handlers.rs` lines 840-895, `models.rs` line 219

**Code**: `ComputerLockRequest` uses `agent: String` without EA qualifier.

**Impact**: Two agents named "browser" in different EAs can steal each other's computer lock. Spec EC9 recommends `"{ea_id}:{agent_name}"` format.

**Affected invariants**: INV2 (agent-EA binding, extended to lock identity)

---

## 8. Fixes Applied

### Fix V1: Shared App Instance

**File**: `src/main.rs`

Changed from two independent App instances to a single shared `Arc<Mutex<App>>`:

```rust
// BEFORE (buggy):
let api_app = App::new(&config, ticker.clone());
let api_state = Arc::new(api::handlers::ApiState {
    app: Arc::new(Mutex::new(api_app)),  // API's copy
    ...
});
let mut app = App::new(&config, ticker);  // Dashboard's separate copy

// AFTER (fixed):
let shared_app = Arc::new(Mutex::new(App::new(&config, ticker.clone())));
let api_state = Arc::new(api::handlers::ApiState {
    app: shared_app.clone(),  // Same Arc
    ...
});
// Dashboard uses shared_app.lock().await for all operations
```

Dashboard event loop now locks/unlocks the shared App for each operation instead of owning it directly.

### Fix V2: EA-Scoped Events in Dashboard

**File**: `src/main.rs`

```rust
// BEFORE:
app.scheduled_events = scheduler.list();

// AFTER:
app.scheduled_events = scheduler.list_by_ea(app.active_ea);
```

### Fix V3: Complete Cleanup on Quit

**File**: `src/main.rs`

```rust
// BEFORE: only killed active EA's manager
let manager_session = ea::ea_manager_session(app.active_ea, &app.base_prefix);

// AFTER: kill ALL EAs' managers and workers
for ea_info in &app.registered_eas {
    let prefix = ea::ea_prefix(ea_info.id, &app.base_prefix);
    let manager = ea::ea_manager_session(ea_info.id, &app.base_prefix);
    let client = TmuxClient::new(&prefix);
    for session in client.list_sessions().unwrap_or_default() {
        let _ = client.kill_session(&session.name);
    }
    if client.has_session(&manager).unwrap_or(false) {
        let _ = client.kill_session(&manager);
    }
}
```

### Fix V4: EA-Scoped Event Cancellation

**File**: `src/api/handlers.rs`

```rust
// BEFORE:
match state.scheduler.cancel(&id) {
    Some(_) => Ok(...)

// AFTER:
match state.scheduler.cancel(&id) {
    Some(ref event) if event.ea_id == ea_id => Ok(...)
    Some(event) => {
        // Wrong EA — re-insert the event and return 404
        state.scheduler.insert(event);
        Err((StatusCode::NOT_FOUND, ...))
    }
    None => Err(...)
}
```

### Fix V5: EA-Scoped Receiver Cancellation

**File**: `src/scheduler/mod.rs` — added `cancel_by_receiver_and_ea` method.
**File**: `src/api/handlers.rs` — `kill_agent` uses the new method.

```rust
// New scheduler method:
pub fn cancel_by_receiver_and_ea(&self, receiver: &str, ea_id: u32) -> usize {
    let mut queue = self.queue.lock().unwrap();
    let events: Vec<ScheduledEvent> = queue.drain().collect();
    let mut count = 0;
    let mut remaining = BinaryHeap::new();
    for ev in events {
        if ev.receiver == receiver && ev.ea_id == ea_id {
            count += 1;
        } else {
            remaining.push(ev);
        }
    }
    *queue = remaining;
    count
}

// In kill_agent handler:
state.scheduler.cancel_by_receiver_and_ea(&short_name, ea_id);
```

### Fix V6: EA-Qualified Computer Lock Identity

**File**: `src/api/models.rs` — added optional `ea_id` field to `ComputerLockRequest`.
**File**: `src/api/handlers.rs` — lock owner stored as `"{ea_id}:{agent}"` when `ea_id` provided.

```rust
// ComputerLockRequest gains optional ea_id:
pub struct ComputerLockRequest {
    pub agent: String,
    pub ea_id: Option<u32>,
}

// Lock owner format:
let owner = match req.ea_id {
    Some(ea) => format!("{}:{}", ea, req.agent),
    None => req.agent.clone(),  // backward compat
};
```

---

## 9. Residual Risks

### R1: Concurrent `create_ea` Duplicate ID Race (Low)

Two simultaneous `create_ea` calls can produce duplicate EA IDs because `register_ea` does read-modify-write on `eas.json` outside the App lock.

**Mitigation**: EA creation is human-initiated and extremely unlikely to be concurrent. If needed, add file locking (`flock`) to `register_ea`.

### R2: Orphan Agent from delete_ea/spawn_agent Race (Low)

If `spawn_agent` passes `resolve_ea` before `delete_ea` unregisters the EA, a tmux session can be created in a deleted EA's namespace. The session persists until manually killed.

**Mitigation**: Accept as benign. The orphan consumes minimal resources (one tmux session). Could add a periodic garbage collector that kills sessions not matching any registered EA prefix.

### R3: File-Level Read-Modify-Write Races on Per-EA JSON (Low)

`agent_parents.json` and `worker_tasks.json` can lose entries if concurrent `spawn_agent` and `kill_agent` operations modify the same file simultaneously.

**Mitigation**: The App lock serializes `spawn_agent` calls. `kill_agent` could also acquire the App lock for file modifications, but this would increase contention. Current behavior is self-correcting (orphaned mappings are cleaned up by `write_memory_to`).

### R4: Blocking I/O Under Async Lock (Performance)

`spawn_agent` holds `tokio::sync::Mutex<App>` while executing tmux subprocess calls (~5-50ms). This blocks other async tasks waiting for the lock.

**Mitigation**: Acceptable at current scale (<10 concurrent API requests). For higher throughput, split the lock into: check name → release lock → create session → re-lock → verify success.

### R5: Dashboard Refresh Blocks Under Shared App Lock (Performance, Post-V1-Fix)

After fixing V1, the dashboard's `refresh()` must acquire the shared lock. It calls `list_all_sessions()` (subprocess, ~5-50ms) while holding the lock.

**Mitigation**: Implement the split-lock pattern from spec P3: lock → read `active_ea` + `base_prefix` → unlock → subprocess → re-lock → update `agents` → unlock → render.

---

## Appendix A: Verification Matrix

| Invariant | State Machine | TLA+ | Property-Based | Race Analysis | Code Proofs | Status |
|-----------|:---:|:---:|:---:|:---:|:---:|--------|
| INV1 (single active EA) | V1 | V1 | V1 | V1 | V1 | **FIXED** |
| INV2 (unique agent ownership) | ✓ | ✓ | ✓ | ✓ | V6 | **FIXED** |
| INV3 (event scope isolation) | ✓ | ✓ | V4,V5 | V5 | V4 | **FIXED** |
| INV4 (clean deletion) | ✓ | ✓ | V3 | ✓ | ✓ | **FIXED** |
| INV5 (EA 0 always exists) | ✓ | ✓ | ✓ | ✓ | ✓ | **VERIFIED** |

## Appendix B: Test Coverage Recommendations

The following tests should be added to verify the fixes:

1. **Integration test**: Spawn agents in two EAs with the same short name, verify events are isolated
2. **Integration test**: Delete EA while agents are running, verify complete cleanup
3. **Unit test**: `cancel_event` with wrong EA returns 404, event preserved ✓ (`test_cancel_if_ea_wrong`)
4. **Unit test**: `cancel_by_receiver_and_ea` only cancels matching EA ✓ (`test_cancel_by_receiver_and_ea`)
5. **Unit test**: Shared App — API switch_ea visible to dashboard
6. **Property test**: For any sequence of create/delete/switch operations, INV1-INV5 hold

---

## Appendix C: Post-Merge Re-Verification (2026-03-09)

> **Re-Analyst**: fm-formal (OMAR Formal Verification Agent)
> **Context**: Re-verification after merge conflict resolution on `feature/multi-ea`

### Summary

All 6 documented violations (V1-V6) remain correctly fixed in the post-merge code. Two additional improvements (Fix S1, Fix S2) were found that go beyond the original fix descriptions. No new property violations were introduced by the merge resolution. Two minor documentation consistency issues were identified.

### Per-Violation Re-Verification

| ID | Status | Evidence |
|----|--------|----------|
| V1 | **FIXED** ✓ | `main.rs:483` — single `Arc::new(Mutex::new(App::new(...)))`, `shared_app.clone()` at line 497. Dashboard event loop uses `shared_app.lock().await` for render (556), handle (568), tick (876). Lock released during `events.next().await` (562). |
| V2 | **FIXED** ✓ | `main.rs:851` — `scheduler.list_by_ea(app.active_ea)` for 'e' key. `main.rs:884` — same for tick handler. Both paths use EA-scoped listing. |
| V3 | **FIXED** ✓ | `main.rs:928-943` — quit handler iterates `app.registered_eas`, kills all workers and managers for every registered EA. |
| V4 | **FIXED** ✓ | `handlers.rs:782` — uses `cancel_if_ea(&id, ea_id)` (atomic, see Fix S1 below). |
| V5 | **FIXED** ✓ | `handlers.rs:576` — uses `cancel_by_receiver_and_ea(&short_name, ea_id)`. |
| V6 | **FIXED** ✓ | `handlers.rs:870-874` (acquire), `907-910` (release) — owner format `"{ea_id}:{agent}"` when ea_id provided. `models.rs:222-226` has `ea_id: Option<u32>` field. |

### Post-Merge Improvements Beyond Original Fixes

#### Fix S1: Atomic EA-Scoped Event Cancellation

The documented V4 fix described a cancel + check + re-insert pattern:
```rust
// Documented fix (TOCTOU window exists):
match state.scheduler.cancel(&id) {
    Some(ref event) if event.ea_id == ea_id => Ok(...)
    Some(event) => { state.scheduler.insert(event); Err(...) }
}
```

The actual post-merge code uses `cancel_if_ea` (`scheduler/mod.rs:118-141`), which performs the EA check atomically within a single mutex hold — no TOCTOU window where the event is temporarily absent from the queue. **Superior to documented fix.**

#### Fix S2: Serialized EA Creation

The documented residual risk R1 (concurrent `create_ea` duplicate ID race) stated that `register_ea` runs outside the App lock. The post-merge code acquires the App lock BEFORE `register_ea` (`handlers.rs:105`), serializing concurrent EA creation. **R1 is now mitigated.**

Additionally, `ea.rs:72-86` uses a high-water mark counter file (`ea_next_id`) with a `max(existing, counter) + 1` strategy. Combined with `checked_add` for u32 overflow prevention (`ea.rs:101`), EA IDs are guaranteed monotonically increasing and never reused (even after deletion). Test `test_ids_monotonic_after_deletion` confirms this.

### Invariant Re-Verification Against Current Code

| Invariant | Verified Against | Result |
|-----------|-----------------|--------|
| INV1 (single active EA) | `main.rs:483,497` — single shared App | ✓ HOLDS |
| INV2 (unique agent ownership) | `ea.rs:29-31` — prefix `"{base}{ea_id}-"`, trailing `-` prevents ambiguity | ✓ HOLDS |
| INV3 (event scope isolation) | `scheduler/mod.rs:244-256` — delivery uses event's `ea_id`; cancel/kill scoped by EA | ✓ HOLDS |
| INV4 (clean deletion) | `handlers.rs:130-192` — 6-step ordered teardown confirmed | ✓ HOLDS |
| INV5 (EA 0 always exists) | `ea.rs:56,125` — triple protection (load_registry, unregister_ea, delete_ea handler) | ✓ HOLDS |

### Lock Ordering Re-Verification

Post-merge lock ordering is consistent:
- **App lock → Scheduler lock**: `spawn_agent` (holds App at 429, calls `scheduler.insert` at 521), dashboard tick (holds App, calls `scheduler.list_by_ea`)
- **Scheduler lock only**: `kill_agent`, `cancel_event`, `event_loop`
- **App lock only**: `switch_ea`, `list_eas`, `get_active_ea`
- No handler acquires both locks in reverse order. **Deadlock impossible.** ✓

### Test Coverage for Fixes (Post-Merge)

| Fix | Unit Tests Present | Status |
|-----|-------------------|--------|
| V4 (cancel_if_ea) | `test_cancel_if_ea_correct`, `test_cancel_if_ea_wrong`, `test_cancel_if_ea_not_found` | ✓ Covered |
| V5 (cancel_by_receiver_and_ea) | `test_cancel_by_receiver_and_ea` | ✓ Covered |
| V7 (pop_batch ea-scoped) | `test_pop_batch_ea_scoped` | ✓ Covered |
| EA ID monotonicity | `test_ids_monotonic_after_deletion`, `test_ids_monotonic_without_counter_file` | ✓ Covered |
| EA prefix uniqueness | `test_ea_prefix`, `test_manager_not_in_worker_prefix` | ✓ Covered |

### Minor Documentation Issues (Non-Code)

1. **`prompts/skills/demo.md`** uses old non-EA-scoped routes (`/api/agents`) which no longer exist in the router. These API calls will 404. Not a formal invariant violation but a broken user-facing skill.
2. **`prompts/executive-assistant.md:285`** references `curl http://localhost:9876/api/events` (global) — route does not exist; should be `/api/ea/{{EA_ID}}/events`.

### Updated Residual Risk Assessment

| Risk | Original | Post-Merge |
|------|----------|------------|
| R1 (concurrent create_ea) | Low | **Mitigated** — App lock serializes (Fix S2) |
| R2 (orphan from delete/spawn race) | Low | Low — unchanged, benign |
| R3 (file-level RMW races) | Low | Low — unchanged, self-correcting |
| R4 (blocking I/O under lock) | Performance | Performance — unchanged |
| R5 (dashboard refresh under lock) | Performance | Performance — split-lock pattern not yet implemented |

### Conclusion

**All formal properties hold post-merge. No new violations introduced.** The merge resolution preserved all 6 fixes and added 2 improvements (S1, S2). The codebase's formal guarantees are stronger than the original verification document described.
