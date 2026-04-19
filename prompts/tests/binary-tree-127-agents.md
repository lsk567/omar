# Binary Tree Self-Terminating Stress Test

**Purpose:** Validate OMAR supports recursive agent hierarchies where agents spawn children, coordinate, and self-terminate. Tests multi-level delegation, inter-agent polling, and cascading cleanup.

## Structure

- Perfect binary tree, 7 levels deep.
- 127 total agents (nodes 1-127, heap-numbered).
- Naming: `t-{node}` (e.g., `t-1`, `t-2`, ..., `t-127`).
- Heap numbering: left child = `2*n`, right child = `2*n+1`.
- Leaf nodes: level 7, nodes 64-127 (64 leaves).
- Internal nodes: nodes 1-63 (63 internal).

## How to Run

EA spawns only the root `t-1` via `spawn_agent_session`:

```
spawn_agent_session({
  "name": "t-1",
  "task": "<paste the root agent task below>",
  "parent": "ea"
})
```

The root recursively spawns the whole tree. EA only monitors `t-1` for `[TASK COMPLETE]`.

## Root Agent Task

Give this to `t-1`. Each internal node passes the same protocol to its children with updated node/level.

```
You are node 1 (level 1) in a binary tree experiment with 7 levels and 127 total nodes.

## Protocol

Your node number is 1. Your level is 1. The max level is 7.

### If you are a LEAF node (level == 7)
Output [TASK COMPLETE], then immediately call:
  notify_parent({"name": "t-YOUR_NODE", "summary": "Leaf t-YOUR_NODE complete."})

### If you are an INTERNAL node (level < 7)

1. Calculate your children:
   - Left  child: node 2*YOUR_NODE, level YOUR_LEVEL+1
   - Right child: node 2*YOUR_NODE+1, level YOUR_LEVEL+1

2. Spawn each child with the OMAR `spawn_agent_session` MCP tool, passing the same
   protocol (updated node/level) and `parent` set to your own name:

     spawn_agent_session({
       "name": "t-<CHILD_NODE>",
       "task": "<this same protocol with CHILD_NODE and CHILD_LEVEL>",
       "parent": "t-<YOUR_NODE>"
     })

3. Wait for both children to call notify_parent — you will receive two
   `[CHILD COMPLETE]` messages. If a child hasn't reported after a reasonable
   time, use `get_agent({"name": "t-<CHILD_NODE>"})` to check its output_tail.
   Use `schedule_event` (sender/receiver both your own name) for timed fallback
   checks instead of busy loops.

4. When BOTH children have reported [CHILD COMPLETE]:
   a. kill_agent({"name": "t-<LEFT_CHILD>"})
   b. kill_agent({"name": "t-<RIGHT_CHILD>"})
   c. Output [TASK COMPLETE]
   d. Call notify_parent({"name": "t-YOUR_NODE", "summary": "Subtree rooted at t-YOUR_NODE complete. Both children killed."})

IMPORTANT: Do NOT output [TASK COMPLETE] until both children are confirmed
complete AND killed.
```

## Expected Behavior

1. EA spawns `t-1`.
2. `t-1` spawns `t-2` and `t-3`.
3. Recursion continues until all 64 leaf nodes (level 7) exist.
4. Leaves immediately report `[TASK COMPLETE]`.
5. Parents detect both children complete, kill them, report `[TASK COMPLETE]`.
6. Cascade continues up to `t-1`.
7. `t-1` reports `[TASK COMPLETE]` to EA.

## Success Criteria

- Root `t-1` eventually reports `[TASK COMPLETE]`.
- Tree self-terminates from leaves to root.
- All 127 agents are cleaned up (no stragglers).

## Previous Result

- **Duration:** ~4 minutes total.
- **Self-cleanup rate:** 89% (113/127 agents self-terminated).
- **Stragglers:** 14/127 needed manual cleanup by EA.
- **Root cause:** Some parents exit before confirming child `kill_agent` calls completed, leaving orphans.

## Known Issues

- **Straggler problem:** Parents sometimes output `[TASK COMPLETE]` and exit before kills for their children are confirmed. Mitigation: EA should sweep remaining `t-*` agents after `t-1` reports complete, via `list_agents` + `kill_agent`.
- **Polling overhead:** Deep trees create many concurrent polling loops. Use `schedule_event` wake-ups rather than sleep loops to avoid resource waste.
