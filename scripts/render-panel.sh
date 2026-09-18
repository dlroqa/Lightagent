#!/usr/bin/env bash
# Build and render the standalone Lightagent Web UI against its own API.
set -euo pipefail
cd "$(dirname "$0")/.."

AGENT_PORT="${AGENT_PORT:-8735}"
OUT_DIR="${OUT_DIR:-e2e/screens}"

if ! command -v cargo >/dev/null 2>&1 && [ -f "${HOME:-}/.cargo/env" ]; then
  . "${HOME:-}/.cargo/env"
fi

work_dir="$(mktemp -d)"
export LIGHTAGENT_HOME="$work_dir/agent-home"
agent_log="$work_dir/agent.log"
agent_pid=""

cleanup() {
  local status=$?
  [ -n "$agent_pid" ] && kill "$agent_pid" 2>/dev/null || true
  wait 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    echo "== agent server log =="
    [ -f "$agent_log" ] && sed -n '1,240p' "$agent_log" || echo "(none)"
  fi
  rm -rf -- "$work_dir"
  exit "$status"
}
trap cleanup EXIT

wait_for() {
  local url="$1" name="$2" pid="$3" tries=60
  while [ "$tries" -gt 0 ]; do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "error: $name exited before becoming ready at $url" >&2
      return 1
    fi
    if curl -fsS -o /dev/null "$url" 2>/dev/null; then
      sleep 0.1
      if kill -0 "$pid" 2>/dev/null; then return 0; fi
      echo "error: $name could not claim its port; another service answered at $url" >&2
      return 1
    fi
    tries=$((tries - 1))
    sleep 0.5
  done
  echo "error: $name did not become ready at $url" >&2
  return 1
}

echo "== build =="
cargo build -p lightagent --bin lightagent
( cd frontend && npm run build )

# Serving the panel needs an active profile, but the render never invokes a
# model. Port zero is deliberately unreachable and keeps the test isolated.
./target/debug/lightagent init --base-url http://127.0.0.1:0 >/dev/null

echo "== start standalone Lightagent (port $AGENT_PORT) =="
./target/debug/lightagent serve --host 127.0.0.1 --port "$AGENT_PORT" \
  --web-root frontend/dist >"$agent_log" 2>&1 &
agent_pid=$!
wait_for "http://127.0.0.1:$AGENT_PORT/health" "Lightagent" "$agent_pid"

if ! curl -fsS "http://127.0.0.1:$AGENT_PORT/api/lightagent/v1/tools" | grep -q '"tools"'; then
  echo "error: the standalone Lightagent API did not return tools JSON" >&2
  exit 1
fi

echo "== render in a headless browser =="
PANEL_BASE="http://127.0.0.1:$AGENT_PORT" OUT_DIR="$OUT_DIR" node e2e/render.mjs

echo "Lightagent Web UI render complete. Screenshots in $OUT_DIR/"
