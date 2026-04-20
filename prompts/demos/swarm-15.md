# 15-Agent Swarm Demo

Demo OMAR's multi-level hierarchy with heterogeneous backends building a real system. 15 persistent agents across 4 levels, mixing all available backends with default models.

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
| claude-architect | 1 | Architect | claude | Coordinates everything, writes `package.json` |
| codex-backend | 2 | Backend Lead | codex | Coordinates API + data teams, writes `src/server.js` |
| opencode-frontend | 2 | Frontend Lead | opencode | Coordinates UI + infra teams |
| cursor-api | 3 | API Lead | cursor | Coordinates REST + WebSocket workers |
| claude-data | 3 | Data Lead | claude | Coordinates storage + auth workers |
| codex-ui | 3 | UI Lead | codex | Coordinates chat UI + admin workers |
| opencode-infra | 3 | Infra Lead | opencode | Coordinates CLI + test workers |
| claude-rest | 4 | REST API | claude | `src/api.js` — Express routes |
| codex-ws | 4 | WebSocket | codex | `src/ws.js` — Real-time messaging |
| opencode-db | 4 | Storage | opencode | `src/db.js` — SQLite layer |
| cursor-auth | 4 | Auth | cursor | `src/auth.js` — JWT auth |
| claude-chat | 4 | Chat UI | claude | `public/index.html` + `public/app.js` |
| codex-admin | 4 | Admin Panel | codex | `public/admin.html` + `public/admin.js` |
| opencode-cli | 4 | CLI Client | opencode | `src/cli.js` — Terminal client |
| cursor-tests | 4 | Test Suite | cursor | `test/` — Tests for all modules |

**Backend distribution:** claude ×4, codex ×4, opencode ×4, cursor ×3.

## How to Run

EA spawns only the root agent `claude-architect`:

```bash
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "claude-architect",
    "task": "<paste root agent task below>",
    "backend": "claude"
  }'
```

The root spawns the entire tree. EA monitors `claude-architect` for `[TASK COMPLETE]` but does NOT kill any agents.

## Root Agent Task

