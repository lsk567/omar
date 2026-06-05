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

mkdir -p "$home_dir/.omar"
cat >"$home_dir/.omar/config.toml" <<'EOF'
[dashboard]
refresh_interval = 1
session_prefix = "omar-agent-"

[agent]
default_command = "bash"
default_workdir = "."
EOF
mkdir -p "$home_dir/.omar/ea/0/status" "$home_dir/.omar/mcp/ea-0" "$home_dir/.omar/slack_outbox" "$home_dir/.omar/logs/panics"
cat >"$home_dir/.omar/eas.json" <<'EOF'
[
  {"id": 0, "name": "OldSessionName", "description": "old session", "created_at": 1}
]
EOF
printf '0' >"$home_dir/.omar/active_ea"
printf '0' >"$home_dir/.omar/ea_next_id"
printf '[]' >"$home_dir/.omar/scheduled_events.json"
printf '%s\n' '1. Keep project' >"$home_dir/.omar/ea/0/tasks.md"
printf 'keep action log\n' >"$home_dir/.omar/ea/0/action_log.jsonl"
printf 'keep manager notes\n' >"$home_dir/.omar/manager_notes_ea0.md"
printf 'queued slack\n' >"$home_dir/.omar/slack_outbox/keep"
printf 'panic log\n' >"$home_dir/.omar/logs/panics/keep.log"

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

REPO_ROOT="$REPO_ROOT" OMAR_BIN="$OMAR_BIN" python3 - "$server" "$home_dir" <<'PY'
import os
import pty
import signal
import subprocess
import sys
import time

server = sys.argv[1]
home_dir = sys.argv[2]


def tmux(*args):
    return subprocess.run(
        ["tmux", "-L", server, *args],
        text=True,
        capture_output=True,
    )


def start_dashboard():
    command = (
        f"cd {sh_quote(os.environ['REPO_ROOT'])} && "
        f"HOME={sh_quote(home_dir)} OMAR_TMUX_SERVER={sh_quote(server)} "
        f"{sh_quote(os.environ['OMAR_BIN'])}"
    )
    result = tmux("new-session", "-d", "-s", "omar-dashboard", command)
    if result.returncode != 0:
        fail(f"Failed to restart dashboard: {result.stderr}")
    if not wait_for(lambda: session_exists("omar-dashboard"), timeout=5.0):
        fail("Dashboard session did not restart")
    if not wait_for(lambda: session_exists("omar-agent-ea-0"), timeout=5.0):
        fail("EA manager session did not restart")


def sh_quote(value):
    return "'" + value.replace("'", "'\"'\"'") + "'"


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


def session_attached(name):
    session = sessions().get(name)
    return bool(session and session["attached"])


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


def open_dashboard():
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.setdefault("TERM", "xterm-256color")
        os.execvp("tmux", ["tmux", "-L", server, "attach-session", "-t", "omar-dashboard"])
    return pid, fd


def close_dashboard(pid, fd):
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.waitpid(pid, os.WNOHANG)
    except OSError:
        pass


def wait_for_attached(session, timeout=5.0):
    if not wait_for(lambda: session_attached(session), timeout=timeout):
        fail(f"Session was not attached when expected: {session}")


def wait_for_detached(session, timeout=5.0):
    if not wait_for(
        lambda: session_exists(session) and not session_attached(session),
        timeout=timeout,
    ):
        fail(f"Session did not detach within timeout: {session}")


def test_detach_key():
    pid, fd = open_dashboard()
    try:
        wait_for_attached("omar-dashboard")
        os.write(fd, b"z")

        wait_for_detached("omar-dashboard")
        if not session_exists("omar-dashboard"):
            fail("Dashboard session disappeared after detach; expected session to persist")
        assert_runtime_state_present("z")

        print("PASS: z detaches without terminating dashboard session")
    finally:
        close_dashboard(pid, fd)


