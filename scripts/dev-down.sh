#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="$ROOT/.dev"

step() { printf '\n==> %s\n' "$1"; }
ok() { printf '  [ok] %s\n' "$1"; }
info() { printf '  .. %s\n' "$1"; }

stop_pid_file() {
  local file="$1"
  local label="$2"
  if [[ -f "$file" ]]; then
    local pid
    pid="$(cat "$file")"
    if kill -0 "$pid" 2>/dev/null; then
      info "stopping $label (PID $pid)"
      kill "$pid" 2>/dev/null || true
      for _ in $(seq 1 10); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
      done
      kill -9 "$pid" 2>/dev/null || true
    else
      info "$label PID $pid already gone"
    fi
    rm -f "$file"
    ok "$label stopped"
  else
    info "$label already stopped"
  fi
}

step "Stopping app processes"
stop_pid_file "$STATE_DIR/worker.pid" "worker"
stop_pid_file "$STATE_DIR/naw.pid" "naw"
ok "application processes stopped; Docker containers were left running"
