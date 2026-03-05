# OMAR Computer Bridge

Standalone HTTP service exposing computer-use (mouse, keyboard, screenshots) via REST.
Agents make HTTP calls instead of running X11 tools directly.

## Quick Start

```bash
# Default port 9878
cargo run -p omar-computer-bridge

# Custom port
COMPUTER_BRIDGE_PORT=9878 cargo run -p omar-computer-bridge
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `COMPUTER_BRIDGE_PORT` | `9878` | HTTP server port |
| `SCREENSHOT_MAX_WIDTH` | `1280` | Default max screenshot width |
| `SCREENSHOT_MAX_HEIGHT` | `800` | Default max screenshot height |
| `DISPLAY` | auto-detect | X11 display |
| `XAUTHORITY` | auto-detect | X11 auth file |

## Endpoints

### `GET /health`
Health check. Returns tool availability status.

### `GET /screen-size`
Returns `{ "ok": true, "width": 1920, "height": 1080 }`.

### `POST /screenshot`
Capture screen as base64 PNG.

```json
{ "max_width": 1280, "max_height": 800 }
```

Returns `{ "ok": true, "image": "<base64>" }`.

### `POST /click`
Click at coordinates. `button`: 1=left, 2=middle, 3=right (default: 1).

```json
{ "x": 100, "y": 200, "button": 1 }
```

### `POST /type`
Type text string.

```json
{ "text": "hello world" }
```

### `POST /key`
Press key combo (xdotool key names).

```json
{ "keys": "ctrl+s" }
```

### `POST /move`
Move mouse cursor.

```json
{ "x": 500, "y": 300 }
```

### `POST /drag`
Drag from one point to another.

```json
{ "from_x": 100, "from_y": 200, "to_x": 300, "to_y": 400, "button": 1 }
```

### `POST /scroll`
Scroll at coordinates. `direction`: up, down, left, right.

```json
{ "x": 500, "y": 300, "direction": "down", "amount": 3 }
```

## Requirements

- `xdotool` — mouse/keyboard control
- ImageMagick `import` — screenshots
- X11 display (DISPLAY env var or auto-detected)