```
You are claude-architect, the root of a 15-agent swarm building a real-time chat system in junk/chat-system/.

## Your Job
1. Create junk/chat-system/ and write package.json (deps: express, ws, better-sqlite3, jsonwebtoken, bcryptjs, uuid, and dev deps: jest, supertest)
2. Spawn your two children (codex-backend and opencode-frontend) with their full task descriptions
3. Monitor them until both report [TASK COMPLETE]
4. Run the full test suite: cd junk/chat-system && npm install && npm test
5. Report [TASK COMPLETE] with results — do NOT kill any agents

## CRITICAL RULES
- Do NOT kill any agents. All 15 agents must remain alive after completion.
- Use the OMAR events API for scheduling check-ins, never use sleep loops.
- When both children report [TASK COMPLETE], run tests and then report [TASK COMPLETE] yourself.
- Do NOT specify "model" when spawning agents. Only specify "backend".

## Spawn codex-backend (Backend Lead)

curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "codex-backend",
    "task": "You are codex-backend, Backend Lead in a 15-agent swarm. You coordinate the backend of a real-time chat system in junk/chat-system/.\n\n## Your Job\n1. Spawn cursor-api (API Lead) and claude-data (Data Lead)\n2. Monitor both until they report [TASK COMPLETE]\n3. Write junk/chat-system/src/server.js that imports and wires together api.js, ws.js, db.js, and auth.js into a working Express+WebSocket server on port 3000\n4. Report [TASK COMPLETE] — do NOT kill any agents\n\n## RULES\n- Do NOT specify \"model\" when spawning. Only specify \"backend\".\n- Use events API for check-ins, not sleep. Do NOT kill any agents.\n\n## Spawn cursor-api\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \"Content-Type: application/json\" -d \x27{\"name\": \"cursor-api\", \"task\": \"You are cursor-api, API Lead. You coordinate REST and WebSocket workers for junk/chat-system/.\\n\\n## Your Job\\n1. Spawn claude-rest and codex-ws\\n2. Monitor both until they report [TASK COMPLETE]\\n3. Verify their files exist and are syntactically valid\\n4. Report [TASK COMPLETE] — do NOT kill any agents\\n\\nRULES: Do NOT specify model when spawning. Only specify backend. Use events API, not sleep. Do NOT kill any agents.\\n\\n## Spawn claude-rest\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"claude-rest\\\", \\\"task\\\": \\\"You are claude-rest. Build junk/chat-system/src/api.js — an Express router with REST endpoints:\\n- POST /api/signup, POST /api/login (use auth.js)\\n- GET /api/rooms, POST /api/rooms (create room)\\n- GET /api/rooms/:id/messages, POST /api/rooms/:id/messages\\n- GET /api/users/me\\nAll routes expect JSON. Auth routes are public, others require JWT via auth middleware.\\nAssume auth.js exports: hashPassword, comparePassword, generateToken, authMiddleware.\\nAssume db.js exports: createUser, findUser, getRooms, createRoom, getMessages, addMessage.\\nWrite clean Express code. When done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"claude\\\"}\x27\\n\\n## Spawn codex-ws\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"codex-ws\\\", \\\"task\\\": \\\"You are codex-ws. Build junk/chat-system/src/ws.js — a WebSocket handler using the ws library:\\n- On connection: authenticate via token query param, track connected users\\n- Message types: chat (broadcast to room), join_room, leave_room, typing (broadcast typing indicator)\\n- Track presence: who is online, who is in which room\\n- Export a function setupWebSocket(server) that attaches WS to an HTTP server\\nAssume auth.js exports: verifyToken. Assume db.js exports: addMessage.\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"codex\\\"}\x27\", \"backend\": \"cursor\"}\x27\n\n## Spawn claude-data\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \"Content-Type: application/json\" -d \x27{\"name\": \"claude-data\", \"task\": \"You are claude-data, Data Lead. You coordinate storage and auth workers for junk/chat-system/.\\n\\n## Your Job\\n1. Spawn opencode-db and cursor-auth\\n2. Monitor both until they report [TASK COMPLETE]\\n3. Verify their files exist and are syntactically valid\\n4. Report [TASK COMPLETE] — do NOT kill any agents\\n\\nRULES: Do NOT specify model when spawning. Only specify backend. Use events API, not sleep. Do NOT kill any agents.\\n\\n## Spawn opencode-db\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"opencode-db\\\", \\\"task\\\": \\\"You are opencode-db. Build junk/chat-system/src/db.js — SQLite database layer using better-sqlite3:\\n- initDb() — create tables: users(id,username,password_hash,created_at), rooms(id,name,created_by,created_at), messages(id,room_id,user_id,content,created_at)\\n- createUser(username, passwordHash) / findUser(username) / findUserById(id)\\n- getRooms() / createRoom(name, createdBy) / getRoom(id)\\n- getMessages(roomId, limit=50) / addMessage(roomId, userId, content)\\nAll functions are synchronous (better-sqlite3 is sync). Export all functions.\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"opencode\\\"}\x27\\n\\n## Spawn cursor-auth\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"cursor-auth\\\", \\\"task\\\": \\\"You are cursor-auth. Build junk/chat-system/src/auth.js — JWT authentication module:\\n- hashPassword(password) — bcryptjs hash\\n- comparePassword(password, hash) — bcryptjs compare\\n- generateToken(user) — JWT with {id, username}, 24h expiry, secret from env or default\\n- verifyToken(token) — decode and return payload\\n- authMiddleware(req, res, next) — Express middleware, reads Bearer token from Authorization header, attaches req.user\\nExport all functions. Use jsonwebtoken and bcryptjs packages.\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"cursor\\\"}\x27\", \"backend\": \"claude\"}\x27",
    "backend": "codex"
  }'
```

## Spawn opencode-frontend (Frontend Lead)

