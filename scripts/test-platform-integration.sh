#!/usr/bin/env bash
# Start the pinned remote-RAG dependencies and prove their live HTTP contracts.
# Docker is intentionally optional for normal development: this script exits 2
# with a helpful message when it is unavailable instead of silently passing.
set -euo pipefail

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "Docker is required for platform integration tests. Start Docker Desktop (or Docker Engine), then rerun this script." >&2
  exit 2
fi

compose=(docker compose -f compose.platform-integration.yml)
if ! "${compose[@]}" version >/dev/null 2>&1; then
  echo "Docker Compose v2 is required for platform integration tests." >&2
  exit 2
fi

cleanup() {
  # Keep named caches locally so repeat runs do not redownload models. CI uses
  # an ephemeral runner and removes its volumes in its workflow cleanup step.
  "${compose[@]}" down --remove-orphans
}
trap cleanup EXIT

"${compose[@]}" up --wait --pull always
python3 scripts/platform-contract-test.py

# The Rust suite is ignored by default because it needs the real containers.
if [[ "${LIGHTAGENT_RUN_RUST_PLATFORM_TESTS:-0}" == "1" ]]; then
  LIGHTAGENT_QDRANT_URL="${QDRANT_URL:-http://127.0.0.1:6333}" \
  LIGHTAGENT_INFINITY_URL="${INFINITY_URL:-http://127.0.0.1:7997}" \
    cargo test -p lightagent --test platform_services -- --ignored
fi
