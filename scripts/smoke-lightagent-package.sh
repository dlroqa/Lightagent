#!/usr/bin/env bash
# Build the Lightagent archive and run what it holds.
#
# "The file exists" is not evidence that anything works. The archive is built,
# extracted, and then its binary is *executed*, because the failures that matter
# — a stale binary, a missing shared library, a package that unpacks to the wrong
# place — all produce a file of exactly the right name and size. This mirrors
# `scripts/smoke-artifacts.sh` for the one archive `scripts/package-lightagent.sh`
# produces, and it is self-contained: it builds the binary if one is not present.
set -euo pipefail
cd "$(dirname "$0")/.."

failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml \
  | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
case "$TRIPLE" in
  *windows*) EXE=".exe" ;;
  *) EXE="" ;;
esac

echo "== build =="
if [ ! -f "target/release/lightagent$EXE" ] && [ ! -f "target/$TRIPLE/release/lightagent$EXE" ]; then
  cargo build --release -p lightagent
fi
pass "release binary present"

echo "== package =="
archive="$(bash ./scripts/package-lightagent.sh "$TRIPLE")"
if [ ! -f "$archive" ]; then
  fail "package-lightagent.sh produced no archive"
  echo "$failures failure(s)."; exit 1
fi
pass "archive built: $archive"

work="$(mktemp -d)"
case "$archive" in
  *.tar.gz) tar -xzf "$archive" -C "$work" ;;
  *.zip)
    # The separator inside the zip, asserted rather than assumed (see
    # smoke-artifacts.sh for why a backslash separator is a silent disaster).
    if unzip -l "$archive" | grep -q '\\'; then
      fail "the zip uses backslash separators; it would unpack to one file"
    fi
    unzip -q "$archive" -d "$work" ;;
esac

root="$work/lightagent-$VERSION-$TRIPLE"
[ -d "$root" ] && pass "unpacks to a named directory" || fail "no lightagent-$VERSION-$TRIPLE/ in the archive"

for f in "lightagent$EXE" LICENSE README.md lightagent.service lightagent.env.example web/index.html; do
  [ -f "$root/$f" ] && pass "contains $f" || fail "missing $f"
done

echo "== run =="
bin="$root/lightagent$EXE"
chmod +x "$bin" 2>/dev/null || true

# On macOS the packaged binary must be exactly this machine's architecture
# (not a universal or translated binary), and must declare the minimum macOS
# the build asked for. What it declares is a floor the linker recorded; only
# the macOS this runs on is actually exercised.
case "$TRIPLE" in
  *apple-darwin)
    want="${TRIPLE%%-*}"
    [ "$want" = aarch64 ] && want=arm64
    archs="$(lipo -archs "$bin" 2>/dev/null || true)"
    [ "$archs" = "$want" ] && pass "binary architecture is $want" || fail "binary architecture is '$archs', expected $want"
    minos="$(vtool -show-build "$bin" 2>/dev/null | awk '$1 == "minos" { print $2; exit }')"
    echo "  info  minimum macOS recorded in the binary: ${minos:-unknown}; running on macOS $(sw_vers -productVersion)"
    if [ -n "${MACOSX_DEPLOYMENT_TARGET:-}" ]; then
      [ "$minos" = "$MACOSX_DEPLOYMENT_TARGET" ] && pass "minimum macOS is $minos" \
        || fail "minimum macOS is '${minos:-unknown}', expected $MACOSX_DEPLOYMENT_TARGET"
    fi
    otool -L "$bin" | sed 's/^/  info  /'
    ;;
esac

if version_line="$("$bin" --version 2>&1)"; then
  pass "--version runs: $version_line"
else
  fail "--version failed"
fi
"$bin" banner --preview 2>&1 | grep -q $'▀' && pass "banner renders" || fail "banner did not render"

# `doctor` in a throwaway home, pointed at reserved port zero so a developer's
# live gateway cannot affect the assertion. Doctor deliberately reports
# configuration only: provider readiness belongs to the public `/v1` capability
# contract, not the provider's private control plane.
home="$(mktemp -d)"
LIGHTAGENT_HOME="$home" "$bin" config set inference.base_url http://localhost:0 >/dev/null
if LIGHTAGENT_HOME="$home" "$bin" doctor 2>&1 | grep -qi "Provider:  configured"; then
  pass "doctor runs without a private provider probe"
else
  fail "doctor did not report configured provider"
fi
rm -rf "$home" "$work"

echo
if [ "$failures" -eq 0 ]; then
  echo "All Lightagent package checks passed."
else
  echo "$failures failure(s)."
  exit 1
fi
