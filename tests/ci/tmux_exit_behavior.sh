#!/usr/bin/env bash
set -euo pipefail

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

if [ ! -x "$OMAR_BIN" ]; then
  echo "OMAR binary not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

server="omar-exit-behavior-${RANDOM}-$$"
home_dir="$(mktemp -d)"

cleanup() {
  tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$home_dir"
}
trap cleanup EXIT

mkdir -p \
  "$home_dir/.omar/ea/0/status" \
  "$home_dir/.omar/mcp/ea-0" \
  "$home_dir/.omar/slack_outbox" \
  "$home_dir/.omar/logs/panics"

cat >"$home_dir/.omar/config.toml" <<'EOF'
[dashboard]
refresh_interval = 1
session_prefix = "omar-agent-"

[agent]
default_command = "bash"
default_workdir = "."
EOF

cat >"$home_dir/.omar/eas.json" <<'EOF'
[
  {
    "id": 0,
    "name": "OldSessionName",
    "description": null,
    "created_at": 1
  }
]
EOF
printf '0' >"$home_dir/.omar/active_ea"
printf '1' >"$home_dir/.omar/ea_next_id"
printf '[]' >"$home_dir/.omar/scheduled_events.json"
printf 'keep task\n' >"$home_dir/.omar/ea/0/tasks.md"
printf 'keep action log\n' >"$home_dir/.omar/ea/0/action_log.jsonl"
printf 'keep manager notes\n' >"$home_dir/.omar/manager_notes_ea0.md"
printf 'keep slack\n' >"$home_dir/.omar/slack_outbox/keep"
printf 'keep panic\n' >"$home_dir/.omar/logs/panics/keep.log"

tmux_cmd() {
  HOME="$home_dir" OMAR_TMUX_SERVER="$server" tmux -L "$server" "$@"
}

wait_for_session() {
  local session="$1"
  for _ in $(seq 1 80); do
    if tmux_cmd has-session -t "$session" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "Timed out waiting for tmux session: $session" >&2
  tmux_cmd list-sessions -F '#{session_name}|#{session_attached}|#{pane_current_command}' >&2 || true
  return 1
}

tmux -L "$server" new-session -d -s omar-dashboard \
  "cd '$REPO_ROOT' && HOME='$home_dir' OMAR_TMUX_SERVER='$server' '$OMAR_BIN'"

wait_for_session omar-dashboard
wait_for_session omar-agent-ea-0

python3 - "$server" "$home_dir" <<'PY'
import os
import subprocess
import sys
import time
from pathlib import Path

server = sys.argv[1]
home_dir = Path(sys.argv[2])
omar_dir = home_dir / ".omar"


def tmux(*args):
    return subprocess.run(
        ["tmux", "-L", server, *args],
        text=True,
        capture_output=True,
    )


def sessions():
    output = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|#{session_attached}|#{pane_current_command}",
    )
    if output.returncode != 0:
        return {}

    result = {}
    for line in output.stdout.splitlines():
        parts = line.split("|", 2)
        if len(parts) != 3:
            continue
        name, attached, command = parts
        result[name] = {"attached": attached == "1", "command": command}
    return result


def session_exists(name):
    return name in sessions()


def has_prefix_session(prefix):
    return any(name.startswith(prefix) for name in sessions())


def wait_for(predicate, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def fail(message):
    print(message, file=sys.stderr)
    print("Dashboard pane:", file=sys.stderr)
    pane = tmux("capture-pane", "-pt", "omar-dashboard:0.0", "-S", "-80")
    print(pane.stdout or pane.stderr, file=sys.stderr)
    print("Sessions:", file=sys.stderr)
    sessions_output = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|attached=#{session_attached}|cmd=#{pane_current_command}",
    )
    print(sessions_output.stdout or sessions_output.stderr, file=sys.stderr)
    sys.exit(1)


def send_keys(*keys):
    result = tmux("send-keys", "-t", "omar-dashboard:0.0", *keys)
    if result.returncode != 0:
        fail(f"Failed to send keys {keys}: {result.stderr}")


def assert_exists(path):
    if not (omar_dir / path).exists():
        fail(f"Expected path to exist: {path}")


def assert_missing(path):
    if (omar_dir / path).exists():
        fail(f"Expected path to be removed: {path}")


def assert_runtime_state_present(label):
    for path in [
        "config.toml",
        "eas.json",
        "active_ea",
        "ea_next_id",
        "scheduled_events.json",
        "ea/0/tasks.md",
        "ea/0/action_log.jsonl",
        "manager_notes_ea0.md",
        "mcp/ea-0",
        "slack_outbox/keep",
        "logs/panics/keep.log",
    ]:
        assert_exists(path)
    print(f"PASS: {label} preserves persisted runtime state")


def test_detach_key():
    send_keys("z")
    time.sleep(0.2)
    if not session_exists("omar-dashboard"):
        fail("Dashboard session disappeared after detach; expected session to persist")

    assert_runtime_state_present("z")
    print("PASS: z preserves dashboard session and runtime state")


def test_ctrl_c_is_ignored():
    send_keys("C-c")
    time.sleep(0.2)

    if not session_exists("omar-dashboard"):
        fail("Dashboard session disappeared after Ctrl+C; expected Ctrl+C to be ignored")
    if not session_exists("omar-agent-ea-0"):
        fail("EA manager session disappeared after Ctrl+C; expected Ctrl+C to be ignored")

    assert_runtime_state_present("Ctrl+C")
    print("PASS: Ctrl+C is ignored")


def test_quit_key_kills_all_sessions_and_resets_runtime_state():
    send_keys("Q")
    time.sleep(0.1)
    send_keys("y")

    if not wait_for(lambda: not session_exists("omar-dashboard"), timeout=10.0):
        fail("Dashboard session did not terminate after Q+y")

    if has_prefix_session("omar-agent-"):
        fail("OMAR agent sessions still exist after Q+y; expected cleanup on quit")

    for path in [
        "eas.json",
        "active_ea",
        "ea_next_id",
        "scheduled_events.json",
        "ea",
        "mcp",
        "manager_notes_ea0.md",
    ]:
        assert_missing(path)

    for path in ["config.toml", "slack_outbox/keep", "logs/panics/keep.log"]:
        assert_exists(path)

    action_logs = sorted((omar_dir / "logs" / "action_logs").iterdir())
    if len(action_logs) != 1:
        fail(f"Expected one archived action log, found {len(action_logs)}")
    action_name = action_logs[0].name
    if not (action_name.startswith("ea-0-") and action_name.endswith(".jsonl")):
        fail(f"Unexpected archived action log name: {action_name}")
    if action_logs[0].read_text() != "keep action log\n":
        fail("Archived action log content did not match")

    manager_notes = sorted((omar_dir / "logs" / "manager_notes").iterdir())
    if len(manager_notes) != 1:
        fail(f"Expected one archived manager notes file, found {len(manager_notes)}")
    notes_name = manager_notes[0].name
    if not (notes_name.startswith("manager_notes_ea0-") and notes_name.endswith(".md")):
        fail(f"Unexpected archived manager notes name: {notes_name}")
    if manager_notes[0].read_text() != "keep manager notes\n":
        fail("Archived manager notes content did not match")

    print("PASS: Q+y exits dashboard, kills OMAR sessions, and resets runtime state")


test_detach_key()
test_ctrl_c_is_ignored()
test_quit_key_kills_all_sessions_and_resets_runtime_state()
PY
