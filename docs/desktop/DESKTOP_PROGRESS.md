# Lightagent Desktop — progress

Governing documents: [DESKTOP_BUILD_PLAN.md](DESKTOP_BUILD_PLAN.md) and
[DESKTOP_CLAUDE_PROMPT.md](DESKTOP_CLAUDE_PROMPT.md).

States: **proposed** → **implemented** (committed on a branch) →
**CI-validated** (a named hosted run passed on that exact commit) →
**packaged-tested** → **approved**. Nothing has been merged, tagged,
published, or deployed.

A commit cannot record its own SHA. Each update records evidence for commits
that already exist; the commit carrying an update is itself untested until the
next update names its run.

## Status board

| Slice | State | Branch | Draft PR | Latest implementation commit | Latest tested commit |
|---|---|---|---|---|---|
| D0 Baseline and feature audit | **CI-validated** (gate passed) | `desktop/d0-audit` | https://github.com/dlroqa/Lightagent/pull/90 | `bc01426ed6d55c1ceb951d886e1a58ea9ad4c008` | `bc01426` / tree `4b135f94d45dc7d5e1f3142cc1c5ed88ac30a868` |
| D1 Architecture and UI specification | **CI-validated** (gate passed) | `desktop/d1-architecture` (stacked on D0) | https://github.com/dlroqa/Lightagent/pull/92 | `350b48b3d0d072213e2028d8b5fb53022d61c340` | `350b48b` / tree `e43ab411568dda5c183963966d09131d21898581` |
| D2 CI and desktop skeleton | implemented; validation pending | `desktop/d2-shell` (stacked on D1) | recorded in the next update | this commit | none yet |
| D1 spike (evidence only, never merged) | runner evidence complete | `desktop/d1-spike` | https://github.com/dlroqa/Lightagent/pull/91 | `82896bcd63f3bd6786dcea12b603241e50a3a9ea` | `82896bc` / tree `d9cde26142a63a716a58f35dc33330be0e90d8b7` |
| D3–D10 | not started | — | — | — | — |

The latest progress record is the copy on the highest stacked desktop branch.
Branches lower in the stack keep the record as of their own commit.

## Execution method

- **Editing:** the existing `source-edit` workflow on the never-merged
  `tools/source-edit` branch. An edit request pinned to a base SHA is applied
  on a GitHub-hosted `ubuntu-24.04` runner, which runs `cargo fmt` and creates
  a commit through the git data API without moving any branch. Blobs for
  workflow files are created there. The final commit, which attaches them, is
  created through the git data API under the owner's token (scopes `repo`,
  `workflow`). A `desktop/*` branch ref is then created or fast-forwarded to
  it, never forced.
- **Validation:** `.github/workflows/desktop.yml` (this slice) plus the
  existing `check` workflow, on GitHub-hosted runners only.
- **Local machine:** a repository backup and these documents only. No local
  builds, tests, or source edits.

## D0 — Baseline and feature audit

### Baseline

