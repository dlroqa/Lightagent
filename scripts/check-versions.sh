#!/usr/bin/env bash
# The Rust workspace, the Web UI and Lightagent Desktop (its workspace and
# its bundle) publish one Lightagent version.
set -euo pipefail
cd "$(dirname "$0")/.."

workspace="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
web="$(node -p "require('./frontend/package.json').version" 2>/dev/null || echo "")"
desktop="$(sed -n '/^\[workspace\.package\]/,/^\[/p' desktop/Cargo.toml \
  | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
bundle="$(node -p "require('./desktop/src-tauri/tauri.conf.json').version" 2>/dev/null || echo "")"

echo "  workspace       $workspace"
echo "  frontend        $web"
echo "  desktop         $desktop"
echo "  desktop bundle  $bundle"

if [ -z "$workspace" ] || [ -z "$web" ] || [ -z "$desktop" ] || [ -z "$bundle" ]; then
  echo "could not read every version (is node installed?)" >&2
  exit 1
fi
if [ "$workspace" != "$web" ] || [ "$workspace" != "$desktop" ] || [ "$workspace" != "$bundle" ]; then
  echo "  FAIL  versions disagree" >&2
  exit 1
fi
if [ "${GITHUB_REF_TYPE:-}" = "tag" ] && [ "${GITHUB_REF_NAME:-}" != "v$workspace" ]; then
  echo "  FAIL  tag ${GITHUB_REF_NAME:-} does not match v$workspace" >&2
  exit 1
fi
echo "Versions agree."
