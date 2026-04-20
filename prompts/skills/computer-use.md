## Computer Use (Desktop Control)

OMAR agents can control the desktop — move/click the mouse, type on the keyboard, and take screenshots — via the OMAR API. Requires `xdotool` and ImageMagick `import` on a Linux X11 system.

### Prerequisites
- `xdotool` installed
- ImageMagick installed (provides `import` command)
- `$DISPLAY` set (X11 session)

Check availability:
```bash
curl http://localhost:9876/api/computer/status
```

### Exclusive Lock

Only one agent can control the computer at a time. You MUST acquire the lock before any action.

```bash
# Acquire lock — always include ea_id to prevent cross-EA identity collisions
curl -X POST http://localhost:9876/api/computer/lock \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>}'

# Release lock when done
curl -X DELETE http://localhost:9876/api/computer/lock \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>}'
```

Always release the lock when you're finished so other agents can use the computer.

### Screenshot

```bash
# Full resolution
curl -X POST http://localhost:9876/api/computer/screenshot \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>}'

# Resized (recommended — reduces payload size)
curl -X POST http://localhost:9876/api/computer/screenshot \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>, "max_width": 800, "max_height": 600}'
```

Returns `{"image_base64": "...", "width": N, "height": N, "format": "png"}`.

### Mouse Control

```bash
curl -X POST http://localhost:9876/api/computer/mouse \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>, "action": "<ACTION>", "x": X, "y": Y}'
```

Actions:
- `"move"` — move cursor to (x, y)
- `"click"` — click at (x, y). Optional `"button": 1|2|3` (default: 1=left)
- `"double_click"` — double-click at (x, y)
- `"drag"` — drag from (x, y) to (to_x, to_y). Requires `"to_x"` and `"to_y"`
- `"scroll"` — scroll at (x, y). Requires `"scroll_direction": "up"|"down"|"left"|"right"`. Optional `"scroll_amount": N` (default: 3)

### Keyboard Control

```bash
# Type text
curl -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>, "action": "type", "text": "hello world"}'

# Press key combo
curl -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent": "<YOUR NAME>", "ea_id": <YOUR EA_ID>, "action": "key", "text": "ctrl+s"}'
```

### Read-Only Queries (no lock needed)

```bash
# Screen size
curl http://localhost:9876/api/computer/screen-size

# Mouse position
curl http://localhost:9876/api/computer/mouse-position
```

### Typical Workflow

1. Acquire lock
2. Take screenshot to see current screen state
3. Identify target coordinates from the screenshot
4. Perform mouse/keyboard actions
5. Take another screenshot to verify result
6. Repeat 3-5 as needed
7. Release lock
