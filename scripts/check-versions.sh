#!/usr/bin/env bash
# The Rust workspace and Web UI publish one Lightagent version.
set -euo pipefail
cd "$(dirname "$0")/.."

workspace="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
web="$(node -p "require('./frontend/package.json').version" 2>/dev/null || echo "")"

echo "  workspace       $workspace"
echo "  frontend        $web"

if [ -z "$workspace" ] || [ -z "$web" ]; then
  echo "could not read every version (is node installed?)" >&2
  exit 1
fi
if [ "$workspace" != "$web" ]; then
  echo "  FAIL  versions disagree" >&2
  exit 1
fi
if [ "${GITHUB_REF_TYPE:-}" = "tag" ] && [ "${GITHUB_REF_NAME:-}" != "v$workspace" ]; then
  echo "  FAIL  tag ${GITHUB_REF_NAME:-} does not match v$workspace" >&2
  exit 1
fi
echo "Versions agree."
