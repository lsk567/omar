# Binary Tree Self-Terminating Stress Test

> Legacy note: this scenario still references the removed HTTP API and event endpoints. Rewrite it around OMAR MCP task/session tools before using it after the April 17, 2026 MCP cutover.

**Purpose:** Validate OMAR supports recursive agent hierarchies where agents spawn children, coordinate, and self-terminate. Tests multi-level delegation, inter-agent polling, and cascading cleanup.

## Structure

- Perfect binary tree, 7 levels deep
- 127 total agents (nodes 1-127, heap-numbered)
- Naming: `t-{node}` (e.g., `t-1`, `t-2`, `t-3`, ..., `t-127`)
- Heap numbering: left child = `2*n`, right child = `2*n+1`
- Leaf nodes: level 7, nodes 64-127 (64 leaves)
- Internal nodes: nodes 1-63 (63 internal)

## How to Run

EA spawns only the root agent `t-1`:

```bash
# Replace {{EA_ID}} with the target EA id (e.g. 0 for the default EA).
curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "t-1",
    "task": "<paste the root agent task template below>",
    "parent": "ea"
  }'
```

The root agent recursively spawns the entire tree. EA only needs to monitor `t-1` for `[TASK COMPLETE]`.

## Root Agent Task Template

Give this task to `t-1`. The protocol is recursive — each internal node passes it (with updated parameters) to its children.

```
You are node 1 (level 1) in a binary tree experiment with 7 levels and 127 total nodes.

## Protocol

Your node number is 1. Your level is 1. The max level is 7.

### If you are a LEAF node (level == 7):
Output [TASK COMPLETE] immediately.

### If you are an INTERNAL node (level < 7):

1. Calculate your children:
   - Left child: node 2, level 2
   - Right child: node 3, level 2

2. Spawn both children using the OMAR API:

   For each child (left and right), POST to spawn with the SAME protocol but updated node/level:

   ```bash
   curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/agents \
     -H "Content-Type: application/json" \
     -d '{
       "name": "t-{CHILD_NODE}",
       "task": "You are node {CHILD_NODE} (level {CHILD_LEVEL}) in a binary tree experiment with 7 levels and 127 total nodes.\n\n## Protocol\n\nYour node number is {CHILD_NODE}. Your level is {CHILD_LEVEL}. The max level is 7.\n\n### If you are a LEAF node (level == 7):\nOutput [TASK COMPLETE] immediately.\n\n### If you are an INTERNAL node (level < 7):\n\n1. Calculate your children:\n   - Left child: node {CHILD_NODE*2}, level {CHILD_LEVEL+1}\n   - Right child: node {CHILD_NODE*2+1}, level {CHILD_LEVEL+1}\n\n2. Spawn both children using the OMAR API (same protocol, updated node/level).\n\n3. Poll both children by checking their output:\n   ```\n   curl http://localhost:9876/api/ea/{{EA_ID}}/agents/t-{CHILD_NODE}\n   ```\n   Schedule wake-up events to check on children rather than using sleep loops.\n\n4. When BOTH children have reported [TASK COMPLETE] in their output:\n   a. Kill left child: curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/t-{LEFT}\n   b. Kill right child: curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/t-{RIGHT}\n   c. Output [TASK COMPLETE]\n\nIMPORTANT: Do NOT output [TASK COMPLETE] until both children are confirmed complete AND killed.",
       "parent": "t-{YOUR_NODE}"
     }'
   ```

3. Poll both children by checking their output:
   ```bash
   curl http://localhost:9876/api/ea/{{EA_ID}}/agents/t-{CHILD_NODE}
   ```
   Schedule wake-up events to check on children rather than using sleep loops:
   ```bash
   NOW=$(python3 -c "import time; print(int(time.time() * 1e9) + 15_000_000_000)")
   curl -X POST http://localhost:9876/api/ea/{{EA_ID}}/events \
     -H "Content-Type: application/json" \
     -d "{\"sender\": \"t-1\", \"receiver\": \"t-1\", \"timestamp\": $NOW, \"payload\": \"Check children progress\"}"
   ```

4. When BOTH children have reported [TASK COMPLETE] in their output:
   a. Kill left child: `curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/t-2`
   b. Kill right child: `curl -X DELETE http://localhost:9876/api/ea/{{EA_ID}}/agents/t-3`
   c. Output [TASK COMPLETE]

IMPORTANT: Do NOT output [TASK COMPLETE] until both children are confirmed complete AND killed.
```

## Expected Behavior

1. EA spawns `t-1`
2. `t-1` spawns `t-2` and `t-3`
3. Recursion continues until all 64 leaf nodes (level 7) exist
4. Leaves immediately report `[TASK COMPLETE]`
5. Parents detect both children complete, kill them, report `[TASK COMPLETE]`
6. Cascade continues up to `t-1`
7. `t-1` reports `[TASK COMPLETE]` to EA

## Success Criteria

- Root `t-1` eventually reports `[TASK COMPLETE]`
- Tree self-terminates from leaves to root
- All 127 agents are cleaned up (no stragglers on dashboard)

## Previous Result

- **Duration:** ~4 minutes total
- **Self-cleanup rate:** 89% (113/127 agents self-terminated)
- **Stragglers:** 14/127 needed manual cleanup by EA
- **Root cause of stragglers:** Some parent agents exit before confirming child kill requests completed, leaving orphaned children

## Known Issues

- **Straggler problem:** Parents sometimes output `[TASK COMPLETE]` and exit before the DELETE requests for their children are confirmed. This leaves orphaned agents. Mitigation: EA should run a cleanup sweep after `t-1` completes, killing any remaining `t-*` agents.
- **Polling overhead:** Deep trees create many concurrent polling loops. Using OMAR events API (wake-ups) instead of sleep loops is critical to avoid resource waste.
