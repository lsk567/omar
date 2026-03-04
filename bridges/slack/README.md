# OMAR Slack Bridge

Connects Slack channels to OMAR agents via Slack Socket Mode (no public URL needed).

Messages in Slack channels/threads create and interact with OMAR agents. Agent output is polled and posted back as threaded replies.

## Architecture

```
Slack (Socket Mode WS) <---> omar-slack bridge <---> OMAR API (localhost:9876)
       ^                                                    ^
       |                                                    |
  messages/threads                                   agents/send/output
```

- **Socket Mode**: WebSocket connection via app-level token — no public endpoint required
- **Thread mapping**: each Slack thread maps to one OMAR agent session
- **Output polling**: background task polls agent output and posts new content back to Slack
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
2. When a message arrives in a channel where the bot is a member:
   - If it's a new thread/top-level message: spawns a new OMAR agent via `POST /api/agents`
   - If it's a reply in an existing thread: sends input to the mapped agent via `POST /api/agents/:name/send`
3. Background poller checks agent output every `POLL_INTERVAL_MS` milliseconds
4. New output is posted back to the Slack thread (auto-chunked if >3900 chars)
5. Completed agents (output contains `[PROJECT COMPLETE]`) are auto-cleaned

## Agent Naming

Agents are named `slack-<channel_suffix>-<thread_ts>` for traceability in OMAR's dashboard.

## Config Reference

| Env Var | Required | Default | Description |
|---------|----------|---------|-------------|
| `SLACK_BOT_TOKEN` | Yes | — | Bot OAuth token (`xoxb-...`) |
| `SLACK_APP_TOKEN` | Yes | — | App-level token (`xapp-...`) |
| `OMAR_URL` | No | `http://127.0.0.1:9876` | OMAR API endpoint |
| `POLL_INTERVAL_MS` | No | `2000` | Agent output poll interval (ms) |
| `MAX_MESSAGE_LENGTH` | No | `3900` | Max Slack message chunk size |
| `RUST_LOG` | No | `info` | Log level (trace/debug/info/warn/error) |
