#!/usr/bin/env bash
# Decide whether a run of release.yml may write a GitHub release.
#
# Every run builds, smoke-tests and checksums the packages; only the `publish`
# job writes, and only when this prints `publish=true`. The inputs come from the
# environment, so scripts/test-release-gate.py can test every case without
# GitHub:
#   EVENT     github.event_name
#   REF_TYPE  github.ref_type (`tag` or `branch`)
#   REF_NAME  github.ref_name
#   DRY_RUN   toJSON(inputs.dry_run). Only the JSON boolean `false` turns a
#             manual run's dry run off. `true`, `null` (no input), an empty
#             value or a string such as `"false"` all stay dry.
#
# A run that would publish without the tag `v<workspace version>` fails here
# rather than building something nobody can release.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$VERSION" ] || { echo "could not read the workspace version" >&2; exit 1; }
TAG="v$VERSION"

EVENT="${EVENT:-}"
REF_TYPE="${REF_TYPE:-}"
REF_NAME="${REF_NAME:-}"
DRY_RUN="${DRY_RUN:-}"

decide() {
  echo "publish=$1"
  echo "reason=$2"
}
refuse() {
  echo "  FAIL  $1" >&2
  exit 1
}

case "$EVENT" in
  push)
    if [ "$REF_TYPE" != tag ]; then
      decide false "a branch push never publishes"
    elif [ "$REF_NAME" != "$TAG" ]; then
      refuse "tag $REF_NAME does not match $TAG"
    else
      decide true "tag push $TAG"
    fi
    ;;
  workflow_dispatch)
    if [ "$DRY_RUN" != false ]; then
      decide false "manual dry run (dry_run=$DRY_RUN)"
    elif [ "$REF_TYPE" != tag ]; then
      refuse "dry_run=false must be dispatched on the tag $TAG, not $REF_TYPE $REF_NAME"
    elif [ "$REF_NAME" != "$TAG" ]; then
      refuse "tag $REF_NAME does not match $TAG"
    else
      decide true "manual release of $TAG"
    fi
    ;;
  *)
    decide false "event ${EVENT:-<none>} never publishes"
    ;;
esac
