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
  echo "OMAR BIN not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

server="omar-manager-legacy-${RANDOM}-$$"
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

legacy_manager="omar-agent-0-ea"
legacy_worker="omar-agent-0-recovery-worker"
canonical_manager="omar-agent-ea-0"
tmux_cmd kill-session -t "$legacy_manager" 2>/dev/null || true
tmux_cmd kill-session -t "$canonical_manager" 2>/dev/null || true
tmux_cmd kill-session -t "$legacy_worker" 2>/dev/null || true

tmux_cmd new-session -d -s "$legacy_manager" "sleep 9999"
tmux_cmd new-session -d -s "$legacy_worker" "sleep 9999"

tmux_cmd new-session -d -s omar-dashboard \
  "cd '$REPO_ROOT' && HOME='$home_dir' OMAR_TMUX_SERVER='$server' '$OMAR_BIN'"

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

wait_for_session omar-dashboard

python3 - "$server" "$legacy_manager" "$canonical_manager" "$legacy_worker" <<'PY'
import re
import subprocess
import sys
import time

server = sys.argv[1]
legacy_manager = sys.argv[2]
canonical_manager = sys.argv[3]
legacy_worker = sys.argv[4]


def tmux(*args):
    return subprocess.run(
        ["tmux", "-L", server, *args],
        text=True,
        capture_output=True,
    )


def strip_ansi(s):
    ansi = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")
    return ansi.sub("", s)


def capture_dashboard():
    output = tmux(
        "capture-pane",
        "-pt",
        "omar-dashboard:0.0",
        "-S",
        "-120",
    )
    if output.returncode != 0:
        return ""
    return strip_ansi(output.stdout)


def sessions():
    output = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|attached=#{session_attached}|cmd=#{pane_current_command}",
    )
    if output.returncode != 0:
        return {}
    result = {}
    for line in output.stdout.splitlines():
        parts = line.split("|", 2)
        if len(parts) != 3:
            continue
        name, attached, cmd = parts
        result[name] = {
            "attached": attached == "1",
            "command": cmd,
        }
    return result


def wait_for(predicate, timeout=12.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def fail(message):
    print(message, file=sys.stderr)
    print("Dashboard pane:", file=sys.stderr)
    print(capture_dashboard(), file=sys.stderr)
    print("Sessions:", file=sys.stderr)
    session_output = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|attached=#{session_attached}|cmd=#{pane_current_command}",
    )
    print(session_output.stdout or session_output.stderr, file=sys.stderr)
    sys.exit(1)


def recovered():
    names = sessions()
    if canonical_manager not in names:
        return False
    if legacy_manager in names:
        return False
    if legacy_worker not in names:
        return False
    view = capture_dashboard()
    if "Starting Executive Assistant" in view:
        return False
    return legacy_worker in view or "recovery-worker" in view


if not wait_for(recovered):
    fail("Manager was not auto-recovered from legacy <id>-ea naming")

print(
    "PASS: legacy manager {} was recovered and normalized to {}".format(
        legacy_manager,
        canonical_manager,
    )
)
PY
