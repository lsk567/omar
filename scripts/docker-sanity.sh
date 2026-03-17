#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

cleanup() {
  docker compose down >/dev/null 2>&1 || true
}

port_is_free() {
  python3 - "$1" <<'PY'
import socket
import sys

port = int(sys.argv[1])
with socket.socket() as sock:
    sock.settimeout(0.2)
    sys.exit(0 if sock.connect_ex(("127.0.0.1", port)) != 0 else 1)
PY
}

pick_host_port() {
  local candidate="${OMAR_API_PORT:-19876}"
  while ! port_is_free "${candidate}"; do
    candidate=$((candidate + 1))
  done
  printf '%s\n' "${candidate}"
}

OMAR_API_PORT="$(pick_host_port)"
export OMAR_API_PORT

if docker container inspect omar >/dev/null 2>&1; then
  printf 'Refusing to run compose sanity check: container "omar" already exists.\n' >&2
  printf 'Stop or remove it first, or run the non-compose checks manually.\n' >&2
  exit 1
fi

trap cleanup EXIT

printf '==> Validating compose config\n'
docker compose config >/dev/null

printf '==> Building dev image\n'
docker compose build omar >/dev/null

printf '==> Running containerized test suite\n'
docker run --rm omar:dev cargo test -- --test-threads=1

printf '==> Starting OMAR container on host port %s\n' "${OMAR_API_PORT}"
docker compose up -d omar >/dev/null

printf '==> Waiting for API health\n'
for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${OMAR_API_PORT}/api/health" >/dev/null; then
    break
  fi
  sleep 2
done

curl -fsS "http://127.0.0.1:${OMAR_API_PORT}/api/health"
printf '\n'

printf '==> Verifying tmux sessions and init log\n'
docker compose exec -T omar bash -lc 'tmux has-session -t omar-dashboard && tmux has-session -t omar-agent-ea && test -f "$HOME/.omar/container-init.log"'

printf '==> Verifying tmux setup and cargo availability in login shell\n'
docker compose exec -T omar bash -lc 'command -v cargo >/dev/null && test "$(tmux show-options -gv history-limit)" = "9999" && test -f "$HOME/.tmux.conf"'

printf '==> Validating optional compose profiles\n'
docker compose --profile slack config >/dev/null
docker compose --profile computer config >/dev/null

printf 'Docker sanity checks passed.\n'
