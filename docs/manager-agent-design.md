# Manager Agent Design

## Overview

A manager agent that orchestrates multiple worker agents, allowing users to issue high-level commands that get broken down into parallel sub-tasks.

## User Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                         USER                                     │
│                          │                                       │
│                          ▼                                       │
│  "Build a REST API with auth, database, and tests"              │
│                          │                                       │
└──────────────────────────┼───────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    MANAGER AGENT                                 │
│                                                                  │
│  Proposes:                                                       │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ I suggest 4 worker agents:                               │    │
│  │                                                          │    │
│  │ Agent 1 (api):    Set up Express server, routes         │    │
│  │ Agent 2 (auth):   Implement JWT auth middleware         │    │
│  │ Agent 3 (db):     Design schema, Prisma setup           │    │
│  │ Agent 4 (test):   Write integration tests               │    │
│  │                                                          │    │
│  │ Approve? [Y/n/modify]                                    │    │
│  └─────────────────────────────────────────────────────────┘    │
│                          │                                       │
└──────────────────────────┼───────────────────────────────────────┘
                           │
                    User: "Y"
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    WORKER AGENTS                                 │
│                                                                  │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐            │
│  │ Agent 1 │  │ Agent 2 │  │ Agent 3 │  │ Agent 4 │            │
│  │  (api)  │  │ (auth)  │  │  (db)   │  │ (test)  │            │
│  │         │  │         │  │         │  │         │            │
│  │ Working │  │ Working │  │ Working │  │ Waiting │            │
│  └─────────┘  └─────────┘  └─────────┘  └─────────┘            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Architecture Options

### Option A: Manager as Claude Session + OMAR Commands

The manager is a regular Claude session with special system prompt. OMAR provides CLI commands that the manager can invoke.

```
┌─────────────────────────────────────────────┐
│           Manager Agent (Claude)            │
│                                             │
│  System prompt includes:                    │
│  - You are a manager agent                  │
│  - Use /omar commands to control workers     │
│  - Available commands:                      │
│    /omar spawn <name> <task>                 │
│    /omar send <name> <message>               │
│    /omar status                              │
│    /omar wait <name>                         │
│                                             │
└─────────────────────────────────────────────┘
```

**Pros:**
- Simple to implement - just add CLI commands
- Manager is flexible (Claude handles the planning)
- Works with existing infrastructure

**Cons:**
- Manager must parse and execute text commands
- No structured communication protocol
- Hard to enforce approval flow

### Option B: OMAR Native Orchestration

OMAR itself becomes the orchestrator with a TUI for manager mode.

```
┌─────────────────────────────────────────────┐
│              OMAR Manager Mode               │
│                                             │
│  ┌───────────────────────────────────────┐  │
│  │ Your request:                         │  │
│  │ > Build a REST API with auth          │  │
│  └───────────────────────────────────────┘  │
│                                             │
│  ┌───────────────────────────────────────┐  │
│  │ Proposed plan:                        │  │
│  │ [ ] Agent 1: API routes               │  │
│  │ [ ] Agent 2: Auth middleware          │  │
│  │ [ ] Agent 3: Database                 │  │
│  │                                       │  │
│  │ [Approve] [Modify] [Cancel]           │  │
│  └───────────────────────────────────────┘  │
│                                             │
│  Worker Status:                             │
│  ● api:  Working - creating routes...      │
│  ● auth: Waiting for api to finish         │
│  ○ db:   Idle                              │
│                                             │
└─────────────────────────────────────────────┘
```

**Pros:**
- Clean UI for approval flow
- Structured task management
- Can enforce dependencies between agents

**Cons:**
- More complex to implement
- Planning logic must be built in or call Claude API
- Less flexible than pure Claude approach

### Option C: Hybrid - Manager Claude + Structured Protocol

Manager is Claude, but communicates via structured JSON protocol that OMAR interprets.

