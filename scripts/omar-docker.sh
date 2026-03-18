#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

docker compose up -d omar >/dev/null

for _ in $(seq 1 30); do
  if docker compose exec -T omar tmux has-session -t omar-dashboard >/dev/null 2>&1; then
    exec docker compose exec omar tmux attach -t omar-dashboard
  fi
  sleep 1
done

printf 'Timed out waiting for OMAR dashboard session to be ready.\n' >&2
docker compose logs --tail 50 omar >&2 || true
exit 1
