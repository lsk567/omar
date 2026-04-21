# OMAR Slack Bridge

Connects Slack channels to OMAR agents via Slack Socket Mode (no public URL needed).

Messages in Slack channels/threads create and interact with OMAR agents. Agent output is polled and posted back as threaded replies.

## Architecture

```
Slack (Socket Mode WS)                           OMAR (localhost:9876)
       │                                                │
       │ @mention event                                 │
       ▼                                                │
  omar-slack bridge ─POST /api/ea/{id}/events───►  Event Queue
       ▲            (receiver: "ea")                    │
       │                                                ▼
       │                                           EA (tmux)
       │                                                │
       │◄──curl POST /api/slack/reply──────────────────┘
       │   (localhost:9877)
       ▼
  Slack (chat.postMessage)
```

- **Socket Mode**: WebSocket connection via app-level token — no public endpoint required
- **Event-driven**: Slack messages are routed to the EA via OMAR's event queue
- **Bidirectional**: Bridge runs an HTTP server so the EA can push replies back to Slack
- **Popup-aware**: Events are deferred when the user is interacting with the EA popup
- **Auto-reconnect**: WebSocket reconnects automatically on disconnection

## Setup

### 1. Create a Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App** > **From scratch**
2. Name it (e.g., "OMAR") and select your workspace

### 2. Enable Socket Mode

1. Go to **Settings > Socket Mode** and enable it
2. Generate an **App-Level Token** with scope `connections:write`
3. Save this as `SLACK_APP_TOKEN` (starts with `xapp-`)

### 3. Configure Bot Token Scopes

Go to **Features > OAuth & Permissions** and add these **Bot Token Scopes**:

| Scope | Purpose |
|-------|---------|
| `chat:write` | Post messages to channels |
| `channels:history` | Read messages in public channels |
| `groups:history` | Read messages in private channels |
| `im:history` | Read direct messages |
| `users:read` | Resolve user display names |

### 4. Subscribe to Events

Go to **Features > Event Subscriptions**, enable events, and subscribe to these **bot events**:

| Event | Purpose |
|-------|---------|
| `message.channels` | Messages in public channels |
| `message.groups` | Messages in private channels |
| `message.im` | Direct messages |

### 5. Install App to Workspace

1. Go to **Settings > Install App** and click **Install to Workspace**
2. Authorize the requested permissions
3. Copy the **Bot User OAuth Token** as `SLACK_BOT_TOKEN` (starts with `xoxb-`)

### 6. Invite Bot to Channels

In Slack, invite the bot to any channel where you want it active:
```
/invite @OMAR
```

## Usage

```bash
export SLACK_BOT_TOKEN="xoxb-..."
export SLACK_APP_TOKEN="xapp-..."

# OMAR auto-launches the bridge when these env vars are set:
cargo run --release
```

The bridge can also be run standalone for debugging:
```bash
cd bridges/slack
RUST_LOG=debug cargo run
```

## Build

Both `omar` and `omar-slack` are workspace members — a single build produces both:

```bash
cargo build --release
# -> target/release/omar
# -> target/release/omar-slack
```

## How It Works

1. Bridge authenticates with Slack (`auth.test`) and connects via Socket Mode WebSocket
2. Bridge starts an HTTP callback server on `SLACK_BRIDGE_PORT` (default 9877)
3. When someone @mentions the bot in a channel:
   - Bridge formats the message as a `[SLACK MESSAGE]` event payload (includes channel, thread, user, reply curl command)
   - Posts it to the OMAR event queue via `POST /api/ea/{OMAR_EA_ID}/events` with `receiver: "ea"`
4. OMAR's event scheduler delivers the event to the EA (deferred if popup is open)
5. EA processes the request and replies by curling `POST /api/slack/reply` on the bridge
6. Bridge posts the reply back to the correct Slack thread (auto-chunked if >3900 chars)

## Agent Naming

Agents are named `slack-<channel_suffix>-<thread_ts>` for traceability in OMAR's dashboard.

## Config Reference

| Env Var | Required | Default | Description |
|---------|----------|---------|-------------|
| `SLACK_BOT_TOKEN` | Yes | — | Bot OAuth token (`xoxb-...`) |
| `SLACK_APP_TOKEN` | Yes | — | App-level token (`xapp-...`) |
| `OMAR_URL` | No | `http://127.0.0.1:9876` | OMAR API endpoint |
| `OMAR_EA_ID` | No | `0` | OMAR EA id for routing (`/api/ea/{id}/...`) |
| `SLACK_BRIDGE_PORT` | No | `9877` | Bridge HTTP callback server port |
| `MAX_MESSAGE_LENGTH` | No | `3900` | Max Slack message chunk size |
| `RUST_LOG` | No | `info` | Log level (trace/debug/info/warn/error) |
