# OMAR Slack Bridge

Connects Slack channels to OMAR agents via Slack Socket Mode (no public URL needed).

Messages in Slack channels/threads become events for the EA. The EA replies via the `slack_reply` OMAR MCP tool; the bridge picks those replies up and posts them back to the right thread.

## Architecture

```
Slack (Socket Mode WS)                       OMAR
       │                                       │
       │ @mention event                        │
       ▼                                       │
  omar-slack bridge ──────────────────────►  omar mcp-server
       │          omar_wake_later (stdio)    (subprocess)
       │                                       │
       │                                       ▼
       │                                  EA (tmux)
       │                                       │
       │                 slack_reply tool      │
       │          writes ~/.omar/slack_outbox/ │
       │          ◄────────────────────────────┘
       ▼
  Slack (chat.postMessage)
```

- **Socket Mode**: WebSocket connection via app-level token — no public endpoint required.
- **Inbound**: bridge spawns `omar mcp-server` as a subprocess, calls `omar_wake_later` over its stdio.
- **Outbound**: EA calls the `slack_reply` MCP tool, which atomically queues a JSON file in `~/.omar/slack_outbox/`. The bridge polls the directory every 500 ms and delivers to Slack; files are deleted on successful delivery, retained on failure, so transient Slack errors (or a bridge restart) don't lose messages.
- **No HTTP surface**: the bridge no longer binds any loopback port.

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

1. Bridge authenticates with Slack (`auth.test`) and connects via Socket Mode WebSocket.
2. Bridge spawns `omar mcp-server` as a subprocess and completes the MCP `initialize` handshake over stdio.
3. When someone @mentions the bot in a channel:
   - Bridge formats the message as a `[SLACK MESSAGE]` event payload (includes channel, thread, user, and reply instructions that point at the `slack_reply` MCP tool).
   - Calls `omar_wake_later` on the MCP server with `receiver: "ea"`.
4. OMAR's scheduler delivers the event to the EA (deferred if a popup is open).
5. EA processes the request and replies by invoking the `slack_reply` MCP tool with the channel and thread from the inbound payload.
6. Bridge's outbox watcher picks the queued reply up and posts it to the correct Slack thread (auto-chunked if >3900 chars).

## Config Reference

| Env Var | Required | Default | Description |
|---------|----------|---------|-------------|
| `SLACK_BOT_TOKEN` | Yes | — | Bot OAuth token (`xoxb-...`) |
| `SLACK_APP_TOKEN` | Yes | — | App-level token (`xapp-...`) |
| `OMAR_BINARY` | No | next to `omar-slack`, else PATH | Path to the `omar` executable |
| `OMAR_DIR` | No | `~/.omar` | OMAR state dir (for the `slack_outbox/` rendezvous) |
| `MAX_MESSAGE_LENGTH` | No | `3900` | Max Slack message chunk size |
| `RUST_LOG` | No | `info` | Log level (trace/debug/info/warn/error) |
