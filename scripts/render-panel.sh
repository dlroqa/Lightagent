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
release_pid=""
env_pid=""

cleanup() {
  local status=$?
  [ -n "$agent_pid" ] && kill "$agent_pid" 2>/dev/null || true
  [ -n "$release_pid" ] && kill "$release_pid" 2>/dev/null || true
  [ -n "$env_pid" ] && kill "$env_pid" 2>/dev/null || true
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

# The credential vault: the server opens it with a key outside the home. One
# grant, for an MCP server that does not exist, is sealed with a previous key
# so the browser check has something to re-seal.
export RENDER_PREVIOUS_CREDENTIAL_KEY="$work_dir/previous-credential.key"
./target/debug/lightagent remote-credential keygen "$RENDER_PREVIOUS_CREDENTIAL_KEY" >/dev/null
printf 'render-previous-secret\n' >"$work_dir/previous.secret"
LIGHTAGENT_CREDENTIAL_KEY_FILE="$RENDER_PREVIOUS_CREDENTIAL_KEY" ./target/debug/lightagent \
  remote-credential add default --service mcp:render-seed --destination https://seed.render.invalid \
  --secret-file "$work_dir/previous.secret" >/dev/null
rm -f -- "$work_dir/previous.secret"
./target/debug/lightagent remote-credential keygen "$work_dir/credential.key" >/dev/null
export LIGHTAGENT_CREDENTIAL_KEY_FILE="$work_dir/credential.key"

# A controlled release API for `update --check` (read-only), on loopback.
mkdir -p "$work_dir/releases"
printf '%s\n' '{"tag_name":"v99.0.0","html_url":"https://github.com/dlroqa/Lightagent/releases/tag/v99.0.0","assets":[]}' \
  >"$work_dir/releases/latest"
python3 -m http.server 8737 --bind 127.0.0.1 --directory "$work_dir/releases" >"$work_dir/releases.log" 2>&1 &
release_pid=$!
wait_for "http://127.0.0.1:8737/latest" "the release fixture" "$release_pid"
export LIGHTAGENT_TEST_RELEASE_API="http://127.0.0.1:8737/latest"

echo "== start standalone Lightagent (port $AGENT_PORT) =="
./target/debug/lightagent serve --host 127.0.0.1 --port "$AGENT_PORT" \
  --web-root frontend/dist >"$agent_log" 2>&1 &
agent_pid=$!
wait_for "http://127.0.0.1:$AGENT_PORT/health" "Lightagent" "$agent_pid"

if ! curl -fsS "http://127.0.0.1:$AGENT_PORT/api/lightagent/v1/tools" | grep -q '"tools"'; then
  echo "error: the standalone Lightagent API did not return tools JSON" >&2
  exit 1
fi

# A second server, started as a user would start one with the vault key
# itself in LIGHTAGENT_CREDENTIAL_KEY (no key file) and HERMES_HOME naming a
# Hermes home, in its own home. The key is random for this run and is only
# ever in the environment; e2e/environment.mjs checks the browser's view.
ENV_PORT="${ENV_PORT:-8736}"
env_home="$work_dir/env-home"
export RENDER_ENV_HERMES_HOME="$work_dir/env-hermes"
mkdir -p "$RENDER_ENV_HERMES_HOME/profiles/render-env-hermes"
printf 'You came from HERMES_HOME in the render.\n' \
  >"$RENDER_ENV_HERMES_HOME/profiles/render-env-hermes/SOUL.md"
LIGHTAGENT_HOME="$env_home" ./target/debug/lightagent init --base-url http://127.0.0.1:0 >/dev/null
RENDER_ENV_CREDENTIAL_KEY="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
export RENDER_ENV_CREDENTIAL_KEY
echo "== start the environment server (port $ENV_PORT) =="
env -u LIGHTAGENT_CREDENTIAL_KEY_FILE LIGHTAGENT_HOME="$env_home" \
  LIGHTAGENT_CREDENTIAL_KEY="$RENDER_ENV_CREDENTIAL_KEY" HERMES_HOME="$RENDER_ENV_HERMES_HOME" \
  ./target/debug/lightagent serve --host 127.0.0.1 --port "$ENV_PORT" \
  --web-root frontend/dist >"$work_dir/env-agent.log" 2>&1 &
env_pid=$!
wait_for "http://127.0.0.1:$ENV_PORT/health" "the environment server" "$env_pid"

# axe-core for e2e/accessibility.mjs, pinned and not saved as a dependency.
( cd e2e && npm install --no-save --no-audit --no-fund axe-core@4.10.3 >/dev/null )

echo "== render in a headless browser =="
PANEL_BASE="http://127.0.0.1:$AGENT_PORT" PANEL_ENV_BASE="http://127.0.0.1:$ENV_PORT" OUT_DIR="$OUT_DIR" \
  node e2e/render.mjs

echo "Lightagent Web UI render complete. Screenshots in $OUT_DIR/"
