# Behavior parity findings: branch MCP vs main API

Compared field-by-field, side-effect-by-side-effect.
Status legend:
- [BUG] real divergence — fix on branch
- [OK] intentional + correct
- [N/A] cosmetic-only diff (different message string, etc.)

## list_backends
- main: `Command::new(executable).arg("--version").output().is_ok_and(|o| o.status.success())`.
- branch: same logic via `backend_available_from_command()` in src/backend_probe.rs.
- Diff: branch ALSO probes for hangs (timeout). main does not.
- [OK] new file backend_probe.rs is in user's accepted scope (item 7 of bandaid_findings).

## list_eas
- main response per EA: `{id, name, description, agent_count, is_active}` plus top-level `{eas, active}`.
- branch response per EA: `{id, name, description, is_active}` plus `{eas, active}` — **agent_count missing**.
- [BUG] add agent_count.

## get_active_ea
- main: `{"active": id}`.
- branch: `{"ea_id": id, "name": name}`.
- Different shape but branch carries a strict superset minus 'active' key. LLM-callable; rename effect.
- [BUG] dashboard and any other consumer of the existing `{"active": id}` contract still expects that. To match parity, keep `active` and ALSO include name. Current branch drops `active`.

## switch_ea
- main: validates registry, calls `app.switch_ea(req.id)` (which both persists the active-EA file AND updates in-memory App). Returns `{status, message}`.
- branch: validates registry, calls `ea::save_active_ea` (persists only; no in-memory update because MCP server is a separate process per EA). Returns `{ea_id, name}`.
- Functional parity: persistence is the same. The MCP server has no in-memory App. **OK as-is.**
- Return shape differs but per user's "renames are not an issue", **N/A**.

## create_ea
- main response: `{id, name, description, agent_count: 0, is_active: false}`.
- branch response: `{id, name, description}`.
- main holds App.lock().await across register_ea (concurrency serialization). Branch is one-process-per-EA so not racy.
- [BUG] add `agent_count: 0, is_active: false` for parity.

## delete_ea
- main: switches active EA to next-lowest id if the deleted EA was active.
- branch: does NOT update active-EA pointer on disk after deletion.
- [BUG-CRITICAL] if active EA is deleted, dashboard's resolve_active_ea() would see a missing EA next launch. Even though main does this in-memory, on-disk parity requires writing the new active EA.
- main: kill_session errors propagate as 500.
- branch: kill_session errors swallowed via `let _`.
- [BUG] propagate kill failures.
- branch adds attached-session-guard (protects against accidentally killing an attached terminal). This is a NEW feature, not a band-aid. Test exists for it. **KEEP** (Section B / accepted feature).

## list_agents
- main: filters by manager_session, returns extra fields per agent (id, status, health, parent, etc — need to check exact main shape).
- branch: returns `{id, health, last_output}` only.
- Need to check what fields main has.

## (continuing after deeper read)
