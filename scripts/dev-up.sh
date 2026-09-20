#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_DIR="$ROOT/.dev"
APP_PID_FILE="$STATE_DIR/naw.pid"
WORKER_PID_FILE="$STATE_DIR/worker.pid"
APP_LOG="$STATE_DIR/naw.log"
APP_ERR="$STATE_DIR/naw.err"
WORKER_LOG="$STATE_DIR/worker.log"
WORKER_ERR="$STATE_DIR/worker.err"
WITH_WORKER=0

for arg in "$@"; do
  case "$arg" in
    --with-worker) WITH_WORKER=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

mkdir -p "$STATE_DIR"

export DATABASE_URL="${DATABASE_URL:-postgres://naw:naw@127.0.0.1:5433/naw_dev}"
export VALKEY_URL="${VALKEY_URL:-redis://127.0.0.1:6380}"
export NAW_HTTP_BIND="${NAW_HTTP_BIND:-127.0.0.1}"
export NAW_HTTP_PORT="${NAW_HTTP_PORT:-4242}"
export NAW_SKIN_DIR="${NAW_SKIN_DIR:-skins/snackers}"
export NAW_SEED_DIR="${NAW_SEED_DIR:-seeds}"

docker compose -f "$ROOT/docker-compose.yml" up -d postgres valkey

wait_for_health() {
  local container="$1"
  for _ in $(seq 1 60); do
    if [[ "$(docker inspect -f '{{.State.Health.Status}}' "$container" 2>/dev/null || true)" == "healthy" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "container did not become healthy: $container" >&2
  docker inspect -f '{{.State.Status}} {{.State.Health.Status}}' "$container" >&2 || true
  exit 1
}

wait_for_health nawwk-postgres
wait_for_health nawwk-valkey

if [[ ! -x "$ROOT/target/debug/naw" && ! -x "$ROOT/target/debug/naw.exe" ]]; then
  (cd "$ROOT" && SQLX_OFFLINE=true cargo build --bin naw)
fi

if [[ -x "$ROOT/target/debug/naw" ]]; then
  BINARY="$ROOT/target/debug/naw"
else
  BINARY="$ROOT/target/debug/naw.exe"
fi

(cd "$ROOT" && "$BINARY" migrate)
(cd "$ROOT" && "$BINARY" seed --flavor filian --slug filian --name FilianWIKI --domain snackers.vai-rice.space --vtuber Filian --community Snackers)

if [[ -f "$APP_PID_FILE" ]] && kill -0 "$(cat "$APP_PID_FILE")" 2>/dev/null; then
  echo "naw is already running with PID $(cat "$APP_PID_FILE")"
else
  rm -f "$APP_PID_FILE"
  (
    cd "$ROOT"
    exec env NAW_HTTP_BIND="$NAW_HTTP_BIND" NAW_HTTP_PORT="$NAW_HTTP_PORT" \
      NAW_SKIN_DIR="$NAW_SKIN_DIR" NAW_SEED_DIR="$NAW_SEED_DIR" \
      DATABASE_URL="$DATABASE_URL" VALKEY_URL="$VALKEY_URL" \
      "$BINARY" serve >"$APP_LOG" 2>"$APP_ERR"
  ) &
  echo $! >"$APP_PID_FILE"
fi

if [[ "$WITH_WORKER" == "1" ]]; then
  command -v bun >/dev/null 2>&1 || { echo "--with-worker requires Bun >= 1.4.2" >&2; exit 1; }
  if [[ -f "$WORKER_PID_FILE" ]] && kill -0 "$(cat "$WORKER_PID_FILE")" 2>/dev/null; then
    echo "worker is already running with PID $(cat "$WORKER_PID_FILE")"
  else
    rm -f "$WORKER_PID_FILE"
    (
      cd "$ROOT"
      exec env WORKER_PORT="${WORKER_PORT:-8081}" bun run worker/src/index.ts >"$WORKER_LOG" 2>"$WORKER_ERR"
    ) &
    echo $! >"$WORKER_PID_FILE"
  fi
fi

for _ in $(seq 1 30); do
  if curl --silent --show-error --fail "http://${NAW_HTTP_BIND}:${NAW_HTTP_PORT}/health" >/dev/null 2>&1; then
    echo "FilianWIKI dev server: http://${NAW_HTTP_BIND}:${NAW_HTTP_PORT}"
    echo "Logs: $APP_LOG"
    [[ "$WITH_WORKER" == "1" ]] && echo "Worker: http://127.0.0.1:${WORKER_PORT:-8081}/health"
    exit 0
  fi
  if [[ -f "$APP_PID_FILE" ]] && ! kill -0 "$(cat "$APP_PID_FILE")" 2>/dev/null; then
    echo "naw exited before /health became ready" >&2
    tail -n 40 "$APP_ERR" "$APP_LOG" >&2 || true
    exit 1
  fi
  sleep 1
done

echo "naw did not become ready on port $NAW_HTTP_PORT" >&2
tail -n 40 "$APP_ERR" "$APP_LOG" >&2 || true
exit 1
