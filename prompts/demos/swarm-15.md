# 15-Agent Swarm Demo

Demo OMAR's multi-level hierarchy with heterogeneous backends building a real system. 15 persistent agents across 4 levels, mixing all available backends with their default models.

## Structure

4-level binary tree (1 + 2 + 4 + 8 = 15 agents). All agents persist after completion — nothing gets killed.

```
                              claude-architect
                            /                   \
                 codex-backend               opencode-frontend
                /            \                /              \
         cursor-api     claude-data     codex-ui         opencode-infra
          /     \         /     \        /     \            /       \
  claude-rest codex-ws opencode-db cursor-auth claude-chat codex-admin opencode-cli cursor-tests
```

### Project: Real-Time Chat System (`junk/chat-system/`)

A working chat app with REST API, WebSocket messaging, SQLite storage, auth, browser UI, admin panel, CLI client, and full test suite.

### Agent Roles & Backend Assignments

All agents use their backend's default model — no model overrides.

| Name | Level | Role | Backend | Builds |
|------|-------|------|---------|--------|
| claude-architect  | 1 | Architect     | claude   | Coordinates everything, writes `package.json` |
| codex-backend     | 2 | Backend Lead  | codex    | Coordinates API + data teams, writes `src/server.js` |
| opencode-frontend | 2 | Frontend Lead | opencode | Coordinates UI + infra teams |
| cursor-api        | 3 | API Lead      | cursor   | Coordinates REST + WebSocket workers |
| claude-data       | 3 | Data Lead     | claude   | Coordinates storage + auth workers |
| codex-ui          | 3 | UI Lead       | codex    | Coordinates chat UI + admin workers |
| opencode-infra    | 3 | Infra Lead    | opencode | Coordinates CLI + test workers |
| claude-rest       | 4 | REST API      | claude   | `src/api.js` — Express routes |
| codex-ws          | 4 | WebSocket     | codex    | `src/ws.js` — Real-time messaging |
| opencode-db       | 4 | Storage       | opencode | `src/db.js` — SQLite layer |
| cursor-auth       | 4 | Auth          | cursor   | `src/auth.js` — JWT auth |
| claude-chat       | 4 | Chat UI       | claude   | `public/index.html` + `public/app.js` |
| codex-admin       | 4 | Admin Panel   | codex    | `public/admin.html` + `public/admin.js` |
| opencode-cli      | 4 | CLI Client    | opencode | `src/cli.js` — Terminal client |
| cursor-tests      | 4 | Test Suite    | cursor   | `test/` — Tests for all modules |

**Backend distribution:** claude ×4, codex ×4, opencode ×4, cursor ×3.

## How to Run

EA spawns only the root. Paste the root task below and use the `spawn_agent_session` MCP tool:

```
spawn_agent_session({
  "name": "claude-architect",
  "task": "<paste root agent task below>",
  "backend": "claude"
})
```

The root spawns the rest of the tree via its own `spawn_agent_session` calls. EA monitors `claude-architect` for `[TASK COMPLETE]` but does NOT kill any agents.

## Global Rules for Every Agent

- Use the OMAR `spawn_agent_session` MCP tool to spawn children. Never curl, never use built-in background agents.
- Always pass `parent` set to your own agent name so the hierarchy is visible in OMAR.
- Always pass `backend` (see table above). Never pass `model`.
- Use `schedule_event` for check-ins; never sleep loops.
- Do NOT kill any agents — all 15 must remain alive after completion.
- Reply `[TASK COMPLETE]` only once all children report `[TASK COMPLETE]`.
- After `[TASK COMPLETE]`, always call `notify_parent({"name": "YOUR_NAME", "summary": "..."})` to wake your parent immediately.

## Root Agent Task (`claude-architect`)

