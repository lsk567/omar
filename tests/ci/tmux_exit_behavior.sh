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

python3 - "$server" <<'PY'
import os
import pty
import signal
import subprocess
import sys
import time

server = sys.argv[1]


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

        print("PASS: z detaches without terminating dashboard session")
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

        print("PASS: Q+y exits dashboard and kills OMAR sessions")
    finally:
        close_dashboard(pid, fd)


test_detach_key()
test_quit_key_kills_all_sessions()
PY
