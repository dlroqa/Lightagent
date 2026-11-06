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
#   SIGNING   inputs.signing: `signed` or `unsigned` for a manual run (empty
#             means unsigned); anything else is refused. A tag push signs,
#             unless the owner approved that exact tag to publish unsigned
#             (UNSIGNED_RELEASES below); no other event signs. Printed as
#             `signing=`. A manual publishing run must be signed, and is
#             refused for a tag approved to publish unsigned.
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
SIGNING="${SIGNING:-}"

# Owner-approved unsigned publication, by exact tag. The owner's approval of
# 2026-11-06: v0.6.0 ships its macOS and Windows installers unsigned. A
# listed tag publishes unsigned from its own tag push only; every other tag
# publishes signed. RELEASE_GATE_UNSIGNED_RELEASES replaces the list for
# scripts/test-release-gate.py only (release.yml never sets it; a test checks).
UNSIGNED_RELEASES="${RELEASE_GATE_UNSIGNED_RELEASES-v0.6.0}"
unsigned_approved() {
  local tags tag
  read -r -a tags <<<"$UNSIGNED_RELEASES"
  for tag in ${tags[@]+"${tags[@]}"}; do
    [ "$tag" = "$1" ] && return 0
  done
  return 1
}

decide() {
  echo "publish=$1"
  echo "reason=$2"
}
refuse() {
  echo "  FAIL  $1" >&2
  exit 1
}

# Decided first, so a refusal here never follows a printed publish=true.
case "$EVENT" in
  push)
    if [ "$REF_TYPE" != tag ]; then
      sign=unsigned
    elif [ "$REF_NAME" = "$TAG" ] && unsigned_approved "$TAG"; then
      sign=unsigned
    else
      sign=signed
    fi
    ;;
  workflow_dispatch)
    case "$SIGNING" in
      signed|unsigned) sign="$SIGNING" ;;
      "") sign=unsigned ;;
      *) refuse "signing must be 'signed' or 'unsigned', not '$SIGNING'" ;;
    esac
    ;;
  *)
    sign=unsigned
    ;;
esac
echo "signing=$sign"

case "$EVENT" in
  push)
    if [ "$REF_TYPE" != tag ]; then
      decide false "a branch push never publishes"
    elif [ "$REF_NAME" != "$TAG" ]; then
      refuse "tag $REF_NAME does not match $TAG"
    else
      decide true "tag push $TAG ($sign)"
    fi
    ;;
  workflow_dispatch)
    if [ "$DRY_RUN" != false ]; then
      decide false "manual dry run (dry_run=$DRY_RUN)"
    elif [ "$REF_TYPE" != tag ]; then
      refuse "dry_run=false must be dispatched on the tag $TAG, not $REF_TYPE $REF_NAME"
    elif [ "$REF_NAME" != "$TAG" ]; then
      refuse "tag $REF_NAME does not match $TAG"
    elif unsigned_approved "$TAG"; then
      refuse "$TAG publishes unsigned from its tag push only; a manual publishing run could race it"
    elif [ "$sign" != signed ]; then
      refuse "unsigned publication of $TAG is not authorized (approved: ${UNSIGNED_RELEASES:-none})"
    else
      decide true "manual release of $TAG (signed)"
    fi
    ;;
  *)
    decide false "event ${EVENT:-<none>} never publishes"
    ;;
esac
