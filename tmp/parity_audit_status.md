# Status of behavior-parity + finish-the-plan task

## State of repo as of this checkpoint

- Branch: `feature/mcp-runtime-logging`
- Section A fixes (from prior session) are still **uncommitted** on disk:
  - src/app.rs, src/ea.rs, src/main.rs, src/manager/mod.rs, src/scheduler/mod.rs
- New work this session: NONE — only audit prep.
- Saved files in tmp/: bandaid_findings.md, main_handlers.rs, main_api_mod.rs, main_api_models.rs (1970 + 99 + 384 lines).

## Tool ↔ handler one-line cross-reference (built this session)

| MCP tool                | API handler             | Notes                                                                 |
|-------------------------|-------------------------|----------------------------------------------------------------------|
| list_backends           | list_backends           | both return list of backend objects                                   |
| list_eas                | list_eas                |                                                                       |
| get_active_ea           | get_active_ea           |                                                                       |
| switch_ea               | switch_ea               |                                                                       |
| create_ea               | create_ea               |                                                                       |
| delete_ea               | delete_ea               |                                                                       |
| list_agents             | list_agents             |                                                                       |
| get_agent               | get_agent               |                                                                       |
| get_agent_summary       | get_agent_summary       |                                                                       |
| update_agent_status     | update_agent_status     |                                                                       |
| spawn_agent             | spawn_agent             | 200+ lines each — most likely place for divergences                   |
| kill_agent              | kill_agent              |                                                                       |
| send_input              | send_input              |                                                                       |
| list_projects           | list_projects           |                                                                       |
| add_project             | add_project             |                                                                       |
| complete_project        | complete_project        |                                                                       |
| omar_wake_later         | schedule_event          | renamed; user said renames are fine                                   |
| list_events             | list_events             |                                                                       |
| cancel_event            | cancel_event            |                                                                       |
| log_justification       | log_justification       |                                                                       |
| computer_*              | computer_*              | 8 tools, 1:1                                                          |
| append_manager_note     | (none)                  | NEW. Confirm with user; behavior parity n/a                          |
| slack_reply             | (none)                  | NEW. Plausibly required by slack-bridge MCP rewrite                   |
| (none)                  | health                  | dropped. JSON-RPC initialize handles this                             |

## What still needs to happen (continue in next session)

### Step 1 — commit Section A fixes
```
git add -u src/app.rs src/ea.rs src/main.rs src/manager/mod.rs src/scheduler/mod.rs
git commit -m "remove legacy migration / poison-recovery / tmp fallback band-aids"
```

### Step 2 — run full CI matrix locally and fix red
- cargo fmt --all -- --check
- cargo check --all-targets
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test --workspace -- --test-threads=1
- bash tests/ci/tmux_popup_quote.sh
- bash tests/ci/tmux_exit_behavior.sh
- bash tests/ci/tmux_unresolved_session.sh
- bash tests/ci/tmux_codex_fallback.sh
- bash tests/ci/tmux_ea_selector_zero.sh

### Step 3 — behavior-parity audit (THE main job per user)

Methodically diff each pair below against tmp/main_handlers.rs (line ranges shown):

Priority order (most behavior, most likely divergence):

1. **spawn_agent** — main handlers.rs L592–805 vs branch mcp.rs (search for `fn spawn_agent`).
   - Args, project lookup, name dedup, backend/command mutual-exclusion, model whitelist, workdir default, parent rules, initial-prompt delivery vs supports_initial_prompt_delivery, auto-create raw vs backend session.
2. **delete_ea** — main L241–366 vs branch.
   - "only remaining EA" guard, kill sessions, remove state dir, cancel scheduled events.
3. **kill_agent** — main L805–860 vs branch.
   - Manager guard, attached-session guard (NEW on branch — verify it exists in main; if it doesn't, that's scope creep but user has accepted recovery features stay).
4. **send_input** — main L860–913 vs branch.
   - Enter handling, escape, alive check.
5. **schedule_event vs omar_wake_later** — main L979–1034 vs branch.
   - delay_seconds vs timestamp_ns precedence; recurring; receiver='ea' resolution.
6. **list_events / cancel_event** — main L1034–1259 vs branch.
   - EA-scoping (cancel must fail for events belonging to other EA).
7. **list_eas / create_ea / get_active_ea / switch_ea** — main L172–406 vs branch.
8. **list_agents / get_agent / get_agent_summary / update_agent_status** — main L406–592 vs branch.
9. **list_projects / add_project / complete_project** — main L913–979 vs branch.
10. **log_justification** — main L1260–1334 vs branch.
11. **computer_*** — main L1334–1633 vs branch. 8 tools.

For each, write findings into `tmp/parity_$tool.md` with:
- request schema diff (field names, required, defaults)
- response shape diff
- error semantics diff (status codes don't matter; we want JSON-RPC tool-result error vs success match)
- internal logic diff (input validation, side effects, ordering)

### Step 4 — fix every divergence found in Step 3 to match main.

### Step 5 — append_manager_note: ASK USER whether to keep or delete.

### Step 6 — re-evaluate the 8 "smelly" commits individually:
- 8e3d7b0 (reverted), cec14c7, 51a9dac, 98f340e, 0f360a8, b0591a7, 7f50b00, 4a6d8fb.
- For each: read its diff (`git show <sha>`), classify as "real bug fix needed in MCP" or "band-aid for problem this branch introduced".

### Step 7 — read the unread big files chunk-by-chunk:
- src/app.rs (+1299 vs main)
- src/manager/mod.rs (+730 net)
- src/scheduler/mod.rs (+564 net)
- src/tmux/client.rs (+349)
- bridges/slack/src/omar.rs (+491)

### Step 8 — final pass: commit, push, run CI on origin, verify green.

## Why I stopped this session

Hit ~94% context after orienting and capturing the cross-reference table.
No code changes made this session. Original disk state preserved
(Section A fixes from previous session still pending commit).