```
Manager Claude outputs:
{
  "action": "propose_plan",
  "agents": [
    {"name": "api", "task": "Set up Express server", "depends_on": []},
    {"name": "auth", "task": "JWT middleware", "depends_on": ["api"]},
    {"name": "db", "task": "Prisma schema", "depends_on": []},
    {"name": "test", "task": "Integration tests", "depends_on": ["api", "auth", "db"]}
  ]
}

User approves via OMAR UI.

OMAR then sends to each worker:
{
  "role": "worker",
  "task": "Set up Express server with routes for /users, /posts",
  "context": "Part of REST API project. Other agents handling: auth, db, tests",
  "report_to": "manager"
}
```

**Pros:**
- Best of both worlds
- Structured but flexible
- Can validate plans before execution

**Cons:**
- Requires Claude to output structured format reliably
- More complex protocol design

## Recommended Approach: Option C (Hybrid)

### Components

1. **Manager Session**
   - Special Claude session with orchestration system prompt
   - Outputs structured JSON plans
   - Monitors worker status

2. **OMAR Protocol Layer**
   - Parses manager JSON output
   - Presents plan to user for approval
   - Spawns/controls worker agents
   - Routes status updates back to manager

3. **Worker Sessions**
   - Regular Claude sessions with worker context
   - Know they're part of a larger project
   - Can signal completion/blockers

### Implementation Steps

#### Phase 1: Basic Manager Commands
```bash
omar manager start              # Start manager session
omar manager propose "task"     # Ask manager to break down task
omar manager approve            # Approve proposed plan
omar manager status             # Show all agent status
```

#### Phase 2: Structured Communication
- Define JSON protocol for manager ↔ OMAR
- Add plan parsing and validation
- Implement approval UI in dashboard

#### Phase 3: Worker Coordination
- Workers report completion via special markers
- Manager can reassign/replan based on progress
- Handle failures and retries

#### Phase 4: Advanced Features
- Dependency graphs between workers
- Automatic aggregation agent
- Progress visualization
- Persistent project state

## Protocol Specification

### Manager → OMAR Messages

```json
// Propose a plan
{
  "type": "plan",
  "id": "plan-001",
  "description": "Build REST API",
  "agents": [
    {
      "name": "api",
      "role": "API Developer",
      "task": "Create Express server with /users and /posts endpoints",
      "depends_on": [],
      "estimated_complexity": "medium"
    }
  ]
}

// Send message to worker
{
  "type": "send",
  "target": "api",
  "message": "Please also add /comments endpoint"
}

// Query status
{
  "type": "query",
  "target": "all"  // or specific agent name
}
```

### OMAR → Manager Messages

```json
// Plan approval
{
  "type": "plan_approved",
  "id": "plan-001",
  "modifications": []  // or list of user changes
}

// Worker status update
{
  "type": "status",
  "agent": "api",
  "state": "working",
  "last_output": "Creating user routes..."
}

// Worker completion
{
  "type": "complete",
  "agent": "api",
  "summary": "Created /users and /posts endpoints"
}
```

### Worker Context Injection

When spawning a worker, OMAR injects context:

```
You are a worker agent in a multi-agent project.

PROJECT: Build REST API
YOUR ROLE: API Developer
YOUR TASK: Create Express server with /users and /posts endpoints

OTHER AGENTS:
- auth: Handling JWT authentication (depends on your work)
- db: Setting up Prisma schema (parallel)
- test: Writing tests (waiting for all)

INSTRUCTIONS:
- Focus only on your assigned task
- When done, end with: [TASK COMPLETE]
- If blocked, say: [BLOCKED: reason]
- If you need input, say: [NEED INPUT: question]
```

## Open Questions

1. **How to handle long-running tasks?**
   - Periodic status checks?
   - Explicit progress markers?

2. **What if workers need to collaborate?**
   - Shared context/files?
   - Inter-agent messaging?

3. **How to handle conflicts?**
   - Multiple agents editing same file?
   - Incompatible implementations?

4. **State persistence?**
   - Save project state across restarts?
   - Resume interrupted orchestrations?

5. **Error recovery?**
   - What if a worker gets stuck?
   - Automatic retry? Human intervention?

## Next Steps

1. [ ] Decide on architecture (recommend Option C)
2. [ ] Design manager system prompt
3. [ ] Implement basic `omar manager` commands
4. [ ] Add JSON protocol parsing
5. [ ] Build approval UI in dashboard
6. [ ] Test with simple multi-agent task
