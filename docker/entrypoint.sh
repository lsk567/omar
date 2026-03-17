#!/usr/bin/env bash
set -euo pipefail

STATE_DIR="${OMAR_STATE_DIR:-$HOME/.omar}"
STATE_CONFIG="${STATE_DIR}/config.toml"
LOG_FILE="${OMAR_CONTAINER_LOG:-${STATE_DIR}/container-init.log}"
SESSION_NAME="${OMAR_SESSION_NAME:-omar-dashboard}"
START_COMMAND="${OMAR_START_COMMAND:-omar}"

mkdir -p "${STATE_DIR}"

log() {
  local message="$1"
  local timestamp
  timestamp="$(date -Iseconds)"
  printf '[%s] %s\n' "${timestamp}" "${message}" | tee -a "${LOG_FILE}"
}

copy_default_config() {
  if [ ! -f "${STATE_CONFIG}" ] && [ -f /etc/omar/config.toml ]; then
    cp /etc/omar/config.toml "${STATE_CONFIG}"
    log "Copied default OMAR config to ${STATE_CONFIG}"
  fi
}

record_environment() {
  log "Container user: $(id -un) ($(id -u):$(id -g))"
  log "Workspace: ${PWD}"
  log "HOME: ${HOME}"
  log "RUST_LOG: ${RUST_LOG:-unset}"
  log "DISPLAY: ${DISPLAY:-unset}"
  log "OMAR_AUTO_START: ${OMAR_AUTO_START:-0}"
  log "tmux version: $(tmux -V 2>/dev/null || echo unavailable)"
  log "rustc version: $(rustc --version 2>/dev/null || echo unavailable)"
  log "cargo version: $(cargo --version 2>/dev/null || echo unavailable)"
  log "omar version: $(omar --version 2>/dev/null || echo unavailable)"
}

start_omar_session() {
  if tmux has-session -t "${SESSION_NAME}" 2>/dev/null; then
    log "tmux session ${SESSION_NAME} already exists"
    return
  fi

  log "Starting OMAR tmux session ${SESSION_NAME}: cd /workspace && ${START_COMMAND}"
  tmux new-session -d -s "${SESSION_NAME}" "cd /workspace && exec ${START_COMMAND}"
}

copy_default_config
record_environment

if [ "${OMAR_AUTO_START:-0}" = "1" ]; then
  case "${1:-}" in
    ""|sleep|tail)
      start_omar_session
      ;;
  esac
fi

if [ "$#" -eq 0 ]; then
  set -- sleep infinity
fi

log "Executing command: $*"
exec "$@"