```
You are claude-architect, the root of a 15-agent swarm building a real-time chat system in junk/chat-system/.

Follow the global rules in the swarm-15 spec.

Steps:
1. Create junk/chat-system/ and write package.json (deps: express, ws, better-sqlite3, jsonwebtoken, bcryptjs, uuid; devDeps: jest, supertest; test script `jest --testMatch=**/test/*.test.js`).
2. Spawn codex-backend (backend="codex") with the Backend-Lead task below.
3. Spawn opencode-frontend (backend="opencode") with the Frontend-Lead task below.
4. Wait for `[CHILD COMPLETE]` messages from both. Use `get_agent` + `schedule_event` as fallback if a child hasn't notified after a reasonable time.
5. Run the tests: `cd junk/chat-system && npm install && npm test`.
6. Report [TASK COMPLETE] with results. Call `notify_parent({"name": "claude-architect", "summary": "<results>"})`. Do NOT kill any agents.
```

## Level-2 Tasks

### `codex-backend`

Spawns `cursor-api` (backend="cursor") and `claude-data` (backend="claude").

```
You are codex-backend, Backend Lead in the 15-agent swarm. Follow swarm-15 global rules.

1. Spawn cursor-api (backend="cursor") with the API-Lead task.
2. Spawn claude-data (backend="claude") with the Data-Lead task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback.
4. Write junk/chat-system/src/server.js: wire api.js, ws.js, db.js, and auth.js into an Express+WebSocket server on port 3000.
5. Report [TASK COMPLETE]. Call `notify_parent({"name": "codex-backend", "summary": "Backend complete."})`. Do NOT kill any agents.
```

### `opencode-frontend`

Spawns `codex-ui` (backend="codex") and `opencode-infra` (backend="opencode").

```
You are opencode-frontend, Frontend Lead in the 15-agent swarm. Follow swarm-15 global rules.

1. Spawn codex-ui (backend="codex") with the UI-Lead task.
2. Spawn opencode-infra (backend="opencode") with the Infra-Lead task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback.
4. Report [TASK COMPLETE]. Call `notify_parent({"name": "opencode-frontend", "summary": "Frontend complete."})`. Do NOT kill any agents.
```

## Level-3 Tasks

### `cursor-api` (API Lead)

Spawns `claude-rest` and `codex-ws`.

```
You are cursor-api. Coordinate REST and WebSocket workers for junk/chat-system/.
Follow swarm-15 global rules.

1. Spawn claude-rest (backend="claude") with the REST-API task.
2. Spawn codex-ws (backend="codex") with the WebSocket task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback. Verify src/api.js and src/ws.js exist.
4. Report [TASK COMPLETE]. Call `notify_parent({"name": "cursor-api", "summary": "API layer complete."})`. Do NOT kill any agents.
```

### `claude-data` (Data Lead)

Spawns `opencode-db` and `cursor-auth`.

```
You are claude-data. Coordinate storage and auth workers for junk/chat-system/.
Follow swarm-15 global rules.

1. Spawn opencode-db (backend="opencode") with the Storage task.
2. Spawn cursor-auth (backend="cursor") with the Auth task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback. Verify src/db.js and src/auth.js exist.
4. Report [TASK COMPLETE]. Call `notify_parent({"name": "claude-data", "summary": "Data layer complete."})`. Do NOT kill any agents.
```

### `codex-ui` (UI Lead)

Spawns `claude-chat` and `codex-admin`.

```
You are codex-ui. Coordinate chat UI and admin-panel workers for junk/chat-system/.
Follow swarm-15 global rules.

1. Spawn claude-chat (backend="claude") with the Chat-UI task.
2. Spawn codex-admin (backend="codex") with the Admin-Panel task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback. Verify their files exist.
4. Report [TASK COMPLETE]. Call `notify_parent({"name": "codex-ui", "summary": "UI complete."})`. Do NOT kill any agents.
```

### `opencode-infra` (Infra Lead)

Spawns `opencode-cli` and `cursor-tests`.

```
You are opencode-infra. Coordinate CLI client and test-suite workers for junk/chat-system/.
Follow swarm-15 global rules.

1. Spawn opencode-cli (backend="opencode") with the CLI task.
2. Spawn cursor-tests (backend="cursor") with the Test-Suite task.
3. Wait for `[CHILD COMPLETE]` from both. Use `get_agent` + `schedule_event` as fallback. Verify their files exist.
4. Report [TASK COMPLETE]. Call `notify_parent({"name": "opencode-infra", "summary": "Infra complete."})`. Do NOT kill any agents.
```

## Level-4 Worker Tasks

### `claude-rest` — REST API

```
Build junk/chat-system/src/api.js — an Express router:
- POST /api/signup, POST /api/login (use auth.js)
- GET /api/rooms, POST /api/rooms
- GET /api/rooms/:id/messages, POST /api/rooms/:id/messages
- GET /api/users/me
JSON-only; auth routes public, others require JWT via the auth middleware.
auth.js exports: hashPassword, comparePassword, generateToken, authMiddleware.
db.js exports:   createUser, findUser, getRooms, createRoom, getMessages, addMessage.
Report [TASK COMPLETE], then call `notify_parent({"name": "claude-rest", "summary": "REST API complete: src/api.js written."})`.
```

### `codex-ws` — WebSocket

```
Build junk/chat-system/src/ws.js using the ws library:
- On connection: authenticate via token query param, track connected users.
- Message types: chat (broadcast to room), join_room, leave_room, typing.
- Track presence: who is online, who is in which room.
- Export setupWebSocket(server) that attaches WS to an HTTP server.
auth.js exports verifyToken. db.js exports addMessage.
Report [TASK COMPLETE], then call `notify_parent({"name": "codex-ws", "summary": "WebSocket complete: src/ws.js written."})`.
```

### `opencode-db` — Storage

```
Build junk/chat-system/src/db.js using better-sqlite3 (synchronous):
- initDb(): tables users(id,username,password_hash,created_at), rooms(id,name,created_by,created_at), messages(id,room_id,user_id,content,created_at).
- createUser/findUser/findUserById
- getRooms/createRoom/getRoom
- getMessages(roomId, limit=50)/addMessage
Export all. Report [TASK COMPLETE], then call `notify_parent({"name": "opencode-db", "summary": "Storage complete: src/db.js written."})`.
```

### `cursor-auth` — Auth

```
Build junk/chat-system/src/auth.js:
- hashPassword / comparePassword (bcryptjs)
- generateToken (JWT, {id, username}, 24h expiry; secret from env or default)
- verifyToken
- authMiddleware (Express: read Bearer token from Authorization, attach req.user)
Export all. Report [TASK COMPLETE], then call `notify_parent({"name": "cursor-auth", "summary": "Auth complete: src/auth.js written."})`.
```

### `claude-chat` — Chat UI

```
Build junk/chat-system/public/index.html + public/app.js:
- Responsive HTML, login/signup forms, room list sidebar, message area, input box.
- app.js: /api for auth, WebSocket for real-time messaging, typing indicators, online users.
- Dark theme, embedded CSS, no frameworks.
Report [TASK COMPLETE], then call `notify_parent({"name": "claude-chat", "summary": "Chat UI complete: public/index.html and public/app.js written."})`.
```

### `codex-admin` — Admin Panel

```
Build junk/chat-system/public/admin.html + public/admin.js:
- List all users, list rooms, message counts per room, create/delete rooms.
- Match main chat UI's dark theme, embedded CSS.
Report [TASK COMPLETE], then call `notify_parent({"name": "codex-admin", "summary": "Admin panel complete: public/admin.html and public/admin.js written."})`.
```

### `opencode-cli` — CLI Client

```
Build junk/chat-system/src/cli.js (terminal chat client):
- Commands: /login <user> <pass>, /signup <user> <pass>, /rooms, /join <room>, /create <room>, /quit.
- Plain typed text sends as message in current room.
- Receive via WebSocket, display in terminal.
- readline for input, ws for WebSocket. Defaults to http://localhost:3000 (configurable via CLI arg).
- Proper CLI entry point (`#!/usr/bin/env node`).
Report [TASK COMPLETE], then call `notify_parent({"name": "opencode-cli", "summary": "CLI client complete: src/cli.js written."})`.
```

### `cursor-tests` — Test Suite

```
Build junk/chat-system/test/:
- db.test.js: all db.js functions (users/rooms/messages CRUD).
- auth.test.js: password hashing, token generation/verification, middleware.
- api.test.js: REST endpoints via supertest (signup/login/rooms/messages).
- Use jest + supertest. Fresh in-memory / temp SQLite per test for isolation.
- At least 20 test cases total.
Add the `test` script to package.json if it's not already there.
Report [TASK COMPLETE], then call `notify_parent({"name": "cursor-tests", "summary": "Test suite complete: test/ directory written with 20+ cases."})`.
```

## Expected Behavior

1. EA spawns `claude-architect`.
2. `claude-architect` creates project scaffolding, spawns `codex-backend` and `opencode-frontend`.
3. `codex-backend` spawns `cursor-api` and `claude-data`.
4. `opencode-frontend` spawns `codex-ui` and `opencode-infra`.
5. Level-3 leads each spawn their two level-4 workers.
6. 8 leaf workers build code in parallel across all 4 backends.
7. Completion cascades up: leaves → level 3 → level 2 → root.
8. Root runs `npm test`, reports `[TASK COMPLETE]`.
9. **All 15 agents remain alive on the dashboard.**

## Success Criteria

- All 15 agents visible on the dashboard simultaneously during execution.
- At least 3 different backends active at the same time.
- Root reports `[TASK COMPLETE]` with passing tests.
- All 15 agents remain alive after completion (persistent — no kills).
- The chat system is functional: `cd junk/chat-system && npm install && node src/server.js`.

## Dashboard Visual

When running, the OMAR dashboard shows all 15 agents with their backends:

```
┌─────────────────────────────────────────────────────────────────────┐
│  claude-architect   codex-backend      opencode-frontend            │
│  cursor-api         claude-data        codex-ui        opencode-infra│
│  claude-rest        codex-ws           opencode-db     cursor-auth  │
│  claude-chat        codex-admin        opencode-cli    cursor-tests │
└─────────────────────────────────────────────────────────────────────┘
```

## Tips

- Leaf workers may finish before their siblings — level-3 leads should wait for both.
- If a worker stalls, the lead above should notice and `replace_stuck_task_agent` is NOT used here (no kills). Instead, send a nudge via `schedule_event`.
- `cursor-tests` needs the other modules to exist first — it may need to wait or write tests against expected interfaces.
- The root's final `npm test` is the integration check that validates everything works together.
