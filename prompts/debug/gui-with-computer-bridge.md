# Debugging Omar GUI with Claude Code + Computer Bridge

Debug the omar TUI dashboard visually using plain Claude Code (not running inside omar) and omar's computer bridge API.

## Prerequisites

- Omar running in a tmux session with its HTTP API on `localhost:9876`
- X11 display available (the computer bridge uses `xdotool` + ImageMagick `import`)
- Claude Code session in a separate terminal

## Setup

1. Start omar in a tmux session:
```bash
tmux new-session -d -s omar-demo "DISPLAY=:1 omar"
```

2. Verify the computer bridge is up:
```bash
curl -s http://localhost:9876/api/computer/status
# → {"available":true,"xdotool":true,"screenshot":true,...}
```

3. Open a terminal on the X11 display and attach to omar:
```bash
# From Claude Code, use the computer bridge to open a terminal and attach
curl -s -X POST http://localhost:9876/api/computer/lock \
  -H "Content-Type: application/json" -d '{"agent":"debug"}'

curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"key","text":"ctrl+alt+t"}'
# wait ~2s for terminal to open

curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"type","text":"tmux attach -t omar-demo"}'

curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"key","text":"Return"}'
```

## Core Workflow

### Take a screenshot

```bash
curl -s -X POST http://localhost:9876/api/computer/screenshot \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","max_width":1280,"max_height":960}' \
  | python3 -c "
import sys,json,base64
d = json.load(sys.stdin)
open('/tmp/screen.png','wb').write(base64.b64decode(d['image_base64']))
print(f'{d[\"width\"]}x{d[\"height\"]}')"
```

Then read the image in Claude Code — it's multimodal and can interpret the screenshot directly.

### Send keystrokes to omar

```bash
# Single key
curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"key","text":"Tab"}'

# Type text
curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"type","text":"hello"}'

# Key combo
curl -s -X POST http://localhost:9876/api/computer/keyboard \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"key","text":"ctrl+c"}'
```

### Click on UI elements

```bash
curl -s -X POST http://localhost:9876/api/computer/mouse \
  -H "Content-Type: application/json" \
  -d '{"agent":"debug","action":"click","x":400,"y":300}'
```

## Debug Loop

1. **Screenshot** — see the current state of the GUI
2. **Identify** — read the screenshot, spot layout bugs, missing elements, wrong colors
3. **Interact** — press keys or click to trigger the behavior under test
4. **Screenshot again** — verify the result
5. **Fix code** — edit the Rust source, `cargo build --release`, restart omar
6. **Repeat**

## Tips

- Always acquire the computer lock before interacting, release when done
- Use `max_width`/`max_height` on screenshots to keep payload size reasonable
- Omar redraws on every tick (~1s), so wait 1–2s after actions before screenshotting
- To close a tmux popup overlay, use the tmux detach sequence (`ctrl+b d`) or the popup's own escape key — not omar's keybindings (they go to the inner session)
- Spawn test agents via the API to populate the dashboard:
  ```bash
  curl -s -X POST http://localhost:9876/api/ea/0/agents \
    -H "Content-Type: application/json" \
    -d '{"name":"test-agent","task":"do something","role":"agent","parent":"ea"}'
  ```
- Kill test agents when done:
  ```bash
  curl -s -X DELETE http://localhost:9876/api/ea/0/agents/test-agent
  ```

## Cleanup

```bash
curl -s -X DELETE http://localhost:9876/api/computer/lock \
  -H "Content-Type: application/json" -d '{"agent":"debug"}'
tmux kill-session -t omar-demo
tmux kill-session -t omar-agent-ea
```
