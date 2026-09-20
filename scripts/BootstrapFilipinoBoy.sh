#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export NAW_HTTP_BIND="${NAW_HTTP_BIND:-127.0.0.1}"
export NAW_HTTP_PORT="${NAW_HTTP_PORT:-4242}"
export NAW_SKIN_DIR="skins/snackers"
export NAW_SEED_DIR="seeds"
export NAW_RUN_LIVE_TESTS=1
export NAW_TEST_REQUIRE_BRAND_ASSETS=1
export NAW_TEST_BASE_URL="http://${NAW_HTTP_BIND}:${NAW_HTTP_PORT}"

step() { printf '\n==> %s\n' "$1"; }
ok() { printf '  [ok] %s\n' "$1"; }

step "Booting dev stack"
"$ROOT/scripts/dev-up.sh" --with-worker
ok "dev stack up"

step "Checking FilianWIKI brand"
curl --silent --show-error --fail "$NAW_TEST_BASE_URL/home" | grep -q "FilianWIKI"
ok "/home carries the FilianWIKI brand"
curl --silent --show-error --fail "$NAW_TEST_BASE_URL/site.webmanifest" | grep -q "FilianWIKI"
ok "manifest carries the FilianWIKI brand"

step "Running WeCantGive500"
(cd "$ROOT" && SQLX_OFFLINE=true cargo test -p naw-web --test WeCantGive500 -- --nocapture)
ok "WeCantGive500 passed"

ok "BootstrapFilipinoBoy is ready: $NAW_TEST_BASE_URL"
echo "Stop application processes with: scripts/dev-down.sh"
exit 0
