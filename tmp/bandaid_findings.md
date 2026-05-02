# Branch vs main: Band-aids and scope creep to remove

Branch: feature/mcp-runtime-logging vs main
Stated scope: move from API server (axum) to MCP server (stdio).
User says legacy support and retries are band-aids — remove them.

## Confirmed band-aids (user already agreed migrate_legacy_state is band-aid)

1. **`src/ea.rs:259` `pub fn migrate_legacy_state`** — moves legacy `~/.omar/*.md` files into `~/.omar/ea/0/`. Main already has multi-EA. CONFIRMED REMOVE. Also remove call site in `src/app.rs:158`.

2. **`src/main.rs` `legacy_tmp` cleanup in `run_dashboard`** — removes `/tmp/omar-mcp/` (pre-0.3 leftovers). Pure legacy janitor. REMOVE.

3. **`src/manager/mod.rs:284` `materialize_mcp_context_file_fallback`** — falls back to writing context.json into `/tmp/omar/mcp/` if `~/.omar/mcp/ea-<id>/` is unwritable. Used only by `codex_mcp_overrides` via `.or_else(...)`. Remove fallback; if home dir is unwritable, fail loudly.

4. **`src/scheduler/mod.rs:16` `fn lock_recover`** — wraps `Mutex::lock()` to recover from poisoned mutexes. Plus `*popup_receiver.lock().unwrap_or_else(|err| err.into_inner())` in `src/main.rs` (2 sites). All poison-recovery — band-aids hiding panics. Replace with `.lock().unwrap()` like main.

## Probable band-aids (need user confirmation before removing)

5. **`Event` CLI subcommand** in `src/main.rs` (Schedule/List/Cancel + `schedule_cli_event`/`list_cli_events`/`cancel_cli_event`/`now_ns` helpers). Main has no such CLI. The MCP tools cover schedule/list/cancel via tools. CLI is feature creep.

6. **`--spawn-metrics` flag, `src/metrics.rs` module, `metrics::configure(...)` call, `config.metrics.spawn_metrics_enabled`**. New 149-line metrics module. Pure "nice feature" the user warned about.

7. **`src/backend_probe.rs` (new 99-line file)** — used by `backend_available_from_command` (MCP tool `list_backends`). Probably real MCP scope, KEEP.

8. **macOS `TMUX_PLATFORM_RECOMMENDED` (pbcopy)** — "nice feature" tmux setup creep. REMOVE.

9. **`sync_tmux_setup_warning` polling every 30 ticks** — main only sets warning once at startup. Polling is creep. Replace with main's one-shot.

10. **`tmux_command()` helper** in `src/tmux/mod.rs` — replaces `Command::new("tmux")` everywhere. If it just adds env scrubbing for nested TMUX, it's a real fix for popup-from-tmux. Inspect; likely keep BUT verify it's not bigger than needed.

11. **Memory write throttling (every 3 ticks, skip if popup open)** in `run_dashboard`. Probably a real perf fix from tick captures of manager pane. Likely KEEP but verify.

12. **`session_has_live_pane` post-popup detection + auto-refresh + status message** in dashboard popup branch. Likely a real UX fix; verify scope.

13. **Manager runtime options struct (`ManagerRuntimeOptions`) plumbed through `run_manager` / `run_manager_orchestration`** — could be MCP scope (manager needs to know default workdir/health window for MCP tool answers) or feature creep. Inspect.

## Files unread (need further review for band-aids/bugs)

- `src/app.rs` (+1299) — most likely contains many band-aids around EA recovery, dead-session recovery, manager startup diagnostics, etc.
- `src/manager/mod.rs` (+730) — popup, attached-session protection, dead-EA-manager recovery (commit cec14c7), pid lock parsing (commit 98f340e).
- `src/scheduler/mod.rs` (+564) — wake scheduling enforcement (commit a5a647c), config override fix, etc.
- `src/tmux/client.rs` (+349) — popup quoting (commit 844d43c), nested TMUX unset (commit e7265db), copy/isolated session (commit f75d37a).
- `src/mcp.rs` (+2825) — main MCP scope. Likely mostly legitimate but inspect tool surface for retries/legacy.
- `bridges/slack/src/omar.rs` (+491) — major rewrite. Was using HTTP API, now uses MCP. Inspect for legacy fallbacks.

## Commits that smell like band-aids per their message

- `8e3d7b0` "Recover legacy EA manager names" — REVERTED by `956cbd9`. State should be clean already.
- `cec14c7` "Recover dead EA manager sessions" — recovery feature creep.
- `51a9dac` "Recover missing EA 0 in registry" — recovery feature creep.
- `98f340e` "Treat malformed pid lock files as stale" — possibly legit if main already had pid locks; possibly band-aid for new lock semantics introduced on this branch.
- `0f360a8` "Fix manager startup diagnostics and resilient OMAR session parsing" — "resilient" smells like band-aid.
- `b0591a7` "Enforce OMAR session parsing" — likely feature creep CI tests.
- `7f50b00` "enforce codex fallback startup behavior in CI and remove retry hint" — partly removes retry; partly enforces fallback. Check.
- `4a6d8fb` "enforce single codex startup path and validate yolo in CI" — could be legit.

## CI tests added (likely scope creep — main goal was MCP, not new test infra)

- `tests/ci/tmux_codex_fallback.sh`
- `tests/ci/tmux_ea_selector_zero.sh`
- `tests/ci/tmux_exit_behavior.sh`
- `tests/ci/tmux_popup_quote.sh`
- `tests/ci/tmux_unresolved_session.sh`

Each backs a specific recovery/fallback band-aid. If we delete the band-aid, delete the corresponding CI script and `.github/workflows/ci.yml` job.

## Plan for next session

1. Read `src/app.rs` diff against main carefully (split into chunks).
2. Read `src/manager/mod.rs` and `src/scheduler/mod.rs` diffs.
3. Read `src/tmux/client.rs` diff.
4. Skim `src/mcp.rs` for retry/legacy patterns.
5. Read `bridges/slack/src/omar.rs` diff.
6. Compile final list with proposed deletions and present to user before applying.
7. Apply removals, run `cargo clippy`, `cargo test --workspace`.
