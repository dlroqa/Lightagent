# Lightagent Desktop — acceptance criteria

These are the measurable gates each slice must pass on GitHub-hosted runners
before [DESKTOP_PROGRESS.md](DESKTOP_PROGRESS.md) may call it CI-validated.
"Runner" means the named GitHub-hosted image. No criterion is satisfied by a
local run, a prediction, or build success alone.

## Global

| ID | Criterion | Measured by |
|---|---|---|
| G1 | Every gate names the exact head SHA and tree it ran on | Each desktop job's "Tested commit" summary |
| G2 | `desktop capability inventory` passes: the clap tree matches, the static drift check is clean, and the self-test detects every injected addition | `desktop.yml` |
| G3 | The existing `check` workflow stays green on all four platforms for every desktop PR head | `check.yml` |
| G4 | No secret, launch key, or home-directory path in source, logs, or artifacts | `check-secrets.sh`; D3/D9 key scans |
| G5 | Parity is reported as `implemented / denominator` from `desktop-capabilities.json`. No "complete" claim while any applicable row is not implemented | `check-desktop-inventory.py` summary |

## Performance thresholds

The D1 spike measured an empty window (§3 of DESKTOP_ARCHITECTURE.md). D2
measures the real shell with the mock sidecar on the same runners, and those
medians of three launches become the **baseline B** per target. Each later
slice must stay within these limits; a breach fails the gate and is
investigated, never re-run until green:

| ID | Metric (runner measurement) | Threshold |
|---|---|---|
| P1 | Spawn to first page load | ≤ 1.5 × B and ≤ 10 s |
| P2 | Spawn to sidecar ready (handshake received) | ≤ 1.5 × B and ≤ 15 s |
| P3 | Idle RSS (host + WebView + sidecar) 10 s after ready | ≤ 1.3 × B |
| P4 | Installer size | ≤ 1.3 × B |
| P5 | Streaming: fake-provider token to rendered text, p95 | ≤ 250 ms on the runner |

## Per slice

| Slice | Gate (all must pass) |
|---|---|
| D2 | The shell builds and bundles on `macos-15-intel`, `macos-15`, `windows-latest`, and `ubuntu-22.04`. A screenshot of the app over the mock sidecar is uploaded per OS. A version-handshake mismatch is shown as an error state (test). The contract check fails on a route, operation, or client mismatch (self-test). The renderer's capability file grants only host commands (config test). The CSP is present in the built config |
| D3 | `serve --desktop` contract tests: 401 without or with a wrong key, `host_rejected`, `origin_rejected`, and SSE without auth refused; the key is never in argv, env, or logs. Lifecycle tests: ready within P2; crash leads to a visible state and one restart; shutdown stops only the owned pid or group, verified by a second unrelated `lightagent serve` left running; the parent-death watchdog works; a version mismatch is refused. A clean launch needs no toolchain on `PATH` |
| D4 | CLI and desktop produce identical files for profile create/use/delete and config set (golden comparison in isolated homes). Concurrent `config set` and settings save give exactly one `409`, with no lost update. An unreadable secret file fails closed. No secret value appears in the renderer state dump or the diagnostics bundle |
| D5 | A deterministic fake-provider run covers streaming, a tool call, approval granted and denied, cancel, continue after a time-budget pause, and steering. Reconnect after a forced relay drop has no duplicated or missing event ids. A profile switch with a pending approval records an explicit rejection. A sidecar crash mid-run leaves the run visible as failed, never re-executed |
| D6 | Extension install, enable, disable, and uninstall from fixture directories and an HTTPS Git fixture; path-traversal fixtures rejected; duplicate names handled; MCP stdio and HTTP servers added from typed forms and connected through the fixture servers; skill text cannot change permission policy (fixture) |
| D7 | Every remaining inventory row has a typed UI path that invokes the real operation, with a test. Destructive operations require confirmation. Operator-only operations are refused for non-operator callers |
| D8 | A remote endpoint that fails produces an error, with no local execution (test). Capability or version mismatch is explicit. The WebUI Host/Origin hardening has regression tests, and existing WebUI e2e still passes. Keyboard-only navigation of every screen works (Playwright). axe accessibility scan has no critical violations |
| D9 | Installers for all four targets with SHA-256 checksums, an SBOM, and a license inventory; vulnerability gate (cargo-audit, npm audit, high or critical) clean or documented; clean-user smoke install, launch, and uninstall where the runner allows; per-target report of build, launch, native UI, signing (unsigned), and minimum-OS evidence |
| D10 | The combined tree of all slice heads, in merge order, passes `check` and `desktop` on all four platforms. Parity is reconciled with no unexplained gaps |