| Item | Value |
|---|---|
| Repository | `dlroqa/Lightagent` |
| Default branch | `master` |
| Audited (frozen) SHA | `12582a046ba37a7fd59c86c1d4fb3a911fa7564a` (merge of PR #89, `release/v0.5.0-dry-run`) |
| Audited tree | `59b8f136534d228e328ef6795c26f63de6f47faa` |
| Version | 0.5.0; Rust 1.98.0 pinned (`rust-toolchain.toml`); edition 2024 |
| Latest release | v0.5.0 |
| Open PRs at audit | none |
| Overlapping desktop work | none: no branch or file referenced a desktop shell, Tauri, or Electron |
| AGENTS.md / CONTRIBUTING / SECURITY | absent. The binding repository policy is the dependency policy in `Cargo.toml` (no cmake, no OpenSSL, no libclang; polars/tonic/sqlx banned), gated by `scripts/check-deps.sh`, plus `scripts/check-secrets.sh` |

### Baseline Actions evidence (audited SHA)

| Workflow | Run | Result |
|---|---|---|
| check | https://github.com/dlroqa/Lightagent/actions/runs/40975649064 | success: 13 jobs, including check on macos-arm64 (`macos-15`), macos-x64 (`macos-15-intel`), windows-x64, and linux-x64; Web UI render; package smoke; release gate |
| jev evaluation | https://github.com/dlroqa/Lightagent/actions/runs/40975649134 | success |
| subagents | https://github.com/dlroqa/Lightagent/actions/runs/40975721175 | success |
| subagent fixture (pass) | https://github.com/dlroqa/Lightagent/actions/runs/40975766850 | success (expected) |
| subagent fixture (fail) | https://github.com/dlroqa/Lightagent/actions/runs/40975789839 | failure (**expected**) |
| subagent fixture (verify) | https://github.com/dlroqa/Lightagent/actions/runs/40975863447 | failure (**expected**) |
| release (v0.5.0) | https://github.com/dlroqa/Lightagent/actions/runs/40976901096 | success |

**Why the two fixture failures are deliberate:**
`.github/workflows/subagent-fixture.yml` (header comment, lines 3-10) is a
target for the `subagents` tester broker's live check. In `mode: fail`, its
one step runs `echo "FAIL: deliberate fixture failure"; exit 1`. In
`mode: verify`, it compiles and tests `fixtures/subagents/coder/lib.rs`, whose
test `tests::one_is_one` deliberately fails. The log of run 40975863447 shows
`panicked at fixtures/subagents/coder/lib.rs:14:9: assertion left == right
failed` and `test result: FAILED. 0 passed; 1 failed`. The broker must report
each fixture's outcome as it is. The `subagents` run that dispatched them
(40975721175) passed, so the broker reported them correctly. Neither failure
is a baseline defect.

### Audit findings that shape D1–D3

1. **The process model exists.** `lightagent serve` (`crates/lightagent/src/serve.rs:732`)
   serves `/api/lightagent/v1` (21 method/route pairs, `crates/lightagent-api/src/lib.rs:73-112`)
   and the React 19 + Vite WebUI from `--web-root` (`frontend/`; screens `/`, `/tools`, `/settings`).
2. **The API covers only part of the CLI.** It has no operation for config
   path/get/set and the Jev routing report, models, profile
   show/create/use/preset/delete, the extensions commands, worker
   assign/revoke, memory, RAG, identity, remote credentials, import, doctor,
   init/setup, onboard, reload, **continue** (resume after the time budget), or
   **steering** (input queued while a run is in progress).
3. **CLI handlers are print-bound.** They have the shape
   `fn(args…, json: bool) -> Result<(), String>` and print their results
   (for example `extensions.rs:27`, `rag.rs:697`, `import.rs:28`, `memory.rs`).
   Typed shared operations must be extracted (D3) so CLI, API, and desktop call
   the same code.
4. **Loopback is unauthenticated.** `serve.rs:746-758`: with no `--key-env`,
   a loopback bind uses `AuthConfig::open()`. `crates/lightagent-api` has no
   Host or Origin validation, so a DNS-rebinding page or any local process can
   drive tools. This is an input to the D1 security design.
5. **One profile per server process.** `AppState.session_profile` is fixed
   at start (`serve.rs:739-760`). `POST /runs {profile}` is honoured only in
   multi-user mode (`lib.rs:1148-1160`). The active-profile write
   `ProfileStore::set_active` is atomic (temp file + rename,
   `paths.rs:173-185`), but no lock coordinates a running server with a CLI
   that switches profile. This is an input to the D1 profile-switch decision.
6. **Secret authority.** Provider keys are stored in owner-only
   `~/.lightagent/secrets` and referenced as `SecretRef`. The
   remote-credential vault is sealed with
   `LIGHTAGENT_CREDENTIAL_KEY[_FILE]`. Identity secrets have strict delivery
   rules (`identity_admin.rs:10-14`).
7. **Targets and runners.** `check.yml` and `release.yml` already run
   natively on `macos-15-intel` (x86_64, `MACOSX_DEPLOYMENT_TARGET=10.14`),
   `macos-15` (arm64, 11.0), `windows-latest`, and `ubuntu-22.04`/`24.04`.
   The 10.14 floor is a declared deployment target; no hosted runner executes
   macOS older than 15. It stays an unverified target (D1).

### Capability inventory

- Files: [desktop-capabilities.json](desktop-capabilities.json) (rows),
  [cli-tree.txt](cli-tree.txt) (pinned clap tree), and
  [DESKTOP_CLI_PARITY.md](DESKTOP_CLI_PARITY.md) (readable matrix).
- 306 rows: 86 commands, 56 options, 47 positional arguments, 6 setup-section
  values, 2 aliases, 10 setup options, 16 slash commands, 6 key bindings,
  8 ACP methods, 10 MCP registration fields, 6 extension registration rows,
  1 skill registration row, 14 config sections, 12 environment variables, and
  26 API operations.
- **Parity denominator: 225** applicable, counted rows. **Implemented: 0 of 225.**
- Rows that do not count toward parity:
  - groups, and commands that run a default subcommand;
  - aliases;
  - 26 rows marked `duplicate_of` (for example `/tools` → `cli.tools.list`,
    and ACP methods → Conversations rows);
  - 22 not-applicable rows, each with a reason;
  - 5 host-owned `serve` rows;
  - backend-only API rows. Four API rows count because the WebUI exposes them
    with no CLI equivalent: Jev status/verify, session search, session
    rename/pin, and attachments.
- Six applicability decisions (8 rows) were open at D0 and are **resolved in D1**
  (DESKTOP_ARCHITECTURE.md §11). The parity denominator is now **226**
  (`update --check` became applicable; the other 7 rows are not applicable,
  each with a reason).
- New backend operations required for existing CLI capabilities (not new
  scope): **continue** and **steering** (D5), and typed MCP server
  configuration (D6).

### Drift check

- `desktop_inventory` (Rust test in `crates/lightagent/src/lib.rs`) walks
  `Cli::command()` and requires an exact match with `cli-tree.txt`. Any added,
  removed, or renamed command, option, argument, setup value, or alias fails it.
- `scripts/check-desktop-inventory.py` statically checks:
  - slash words, chat key codes, and ACP methods;
  - MCP transports and fields, and extension manifest fields;
  - config sections and HTTP routes;
  - literal environment names;
  - the inventory schema and denominator, and the parity matrix.

  `--self-test` injects one addition into each surface and requires the check
  to report it.
- It cannot detect setup menu items, new modifier combinations, behaviour
  changes behind unchanged names, config keys below the section level, or
  computed environment names. These are listed in DESKTOP_CLI_PARITY.md.

### D0 log

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-29 | Static audit of the frozen SHA | `12582a0` / `59b8f13` | local reading; baseline runs above |
| 2026-10-30 | Edit request `015-desktop-d0-audit` applied on base `12582a0` | prepared `352200b` / `93c943e`; final `2c9df87` / `50c8bb6` (workflow blob `e637300` checked byte-for-byte against the reviewed file, then attached) | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40995818991 |
| 2026-10-30 | Branch `desktop/d0-audit` created at `2c9df87` (no force); draft PR #90 opened | `2c9df87` | https://github.com/dlroqa/Lightagent/pull/90 |
| 2026-10-30 | `desktop` on `2c9df87`: **failure**. The clap-tree test passed; the static check reported 10 false "chat keys" drifts, because `before_tests` split `chat.rs` at an item-level `#[cfg(test)]` (line 186) instead of the test module | `2c9df87` / `50c8bb6` | https://github.com/dlroqa/Lightagent/actions/runs/40995946041 |
| 2026-10-30 | Fix: edit request `016-desktop-d0-key-scan`; branch fast-forwarded (parent `2c9df87`, no force) | `bc01426` / `4b135f9` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40996341547 |
| 2026-10-30 | `desktop` on `bc01426`: **success**. The self-test detected all 9 injected additions (slash, chat keys, ACP, MCP, extension manifest, config sections, HTTP routes, env, cli-tree); 0 drift errors; 306 rows; parity 0/225; the clap tree matches | `bc01426` / `4b135f9` | https://github.com/dlroqa/Lightagent/actions/runs/40996391964 |
| 2026-10-30 | `check` on `bc01426`: **success**, all 13 jobs (four-platform check, Web UI render, package smoke, release gate, contracts) | `bc01426` | https://github.com/dlroqa/Lightagent/actions/runs/40996392105 |
| 2026-10-30 | **D0 gate passed** at `bc01426` | — | — |

## D1 — Architecture and UI specification

- Decision: **Tauri 2** host with an owned `lightagent serve --desktop`
  sidecar, and typed IPC with a host-only HTTP client. The renderer never holds
  the key. See [DESKTOP_ARCHITECTURE.md](DESKTOP_ARCHITECTURE.md).
- The framework was decided on hosted evidence: spike run
  https://github.com/dlroqa/Lightagent/actions/runs/40997420599 (8/8 jobs) at
  `82896bc`. The Intel binary has `minos 10.14`; Electron 42's runtime
  requires macOS 12.0.
- Open risks carried into D2:
  - a 30 s Tauri first page load on Linux under Xvfb;
  - the arm64 plist minimum inconsistency;
  - slower Tauri startup than Electron on Windows;
  - 10.14 runtime unverifiable on hosted runners (blocked; no allowed alternative).
- Acceptance gates: [DESKTOP_ACCEPTANCE.md](DESKTOP_ACCEPTANCE.md). Merge
  order: [DESKTOP_MERGE_ORDER.md](DESKTOP_MERGE_ORDER.md).

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | Spike: edit request `017-desktop-d1-spike` on base `12582a0`; branch `desktop/d1-spike` created; draft PR #91 | `cf5cb85` / `89fe912` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40996500866 |
| 2026-10-30 | Spike run 1: all Tauri jobs succeeded and were measured; all Electron jobs **failed** before measuring (npm did not fetch the Electron binary) | `cf5cb85` | https://github.com/dlroqa/Lightagent/actions/runs/40996571854 |
| 2026-10-30 | Spike fix `018`: run Electron's install script explicitly; branch fast-forwarded | `82896bc` / `d9cde26` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40997083470 |
| 2026-10-30 | Spike run 2: **8/8 success**. Measurements are in DESKTOP_ARCHITECTURE.md §3 | `82896bc` | https://github.com/dlroqa/Lightagent/actions/runs/40997420599 |
| 2026-10-30 | D1 documents: edit request `019`, base `bc01426`; branch created without force; draft PR #92 (base `desktop/d0-audit`) | `350b48b` / `e43ab41` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40998171345 |
| 2026-10-30 | `desktop` on `350b48b`: **success** (inventory with the D1 resolutions, denominator 226) | `350b48b` | https://github.com/dlroqa/Lightagent/actions/runs/40998231436 |
| 2026-10-30 | `check` on `350b48b`: **success** | `350b48b` | https://github.com/dlroqa/Lightagent/actions/runs/40998231460 |
| 2026-10-30 | **D1 gate passed** at `350b48b` | — | — |

## D2 — CI and desktop skeleton

- `desktop/` is a separate Cargo workspace:
  - `src-tauri`, the host (Tauri 2.12.4): contract allowlist, stdin launch key, handshake and version check, SSE relay, and an HTTP client with no proxy and no redirects;
  - `mock-sidecar`, a std-only stand-in runtime with the same Host/Origin/Bearer checks.
- Frontend: `src/api/transport.ts` (IPC `fetch` and `EventSource` in the desktop build; unchanged in the browser), `DesktopGate`, and `vite build --mode desktop` → `dist-desktop`.
- Contract: `docs/desktop/contract/operations.json` (23 operations), checked by `scripts/check-desktop-contract.py`, which has a self-test.
- CI (`desktop.yml`):
  - contract and inventory checks;
  - host fmt, clippy, tests, and dependency policy;
  - shell bundle plus end-to-end runs (ready, version mismatch, key-leak scan, runtime-orphan observation) on four targets, with artifacts kept 30 days.
- Tooling: `tools/source-edit` now also formats touched `desktop/*.rs` files with
  `rustfmt --edition 2024` (tool commit `bcff82c`; never merged).
- Known D2 gaps, carried to D3 by design:
  - the runtime is the mock;
  - there is no parent-death watchdog, so the e2e records whether the runtime outlives a terminated host;
  - `desktop/Cargo.lock` is generated on the first run and committed afterwards.

## Artifacts and retention

| Artifact | Run | Retention |
|---|---|---|
| `prepared-015-desktop-d0-audit.txt`, `prepared-016-…` (commit, tree, diff) | 40995818991, 40996341547 | 30 days |
| `spike-tauri-<target>`: measure.json, screenshot, Cargo.lock, dmg/msi/nsis/deb/AppImage | 40997420599 (also 40996571854) | 30 days |
| `spike-electron-<target>`: measure.json, screenshot | 40997420599 | 30 days |
| `lightagent-web-screens` (existing `check` job) | 40996392105 | repository default |

## Local backup

- Backup clone: `~/Repositories/LAcode/Lightagent` on the owner's machine
  (origin `https://github.com/dlroqa/Lightagent.git`). It is synchronized with
  `git fetch` only: no reset, no clean, no force.
- Local progress copy: `~/Repositories/LAcode/desktop-progress/`.
- Last synchronization: 2026-10-30T03:33:59Z, by fetch only. Backed up:
  - `master` `12582a0`;
  - `origin/desktop/d0-audit` `bc01426`;
  - `origin/desktop/d1-spike` `82896bc`.

  The working tree was clean.

## Next

1. Get the D2 jobs green on the exact head, commit `desktop/Cargo.lock`, and record the evidence.
2. D3a: `lightagent-ops` plus `serve --desktop` in the real runtime (stdin key, Host/Origin, handshake, `--profile`, `--parent-pid`), replacing the mock.