```bash
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "opencode-frontend",
    "task": "You are opencode-frontend, Frontend Lead in a 15-agent swarm. You coordinate the frontend and tooling of a real-time chat system in junk/chat-system/.\n\n## Your Job\n1. Spawn codex-ui (UI Lead) and opencode-infra (Infra Lead)\n2. Monitor both until they report [TASK COMPLETE]\n3. Report [TASK COMPLETE] — do NOT kill any agents\n\n## RULES\n- Do NOT specify \"model\" when spawning. Only specify \"backend\".\n- Use events API for check-ins, not sleep. Do NOT kill any agents.\n\n## Spawn codex-ui\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \"Content-Type: application/json\" -d \x27{\"name\": \"codex-ui\", \"task\": \"You are codex-ui, UI Lead. You coordinate chat UI and admin panel workers for junk/chat-system/.\\n\\n## Your Job\\n1. Spawn claude-chat and codex-admin\\n2. Monitor both until they report [TASK COMPLETE]\\n3. Verify their files exist\\n4. Report [TASK COMPLETE] — do NOT kill any agents\\n\\nRULES: Do NOT specify model when spawning. Only specify backend. Use events API, not sleep. Do NOT kill any agents.\\n\\n## Spawn claude-chat\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"claude-chat\\\", \\\"task\\\": \\\"You are claude-chat. Build junk/chat-system/public/index.html and junk/chat-system/public/app.js — a browser-based chat interface:\\n- index.html: clean responsive HTML with login/signup forms, room list sidebar, message area, input box\\n- app.js: connects to /api for auth and WebSocket for real-time messaging\\n- Features: login/signup, room list, join room, send messages, see messages in real-time, typing indicators, online users list\\n- Style it with embedded CSS (no frameworks) — dark theme, modern look\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"claude\\\"}\x27\\n\\n## Spawn codex-admin\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"codex-admin\\\", \\\"task\\\": \\\"You are codex-admin. Build junk/chat-system/public/admin.html and junk/chat-system/public/admin.js — an admin panel:\\n- admin.html: clean HTML page for managing the chat system\\n- admin.js: fetches data from /api endpoints\\n- Features: list all users, list all rooms, see message counts per room, create/delete rooms\\n- Style with embedded CSS — match dark theme of main chat UI\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"codex\\\"}\x27\", \"backend\": \"codex\"}\x27\n\n## Spawn opencode-infra\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \"Content-Type: application/json\" -d \x27{\"name\": \"opencode-infra\", \"task\": \"You are opencode-infra, Infra Lead. You coordinate CLI client and test suite workers for junk/chat-system/.\\n\\n## Your Job\\n1. Spawn opencode-cli and cursor-tests\\n2. Monitor both until they report [TASK COMPLETE]\\n3. Verify their files exist\\n4. Report [TASK COMPLETE] — do NOT kill any agents\\n\\nRULES: Do NOT specify model when spawning. Only specify backend. Use events API, not sleep. Do NOT kill any agents.\\n\\n## Spawn opencode-cli\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"opencode-cli\\\", \\\"task\\\": \\\"You are opencode-cli. Build junk/chat-system/src/cli.js — a terminal chat client:\\n- Commands: /login <user> <pass>, /signup <user> <pass>, /rooms (list), /join <room>, /create <room>, /quit\\n- After joining a room, typed text sends as messages\\n- Receives messages via WebSocket and displays them in terminal\\n- Uses readline for input, ws for WebSocket connection\\n- Connects to http://localhost:3000 by default (configurable via CLI arg)\\nMake it a proper CLI entry point (#!/usr/bin/env node).\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"opencode\\\"}\x27\\n\\n## Spawn cursor-tests\\ncurl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents -H \\\"Content-Type: application/json\\\" -d \x27{\\\"name\\\": \\\"cursor-tests\\\", \\\"task\\\": \\\"You are cursor-tests. Build the test suite for junk/chat-system/:\\n- Create junk/chat-system/test/db.test.js — test all db.js functions (CRUD for users, rooms, messages)\\n- Create junk/chat-system/test/auth.test.js — test password hashing, token generation/verification, middleware\\n- Create junk/chat-system/test/api.test.js — test REST endpoints with supertest (signup, login, rooms, messages)\\n- Add test script to package.json if not present: test: jest --testMatch=**/test/*.test.js\\n- Use jest as the test framework. Use supertest for HTTP tests.\\n- Tests should create a fresh in-memory or temp SQLite db for isolation.\\n- Aim for at least 20 test cases total.\\nWhen done, report [TASK COMPLETE].\\\", \\\"backend\\\": \\\"cursor\\\"}\x27\", \"backend\": \"opencode\"}\x27",
    "backend": "opencode"
  }'
```
```

## Expected Behavior

1. EA spawns `claude-architect`
2. `claude-architect` creates project scaffolding, spawns `codex-backend` and `opencode-frontend`
3. `codex-backend` spawns `cursor-api` and `claude-data`
4. `opencode-frontend` spawns `codex-ui` and `opencode-infra`
5. Level-3 leads each spawn their two level-4 workers
6. 8 leaf workers build code in parallel across all 4 backends
7. Completion cascades up: leaves → level 3 → level 2 → root
8. Root runs `npm test`, reports `[TASK COMPLETE]`
9. **All 15 agents remain alive on the dashboard**

## Success Criteria

- All 15 agents visible on the dashboard simultaneously during execution
- At least 3 different backends active at the same time
- Root reports `[TASK COMPLETE]` with passing tests
- All 15 agents remain alive after completion (persistent — no kills)
- The chat system is functional: `cd junk/chat-system && npm install && node src/server.js`

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

- Leaf workers may finish before their siblings — level-3 leads should wait for both
- If a worker gets stuck (especially cursor/opencode), the lead above it should notice and can nudge it
- The test suite worker (cursor-tests) needs the other modules to exist first — it may need to wait or write tests against expected interfaces
- The root's final `npm test` run is the integration check that validates everything works together
