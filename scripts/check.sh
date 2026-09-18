#!/usr/bin/env bash
# Everything Lightagent CI runs, kept identical locally and in automation.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1 && [ -f "${HOME:-}/.cargo/env" ]; then
  . "${HOME:-}/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found. Install rustup, or put ~/.cargo/bin on PATH." >&2
  exit 1
fi

echo "== fmt =="
cargo fmt --all --check
echo "== clippy =="
cargo clippy --workspace --all-targets -- -D warnings
echo "== test =="
cargo test --workspace

if [ -d frontend/node_modules ]; then
  echo "== web UI =="
  ( cd frontend && npm run build )
elif command -v npm >/dev/null 2>&1; then
  echo "== web UI == skipped: run \`npm install\` in frontend/ to include it"
else
  echo "== web UI == skipped: npm unavailable"
fi

echo "== versions =="
./scripts/check-versions.sh
echo "== deps =="
./scripts/check-deps.sh
echo "== secrets =="
./scripts/check-secrets.sh
echo "All Lightagent checks passed."
