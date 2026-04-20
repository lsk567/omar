# Debugging Omar GUI with Claude Code + Computer MCP Tools

Debug the omar TUI dashboard visually using plain Claude Code (not running inside omar) and the OMAR `computer_*` MCP tools.

## Prerequisites

- Omar running in a tmux session on an X11 display.
- The `omar-computer` bridge available (auto-spawned by omar on Linux when `DISPLAY` is set).
- A Claude Code session in a separate terminal, with OMAR registered as an MCP server (`claude mcp add omar <path-to-omar> mcp-server`).

## Setup

1. Start omar in a tmux session:

   ```bash
   tmux new-session -d -s omar-demo "DISPLAY=:1 omar"
   ```

2. Verify the computer bridge is up via the `computer_status` MCP tool:

   ```
   computer_status({})
   # → {"available": true, "xdotool": true, "screenshot": true, ...}
   ```

3. Acquire the computer lock and open a terminal attached to omar using `computer_keyboard`:

   ```
   computer_lock_acquire({"agent": "debug"})

   computer_keyboard({"agent": "debug", "action": "key",  "text": "ctrl+alt+t"})
   # wait ~2s for terminal to open
   computer_keyboard({"agent": "debug", "action": "type", "text": "tmux attach -t omar-demo"})
   computer_keyboard({"agent": "debug", "action": "key",  "text": "Return"})
   ```

## Core Workflow

### Take a screenshot

```
computer_screenshot({"agent": "debug", "max_width": 1280, "max_height": 960})
# → {"image_base64": "...", "width": 1280, "height": 960, "format": "png"}
```

Claude Code is multimodal — feed the returned `image_base64` directly to interpret the screenshot.

### Send keystrokes to omar

```
# Single key
computer_keyboard({"agent": "debug", "action": "key",  "text": "Tab"})

# Type text
computer_keyboard({"agent": "debug", "action": "type", "text": "hello"})

# Key combo
computer_keyboard({"agent": "debug", "action": "key",  "text": "ctrl+c"})
```

### Click on UI elements

```
computer_mouse({"agent": "debug", "action": "click", "x": 400, "y": 300})
```

## Debug Loop

1. **Screenshot** — see the current state of the GUI.
2. **Identify** — read the screenshot, spot layout bugs, missing elements, wrong colors.
3. **Interact** — press keys or click to trigger the behavior under test.
4. **Screenshot again** — verify the result.
5. **Fix code** — edit the Rust source, `cargo build --release`, restart omar.
6. **Repeat**.

## Tips

- Always acquire the computer lock before interacting, release when done.
- Use `max_width`/`max_height` on screenshots to keep payload size reasonable.
- Omar redraws on every tick (~1s), so wait 1-2s after actions before screenshotting.
- To close a tmux popup overlay, use the tmux detach sequence (`ctrl+b d`) or the popup's own escape key — not omar's keybindings (they go to the inner session).
- Populate the dashboard for testing via `spawn_agent_session`:

  ```
  spawn_agent_session({
    "name": "test-agent",
    "task": "do something",
    "role": "agent",
    "parent": "ea"
  })
  ```

- Clean up test agents with `kill_agent`:

  ```
  kill_agent({"name": "test-agent"})
  ```

## Cleanup

```
computer_lock_release({"agent": "debug"})
```

```bash
tmux kill-session -t omar-demo
tmux kill-session -t omar-agent-ea
```