def assert_runtime_state_present(label):
    omar_dir = os.path.join(home_dir, ".omar")
    expected = [
        "eas.json",
        "active_ea",
        "ea_next_id",
        "scheduled_events.json",
        os.path.join("ea", "0", "tasks.md"),
        os.path.join("ea", "0", "action_log.jsonl"),
        "manager_notes_ea0.md",
        "mcp",
    ]
    missing = [path for path in expected if not os.path.exists(os.path.join(omar_dir, path))]
    if missing:
        fail(f"{label} unexpectedly removed persisted runtime state: {missing}")


def test_ctrl_c_is_ignored():
    pid, fd = open_dashboard()
    try:
        wait_for_attached("omar-dashboard")
        os.write(fd, b"\x03")
        time.sleep(0.1)

        if not session_exists("omar-dashboard"):
            fail("Dashboard session terminated after Ctrl+C; expected Ctrl+C to be ignored")
        if not session_exists("omar-agent-ea-0"):
            fail("EA manager session disappeared after Ctrl+C; expected Ctrl+C to be ignored")
        assert_runtime_state_present("Ctrl+C")
        print("PASS: Ctrl+C is ignored")
    finally:
        close_dashboard(pid, fd)


def test_quit_key_kills_all_sessions():
    pid, fd = open_dashboard()
    try:
        wait_for_attached("omar-dashboard")
        os.write(fd, b"Q")
        time.sleep(0.1)
        os.write(fd, b"y")

        if not wait_for(lambda: not session_exists("omar-dashboard"), timeout=10.0):
            fail("Dashboard session did not terminate after Q+y")

        if has_prefix_session("omar-agent-"):
            fail("OMAR agent sessions still exist after Q+y; expected cleanup on quit")

        omar_dir = os.path.join(home_dir, ".omar")
        wiped_paths = [
            "eas.json",
            "active_ea",
            "ea_next_id",
            "scheduled_events.json",
            "ea",
            "mcp",
            "manager_notes_ea0.md",
        ]
        leftovers = [path for path in wiped_paths if os.path.exists(os.path.join(omar_dir, path))]
        if leftovers:
            fail(f"Persisted runtime state survived Q+y: {leftovers}")

        kept_paths = [
            "config.toml",
            os.path.join("slack_outbox", "keep"),
            os.path.join("logs", "panics", "keep.log"),
        ]
        missing = [path for path in kept_paths if not os.path.exists(os.path.join(omar_dir, path))]
        if missing:
            fail(f"Expected preserved files missing after Q+y: {missing}")

        action_log_dir = os.path.join(omar_dir, "logs", "action_logs")
        action_logs = []
        if os.path.isdir(action_log_dir):
            action_logs = [
                name
                for name in os.listdir(action_log_dir)
                if name.startswith("ea-0-") and name.endswith(".jsonl")
            ]
        if len(action_logs) != 1:
            fail(f"Q+y did not archive exactly one action log: {action_logs}")
        with open(os.path.join(action_log_dir, action_logs[0]), encoding="utf-8") as handle:
            if handle.read() != "keep action log\n":
                fail("Archived action log content did not match expected content")

        notes_dir = os.path.join(omar_dir, "logs", "manager_notes")
        notes = []
        if os.path.isdir(notes_dir):
            notes = [
                name
                for name in os.listdir(notes_dir)
                if name.startswith("manager_notes_ea0-") and name.endswith(".md")
            ]
        if len(notes) != 1:
            fail(f"Q+y did not archive exactly one manager notes file: {notes}")
        with open(os.path.join(notes_dir, notes[0]), encoding="utf-8") as handle:
            if handle.read() != "keep manager notes\n":
                fail("Archived manager notes content did not match expected content")

        print("PASS: Q+y exits dashboard and kills OMAR sessions")
    finally:
        close_dashboard(pid, fd)


test_detach_key()
test_ctrl_c_is_ignored()
test_quit_key_kills_all_sessions()
PY
