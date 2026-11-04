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
| D2 CI and desktop skeleton | **CI-validated** (gate passed) | `desktop/d2-shell` (stacked on D1) | https://github.com/dlroqa/Lightagent/pull/93 | `003a71c2473f3e18cb503f326ae3fc8a153dcc38` | `003a71c` / tree `d5fabeed9cb429f227689920ab03139c5d921f74` |
| D3a Real runtime: `serve --desktop` | **CI-validated** (gate passed) | `desktop/d3a-runtime` (stacked on D2) | https://github.com/dlroqa/Lightagent/pull/94 | `130dde98e5cade253b00d69f8e1c499587128e3d` | `130dde9` / tree `ab98037ea76bd083da863dd88c112dc4c7a5f5fc` |
| D3b Host lifecycle: crash, restart, single instance | **CI-validated** (gate passed) | `desktop/d3b-lifecycle` (stacked on D3a) | https://github.com/dlroqa/Lightagent/pull/95 | `1f0ed6900c166fc1dd32ed3ab38d546119929635` | `1f0ed69` / tree `4d70aaf397fc5d4f50f65906579e46b035a88063` |
| D4a Profiles: shared operations and API | **CI-validated** (gate passed) | `desktop/d4a-profiles` (stacked on D3b) | https://github.com/dlroqa/Lightagent/pull/96 | `c544d7ae40f8962c287ae39a8234e1665aaf6796` | `c544d7a` / tree `78dcded80caee0555f30a7304d97f9a42a9813e7` |
| D4b Profile switch, close with runs, Profiles screen | **CI-validated**: the review correction `5f364e2` passed `check` and `desktop` (see "D4b review") | `desktop/d4b-profile-ui` (stacked on D4a) | https://github.com/dlroqa/Lightagent/pull/97 | `5f364e27470f70b65fb13afe246e9302440ff4f4`, plus documentation-only commits (this one included) | `5f364e2` / tree `355aefbccb02b88d530d8b796b06952dc2272eec` |
| #98 response actions (outside the stack; see DESKTOP_MERGE_ORDER.md) | `check` passed on #98's head; with D4b plus the adoption commit, `check` and `desktop` passed | `feat/response-action-bar`; evidence `integration/desktop-d4b-response-actions` | https://github.com/dlroqa/Lightagent/pull/98; evidence https://github.com/dlroqa/Lightagent/pull/99 | `987c6dba0f4d536477f87a78d1ecc46d7a90af96`; integration `b4a61c970989b59e382f9dfe2338c01d4f96747f` | `987c6db`; integration `b4a61c9` / tree `4d4cedbf340ea11ba57c23550a556f7dc400dd1d` |
| D1 spike (evidence only, never merged) | runner evidence complete | `desktop/d1-spike` | https://github.com/dlroqa/Lightagent/pull/91 | `82896bcd63f3bd6786dcea12b603241e50a3a9ea` | `82896bc` / tree `d9cde26142a63a716a58f35dc33330be0e90d8b7` |
| D4c Configuration: one locked, revision-checked writer | **CI-validated** at `0e46892` (check and desktop, every job); the D4 gate items listed under D4c remain | `desktop/d4c-config` (stacked on D4b `7c3e988`) | https://github.com/dlroqa/Lightagent/pull/101 | `0e4689232ae0ce0653367883222b40d41b62218d`, plus this documentation commit | `0e46892` / tree `dd1faa22ddd161d99de2407a312e8ed9d0789c54` |
| Row 9: response actions delivery (#98 with its adoption) | **CI-validated** at `a51449c` (`desktop` after one re-run of an infrastructure-failed bundling job; see "Row 9") | `desktop/d4c1-response-actions` (stacked on D4c) | https://github.com/dlroqa/Lightagent/pull/102 | `a51449ce66790a1092459167545ac645cf226290`, plus this documentation commit | `a51449c` / tree `ab20d7eb19eecbc47a60a0a9e4a4df1bd85926aa` |
| Row 10: session writes (#100) | **CI-validated** at `aebd5d6` (check and desktop, every job) | `desktop/d4c2-session-writes` (stacked on row 9 `fbd008c`) | https://github.com/dlroqa/Lightagent/pull/103 | `aebd5d631238b1b8a302df1ec1aa7bba27972369`, plus the documentation commit `3310dfc` | `aebd5d6` / tree `e9066c30eac669165e9deb505ee67a0884659df8` |
| Row 11: D4 acceptance (golden files, ordered concurrency, fail-closed keys, routing report, kept Settings edits) | **CI-validated** at `205f01b` (check and desktop, every job, first run). The D4c gate items are met, except those carried forward (see "Row 11") | `desktop/d4c3-config-acceptance` (stacked on row 10 `3310dfc`) | https://github.com/dlroqa/Lightagent/pull/104 | `205f01b731e0a3149ff465bc894aa97605284c76`, plus the documentation commit `597604d` (check and desktop passed on it) | `205f01b` / tree `b5bf552d9d4e75630b3c3ecda78c58071d15a87f` |
| Row 12: D4d-1 provider credentials (shared write-only operation; unusable keys fail closed) | **CI-validated** at `6988f6c` (check and desktop, every job) | `desktop/d4d1-credentials` (stacked on row 11 `597604d`) | https://github.com/dlroqa/Lightagent/pull/106 | `6988f6c3256b46d8cc55704e5c3bc2c9e4016b90`, plus the documentation commit `8f0717d` (check and desktop passed on it) | `6988f6c` / tree `bcdfc0b89c6f27047bee39608db9374cc618dad9` |
| Row 13: D4d-2a endpoint check (explicit, bounded, no redirect with the key) | **CI-validated** at `e6f4506` (check and desktop, every job) | `desktop/d4d2-endpoint-check` (stacked on row 12 `8f0717d`) | https://github.com/dlroqa/Lightagent/pull/107 | `e6f45069c88120c530caeb975b66026d780016dd`, plus this documentation commit | `e6f4506` / tree `e33be00726f6236d4e21c8410fe540a2de9283ca` |
| Row 14: D4d-2b-1 a provider key goes only to the endpoint it was saved for | **CI-validated** at `8e3919b` (check, desktop, jev evaluation, subagents; first run) | `desktop/d4d2b-key-binding` (stacked on row 13 `2bed55c`) | https://github.com/dlroqa/Lightagent/pull/108 | `8e3919bd5c2ccd512cbd5c31f78cfb4173197a27` | `8e3919b` / tree `ec8ed729801330750bb2314fcbd201e265060740` |
| Row 15: D4d-2b-2 saved providers and model through one shared operation set | **CI-validated** at `108dac6` (check, desktop, jev evaluation, subagents; first run) | `desktop/d4d2b2-saved-providers` (stacked on row 14) | https://github.com/dlroqa/Lightagent/pull/109 | `108dac6275435d16ede51db4e839f290fb05d4e8` | `108dac6` / tree `868f78d9afbc50ab5369e208d983b8801d488d7a` |
| Row 16: D4d-2b-3 Providers and models card in Settings | **CI-validated** at `bb76e74` (check, desktop, jev evaluation). `subagents` failed on one live-broker job (GitHub did not list a dispatched run in time); recorded, not re-run; row 17's `subagents`, whose tree contains row 16, passed | `desktop/d4d2b3-providers-ui` (stacked on row 15) | https://github.com/dlroqa/Lightagent/pull/110 | `bb76e74454e91f91f09a3a1c4c53dbf5458905dd` | `bb76e74` / tree `d3bc96fa1f0d068c75ec16c8fbebc6eecc159f66` |
| Row 17: D4d-3 every configured remote credential fails closed | **CI-validated** at `c15e249` (check, desktop, jev evaluation, subagents) after a test-setup fix (see "Row 17") | `desktop/d4d3-fail-closed` (stacked on row 16) | https://github.com/dlroqa/Lightagent/pull/111 | `c15e2491cc959b64d1557bf09606e6c71701152a`, plus this documentation commit | `c15e249` / tree `aea17cf71d4615cd76c1c6903213584afb8da4a2` |
| Row 18: D4e the tools workspace through one shared, validated operation | **CI-validated** at `ec1fc4e` (check, desktop, jev evaluation, subagents; first run) | `desktop/d4e-workspaces` (stacked on row 17's documentation commit `bf5e56d`) | https://github.com/dlroqa/Lightagent/pull/112 | `ec1fc4e1eab9428e6f8afd5e38b1b2de97c8846b` | `ec1fc4e` / tree `34e2aa7c0253a236e90394a7c6401d6faaec0c27` |
| Row 19: D5b one turn at a time per session across every process (#105) | **CI-validated** at `1e657e5` (check, desktop, jev evaluation, subagents) after two fixes (see "Row 19") | `desktop/d5b-run-admission` (stacked on row 18) | https://github.com/dlroqa/Lightagent/pull/113 | `1e657e5b5ee471e9ee2dc9817deb176919ddaede`, plus this documentation commit | `1e657e5` / tree `686bf3a3acd773fca3b60f12704fa95ada0ec699` |
| Row 20: D5b follow-through, a dropped run lease is free at once | **CI-validated** at `e5363fc` (check, desktop, jev evaluation, subagents; first run) | `desktop/d5b2-lease-release` (stacked on row 19 `db44a46`) | https://github.com/dlroqa/Lightagent/pull/114 | `e5363fc9c875c6b83c1606b24542d247e25f0e3f` | `e5363fc` / tree `ce2f291600512e2105ccb47ced1895342ba44f0c` |
| Row 21: D5a-1 the host relay resumes a broken stream; runs through the host | **CI-validated** at `64d632f` (check, desktop, jev evaluation, subagents; one macOS arm64 bundling job was never given a runner and was re-run once, passing) | `desktop/d5a1-host-runs` (stacked on row 20) | https://github.com/dlroqa/Lightagent/pull/115 | `64d632ff37f4033343baf3ce2ec663b0dae4b999` | `64d632f` / tree `169f76fb18559562563b792876203128e4f1b7e0` |
| Row 22: D5a-2 a turn its runtime did not live to record is Interrupted, never rerun | **CI-validated** at `c6fd6ac` (check, desktop, jev evaluation, subagents; first run) | `desktop/d5a2-interrupted` (stacked on row 21) | https://github.com/dlroqa/Lightagent/pull/116 | `c6fd6aca300ca93bd276c8f4e879586c9747dd58`, plus this documentation commit | `c6fd6ac` / tree `c98ac0acbcd1f716f2a3610b111337158eddf3bd` |
| Row 23: D5a-3 time-budget pause and Continue over the API; steering semantics; every OS lock unlocks on drop | **CI-validated** at `0d37299` (check, desktop, jev evaluation, subagents) after three fixes (see "Row 23") | `desktop/d5a3-continue` (stacked on row 22 `da950d1`) | https://github.com/dlroqa/Lightagent/pull/117 | `0d372994fb15bb2353af7291bbdaf50976a12809` | `0d37299` / tree `1a85dff15c8bfcef2cdfdca1034342ad711b489e` |
| Row 24: D5a-4 the Continue card, Stop in the conversation view, browser coverage of the run controls | **CI-validated** at `e164dde` (check including the standalone Web UI render, desktop, jev evaluation, subagents) after two product fixes the test found (see "Row 24") | `desktop/d5a4-run-controls-ui` (stacked on row 23) | https://github.com/dlroqa/Lightagent/pull/118 | `e164dde7b172b1c74d84b423cadd9479ee1d7cdc`, plus this documentation commit | `e164dde` / tree `37b104da3779c007d1f65b8b70db1b72e377b4be` |
| D6–D10 | not started | — | — | — | — |

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

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | Tooling: `tools/source-edit` formats touched `desktop/*.rs` files (never merged) | tool `bcff82c` | — |
| 2026-10-30 | Edit request `020-desktop-d2-shell` on base `350b48b`; the runner ran `rustfmt` on 5 files; workflow blob verified and attached; branch created without force; draft PR #93 | prepared `3c724e9`; final `735cffe` / `099a477` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/40999368319 |
| 2026-10-30 | `desktop` on `735cffe`: **success**, 6/6 jobs (details below) | `735cffe` | https://github.com/dlroqa/Lightagent/actions/runs/40999470324 |
| 2026-10-30 | `check` on `735cffe`: **success** (includes the browser Web UI build and render with the transport change) | `735cffe` | https://github.com/dlroqa/Lightagent/actions/runs/40999470417 |
| 2026-10-30 | Lockfile commit: `desktop/Cargo.lock` taken byte-for-byte from the `desktop-cargo-lock` artifact of run 40999470324 (409 crates) | this commit | recorded in the next update |

D2 runner evidence at `735cffe` (runner measurements, mock runtime). Times are seconds from app spawn:

| Target | Runtime ready | Page loaded | UI ready | First API call | Mismatch shown | Package | Minimum OS in binary |
|---|---|---|---|---|---|---|---|
| macOS x86_64 (`macos-15-intel`) | 1.87 | 3.35 | 3.93 | 4.29 | 1.16 | dmg 5.1 MB | host and runtime `minos 10.14`; plist 10.14 |
| macOS arm64 (`macos-15`) | 1.93 | 3.98 | 4.59 | 5.56 | 2.96 | dmg 4.9 MB | `minos 11.0`; plist **11.0** (D1 inconsistency fixed) |
| Windows x64 (`windows-latest`) | 3.02 | 3.12 | 3.12 | 3.22 | 0.64 | nsis 3.0 MB, msi 3.8 MB | — |
| Linux x64 (`ubuntu-22.04`, Xvfb) | **30.26** | 30.96 | 31.06 | 31.16 | 30.46 | deb 3.9 MB, AppImage 78.8 MB | — |

- **Launch-key leak scan:** no 64-hex value in any argv, any readable
  environment, or the report, on all four targets in both modes.
- **Version mismatch:** refused on every target, with the explanatory screen
  (screenshot artifact).
- **Runtime outlived a terminated host:** `true` on all four targets in ready
  mode. This is the expected D2 gap; D3 must make it `false` (parent watchdog,
  job object, process group).
- **Linux 30 s delay:** `RUNTIME_READY` itself arrives at about 30 s, and
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` changes nothing (30.16 s). The delay is
  therefore before Tauri's setup hook runs, not in rendering. It is consistent
  with GTK waiting on a missing D-Bus session bus under bare Xvfb. D3 tests this
  with `dbus-run-session`.
- **Visual note for D8:** in the macOS WKWebView screenshot, the gradient
  heading and the brand text render with low contrast. Compare with the
  Chromium render before changing styles.
- **Dependency policy:** clean on all targets (409 crates).

## D3a — The real runtime in desktop mode

- `lightagent serve --desktop [--profile <id>]` (`crates/lightagent/src/serve.rs`, `run_desktop`):
  - loopback only;
  - the key is the first line of stdin, and the rest of stdin is the **lifeline**: on EOF, graceful shutdown, then exit within 5 s;
  - scaffolds an empty home and the protected `default` profile on first start;
  - writes one JSON handshake line to stdout and nothing else there;
  - serves no Web UI.
- `lightagent_api::desktop` is the guard on every route, including `/health`
  (otherwise unauthenticated) and the event stream. It checks the exact
  `Host`, refuses any `Origin` and any `Sec-Fetch-Site` other than `none`,
  and compares the Bearer key in constant time. Each refusal has its own
  category. Unit tests cover the guard; `crates/lightagent/tests/desktop_serve.rs`
  drives the real binary over raw TCP (no key, wrong key, other Host, Origin,
  cross-site, `/health`, SSE, no Web UI, lifeline exit, refused binds and a
  missing key).
- Host:
  - `--parent-pid` is gone (the real CLI never had it); the host keeps stdin open as the lifeline;
  - `stop()` closes the lifeline, then waits up to 5 s before killing.

  The mock got the same lifeline.
- The shell CI bundles the real `lightagent` (built `--locked` in the root
  workspace) with an isolated `LIGHTAGENT_HOME`. The ready e2e now **fails**
  if the runtime outlives a terminated host (`E2E_REQUIRE_NO_ORPHAN=1`).
  Version mismatch is still proven by swapping in the mock. Linux also runs
  under `dbus-run-session` to test the D-Bus hypothesis for the 30 s delay.
- Inventory: two host-owned rows (`serve --desktop` and `--profile`); 308 rows; denominator unchanged at 226.
- Architecture §4 and §6 were updated: the stdin lifeline replaces `--parent-pid`, with the reasons.

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | D2 lockfile commit validated: `desktop` (all builds `--locked`) and `check` **success**. **D2 gate passed** at `003a71c` | `003a71c` / `d5fabee` | https://github.com/dlroqa/Lightagent/actions/runs/41000548478, https://github.com/dlroqa/Lightagent/actions/runs/41000548458 |
| 2026-10-30 | Edit request `022-desktop-d3a-runtime` on base `003a71c`; workflow blob verified and attached; branch created without force; draft PR #94 | prepared `109be1e`; final `130dde9` / `ab98037` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41001162717 |
| 2026-10-30 | `desktop` on `130dde9`: **success** (details below) | `130dde9` | https://github.com/dlroqa/Lightagent/actions/runs/41001259889 |
| 2026-10-30 | `check` on `130dde9`: **success**. The guard unit tests and both real-binary `desktop_serve` tests pass on macos-arm64, macos-x64, windows-x64 and linux-x64 | `130dde9` | https://github.com/dlroqa/Lightagent/actions/runs/41001259987 |
| 2026-10-30 | **D3a gate passed** at `130dde9` | — | — |

D3a runner evidence with the **real runtime** bundled. Seconds from app spawn:

| Target | Runtime ready | First API call | Runtime outlived the host | Package |
|---|---|---|---|---|
| macOS x86_64 | 2.32 | 4.42 | **no** | dmg 9.8 MB (runtime `minos 10.14`) |
| macOS arm64 | 1.13 | 3.82 | **no** | dmg 9.2 MB (`minos 11.0`) |
| Windows x64 | 4.86 | 5.06 | **no** | nsis 6.1 MB, msi 8.1 MB |
| Linux x64, Xvfb without a session bus | 30.15 | 30.85 | **no** | deb 8.6 MB, AppImage 83.1 MB |
| Linux x64, Xvfb under `dbus-run-session` | **0.20** | **0.40** | **no** | — |

- **The D2 orphan gap is closed:** the lifeline makes the runtime exit with
  its host on every target.
- **The Linux 30 s delay is explained:** with a D-Bus session bus, as every
  Linux desktop session has, the app is ready in 0.2 s. The delay was an
  artifact of bare Xvfb. Linux e2e now runs under `dbus-run-session`, and the
  no-bus case is recorded as a residual risk.
- No key leaks in either mode, and the version mismatch is refused, on every target.

## D3b — Host lifecycle

- **Crash detection:**
  - the host checks its runtime every 0.5 s;
  - an unexpected exit is reported (`failed`, with the exit status), and its output is kept for diagnostics;
  - the runtime is restarted automatically **at most once per 60 s**, so a runtime that keeps failing stays down and says so.
- **`runtime_restart`** (user-initiated) and `runtime_logs`, which also shows the last crashed runtime's output.
- **Single instance:**
  - an exclusive lock (`std::fs::File::try_lock`, no new crates) on `instance.lock` in the app's local data directory, held for the app's lifetime;
  - a second launch starts **no runtime** and explains itself;
  - the OS releases the lock if the app dies.
- **UI (`DesktopGate`):**
  - keeps watching after start-up; a stopped runtime shows a modal notice over the kept, inert app;
  - failures offer **Restart runtime** and a **Runtime log** disclosure;
  - duplicate, mismatch and failure each state their cause and remedy.
- **CI:** e2e modes `crash` (kill the runtime under a ready app; require the
  exit to be reported and a restart) and `duplicate` (second launch; require
  no second runtime and the first still running), both with
  `E2E_REQUIRE_NO_ORPHAN=1`. Linux runs under a session bus by default.
- **Deferred to D4a:** a close with active runs (warn, or cancel then quit),
  which needs the active-runs operation.

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | Edit request `023-desktop-d3b-lifecycle` on base `130dde9`; workflow blob verified and attached; branch created without force; draft PR #95 | prepared `abc1e9f`; final `9dc6783` / `4465812` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41002983519 |
| 2026-10-30 | `check` on `9dc6783`: **success** | `9dc6783` | https://github.com/dlroqa/Lightagent/actions/runs/41003068075 |
| 2026-10-30 | `desktop` on `9dc6783`: **failure**. Inventory, host (fmt, clippy, tests), and the macos-arm64, windows-x64 and linux-x64 shells passed every e2e mode. The **macos-x64 shell failed while packaging, before any test ran**: `hdiutil: create failed - Resource busy` from Tauri's `bundle_dmg.sh` | `9dc6783` | https://github.com/dlroqa/Lightagent/actions/runs/41003067980 (job 123891687408) |
| 2026-10-30 | Investigation: the failing step is the OS disk-image tool, not desktop code; D3b changed no bundling input; the same step passed on that runner image in D2 and D3a. Mitigation (the next commit): retry **only** that exact `hdiutil` message, at most 3 attempts, with every attempt's log kept as an artifact and each retry reported in the job summary. Any other failure still fails at once | — | — |
| 2026-10-30 | Fix: edit request `024-desktop-d3b-dmg-retry`; branch fast-forwarded without force | `1f0ed69` / `4d70aaf` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41004969575 |

D3b e2e results at `9dc6783` on the three shells that ran (seconds from the event that started each check):

| Target | Ready (first API call) | Crash: exit reported → restarted | Duplicate: second launch refused, no runtime | Mismatch refused | Runtime outlived the host |
|---|---|---|---|---|---|
| macOS arm64 | 5.23 | 2.98 → 2.98 | 4.36 (UI 5.10) | yes | no |
| Windows x64 | 2.84 | 1.04 → 1.04 | 0.75 (UI 0.85) | yes | no |
| Linux x64 (session bus) | 1.50 | 0.82 → 0.82 | 0.91 (UI 1.21) | yes | no |
| Linux x64, no session bus (recorded only) | 30.65 | — | — | — | no |

| 2026-10-30 | `desktop` and `check` on `1f0ed69`: **success**. The `hdiutil` retry did **not** fire (every DMG bundled on the first attempt). Crash, duplicate and ready modes passed on both macOS targets with no orphan and no key leak. **D3b gate passed** at `1f0ed69` | `1f0ed69` / `4d70aaf` | https://github.com/dlroqa/Lightagent/actions/runs/41005006873, https://github.com/dlroqa/Lightagent/actions/runs/41005006806 |

## D4a — Profiles: shared operations and API

- **Shared operations** (`crates/lightagent/src/ops.rs`, `ops::Profiles`):
  list, show, create, preset, use (an atomic active-profile write), and
  delete (an active profile hands over to `default`; `default` is
  protected). Each returns data. `lightagent profile …` now calls them with
  **unchanged output**, and `lightagent serve` performs the same operations
  for the API through the existing `RunFactory` seam.
- **Seam decision:** the operations live in the CLI crate and reach the API
  through `RunFactory`, not through a new `lightagent-ops` crate. A new crate
  would change the root `Cargo.lock`, which cannot be regenerated locally under
  the execution policy, and the CLI crate already owns every store. The
  architecture's §12 crate name is superseded by this seam (recorded in D4b's
  architecture update).
- **API** (operator scope, `Scope::Admin`; every other caller is refused):
  `GET /runs` (active runs), `POST /profiles`, `GET|DELETE /profiles/{id}`,
  `PUT /profiles/{id}/active`, `POST /profiles/{id}/preset`. Errors are
  `{"error":{"category","message"}}` with stable categories (`not_found`
  404, `invalid` 400, `conflict` 409, `denied` 403, `unavailable` 503,
  `internal` 500).
- **Contract:** 29 operations, including `profiles.delete`, which is marked
  destructive. **Inventory:** the six new API rows; the CLI profile rows now
  name their shared operation and route. The denominator is unchanged at
  226: these rows count once the desktop UI exists (D4b).
- **Evidence test** (`crates/lightagent/tests/desktop_profiles.rs`): the
  real runtime in desktop mode creates, shows, presets, activates and
  deletes profiles over HTTP, and `lightagent profile list --json` on the
  same home reports exactly that state (CLI–desktop equivalence). It also
  checks the five error categories and the hand-over to `default`.

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | Edit request `025-desktop-d4a-profiles` on base `1f0ed69`; `cargo fmt` ran; branch created without force; draft PR #96 | `c544d7a` / `78dcded` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41005062603 |
| 2026-10-30 | `check` on `c544d7a`: **success**. `api_profile_operations_are_the_cli_profile_operations` passed on all four platforms; the existing CLI tests pass with the refactored `profiles_cmd` | `c544d7a` | https://github.com/dlroqa/Lightagent/actions/runs/41005161337 |
| 2026-10-30 | `desktop` on `c544d7a`: **success** (contract with 29 operations; inventory of 314 rows) | `c544d7a` | https://github.com/dlroqa/Lightagent/actions/runs/41005161216 |
| 2026-10-30 | **D4a gate passed** at `c544d7a` | — | — |

## D4b — Profile switch, close with active runs, Profiles screen

- **Host profile switch** (`desktop/src-tauri/src/switch.rs`, command
  `profile_switch`): start the new runtime, then commit (architecture §7).
  - While runs are working, the switch is blocked and the UI lists them.
  - If the person chooses to stop them, pending approvals are rejected
    explicitly (recorded) and the runs cancelled, with a 10 s drain;
    otherwise nothing changes.
  - The new runtime starts on the profile with its own key. If it is not
    ready, it is stopped and nothing has changed.
  - Commit: the profile is made active through the new runtime (the CLI sees
    it), the app is routed to the new runtime, old streams end with
    `profile_switch`, and the old runtime is stopped off the UI thread.
  - Only one switch runs at a time. The planning decision is unit-tested.
- **Close with active runs:** the host holds the close and the UI asks
  (**Keep working**, focused by default, or **Stop runs and quit**). With no
  runs, the window closes as before.
- **Profiles screen** (`#/profiles`, from the account menu):
  - list, details, preset, create (labelled fields, the id rule shown, errors next to the form);
  - **Use**: in the desktop this switches the runtime and reloads; in the browser it sets the CLI's active profile and says this server keeps serving its own until it restarts;
  - **Delete** behind typed confirmation; `default` is protected.
- **Evidence planned:** `e2e/profiles.mjs` (in the existing `standalone Web UI
  render` job) drives create, invalid id, preset, use and delete through the
  real UI against real `lightagent serve`, and checks the server's profile
  store after each step.
- **Gap, stated rather than claimed:** the desktop-only switch and
  close-with-runs dialogs have **no hosted native-UI automation** yet. They
  are covered by unit tests of the decision, by the API-level D4a test, and
  by the host compile and clippy gates. Native WebView automation
  (`tauri-driver` on Linux and Windows) is planned for D8; until then those
  two flows are not claimed as end-to-end verified.
- **Architecture §7 and §12** now describe the operations seam that was built.

| When (UTC) | Event | Commit / tree | Evidence |
|---|---|---|---|
| 2026-10-30 | A first push of request `026` was **empty**: the helper scripts were lost with a session restart, and `base64` failed. Its source-edit run failed with "request names no base commit"; no commit was created and no branch moved | — | source-edit run for `026` (failure) |
| 2026-10-30 | Edit request `026b-desktop-d4b-profile-ui` on base `c544d7a`; `rustfmt` ran; branch created without force; draft PR #97 | `75fa438` / `1e0f2f5` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41051926559 |
| 2026-10-30 | `check` on `75fa438`: **success**, including `standalone Web UI render`, where the new Profiles flow passed (invalid id refused, create, details, preset, use, typed-confirmation delete, `default` protected; the store checked after each step) | `75fa438` | https://github.com/dlroqa/Lightagent/actions/runs/41052025410 |
| 2026-10-30 | `desktop` on `75fa438`: **success** (host fmt, clippy and tests including `switch` unit tests; four-target shells; e2e ready, crash, duplicate and mismatch) | `75fa438` | https://github.com/dlroqa/Lightagent/actions/runs/41052025446 |
| 2026-10-30 | Reviewing the Profiles screenshot (artifact `lightagent-web-screens`) found two defects the checks did not catch: the row's name and its id·model line ran together, and a deleted profile's details stayed on screen. Fixed in `fb7ac39`, and the e2e now asserts that the details disappear | `fb7ac39` / `2298c7d` | screenshot `profiles.png`, run 41052025410; source-edit https://github.com/dlroqa/Lightagent/actions/runs/41055739535 |
| 2026-10-30 | `desktop` on `fb7ac39`: **success**. `check` on `fb7ac39`: **failure**, only in `standalone Web UI render`: after **Create**, the details card never appeared. Cause: the new "clear details when the profile is gone" effect saw the catalog from *before* the reload, which lacked the just-created profile, and cleared the selection. The fix (this commit) clears the selection explicitly when that profile is deleted, with no inference from a possibly stale catalog | `fb7ac39` | https://github.com/dlroqa/Lightagent/actions/runs/41055791157 (job 124062872105), https://github.com/dlroqa/Lightagent/actions/runs/41055791246 |
| 2026-10-30 | Fix: edit request `028-desktop-d4b-detail-race`; branch fast-forwarded | `cd799a4` / `0627bd8` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41057740598 |
| 2026-10-30 | `desktop` on `cd799a4`: **success** | `cd799a4` | https://github.com/dlroqa/Lightagent/actions/runs/41057799963 |
| 2026-10-30 | `check` on `cd799a4`, attempt 1: **failure**, only in `paired Lightweight contract E2E`. The job failed while GitHub set it up, before any repository code ran: `Unable to resolve action actions/checkout@v7, unable to find version v7`. The other 12 jobs of the same run resolved the same action. Classified as a transient GitHub action-resolution failure, not a code defect. **One** re-run of that job only (GitHub keeps attempt 1's log) | `cd799a4` | https://github.com/dlroqa/Lightagent/actions/runs/41057799814 (attempt 1, job 124069612248) |
| 2026-10-30 | `check` on `cd799a4`, attempt 2: **success**, all jobs, including the Profiles UI flow in `standalone Web UI render` | `cd799a4` | https://github.com/dlroqa/Lightagent/actions/runs/41057799814 |
| 2026-10-30 | **D4b gate passed** at `cd799a4`. Parity: 15 of 226 rows implemented (the profile list, show, create, preset, use and delete capabilities with their inputs), each citing `cd799a4` and its runs. `cli.profile.use` records the desktop switch's native-automation gap | — | DESKTOP_CLI_PARITY.md, "Implemented rows and evidence" |

## D4b review — the switch commit made all or nothing

- **Concern (owner review of `8c27f93`):** `switch.rs` replaced the runtime
  even when the active-profile write failed, and returned `persisted: false`,
  which the UI ignored. The desktop could then serve one profile while the
  CLI's active profile named another. That broke start-new-then-commit. The
  architecture's §7 step 5 had even documented it.
- **Two more problems found while reviewing:**
  - a run could start between the active-run check and the replacement;
  - crash recovery's automatic restart could install a runtime at the same
    time as a switch's swap, so one silently replaced the other.
- **Correction (this commit; architecture §7 revised):**
  - One host lifecycle lock serialises start, restart, crash recovery and
    switch. A second switch or restart during one is refused as `busy`; the
    watcher skips its check while the lock is held.
  - While a switch runs, the host refuses `runs.create` and
    `approvals.respond`, and it checks the old runtime once more just
    before committing.
  - The commit persists the active profile **first**, through the new
    runtime, and replaces the runtime only after that succeeds.
  - Every failure (`start_failed`, `runs_appeared`, `persist_failed`,
    `runs_not_stopped`, `busy`) stops the unused new runtime, keeps the old
    runtime and the CLI's active profile, and returns an actionable error
    saying nothing changed. `persisted` is gone from the outcome, so a
    reported switch is always fully committed.
- **Hosted coverage added:** job `desktop profile switch (linux, e2e
  hooks)`. A Linux test build of the host with `--features e2e-hooks`
  (never the bundled app) runs four real scenarios against the real
  runtime, sharing one isolated home:
  1. **ok:** switched, the CLI's active profile changes, and exactly one runtime remains.
  2. **start failure:** switch to a missing profile; the new runtime cannot start, `work` stays in service, and the CLI is unchanged.
  3. **commit failure:** a directory occupies `active_profile.tmp`, so the atomic write really fails; `work` stays in service, the CLI is unchanged, and the unused runtime is stopped.
  4. **concurrent:** two switches at once; one switches and the other is refused as busy.

  Each scenario requires exactly one runtime to remain. Unit tests cover
  the run gate and the lifecycle lock; clippy covers the hook code.
- **Still not claimed:** native UI automation of the switch and close
  dialogs (D8). The scenarios above exercise the host's switch path for
  real, but not by clicking the WebView.
- **Validated:** the correction `5f364e2` (tree `355aefb`, source-edit
  https://github.com/dlroqa/Lightagent/actions/runs/41139470951) passed
  `desktop` https://github.com/dlroqa/Lightagent/actions/runs/41139553166.
  All 7 jobs passed, including `desktop profile switch (linux, e2e hooks)`
  and the capability inventory. It also passed `check`
  https://github.com/dlroqa/Lightagent/actions/runs/41139553292. The runs
  date from 2026-10-30 and are recorded on 2026-11-01. The gate reopened
  for the correction is met at `5f364e2`.

## #98 response actions and the desktop stack (2026-11-01)

#98 adds an action row under the final Web UI answer and
`POST /api/lightagent/v1/sessions/{id}/regenerate`. It is outside the
desktop stack and does **not** replace D4c. Its own record is
`docs/response-action-bar-progress.md` on its branch, which notes a policy
departure: its first commits were edited locally and pushed with
`git push`. Its review commit and everything after went through
source-edit.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-01 | Review fix (edit request `031`): the saved answer is kept until a regeneration completes with an answer. Session write access is required. New tests cover failure, cancellation, concurrency, a stale id, scopes and ownership | `987c6db` / `d6bd0ad` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41354616741 |
| 2026-11-01 | `check` on `987c6db`: **success** | `987c6db` | https://github.com/dlroqa/Lightagent/actions/runs/41354677426 |
| 2026-11-01 | Evidence branch `integration/desktop-d4b-response-actions` created at `5f364e2`, then a server-side merge of `987c6db`, with no conflicts | `3b64f25` / `675266d` | `POST /repos/…/merges` |
| 2026-11-01 | `check` on `3b64f25`: **success**. `desktop`: **failure**, only in `desktop capability inventory`: the new route is missing from the inventory and from the contract (server and client) | `3b64f25` | https://github.com/dlroqa/Lightagent/actions/runs/41354728282, https://github.com/dlroqa/Lightagent/actions/runs/41354730506 |
| 2026-11-01 | Adoption (edit request `032`): contract operation `sessions.regenerate`; the host refuses it during a switch (unit-tested); inventory and parity row `api.post.sessions.id.regenerate`, **not-started** on desktop (denominator 226 → 227, implemented 15) | `b4a61c9` / `4d4cedb` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41356132619 |
| 2026-11-01 | `desktop` on `b4a61c9`: **success**, all 7 jobs. `check`: **success**, all 13 jobs | `b4a61c9` | https://github.com/dlroqa/Lightagent/actions/runs/41356206421, https://github.com/dlroqa/Lightagent/actions/runs/41356206521 (draft PR #99) |

These are not desktop capabilities. None counts toward parity:
- Ratings are temporary.
- Sharing sends text.
- Branching is disabled.
- There is no previous-answer navigation.
- The regenerate row is **not-started** on desktop: no native UI automation
  covers it (D8), and clipboard, Web Share and speech synthesis are
  unverified in the desktop WebView.

## D4c — Configuration: one locked, revision-checked writer

Prerequisite: D4b head `7c3e988` (tree `494dffb`) passed `check`
https://github.com/dlroqa/Lightagent/actions/runs/41357376468 and `desktop`
https://github.com/dlroqa/Lightagent/actions/runs/41357376560 on every job.

- **One writer.** `ConfigStore::update(expected, change)` in
  `lightagent-core`:
  1. Takes an exclusive OS lock on `config.json.lock` (std
     `File::try_lock`, so every process takes it, with no new dependency).
  2. Reads the file as it is now.
  3. Refuses an `expected` revision that no longer matches as `Conflict`. A
     revision is `sha256:` plus the hash of the file's bytes, or `absent`.
  4. Applies the change, validates the result, and writes atomically.

  A lock held longer than 5 s reports `Busy`, so a writer never waits
  indefinitely. `replace(expected, config)` is the revision-checked form for
  an editor working on a copy. `save` keeps its signature but takes the lock.
- **Every writer converted:**
  - `lightagent config set` and `init`;
  - extension install, enable and disable;
  - chat Markdown onboarding;
  - `setup`, which is revision-checked: it refuses to save over a change
    made while it was open, and says so;
  - `PUT /settings`, now a locked read-modify-write. With
    `expected_revision`, a stale save gets 409.
- **CLI = API.** The key rules of `config get|set` moved to
  `lightagent_core::config_keys`. New routes:
  - `GET /config`: the path, the revision, and the redacted config;
  - `GET /config/{key}`;
  - `PUT /config/{key}`, which takes `expected_revision` and gives 409 when
    it is stale.

  All three require the admin scope, like `/settings`. An invalid value gets
  400 with the CLI's exact message, a busy lock gets 503, and a conflict
  names the current revision.
- **Settings UI.** A save sends the revision it loaded, and on 409 the
  screen reloads and says so. A new "Advanced configuration" card reads and
  sets keys and shows the redacted config. Contract operations
  `config.show`, `config.get` and `config.set` are added, with inventory and
  parity rows (backend, counted through `cli.config.*`).
- **Tests (hosted):**
  - core: revisions, a stale update, refused and invalid changes, waiting
    for the lock and keeping what the holder wrote, and `Busy`;
  - `crates/lightagent/tests/desktop_config.rs`: CLI/API equivalence
    (values, `show`/`path`, errors), stale 409 on `/config` and `/settings`,
    secret redaction across CLI and API, and a cross-process lock test;
  - `e2e/config.mjs`: the editor's write, a stale refusal, and an invalid
    value.
- **Not done yet:** `jev-routing-report` has no API route or UI.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-01 | Edit request `034`: **failure** before any commit. One find block in `crates/lightagent-core/src/lib.rs` did not match | — | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41359082770 |
| 2026-11-01 | `034b` (that find block corrected, nothing else changed): 58 edits to 19 files; branch `desktop/d4c-config` created without force; draft PR #101 | `0e46892` / `dd1faa2` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41359185395 |
| 2026-11-01 | `check` on `0e46892`: **success**, all jobs. The new tests ran and passed on all four platforms: the five core `config::tests`, and the four `desktop_config.rs` tests. `[ok] settings advanced configuration` in `standalone Web UI render` | `0e46892` | https://github.com/dlroqa/Lightagent/actions/runs/41359276745 |
| 2026-11-01 | `desktop` on `0e46892`: **success**, all jobs, including the inventory and contract checks with the three new operations | `0e46892` | https://github.com/dlroqa/Lightagent/actions/runs/41359276774 |
| 2026-11-01 | Parity: `cli.config.show`, `path`, `get` (and its key argument), and `set` (and its key and value arguments) are marked implemented: **22 of 226**. `cli.config.jev-routing-report` is not | — | DESKTOP_CLI_PARITY.md |

**Still open against the D4 gate (DESKTOP_ACCEPTANCE.md):**
- The golden comparison of CLI and desktop files in *isolated* homes. The
  equivalence test uses one shared home and compares values and output.
- "Concurrent `config set` and settings save give exactly one 409": the
  stale save is tested, and the lock is tested across processes, but that
  exact concurrent pair is not.
- An unreadable secret file failing closed.
- No secret value in the renderer state dump or the diagnostics bundle.
  Redaction is tested in CLI and API output only.

The response-action requirements now carried to D5 and D8 are listed in
DESKTOP_ACCEPTANCE.md ("Carried requirements"). The session-write race is
tracked as https://github.com/dlroqa/Lightagent/issues/100. Its fix is
row 10 of the merge order.

## Row 9 — Response actions delivery (#98 with its desktop adoption)

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-01 | D4c docs commit `551e73a` (tree `9acdd82`): `check` and `desktop` **success**, so the inventory, contract and parity edits were re-checked | `551e73a` | https://github.com/dlroqa/Lightagent/actions/runs/41360645573, https://github.com/dlroqa/Lightagent/actions/runs/41360645549 |
| 2026-11-01 | Branch `desktop/d4c1-response-actions` created at `551e73a`, then a server-side merge of #98's head `987c6db`, with no conflicts | `5665b8f` / `5399c06` | `POST /repos/…/merges` |
| 2026-11-01 | One adoption commit (edit request `036`): contract `sessions.regenerate`, the switch gate with tests, and inventory and parity row (227, 22 implemented). Draft PR #102 | `a51449c` / `ab20d7e` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41360714579 |
| 2026-11-01 | `check` on `a51449c`: **success**, all jobs | `a51449c` | https://github.com/dlroqa/Lightagent/actions/runs/41360801995 |
| 2026-11-01 | `desktop` on `a51449c`, attempt 1: **failure**, only `desktop shell (macos-x64)`, while bundling the DMG: `hdiutil: couldn't eject "disk2" - Resource busy` (×3). The D3b retry matches only `hdiutil: create failed - Resource busy`, so this variant is not retried. No PR code runs in that step, and the same job passed on `551e73a`. Classified as runner disk-image contention. **One** re-run of that job only (attempt 1's log is kept, job 125010533746) | `a51449c` | https://github.com/dlroqa/Lightagent/actions/runs/41360802130 (attempt 1) |
| 2026-11-01 | `desktop` on `a51449c`, attempt 2: **success**, all 7 jobs | `a51449c` | https://github.com/dlroqa/Lightagent/actions/runs/41360802130 |

**Follow-ups:**
- The D3b bundling retry should also match the `couldn't eject … Resource busy`
  variant. That is a workflow change for the slice that owns the job, and it
  is not made here.
- #99 is left open as evidence until the owner reviews #102.
- Row 10 (#100): see "Row 10" below.

## Row 10 — Session writes: one locked writer (#100)

**Ownership.** On 2026-11-02 at about 00:12Z, the session that built rows 1–9
had ended its turn at 2026-11-01T23:52:42Z with its final report, and nothing
else was writing to the stack. One session took the implementation from
there. The branch was created at row 9's head `fbd008c`, and the claim and
the audit were recorded on #100.

**Defect.** Every session writer loaded a session whole, changed its copy and
saved it whole, and no lock was shared across processes. In this process,
PATCH already refused while a run was active, so the API's in-process pairs
were serialized by the busy set. The lost updates came from other processes:
- the terminal chat and ACP saved a snapshot they had held for the whole
  conversation;
- a second server on the same profile was not covered by the busy set.

`lightagent sessions delete` was an unlocked writer too.

**Fix.**
- **One store lock**, `sessions/.lock`: an exclusive OS lock that every process
  takes. It uses D4c's primitive, now shared as `paths::lock_exclusive`.
- **`SessionStore::update(id, change)`:** under the lock, re-read the session,
  apply the change, and write atomically.
  - A refused change, a failed write, or an unreadable file writes nothing.
  - A missing session stays missing.
- **`SessionStore::apply(session, change)`:** for clients that hold a session
  open. The change applies to the saved state and refreshes the client's view.
- **`save`** (new sessions) and **`delete`** take the same lock.
- **Converted writers:**
  - run admission;
  - run completion;
  - regenerate's swap;
  - PATCH;
  - the API and CLI deletes;
  - the chat's prompt, turn and approval writes;
  - ACP's prompt and completion.
- **Locking is short.** It covers only the read-modify-write, never inference.
  The API does that work on the blocking pool.
- **Unchanged:** run admission (the busy set), cancellation, permission,
  ownership, and profile isolation.

**Tests (hosted, all four platforms):**
- **Store:** a writer in another process (the test binary run again) waits for
  the lock and keeps the holder's change. Also covered:
  - a stale writer applies to the current session;
  - a refused change, a failed atomic write, or an unreadable file writes
    nothing;
  - `Busy` after the wait;
  - no re-creation after a delete;
  - `apply`;
  - history off.
- **API:** while the test holds the lock and another process waits to rename
  the session:
  - a normal run finishes: rename and answer are both kept;
  - a regeneration finishes: rename and new answer are both kept;
  - a provider failure, then a cancellation: the rename is kept and the
    saved answer is untouched.

  Also:
  - a record whose write fails leaves the file byte-for-byte unchanged;
  - two concurrent prompts admit one run and save one prompt;
  - PATCH during the run is still 409.
- **`serve --desktop` process:** its PATCH and the CLI's delete wait for
  another process's lock. The PATCH keeps the turn written inside the lock,
  and a write after the delete does not re-create the session.
- **ACP:** a rename made while an editor holds the session survives its next
  turn.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Branch `desktop/d4c2-session-writes` created at `fbd008c`; ownership and audit recorded | — | https://github.com/dlroqa/Lightagent/issues/100 |
| 2026-11-02 | Edit request `038`: 24 edits to 11 files; branch moved without force; draft PR #103 | `6c0b7ff` / `e5ff849` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41367313205 |
| 2026-11-02 | `check` on `6c0b7ff`: **failure**. E0308 in `chat.rs`: the new `prompt` binding shadowed the chat loop's `prompt: TerminalPrompt` | `6c0b7ff` | https://github.com/dlroqa/Lightagent/actions/runs/41367409693 |
| 2026-11-02 | `039`: the binding renamed to `asked` | `903528f` / `a3aafb3` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41367879919 |
| 2026-11-02 | `check` on `903528f`: **failure**, only `check (linux-x64)`: `session_history_is_packed_against_the_resolved_window` got 409 on a follow-up prompt. The test posted the follow-up as soon as the record was visible, before the session was released. That gap always existed, but the single-threaded test runtime hid it. Every other job passed, including all new tests on the other three platforms. `desktop` was cancelled by the next push | `903528f` | https://github.com/dlroqa/Lightagent/actions/runs/41367939822, https://github.com/dlroqa/Lightagent/actions/runs/41367939797 |
| 2026-11-02 | `040`: that test and `a_saved_session_is_reused_by_follow_up_runs_and_new_sessions_are_empty` now wait for the release, as #98's `settled` helper already does. No assertion changed | `aebd5d6` / `e9066c3` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41369131207 |
| 2026-11-02 | `check` on `aebd5d6`: **success**, all 13 jobs. The new store, API, `serve --desktop` and ACP tests ran and passed on linux-x64, macos-arm64, macos-x64 and windows-x64 | `aebd5d6` | https://github.com/dlroqa/Lightagent/actions/runs/41369189397 |
| 2026-11-02 | `desktop` on `aebd5d6`: **success**, all 7 jobs | `aebd5d6` | https://github.com/dlroqa/Lightagent/actions/runs/41369189399 |
| 2026-11-02 | `jev evaluation` and `subagents`: **success**. The dispatched `subagent fixture fail` and `verify` runs fail by design (D0 baseline), as on every earlier head | `aebd5d6` | https://github.com/dlroqa/Lightagent/actions/runs/41369189555, https://github.com/dlroqa/Lightagent/actions/runs/41369189398 |

Capability coverage is unchanged: **22 of 227**. Row 10 adds no capability.

**Known limits (not addressed in row 10):**
- **Run admission is per process.** Two processes can each run a turn on one
  session at once. Both turns are kept, but they interleave. A cross-process
  run lease is a separate follow-up.
- **A record can be skipped.** If another writer holds the store lock longer
  than 5 s, the recorder logs `Busy` and does not write. Nothing is
  overwritten.

## Row 11 — D4 acceptance

Edit request `042`, based on row 10's documentation head `3310dfc`, was
applied as 32 edits to 15 files.

| D4 gate item | Now | Evidence (all four platforms unless noted) |
|---|---|---|
| CLI and desktop write identical files (profile create/use/delete, config set) in isolated homes | **met** | `the_cli_and_the_desktop_write_the_same_files_in_isolated_homes`. Every file in the two homes is compared. The one normalization is the home's own path, which differs by construction. No file was excluded |
| A Settings save on a revision loaded before a CLI write: 409, nothing written | **met** (D4c) | `a_stale_write_is_refused_with_409_and_writes_nothing` |
| A Settings save, then a CLI write: both kept | **met** | `a_settings_save_then_a_cli_write_keeps_both` |
| Unordered races: no lost update (no exact `409` count required) | **met** | D4c's cross-process lock test and core `config::tests` |
| An unreadable secret file fails closed, with a redacted error | **met for the desktop runtime** | `an_unreadable_key_file_fails_the_run_closed_before_any_request`: the run fails, the error names no path, and the configured endpoint gets no connection. `SecretRef::for_request` is also used by the runtime's history probe and model listing |
| No secret in the renderer state dump | **met in the browser harness** | `e2e/settings.mjs` checks the DOM, local and session storage, form fields, and the settings response. The native WebView repeats this in D8 |
| No secret in the diagnostics bundle | **carried to D7** | The bundle does not exist until D7 (`/diagnostics`) adds it, with this test |
| `jev-routing-report`: shared operation, API route, UI | **met** | `lightagent_core::config_keys::jev_routing_report`; `GET /jev/routing-report` (contract `jev.routing_report`, inventory `api.get.jev.routing-report`); Settings dry-run table. `the_api_routing_report_is_the_cli_routing_report`; e2e |
| A Settings conflict keeps the unsaved input and offers review and refresh | **met** | e2e: conflict then apply (the other writer's change is kept), and conflict then discard (nothing saved, saved value shown) |

**Carried forward:**
- **D4d:** the terminal chat still resolves its key with `resolve`, so an
  unreadable key file there sends no key rather than failing. It moves to
  protected credentials, with its own test.
- **D7:** the diagnostics bundle's redaction test.
- **D8:** native WebView verification of these screens.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Branch `desktop/d4c3-config-acceptance` created at `3310dfc`; edit request `042`; branch moved without force; draft PR #104 | `205f01b` / `b5bf552` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41370688162 |
| 2026-11-02 | `check` on `205f01b`: **success**, all 13 jobs. The four new `desktop_config.rs` tests ran on all four platforms. `standalone Web UI render` printed `[ok] settings conflict, routing dry run and renderer state` | `205f01b` | https://github.com/dlroqa/Lightagent/actions/runs/41370767535 |
| 2026-11-02 | `desktop` on `205f01b`: **success**, all 7 jobs, including the inventory and contract checks with `jev.routing_report` | `205f01b` | https://github.com/dlroqa/Lightagent/actions/runs/41370767531 |
| 2026-11-02 | `jev evaluation` and `subagents`: **success**. The fixture `fail` and `verify` dispatches failed by design, as on every earlier head | `205f01b` | https://github.com/dlroqa/Lightagent/actions/runs/41370767708, https://github.com/dlroqa/Lightagent/actions/runs/41370767658 |
| 2026-11-02 | Parity: `cli.config.jev-routing-report` is implemented: **23 of 227**. The new `api.get.jev.routing-report` row is backend and not counted (319 rows) | — | DESKTOP_CLI_PARITY.md |

## Row 12 — D4d-1: provider credentials

The owner's instructions of 2026-11-02 (second continuation) set D4d's
scope. This slice does the credential part.

- **One operation, one store.** `lightagent_core::provider_keys` stores,
  replaces and removes the active endpoint's key. It writes the owner-only
  `secrets/provider-<id>.key` atomically and points the config at it.
  `lightagent setup provider` and the HTTP API both use it, so the terminal
  and the desktop write the same file and the same reference. Nothing
  returns a stored key.
- **Absent is not unreadable.**
  - No `inference.api_key` is *intentionally absent*: no key is sent, as
    local providers expect.
  - A configured reference that cannot be used is refused by
    `SecretRef::for_request`: an unreadable file, an unset variable, an
    empty value, or a value with a line break or control character.
  - `SecretUnavailable` says which, never the path, the variable or the
    value. Status displays use `secret_state`.
- **Fail closed** on the inference key wherever a request is made with it:
  - the terminal chat refuses to start, and skips a routed request;
  - `lightagent models`;
  - setup's model listing;
  - the desktop runtime (run, history probe, model listing).

  This closes row 11's carried D4d item.
- **API:** `GET|PUT|DELETE /provider/credential` (admin scope).
  - GET returns the status (`absent`, `ready`, `unreadable`, `unresolved`,
    `empty` or `invalid`) and the source (`file` or `env`).
  - PUT is a write-only save. It is revision-checked: a stale save gets 409,
    and the key is written inside the config lock after the check, so a
    refused save changes nothing.
  - DELETE removes the key.

  Contract operations are `provider.credential.get`, `.set` and `.remove`,
  with backend inventory rows (322 rows).
- **Settings:** a "Provider API key" card with the status, a write-only
  password field, Save or Replace, and Remove after a confirmation.

**Evidence types.** The Settings flows are **browser** evidence: the Web UI
against the real harness. Native desktop WebView verification is D8 and is
not claimed.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Cross-process run admission filed as its own defect, with the topology audit and plan (D5b) | — | https://github.com/dlroqa/Lightagent/issues/105 |
| 2026-11-02 | Branch `desktop/d4d1-credentials` created at `597604d`; edit request `044` (32 edits, 15 files); draft PR #106 | `a1bd655` / `9c74ea4` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41376157867 |
| 2026-11-02 | `check` on `a1bd655`: **failure**, only `standalone Web UI render`. "Save key" matched two buttons (the Jev row has one too). The other 12 jobs passed, including every new Rust test on four platforms | `a1bd655` | https://github.com/dlroqa/Lightagent/actions/runs/41376245339 |
| 2026-11-02 | `045`: the card's buttons and messages say "API key", which also gives a screen reader distinct names | `599b261` / `682ef7b` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41377094979 |
| 2026-11-02 | `check` on `599b261`: **failure**, render only. The card's first save was rightly refused as stale (the conflict checks above it had written the config), so the success message never came. `desktop`: **failure**, inventory only: a core test's literal `"LIGHTAGENT_TEST"` read as an undeclared environment variable | `599b261` | https://github.com/dlroqa/Lightagent/actions/runs/41377130735, https://github.com/dlroqa/Lightagent/actions/runs/41377130716 |
| 2026-11-02 | `046`: the e2e now asserts the stale save (refusal shown, entry kept, nothing stored), then saves on the current revision. The test literal was changed | `6988f6c` / `bcdfc0b` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41378439550 |
| 2026-11-02 | `check` on `6988f6c`: **success**, all 13 jobs. `desktop_credentials.rs`, `provider_keys::tests` and `config::secret_tests` passed on all four platforms, and the render printed `[ok] settings conflict, routing dry run and renderer state` | `6988f6c` | https://github.com/dlroqa/Lightagent/actions/runs/41378487660 |
| 2026-11-02 | `desktop` on `6988f6c`: **success**, all 7 jobs | `6988f6c` | https://github.com/dlroqa/Lightagent/actions/runs/41378487692 |
| 2026-11-02 | `jev evaluation` and `subagents`: **success**. The fixture `fail` and `verify` dispatches failed by design | `6988f6c` | https://github.com/dlroqa/Lightagent/actions/runs/41378487683, https://github.com/dlroqa/Lightagent/actions/runs/41378487629 |
| 2026-11-02 | Parity: `setup.provider.key` is implemented: **24 of 227** | — | DESKTOP_CLI_PARITY.md |

## Row 13 — D4d-2a: the endpoint check

- **`LightweightProvider::for_check`:** a client used only for explicit
  checks. It never follows a redirect, so the key reaches the configured
  endpoint only, and a 3xx is reported as that endpoint's answer. Every
  request is bounded. Runs keep their client.
- **`providers::check_active`:** the one shared check. It lists the active
  endpoint's models with that endpoint's own configured key, bounded as a
  whole. An unusable key fails closed with nothing sent. The result has
  `ok`, `models`, `error`, `key_sent` and `elapsed_ms`, and never a key.
- **Where it is used:**
  - `lightagent models` is this check. It now reports a redirect instead of
    following it, and gives up after 10 s.
  - `POST /provider/check` (admin scope; `timeout_secs` 1–30, default 10)
    runs it through the run factory. It is contract operation
    `provider.check`, with a backend inventory row (323 rows).
  - **Settings:** "Check connection" runs only when clicked. That is
    browser evidence; native WebView verification is D8.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `048`: **failure** before any commit. A find block was part of a line (the whole-line rule) | — | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41380624787 |
| 2026-11-02 | `048b` (that block given its whole line, nothing else changed): 26 edits; branch `desktop/d4d2-endpoint-check` moved without force; draft PR #107 | `e6f4506` / `e33be00` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41380687270 |
| 2026-11-02 | `check` on `e6f4506`: **success**, every job. `an_endpoint_check_is_explicit_bounded_and_sends_the_key_to_that_endpoint_only` passed on all four platforms: key sent to the endpoint and in no response; API models = CLI; 401; 302 not followed and its target untouched; a silent endpoint cut off at 2 s; an unusable key fails closed. Render `[ok]` | `e6f4506` | https://github.com/dlroqa/Lightagent/actions/runs/41380804834 |
| 2026-11-02 | `desktop` on `e6f4506`: **success**, every job | `e6f4506` | https://github.com/dlroqa/Lightagent/actions/runs/41380804835 |
| 2026-11-02 | `jev evaluation` and `subagents`: **success**. The fixture `fail` and `verify` dispatches failed by design | `e6f4506` | https://github.com/dlroqa/Lightagent/actions/runs/41380804791, https://github.com/dlroqa/Lightagent/actions/runs/41380804768 |
| 2026-11-02 | Parity: `cli.models` is implemented: **25 of 227** | — | DESKTOP_CLI_PARITY.md |

**Next in D4d** (the endpoint check above is done):
- **D4d-2:** provider and model selection, and saved providers, through
  shared operations (`setup.provider.url`, `.saved`, `.model`,
  `cli.models`).
- **The endpoint check:** an explicit user action with a bounded timeout,
  sending the intended provider's key and never following a redirect with
  it.
- **`lightagent runtime`:** its control-plane commands still resolve the
  inference key with `resolve()`; that moves to the same fail-closed rule.

## Row 14 — D4d-2b-1: a provider key goes only to the endpoint it was saved for

Session fcfe3d5d took ownership on 2026-11-02T05:36Z (claim on #107) after
verifying no PR, branch or run newer than #107 and no write to the stack
since edit request 049.

**The defect.** Every inference path attached the global
`inference.api_key` to whatever endpoint the run used. So a profile with
its own `routing.base_url` received the active endpoint's key (runs, the
history probe, the provider view, the terminal chat and its subagents),
and `config set inference.base_url` (CLI and `PUT /config/{key}`), `init
--base-url` and line-mode setup moved the endpoint but kept the previous
endpoint's key.

- `provider_keys::bound_key(config, base_url)`: the only key a request to
  `base_url` may carry: the active endpoint's when it is the active
  endpoint, a saved provider's when it is that provider's, otherwise none.
  Every inference request path uses it; a bound key that cannot be used
  still fails closed.
- `provider_keys::select_endpoint(config, base_url)`: the same endpoint
  keeps its key, a saved provider's endpoint takes its key, any other
  endpoint has none.
- **Behaviour change, on purpose:** a profile endpoint that is neither the
  active one nor a saved provider's sends no key; an endpoint changed with
  `config set` needs its key saved again. `desktop_providers.rs` saved the
  key once and then moved the endpoint; it now asserts the key was not
  carried and saves it for each endpoint.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `050` (15 edits, 9 files); branch `desktop/d4d2b-key-binding` created at it; draft PR #108 | `8e3919b` / `ec8ed72` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41395689120 |
| 2026-11-02 | `check`: **success**, every job on four platforms, including `desktop_key_binding.rs` (profiles on the active, a saved and an unsaved endpoint: provider view, desktop run and CLI chat send that endpoint's key or none; trailing slash keeps the key; a new endpoint by CLI or desktop config route gets none; a saved provider's endpoint gets its own) | `8e3919b` | https://github.com/dlroqa/Lightagent/actions/runs/41395777653 |
| 2026-11-02 | `desktop`, `jev evaluation`, `subagents`: **success** (fixture `fail`/`verify` dispatches fail by design) | `8e3919b` | https://github.com/dlroqa/Lightagent/actions/runs/41395777655, https://github.com/dlroqa/Lightagent/actions/runs/41395777599, https://github.com/dlroqa/Lightagent/actions/runs/41395777608 |

## Row 15 — D4d-2b-2: saved providers and the model through one shared operation set

`lightagent_core::saved_providers` is shared by `lightagent setup`
(provider section) and the HTTP API, inside one revision-checked config
update:

- **list / get:** the local endpoint, each saved provider, and an unsaved
  active endpoint (`custom`), with each key's state and source only.
- **create:** refuses a name whose key id another saved provider has (they
  would share a key file), an endpoint another has (its key would be
  ambiguous), the local endpoint, reserved names and non-http(s) URLs.
- **update:** a rename keeps the key (a stored key moves to the new file);
  a new endpoint **releases** the key unless `keep_key`; an active
  provider's change moves the active endpoint with it. Profiles naming the
  old endpoint keep it and send it no key.
- **remove:** the stored key is released after the save; a removed active
  provider leaves the local endpoint active, keyless, automatic model.
- **select / select_model:** the endpoint takes only its own key; the
  active profile follows (endpoint override cleared on select; model).
  Every answer says the change applies to the next run: a run in progress
  keeps its provider and model; an open terminal chat keeps its endpoint
  until restarted.
- Key files a change releases are deleted only after the config is saved.
- API (admin): `GET|POST /providers`, `GET|PATCH|DELETE /providers/{id}`,
  `POST /providers/{id}/check`, `PUT /provider/active`, `PUT
  /provider/model`; stale revision 409, duplicate 409 `reason:
  duplicate`. Contract operations and backend inventory rows (331 rows).
- Setup: an existing name or endpoint is refused instead of silently
  replacing a provider; removal deletes the stored key; the active profile
  is updated after the config save, not before.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `051` (26 edits, 12 files); branch `desktop/d4d2b2-saved-providers`; draft PR #109 | `108dac6` / `868f78d` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41396956160 |
| 2026-11-02 | `check`: **success**, every job, including `saved_providers::tests` and `desktop_saved_providers.rs` (create, stale/duplicate/invalid refusals with config.json byte-identical, select with the profile following, the key only at its provider's endpoint, rename, move, keep_key, ordered concurrency with a CLI write, removal, 404/400, no key in any response) | `108dac6` | https://github.com/dlroqa/Lightagent/actions/runs/41397047625 |
| 2026-11-02 | `desktop`, `jev evaluation`, `subagents`: **success** | `108dac6` | https://github.com/dlroqa/Lightagent/actions/runs/41397047583, https://github.com/dlroqa/Lightagent/actions/runs/41397047530, https://github.com/dlroqa/Lightagent/actions/runs/41397047596 |

## Row 16 — D4d-2b-3: the Providers and models card

Settings → "Providers and models": each provider's endpoint, whether it
is active and its key's state; Add, Edit (rename; an endpoint change says
the key is removed, with an explicit "keep this provider's API key"), Use,
Remove after a confirmation; the model (empty: automatic) with an explicit
"List models" check of the active provider. Every change is made on the
loaded revision; a refusal keeps the entries, reloads the list and asks to
review and try again. After a provider change the API key card remounts,
so a key typed for one provider is never saved to another. **Browser
evidence**; native WebView verification is D8.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `052` (7 edits, 4 files); branch `desktop/d4d2b3-providers-ui`; draft PR #110 | `bb76e74` / `d3bc96f` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41397480896 |
| 2026-11-02 | `check`: **success**, every job; the render e2e covered a stale save refused with the entry kept, the save, rename, Use with the key card following, the model, a stale removal refused then made, and the harness endpoint restored | `bb76e74` | https://github.com/dlroqa/Lightagent/actions/runs/41397533047 |
| 2026-11-02 | `desktop`, `jev evaluation`: **success** | `bb76e74` | https://github.com/dlroqa/Lightagent/actions/runs/41397532957, https://github.com/dlroqa/Lightagent/actions/runs/41397532983 |
| 2026-11-02 | `subagents`: **failure**, one job, `Tester broker (live GitHub Actions)`: `live_the_broker_reports_a_passing_and_a_failing_hosted_run` got `dispatched_unlocated` (the dispatch was accepted but GitHub did not list the run within the broker's window, while about twelve workflows were starting across rows 15-17). Row 16 changes only frontend and e2e files, which this Rust test does not use; the same test passed on rows 14, 15 and 17. **Not re-run.** | `bb76e74` | https://github.com/dlroqa/Lightagent/actions/runs/41397533045 |
| 2026-11-02 | Parity: `setup.provider.saved`, `setup.provider.url`, `setup.provider.model` implemented: **28 of 227** | — | DESKTOP_CLI_PARITY.md |

## Row 17 — D4d-3: every configured remote credential fails closed

A configured credential that cannot be used was dropped by `resolve()`,
and the request went out without it. Every remaining path now uses
`SecretRef::for_request` and sends nothing: web search is withheld (never
anonymous in its place); Open Terminal and the GitHub Actions broker build
no connection; the remote index (Qdrant and Infinity) and the semantic
embedder are not used; `JevClient::from_config` refuses with
`JevError::CredentialUnavailable` (`credential_unavailable`), so
classification, routing and the check fall back. Intentionally absent
credentials are unchanged.

**Audit finding.** `crates/lightagent/src/runtime.rs` is not compiled:
there is no `mod runtime`, no `lightagent runtime` command, and the file is
unchanged since `67acdb5`. The previous report's "runtime control-plane
commands" refers to it. It is left unchanged rather than editing code CI
cannot build. The live control-plane path, the provider view's runtime
catalog, takes the endpoint's bound key and fails closed (row 14).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `053` (15 edits, 7 files); branch `desktop/d4d3-fail-closed`; draft PR #111 | `ecfd61b` / `f39601b` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41397962017 |
| 2026-11-02 | `check`: **failure** on all four platforms, one test: `desktop_fail_closed.rs` edited `config.json` before anything had written it (a fresh home has none). Every other test passed, including the new chat and rag unit tests. `desktop`, `jev evaluation`, `subagents`: **success** | `ecfd61b` | https://github.com/dlroqa/Lightagent/actions/runs/41398039699 |
| 2026-11-02 | `054`: the test creates the config with `lightagent config set` first; the branch moved without force | `c15e249` / `aea17cf` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41400610165 |
| 2026-11-02 | `check`: **success**, every job (the desktop Jev check refuses with `credential_unavailable` for each unusable kind, names no location, and the endpoint receives no connection) | `c15e249` | https://github.com/dlroqa/Lightagent/actions/runs/41400681156 |
| 2026-11-02 | `desktop`, `jev evaluation`, `subagents`: **success** (the subagents tree contains row 16's change) | `c15e249` | https://github.com/dlroqa/Lightagent/actions/runs/41400681200, https://github.com/dlroqa/Lightagent/actions/runs/41400681157, https://github.com/dlroqa/Lightagent/actions/runs/41400681150 |

D4d is complete with rows 12-17: shared write-only keys, the bounded
check, endpoint-bound keys, saved providers and the model, the Settings
card, and fail-closed credentials on every live path.

The documentation commit `bf5e56d` (this section's first version):
`check`, `jev evaluation` and `subagents` passed; `desktop` **failed** on
one job, `desktop shell (macos-x64)`, because macOS `hdiutil` could not
eject the disk image (`couldn't eject "disk2" - Resource busy`), a variant
the workflow's bounded retry does not match (it retries only `create
failed - Resource busy`). The commit changes documentation only; the same
tree plus row 18's code passed `desktop` on every target at `ec1fc4e`.
Not re-run. Finding: the retry could also match the eject variant (a
workflow change, not made here). Runs:
https://github.com/dlroqa/Lightagent/actions/runs/41401954966 (check),
https://github.com/dlroqa/Lightagent/actions/runs/41401955176 (desktop).

## Row 18 — D4e: the tools workspace through one shared, validated operation

The workspace was a free string: setup stored whatever was typed, a
relative path meant a different folder in each process (the terminal's
working directory, the desktop runtime's), and a run created whatever
directory it named. `lightagent_core::workspaces` is now the one way to
read and set it, shared by `lightagent setup tools` and the API:

- validate / set: an absolute path without control characters that is an
  existing folder, or a new folder whose parent exists (the next run
  creates it); blank clears it, so runs use the active profile's own
  `workspace/`.
- view: the configured and effective folder for the next run of the
  active profile (it follows a profile switch when none is set), whether
  it exists, and whether file and terminal tools are on.
- Applies to the next run; a run in progress keeps its folder; an ACP
  session keeps the editor's folder.
- Setup: the full-screen prompt validates as it is typed; line mode
  refuses an invalid new folder and leaves an unchanged existing value as
  it is, so an older relative value keeps working until it is changed.
- API (admin): `GET|PUT /workspace`, revision-checked; contract
  operations `workspace.get` / `workspace.set`; backend inventory rows (333
  rows). Settings: a Workspace card (browser evidence; native WebView is
  D8). A native folder picker is a host feature, not built here.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `056` (24 edits, 12 files); branch `desktop/d4e-workspaces`; draft PR #112 | `ec1fc4e` / `34e2aa7` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41402272857 |
| 2026-11-02 | `check`: **success**, every job, including `workspaces::tests`, the setup line-mode tests and `desktop_workspace.rs` (effective folder follows the active profile; relative, file and missing-parent refusals with config.json unchanged; a stale revision refused after a CLI write; set and clear), and the render e2e (stale save refused with the entry kept, relative refused, saved, cleared) | `ec1fc4e` | https://github.com/dlroqa/Lightagent/actions/runs/41402353290 |
| 2026-11-02 | `desktop`, `jev evaluation`, `subagents`: **success** | `ec1fc4e` | https://github.com/dlroqa/Lightagent/actions/runs/41402353271, https://github.com/dlroqa/Lightagent/actions/runs/41402353347, https://github.com/dlroqa/Lightagent/actions/runs/41402353376 |
| 2026-11-02 | Parity: `setup.tools.workspace` implemented: **29 of 227** | — | DESKTOP_CLI_PARITY.md |

## Row 19 — D5b: one turn at a time per session across every process (#105)

Session writes were already safe across processes (#103); turn admission
was per process. Now:

- `SessionStore::lease_run(id)`: an exclusive OS lock on
  `sessions/.runs/<id>.lock`. Another holder in any process refuses the
  turn with `StoreError::Busy`. Dropping the lease, or the holder's process
  ending (a crash included), releases it; the file holds nothing. It is
  separate from the short `sessions/.lock` write lock, which is never held
  during a turn. Taking it retries for up to 500 ms, because on Linux a
  released lease can read as held for an instant while a child another
  thread is starting still has a copy of the file open; a held lease is
  never waited for beyond that, and nothing queues behind a turn.
- API (runs and regenerate): taken right after the in-process busy check,
  held until the run is recorded, and released **before** the busy entry
  on every path; a lease held elsewhere gets the same 409 body.
- Terminal chat: held while a turn runs and while a time-budget-paused run
  may be continued; none between turns. A refused turn says so, sends
  nothing and keeps the chat open.
- ACP: taken with the prompt, released when it completes, is cancelled or
  fails; a refusal is a JSON-RPC error naming the cause.
- Different sessions still run at the same time.

**D5 gate.** This meets the gate's first sentence (one turn at a time per
session across the desktop runtime, `serve`, CLI chat and ACP; a crashed
holder releases it; the write lock is not held during inference). The rest
of the D5 gate (the deterministic fake-provider run, relay reconnect,
profile switch with a pending approval, sidecar crash mid-run) and R1 are
D5a and are not claimed.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit requests `057` and `058` (the ACP guard field named `_lease`, which is held, never read); branch `desktop/d5b-run-admission`; draft PR #113 | `88aafcb` → `cee3379` / `46fa507` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41402811557, https://github.com/dlroqa/Lightagent/actions/runs/41402894827 |
| 2026-11-02 | `check`: **failure** (linux-x64, macos-x64): the existing `a_failed_or_cancelled_regeneration_keeps_the_saved_answer` got 409. The recorder released the lease after the busy entry, so a caller that saw the entry go could still find the lease held. `desktop`, `jev evaluation`, `subagents`: **success** | `cee3379` | https://github.com/dlroqa/Lightagent/actions/runs/41402975656 |
| 2026-11-02 | `059`: the lease is released before the busy entry on every path; the branch moved without force | `f2f2b88` / `a3eedd7` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41405029314 |
| 2026-11-02 | `check`: **failure** on linux-x64 only, the same test after a cancelled run; the new separate-process tests passed on all four platforms. Cause: on Linux a released lease reads as held while a child that another test thread is starting keeps a copy of the file until its exec. `subagents`: **failure**, the live broker's `dispatched_unlocated` again (recorded, not re-run). `desktop`, `jev evaluation`: **success** | `f2f2b88` | https://github.com/dlroqa/Lightagent/actions/runs/41405132318, https://github.com/dlroqa/Lightagent/actions/runs/41405132450 |
| 2026-11-02 | `060`: a 500 ms grace when taking the lease; the branch moved without force | `1e657e5` / `686bf3a` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41406894786 |
| 2026-11-02 | `check`: **success**, every job: the store's lease test, `desktop_run_admission.rs` (a lease held by the test process refuses an API run, a regenerate, a CLI chat turn with the session unchanged and an ACP prompt, and admits after release; a real CLI chat caught mid-turn holds the lease, the runtime refuses that session, renames it while the turn runs and runs another session; killing the chat releases the lease and the next run is admitted) on all four platforms, and the existing regenerate tests | `1e657e5` | https://github.com/dlroqa/Lightagent/actions/runs/41406983621 |
| 2026-11-02 | `desktop`, `jev evaluation`, `subagents`: **success** | `1e657e5` | https://github.com/dlroqa/Lightagent/actions/runs/41406983578, https://github.com/dlroqa/Lightagent/actions/runs/41406983582, https://github.com/dlroqa/Lightagent/actions/runs/41406983640 |

**Recurring infrastructure failure.** The `subagents` job `Tester broker
(live GitHub Actions)` failed twice (rows 16 and 19) with
`dispatched_unlocated`: a dispatch GitHub accepted but did not list within
the broker's window while many workflows were starting. It passed on every
other head. It is a live GitHub test and is not re-run; a later change
could widen that window.

## Row 20 — D5b follow-through: a dropped run lease is free at once

Review of #113's 500 ms grace. The cause was found and fixed rather than
left to the grace:

- An OS file lock (`flock`, and `LockFileEx` on Windows) belongs to the
  open file, not to one handle. On Unix, a child that another thread is
  starting (fork, then exec) shares every open file of its parent until
  its exec closes the copies, so a lease closed in that instant stayed held
  until the child's exec.
- `RunLease` now **unlocks before it closes**, which releases the lock for
  every copy at once. The grace stays, as the documented bound for refusing
  a held lease.

| Requirement | Evidence (`e5363fc`, all four `check` platforms unless noted) |
|---|---|
| An active holder prevents a second turn | store lease test; `desktop_run_admission.rs` (#113): API run, regenerate, CLI chat, ACP refused while another process holds it |
| A competing turn is refused within the bound | `run_lease.rs`: refused in under 2 s (500 ms grace plus slack) |
| Completion, cancellation and crashes release it | `desktop_run_admission.rs` (killed chat releases); runs.py `cancel` (row 21: the next run is admitted after a cancel); row 22 crash tests |
| Different sessions run concurrently | `run_lease.rs`; `desktop_run_admission.rs` |
| One lease for every entry point | `SessionStore::lease_run` is the only lease, taken by the API (runs, regenerate; the desktop runtime and any `serve`), CLI chat and ACP (#113) |
| Child processes do not keep it | `run_lease.rs`: a child started while it is held does not keep it (all platforms); Unix: a lease dropped while a child is between fork and exec is free at once, and on Linux a control lock closed without unlocking stays held by that child, proving the window is real |
| Separate from the short write lock | store lease test (a save goes through while a turn runs) |

Remaining limitation: the short write locks (`sessions/.lock`,
`config.json.lock`) still close without an explicit unlock. Their waiters
wait up to 5 s, so the same instant can only delay them, never refuse them.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `062`; branch `desktop/d5b2-lease-release`; draft PR #114 | `e5363fc` / `ce2f291` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41429457355 |
| 2026-11-02 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, first run | `e5363fc` | https://github.com/dlroqa/Lightagent/actions/runs/41429597466, https://github.com/dlroqa/Lightagent/actions/runs/41429597459, https://github.com/dlroqa/Lightagent/actions/runs/41429597338, https://github.com/dlroqa/Lightagent/actions/runs/41429597555 |

## Row 21 — D5a-1: the host relay resumes a broken stream; runs through the host

**Product change (desktop host).** The relay of a run's event stream now
resumes a broken stream itself:
- It reconnects from the last event it forwarded, sending `Last-Event-ID`.
  Nothing is skipped, and nothing at or before that id is forwarded twice.
- Five resumes in a row that forward nothing report the break.
- A runtime that no longer has the run ends the relay at once, with that
  reason.
- An ended stream counts as finished only when the runtime says the run is.

The UI's own `?after=` resume remains as a second layer. The IPC command
bodies moved into plain functions; their behavior is unchanged.

**Hosted scenarios** use the real host and the real `serve --desktop`
runtime, with a deterministic fake provider (`desktop/e2e/harness.py`):
scripted turns, named gates, and every request recorded. A test-only stdin
driver (`e2e_driver.rs`, `--features e2e-hooks`, never bundled) calls the
same functions as the UI's IPC commands. Synchronization is on what the host
relayed or the provider received.

| D5 gate / R1 item | Scenario | Result at `64d632f` |
|---|---|---|
| Streaming and a tool call | runs.py `stream` | passed: three or more deltas, `datetime.now` ran without approval, answer saved, two provider turns |
| Approval granted | runs.py `approve` | passed: listed as pending, nothing written before the decision, written exactly once |
| Approval denied | runs.py `deny` | passed: never written, the model was told it was denied |
| Cancel | runs.py `cancel` | passed: cancelled mid-stream, saved as `Cancelled`, next run admitted, the turn not resent |
| Relay reconnect, no missing or duplicate events | runs.py `relay` | passed: two drops mid-run; delivered ids exactly `0..10` (the runtime's count); host report `resumes=2 skipped=0 error=false` |
| Pending approval during a switch | switch.py `pending` | passed: while the switch held its gate, `runs.create`, `sessions.regenerate` and `approvals.respond` were refused; without stopping runs it was `blocked` and the approval stayed pending; stopping runs rejected it explicitly (default's saved session: `the approval for 'fs.write' was denied; the tool did not run`, `Cancelled`); in `work`, the approvals list was empty and the old run and its approval answered 404; the file was never written |
| R1 regenerate through the host | runs.py `regenerate` | passed: a finished answer replaced the saved one (one run record, the new one); a provider failure (500), a refusal, an empty answer and a cancel kept it; a stale answer id and a busy session were refused |
| Continue after a time-budget pause; steering | — | not covered: the API has no continue operation, and steering is held by the renderer (row 23) |

Evidence classes: this is **host-level** evidence (the host transport and
relay, plus the real runtime). It is not browser UI evidence, and not native
WebView interaction (D8). Permission and ownership enforcement for
regenerate is **backend/API** evidence (#98's tests: `SessionsWrite`
required, another user's session answered as missing). The desktop host
always calls as the operator.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `063` (prepared `52d4272`); the final commit attaches the `desktop.yml` blob `86e1720`; branch `desktop/d5a1-host-runs`; draft PR #115 | `64d632f` / `169f76f` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41431358008 |
| 2026-11-02 | `check`, `jev evaluation`, `subagents`: **success**. `desktop`: every job passed, including `desktop profile switch and runs` (both scenario sets reported no failures), except `desktop shell (macos-arm64)`: cancelled after 15 min, "The job was not acquired by Runner of type hosted even after multiple attempts" (GitHub's macOS arm64 capacity notice). That job never ran, so it was re-run once to obtain evidence, and passed | `64d632f` | https://github.com/dlroqa/Lightagent/actions/runs/41431492275, https://github.com/dlroqa/Lightagent/actions/runs/41431492257, https://github.com/dlroqa/Lightagent/actions/runs/41431492204, https://github.com/dlroqa/Lightagent/actions/runs/41431492291 |

## Row 22 — D5a-2: a turn its runtime did not live to record is Interrupted, never rerun

A session turn saves its prompt when admitted and is recorded when it ends.
A runtime that died in between left the prompt with no record.

- **Run journal.** `sessions/.runs/<id>.run` is written when an API turn
  starts and removed once the turn is recorded. One that outlives the
  process holding the session's lease marks an interrupted turn.
- **Recovery.** An appended turn gets a run record with stop reason
  `Interrupted` and no answer. An interrupted regeneration leaves the saved
  answer and its record exactly as they were. Nothing is run again.
- **When recovery runs:** as `lightagent serve` starts (the desktop runtime
  included), for sessions no live process holds; and under the lease,
  before a session's next turn is admitted.
- **Web UI:** the stop reason reads "interrupted, not run again".

| D5 gate item | Evidence at `c6fd6ac` |
|---|---|
| A sidecar crash mid-run is reported accurately | runs.py `crash` (host-level): with an approved write done and the model's next turn in flight, the runtime is killed; the host reports the lost stream and restarts its runtime once; the session records `Interrupted` with no answer; the restarted runtime answers 404 for the old run |
| Recovery never replays the task or its tool side effects | runs.py `crash`: two provider turns before the crash and none after; the file holds `once` only; the next run is admitted. `desktop_interrupted.rs` (separate processes, all four platforms): the next runtime records the turn as Interrupted, the provider gets no second request, and an interrupted regeneration keeps the saved answer |

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `064`; branch `desktop/d5a2-interrupted`; draft PR #116 | `c6fd6ac` / `c98ac0a` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41433988154 |
| 2026-11-02 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, first run | `c6fd6ac` | https://github.com/dlroqa/Lightagent/actions/runs/41434134050, https://github.com/dlroqa/Lightagent/actions/runs/41434134035, https://github.com/dlroqa/Lightagent/actions/runs/41434134009, https://github.com/dlroqa/Lightagent/actions/runs/41434134043 |

**Parity.** The D5a scenarios exercise the operations behind `slash.approve`,
`slash.reject` and `slash.stop`, and #98's regenerate, through the host.
These rows still count only once a UI path is verified: browser UI e2e at
least, and native WebView interaction in D8. No browser test covers the
approval card or Stop yet, so coverage stays **29 of 227**.

**What remains of the D5 gate (row 23):** continue after a time-budget
pause, which needs a new API operation, since API runs use the wrap-up time
policy and never pause; and steering. Steering in the Web UI and the terminal
is a client-held FIFO queue that starts the next turn when the session is
free. Whether it needs a backend operation (the inventory's current note) is
decided in row 23.

## Row 23 — D5a-3: time-budget pause and Continue over the API; steering

**Audit: time-budget behavior before this row.**

| Question | API runs (desktop runtime, `serve`) | Terminal chat |
|---|---|---|
| A run reaches its time budget with tool results the model has not read | wraps up: one final turn with tools withheld, then `WallClockExceeded`; the run finishes | pauses (`WallClockPolicy::Pause`) and asks |
| Resumable execution state kept | none | the loop's suspended state in memory: the whole conversation including the unread tool results, the turn and tool-call counters, the remaining batch; plus the session lease |
| What survives | the saved transcript (prompts and answers) and each tool's history excerpt; no pending approval (an approval wait is separate) | everything above until `/continue`, or until a new prompt drops it |
| `/continue` | — | resumes the **same execution**: Extend renews only the clock (turn and tool budgets still count); Answer takes the tool-less turn |

A new turn on the session (`runs.create`) is **not** a resume. It starts a
new execution from the saved transcript, which holds no unread tool results
and resets the run's counters. It is never labelled a continuation.

**Change.**
- `POST /runs` takes `on_time_budget: "pause"`. The default, `wrap_up`, is
  unchanged.
- A pausing run emits `run.paused` {reason, elapsed_secs} and shows status
  `awaiting_continue`. It keeps its session lease and its interruption
  journal while paused: a crash while paused records `Interrupted`.
- `POST /runs/{id}/continue` {mode: `extend` | `answer`} resumes the same
  execution. It returns 409 unless the run is paused; a revoked run takes
  no continuation; a cancel ends a paused run with `run.cancelled`.
- Desktop contract `runs.continue`, refused during a profile switch. A new
  non-counted inventory row (`api.post.runs.id.continue`).
- `RunFactory::run_controlled`, with a default that runs as before, so
  embeddings that cannot pause never do.

**Steering, defined (no backend operation needed).**

| Aspect | Behavior |
|---|---|
| Kind | Queued follow-up instructions. Nothing is applied inside a running turn (the core loop has no mid-turn injection; the terminal queues too) |
| Ordering | FIFO in the client; each is sent as the session's next turn once the session is free |
| Ownership and permissions | Each is an ordinary `runs.create` on the session: the same ownership, `RunsWrite`/`SessionsWrite` and profile checks |
| Cancellation | Stop cancels the running turn; the queue stays and can be cleared ("Clear queue") |
| Profile switch | A switch is blocked while a turn runs; queued steers wait in the client |
| Reconnect | The queue is in the page, unaffected by stream reconnects |
| Crash | A runtime crash interrupts the running turn (row 22); the queue in the page is kept and sent once the restarted runtime admits the session |
| Persistence | None: the queue disappears with its page, as the terminal queue does with its process |
| Duplicates and several clients | The server admits one turn per session across every process: two clients sending at once get exactly one admitted, and the other is refused (409) and can resend |

The inventory's earlier "backend operation required" note is withdrawn. The
CLI semantics (a process-local queue) are met by the client-held queue.

**Interruption recovery, reviewed.** A second runtime started while the
first is mid-turn does not mark that turn Interrupted, at startup or when
the session is asked for a turn: the live process holds the session's
lease, so recovery skips it. It is recorded only once that process is gone
(`desktop_interrupted.rs` `a_second_runtime_leaves_a_live_turn_alone`, all
four platforms). Recovery never replays a provider request or a tool.

**Short lock release, verified (the row 20 limitation).** Every
`paths::lock_exclusive` lock (config, session writes, run leases) is now an
`ExclusiveLock` that unlocks before it closes. `run_lease.rs` drops a write
lock while a child is between fork and exec and takes it again at once,
alongside the Linux control that shows the window is real. So a child that
is still starting can no longer delay any of these locks, let alone run one
into the 5 s busy timeout. A genuine holder can still make a writer wait up
to 5 s, as designed.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | Edit request `066`: its serve change held a placeholder line; the prepared commit `c5fc37e`'s run was never attached to a branch. `067` failed on a partial-line find (nothing was created). `067b` applied | `8717263` / `7400f1a` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41439662939 |
| 2026-11-02 | `check`: **failure**, clippy denies `expect` in library code (`ExclusiveLock::into_file`). `desktop`: **failure** in run scenarios: a wrap-up turn adds an unmarked instruction, so the fake provider filed it as unmarked (harness) | `8717263` | https://github.com/dlroqa/Lightagent/actions/runs/41439783665, https://github.com/dlroqa/Lightagent/actions/runs/41439783419 |
| 2026-11-02 | `068` (no `expect`; providers attribute wrap-up turns); the branch moved without force | `a398bfb` / `259309b` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41442426431 |
| 2026-11-02 | `check`: **failure**, E0063, the test-only `RunState::detached` lacked the new field. `desktop`: **success** | `a398bfb` | https://github.com/dlroqa/Lightagent/actions/runs/41442503903, https://github.com/dlroqa/Lightagent/actions/runs/41442503498 |
| 2026-11-02 | `070`; the branch moved without force | `0d37299` / `1a85dff` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41445292162 |
| 2026-11-02 | `check`, `desktop`, `jev evaluation`, `subagents`: **success** (desktop_continue.rs and the live-turn test on all four platforms; runs.py continue and steering) | `0d37299` | https://github.com/dlroqa/Lightagent/actions/runs/41445389177, https://github.com/dlroqa/Lightagent/actions/runs/41445389192, https://github.com/dlroqa/Lightagent/actions/runs/41445389256, https://github.com/dlroqa/Lightagent/actions/runs/41445389257 |

## Row 24 — D5a-4: the Continue card, Stop in the conversation view, browser coverage

- The Web UI creates runs with `on_time_budget: "pause"`: interactive, like
  the terminal.
- A paused run shows a card in the approval card's pattern: Continue
  (primary) and Answer now.
- **Defects found by the new browser test, both fixed:**
  - The only Stop button was in the top bar, which the chat shell hides, so
    a working run could not be stopped from the conversation. Stop is now
    in the composer while a run works.
  - The stream hook did not listen for `run.paused`. A browser
    `EventSource` delivers only the named events it listens for, so a
    paused run showed nothing.
- `e2e/runs.mjs` (browser UI evidence; scripted proxy in front of the live
  harness; what each control sends is recorded):
  - Allow once sends approve true;
  - Deny, from the keyboard, sends approve false;
  - Stop cancels the working run;
  - Continue sends mode extend;
  - two steers typed during a run are queued and sent in order after it,
    with nothing sent meanwhile;
  - every run created asks to pause.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `069`; branch `desktop/d5a4-run-controls-ui`; draft PR #118. Row 23's fix was brought in by a merge commit (parents `2809f08`, `0d37299`) created through the git data API, so neither branch was rewritten | `2809f08` → `3bd001c` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41442836704 |
| 2026-11-02 | `check`: **failure**, the render job timed out on Stop | `3bd001c` | https://github.com/dlroqa/Lightagent/actions/runs/41445394833 |
| 2026-11-02 | `071` (each control a separate step, with diagnostics) → `d2a3708`: Allow once and Deny passed; Stop was not found in the conversation (the top bar is hidden in the chat shell) | `d2a3708` | https://github.com/dlroqa/Lightagent/actions/runs/41448883343 |
| 2026-11-02 | `072` (Stop in the composer) → `76506c2`: Stop passed; no card for a paused run | `76506c2` | https://github.com/dlroqa/Lightagent/actions/runs/41449559908 |
| 2026-11-02 | `073` (listen for `run.paused`): `check` (all render checks passed), `desktop`, `jev evaluation`, `subagents`: **success** | `e164dde` / `37b104d` | https://github.com/dlroqa/Lightagent/actions/runs/41451624446, https://github.com/dlroqa/Lightagent/actions/runs/41451624678, https://github.com/dlroqa/Lightagent/actions/runs/41451624448, https://github.com/dlroqa/Lightagent/actions/runs/41451624819 |

**The D5 gate (DESKTOP_ACCEPTANCE.md), reviewed whole.**

| Gate item | Evidence | Class |
|---|---|---|
| One turn at a time per session across processes; a crashed holder releases it; the write lock is not held during inference | rows 19-20 (`desktop_run_admission.rs`, `run_lease.rs`) | backend, separate processes |
| Streaming, a tool call, approval granted and denied, cancel | row 21 runs.py; row 24 runs.mjs | host-level; browser |
| Continue after a time-budget pause | row 23 desktop_continue.rs and runs.py; row 24 runs.mjs | backend; host-level; browser |
| Steering | row 23 runs.py (server side); row 24 runs.mjs (the queue) | host-level; browser |
| Reconnect with no duplicated or missing event ids | row 21 runs.py relay | host-level |
| A profile switch with a pending approval records an explicit rejection | row 21 switch.py pending | host-level |
| A sidecar crash mid-run is visible and never re-executed | row 22 runs.py crash, desktop_interrupted.rs | host-level; backend |
| R1: regenerate through the host keeps the saved answer on failure or cancel | row 21 runs.py regenerate | host-level |

**Native WebView interaction is not claimed for any of these (D8).** With
that recorded, the D5 gate is met at backend, host and browser level.
Coverage: **34 of 227**.

## Row 25 — D5 close-out: a paused run is found from its session

**The pause/continue lifecycle, reviewed case by case** (owner's list, 2026-11-02):

| Case | Evidence | New in row 25 |
|---|---|---|
| Ownership and permissions | `continue` goes through `authorized_run` with `RunsWrite`, the same check as cancel and approve (row 23) | `active_run` is named only through `authorized_run` with `RunsRead` |
| Two simultaneous Continue requests resume once | the status mutex: the first takes `awaiting_continue` → `running`, the second gets 409 | `desktop_continue.rs`: two concurrent continues → exactly one 200 and one 409; the provider sees exactly two requests |
| Extension values validated, limits respected | the only input is `mode` (`extend`/`answer`); extend renews the configured `agent.wall_clock_secs`, never a client amount | an undefined mode is refused (4xx, not 409) and the run stays paused |
| Stop releases a paused run's lease | row 23 `a_paused_run_can_be_cancelled…`: the next turn is admitted | — |
| Profile switch and shutdown | `awaiting_continue` is not terminal, so it is an active run: the switch is refused (or stops runs when asked), and close-with-active-runs asks | — |
| A disconnected client cannot leave an undiscoverable paused run | **gap found and closed**: before, only the operator-only `GET /runs` (no session) knew it, and the session's next turn got a bare 409 | `GET /sessions/{id}` and the busy 409 carry `active_run` {id, status}; the Agent screen follows it; browser: a reload while paused brings the card back and Answer now sends `answer` |
| Another runtime must not mark a live turn Interrupted | row 23 `a_second_runtime_leaves_a_live_turn_alone` | — |
| Interrupted work and tool side effects never replayed | row 22 crash scenarios, `desktop_interrupted.rs` | — |

A paused run still holds its session until it is continued, stopped, or its
runtime exits (then it is recorded Interrupted, never rerun). No time limit
is added: the terminal has none, and the run is now always reachable.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `076` failed on a partial-line find (nothing created); `076b` applied; branch `desktop/d5c-paused-discovery`, draft PR #119 | `65a65dc` / `d08a0c5` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41456237007 |
| 2026-11-02 | `check`: the render job **failed** on the new reload step: a reloaded Agent screen opens on its welcome screen, which has no conversation composer (the paused run had been created and shown). All other jobs passed | `65a65dc` | https://github.com/dlroqa/Lightagent/actions/runs/41456445238 |
| 2026-11-02 | `078`: the step picks the conversation from the list; the branch moved without force | `ed0ae47` / `904dbf5` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41458025671 |
| 2026-11-02 | `check`, `desktop`, `jev evaluation`, `subagents`: **success** | `ed0ae47` | https://github.com/dlroqa/Lightagent/actions/runs/41458389710, https://github.com/dlroqa/Lightagent/actions/runs/41458389511 |

## D6 — Tools, MCP, extensions and skills: inventory map

Audited at `c89c989` against `crates/lightagent/src/{extensions,lib,chat}.rs`,
`crates/lightagent-extensions`, `crates/lightagent-core/src/config.rs`
(`McpConfig`, `McpServerEntry`, `ToolsConfig`, `ExtensionsConfig`),
`crates/lightagent-mcp` and the API routes. Counted rows only; group,
slash-alias and backend `api.*` rows are not counted.

| Inventory rows (counted) | Shared backend operation | API route | Contract op | UI | Evidence required | Slice |
|---|---|---|---|---|---|---|
| `cli.extensions.{list,show,enable,disable,install,uninstall}` and their args/options (13), `extension.manifest.*` (5), `extension.roots` | `lightagent_extensions::manage` (new, row 26; the CLI prints its results) | `GET\|POST /extensions`, `GET\|DELETE /extensions/{name}`, `PUT /extensions/{name}/enabled` | `extensions.*` | Tools → Extensions | backend CLI = API, host, browser | rows 26-27: **done** |
| `config.extensions` (master switch and disabled list) | `config_keys` `extensions.enabled`; `manage::set_enabled` | `/config/{key}`, `/extensions/{name}/enabled` | `config.set`, `extensions.set_enabled` | the card shows the master switch's state; no switch control yet | a switch control with browser evidence | row 28 |
| `mcp.server.stdio{,.command,.args,.env}`, `mcp.server.http{,.url,.headers,.auth,.untrusted_profiles,.credential_profiles}`, `config.mcp` | none today (config file only): new `lightagent_core::mcp_servers` (view with env/header values withheld, add, update keeping withheld values, remove, enabled, timeout), validated as `Config::validate` does; an explicit check (connect and list tools) through the factory, with the resolver runs use | `GET\|PUT /mcp`, `POST /mcp/servers`, `PUT\|DELETE /mcp/servers/{name}`, `POST /mcp/servers/{name}/check` | `mcp.*` | Tools → MCP servers (connection state, tools found, errors; a stdio check says it runs the command) | backend: config written equals a hand-written one, invalid entries refused, unreachable/failed server reported, no secret value returned; host; browser | rows 28-29 |
| `cli.tools.list`, `setup.tools.mode`, `config.tools` | `GET /tools` gains the CLI's permission column (auto/ask/block, same `PolicyEngine`); `/settings` file tools and terminal | existing | `tools.list`, `settings.*` | Tools → Enabled tools; Settings local tools | `tools list --json` = `GET /tools`; browser | row 30 |
| `slash.skills`, `skills.roots` | `GET /skills` gains scope and source (global, profile, extension) | existing | `skills.list` | Tools → Installed skills | backend + browser | row 30 |
| GitHub skill installation | **new scope**: no CLI operation installs a bare skill. Extension install from HTTPS Git exists (row 26 records the commit) | — | — | — | layout detection (`extension.json` root, `skills/<name>/SKILL.md`, a single `SKILL.md`), unsupported layouts reported, nothing executed, a failed update keeps the valid install | row 31, labelled new scope |

## Row 26 — D6-1: extension operations shared by the CLI, the API and the desktop

- `lightagent_extensions::manage`: list, show, set_enabled, install,
  uninstall over the same roots and configuration, with typed errors (404,
  400, 409, 502, 500 over the API). The CLI's text output is unchanged; its
  JSON gains `scope`, and `show` gains `mcp` and `source`.
- An install writes `.lightagent-source.json` (folder, or HTTPS repository,
  sub-folder and the commit from `git rev-parse HEAD`) and is undone if that
  record or the settings cannot be saved. git runs with
  `GIT_ALLOW_PROTOCOL=https`, no prompt; nothing fetched is executed.
- MCP is switched on by an install only when asked (`activate_mcp`): the
  CLI asks, as it always has; the API defaults to no.
- `show` gives each MCP server's command and args or URL; of its env and
  headers only the names, of its auth only whether there is one.
- The host refuses `extensions.install`, `extensions.uninstall` and
  `extensions.set_enabled` during a profile switch.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `077`; branch `desktop/d6a-extension-ops`, draft PR #120 | `9e25804` / `7c0ba3c` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41457764339 |
| 2026-11-02 | Row 25's `078` brought in by a server-side merge (`POST /merges`) | `63f1fba` / `81f1323` | — |
| 2026-11-02 | `check (linux-x64)`: **failure**, clippy `result_large_err` on two helpers returning `Result<_, Response>` | `63f1fba` | https://github.com/dlroqa/Lightagent/actions/runs/41458378742 |
| 2026-11-02 | `080`: the helpers return `Box<Response>`, as `authorized_run` does; the branch moved without force | `3e558b8` / `3be3399` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41459136182 |
| 2026-11-02 | `check`, `desktop`, `jev evaluation`, `subagents`: **success** (24 of 24) | `3e558b8` | https://github.com/dlroqa/Lightagent/actions/runs/41459281969, https://github.com/dlroqa/Lightagent/actions/runs/41459281958 |

## Row 27 — D6-2: the Extensions card on the Tools screen

- Scope badge, Active/Inactive in words, MCP-server count; Details (folder,
  source with commit, skills, what each MCP server runs, env and header
  names only, whether MCP is on); Install (absolute folder or https:// Git
  URL, every profile or this profile, "Turn MCP on" unchecked by default,
  refusals under the field); Disable/Enable; Uninstall asks first.
- Evidence: browser `e2e/extensions.mjs` on the real harness; host
  `desktop/e2e/runs.py` `extensions`; backend row 26. Native WebView: D8.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `079` → `b467489`; row 26's fix merged in server-side; branch `desktop/d6b-extensions-ui`, draft PR #121 | `c89c989` / `aa51c9c` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41459032922 |
| 2026-11-02 | `check` (render with the extensions check, all four platforms), `desktop` (host `extensions` scenario), `jev evaluation`, `subagents`: **success** first run | `c89c989` | https://github.com/dlroqa/Lightagent/actions/runs/41459314993, https://github.com/dlroqa/Lightagent/actions/runs/41459314996 |

Coverage: 19 rows implemented (the 13 `cli.extensions.*` rows, the 5
`extension.manifest.*` rows, `extension.roots`): **53 of 227**.

## Row 28 — D6-3: MCP server configuration through one shared operation set

- `lightagent_core::mcp_servers`: view (env and header values and auth
  secrets withheld; names and the auth reference's kind shown), add,
  update (`null` or auth `keep` keeps a withheld value; keeping something
  never saved is refused), remove, set (enabled, timeout 1-600 s). Every
  write is validated whether MCP is on or off, by `Config::validate`'s
  rules plus a tool-namespace-safe name, through `ConfigStore::update` on
  the caller's revision.
- API (operator): `GET|PUT /mcp`, `POST /mcp/servers`,
  `PUT|DELETE /mcp/servers/{name}`; contract `mcp.*`.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `082`; branch `desktop/d6c-mcp-ops`, draft PR #122 | `7378a33` / `ab88f64` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41465935346 |
| 2026-11-02 | `check`: 12 of 13 jobs passed; `check (macos-arm64)` job 125347244973 **cancelled by infrastructure**: GitHub's annotation "The job was not acquired by Runner of type hosted even after multiple attempts" (macOS arm64 capacity notices); no log exists. Only that job was rerun (attempt 2, job 125447936741): **success**. The attempt-1 record stays in the run history | `7378a33` | https://github.com/dlroqa/Lightagent/actions/runs/41466088883 |

## Row 29 — D6-4: an explicit MCP server check, and the MCP card

- `chat::check_mcp_server` connects one server as an operator run does
  (`plan_mcp_servers`, the configured timeout, the same HTTP client with no
  redirects, an extension's folder as working folder), lists its tools (at
  most 200), and disconnects; the stdio child is killed on drop
  (`kill_on_drop` and `Drop`), also when the outer limit (3× timeout)
  gives up. Errors are cut at 600 characters with every configured env,
  header and auth value replaced. It changes no setting.
- Only on request: `POST /mcp/servers/{name}/check`. Listing servers
  (`GET /mcp`) or opening any screen starts nothing (see row 30 for the
  catalog).
- MCP card: on/off (turning on is confirmed, and explained), timeout,
  servers with names only, Check (a stdio check first shows the exact
  command and asks to confirm), Remove (confirmed), Add (starts nothing).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `083`; branch `desktop/d6d-mcp-check`, draft PR #123 | `1063a88` / `ddd9857` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41497555661 |
| 2026-11-02 | `check`: **failure**, TS2322 (`checks[name]` is possibly undefined under strict index access) | `1063a88` | https://github.com/dlroqa/Lightagent/actions/runs/41497733333 |
| 2026-11-02 | `085` → `98d497b`: **success** (24 of 24) | `98d497b` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41498643954 |

## Row 30 — D6-5: effective tool permissions, an MCP-free catalog, skills with their source

- `tool_permission` (one rule): `lightagent tools list` and
  `RunFactory::tool_catalog` evaluate each tool under the active profile's
  approval policy (auto/ask/block). `GET /tools` adds `permission` and
  `mcp` (off/skipped/connected).
- **Defect found and fixed:** the Agent screen and Settings fetched
  `GET /tools` when opened, and building that catalog connects every MCP
  server when MCP is on, so opening either screen started every stdio MCP
  server. `?mcp=skip` builds the catalog with MCP off; the Web UI always
  uses it; the Tools screen lists MCP tools only after "Include MCP server
  tools", which says it starts their commands. Other API clients keep the
  connecting default, as `tools list` does.
- `GET /skills` lists what a run loads (global, active extensions', the
  profile's; later overrides) with each skill's source and version. The
  active extensions' skills were missing before.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `084`; branch `desktop/d6e-tools-skills`, draft PR #124 | `c06a651` / `ec93e11` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41498319035 |
| 2026-11-02 | `085` merged in (`d7a4491`); render **failure**: the existing tools-recovery check's glob no longer matched `/tools?mcp=skip` | `d7a4491` | https://github.com/dlroqa/Lightagent/actions/runs/41498996321 |
| 2026-11-02 | `087` (regex route) → `d4650af` | `d4650af` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41500383127 |
| 2026-11-02 | Row 31's host run **failed**: "query parameter mcp is not allowed for tools.list". The desktop host refuses query parameters its contract does not list, so in the desktop app the catalog could not load. The same rule refused the Extensions card's `?scope=` on uninstall (row 27). Fixed where each was introduced: `088` on row 27 (`extensions.uninstall` query `scope`; the host scenario now uninstalls with `?scope=global`) merged forward through rows 28-31; `089` on row 30 (`tools.list` query `mcp`) merged into row 31 | `5cc2dad` (r27), `7ddb855` (r28), `f9b5be6` (r29), `831d05f` (r30) | https://github.com/dlroqa/Lightagent/actions/runs/41501324410, source-edit https://github.com/dlroqa/Lightagent/actions/runs/41503092346, https://github.com/dlroqa/Lightagent/actions/runs/41503223111 |

## Row 31 — D6-6 (new scope): import skills from a repository or folder

Tracked as scope beyond the audited CLI (no CLI operation installed bare
skills): one backend inventory row, never counted; the CLI-parity
denominator stays 227.

- Layouts: `extension.json`, `SKILL.md`, `skills/<name>/SKILL.md`,
  `<name>/SKILL.md`; anything else is refused naming what was looked for.
  SKILL.md ≤ 512 KiB, ≤ 200 skills, valid unique names, no links anywhere.
- Git: https only, `GIT_ALLOW_PROTOCOL=https`, no prompt, LFS smudge off;
  optional revision validated and fetched shallowly; the commit is
  recorded. Nothing fetched is run.
- Packaged as a generated, inert extension: installation is separate from
  activation (inactive unless `activate`) and grants no tool permission.
- `replace` installs beside, exchanges, saves activation; any failure puts
  the old copy back.
- **Not exercised in CI:** fetching from a real Git host; revision
  handling is covered by validation unit tests only.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-02 | `086`; branch `desktop/d6f-skill-import`, draft PR #125; later merges of rows 30's and 27's fixes | `814e5ec` → `62e87e6` / `25fbf86` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41499829596 |

## Extensions and MCP: CLI and desktop differences, and when changes apply

- **Install and MCP.** `lightagent extensions install` switches
  `mcp.enabled` on when the bundle declares MCP servers, and prints that it
  did (unchanged CLI behavior). The API and the desktop do so only when
  asked (`activate_mcp`; the card's "Turn MCP on" checkbox starts
  unchecked), and the MCP card's own switch asks for confirmation. The
  difference is deliberate: the desktop never turns on execution as a side
  effect. A skills import never turns MCP on.
- **Enable, disable and running processes.** MCP servers are connected per
  run: each run builds its registry (`serve.rs` start-run →
  `configured_registry_for` → `mcp_tools`) and drops it at the end. So
  enabling or disabling an extension, installing or removing one, and any
  MCP configuration change apply from the next run, with no restart or
  reconnect. A turn already running keeps the servers it connected until it
  ends. A check (row 29) connects and disconnects on its own.

## Carried requirements (unchanged)

- D7: diagnostics-bundle secret-redaction checks.
- D8: native WebView checks for Settings, profiles, close with active runs,
  run controls, response actions R2-R5, and now the Extensions, MCP and
  skills-import controls (confirmations, keyboard use, narrow windows).

Coverage at `62e87e6`: **64 of 227** (11 rows added: the MCP server and `config.mcp` rows, `cli.tools.list`, `slash.skills`, `skills.roots`).

Reruns: besides the reruns this session made (job 125447936741 for #122, job 125475716344 for #123), attempt-2 reruns of the subagents runs 41503194650, 41503200320, 41503201249 ("Tester broker": the fixture run stayed queued past the test's window while five PRs' runs filled the runners) and of check run 41503194699 (macos-arm64 runner not acquired) were started from the owner's account outside this session; all passed. Their attempt-1 records remain in the run histories.

## Ownership and the parity correction (2026-11-03)

Two sessions wrote to this stack on 2026-11-02. The owner stopped the one
that pushed docs `5c98b3e` (request 090); this session resumed as the sole
writer at 00:3xZ (claim comment on #125). `5c98b3e` counted `slash.skills`
and `skills.roots` as implemented citing the host `skills` scenario, which
never called `GET /skills`. Under the evidence rule kept since D5 (backend,
host and browser), coverage was **62 of 227** until row 32's host check
passed; both rows are now counted with that evidence.

## Row 32 — D6 close-out

- Host (`desktop/e2e/runs.py`): `skills` lists `GET /skills` through the
  host (a global, a profile and an extension skill, each with scope and
  source); `mcp` saves an HTTP server's auth reference and profile lists
  through the host and reads them back by name; `extensions` turns the
  master switch off through the host (the CLI sees it) and on by the CLI
  (the host sees it).
- Browser: `e2e/mcp.mjs` saves the auth variable, other profiles and
  credential profiles and finds each shown by name; `e2e/extensions.mjs`
  turns the new master switch off and on.
- The Extensions card's master switch (`extensions.enabled`) uses the
  shared `config.set` operation, the same as `lightagent config set
  extensions.enabled`; backend `desktop_extensions.rs` proves API and CLI
  see one setting.
- Contract: the desktop host refuses query parameters its contract does
  not list for an operation. A third missing one, `providers.remove`'s
  `expected_revision`, dated from row 15 (#110): removing a saved provider
  in the desktop app was refused. It is listed now, and
  `scripts/check-desktop-contract.py` checks every query parameter the
  client sends (self-test included), so a missing one fails CI. Every
  client query (`tools.list` `mcp`, `extensions.uninstall` `scope`,
  `providers.remove` `expected_revision`, `sessions.search` `q`,
  `runs.events` `after`) is now listed.
- **Latent window for D10:** rows 15-31 carry the `providers.remove`
  omission (the fix is at row 32). Before merge approval: backport the
  one-line contract change to row 15 and merge forward, or accept that the
  intermediate states between rows 15 and 32 cannot remove a saved provider
  in the desktop app (no desktop build ships from them).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `091`; branch `desktop/d6g-close-out`, draft PR #126 | `c70c2f9` / `cee4280` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41554358723 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24 (the host scenarios, the browser checks and the contract query check included) | `c70c2f9` | https://github.com/dlroqa/Lightagent/actions/runs/41554449463, https://github.com/dlroqa/Lightagent/actions/runs/41554449310 |

## Row 33 — D6-6: the Git import path against a Git repository built on the runner

- A private `Transport`: `Https`, the only one callers reach, and
  `LocalFixture`, compiled only for tests (`file://`,
  `GIT_ALLOW_PROTOCOL=file`). Production code has no file transport.
- Unit tests with the runner's real git: a tag installs the tagged commit
  (inactive) and records it; the default branch (replace) installs the
  newer commit; a commit id installs exactly that commit; `#packs/writing`
  selects a folder (`single_skill`, folder and commit recorded); an
  unknown revision (Unavailable), a missing folder (Invalid) and a missing
  repository (Unavailable) leave the installed copy byte for byte with
  nothing left beside it.
- **Evidence class:** a local Git fixture over `file://`. A real remote
  Git host over HTTPS is not exercised: its transport and TLS are git's
  own, and CI takes no network dependency on a host.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `092`; branch `desktop/d6h-git-fixture`, draft PR #127 | `a753206` / `320ce65` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41554673176 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24 (the Git-fixture tests ran in all four `check` jobs: Linux, Windows and both macOS) | `a753206` | https://github.com/dlroqa/Lightagent/actions/runs/41554806093, https://github.com/dlroqa/Lightagent/actions/runs/41554806107, https://github.com/dlroqa/Lightagent/actions/runs/41554806138, https://github.com/dlroqa/Lightagent/actions/runs/41554806168 |

## Hosted-runner reliability (2026-11-02/03)

Gate failures that were infrastructure, not code. Only the affected job was
rerun; every first attempt stays in its run's history.

| PR | Job | Attempts | Cause |
|---|---|---|---|
| #122 | `check (macos-arm64)` 125347244973 | 1 failed, 2 passed | not acquired by a hosted runner (arm64 capacity) |
| #121 | `check (macos-arm64)` 125475694355 | 1 failed, 2 passed | same |
| #123 | `desktop shell (macos-arm64)` 125475716344 | 1 failed, 2 passed | same |
| #123 | `check (macos-arm64)` 125475718287 / 125517801995 | 2 failed, 3 passed | same |
| #125 | `check (macos-arm64)` 125520601247, `desktop shell (macos-arm64)` 125520599823 | 1 failed, 2 passed | same |

**The broker test.** `Tester broker (live GitHub Actions)` dispatches a
fixture workflow twice: one run is meant to pass and one is meant to fail,
and the test asserts both outcomes. That deliberate failure is a red
`subagent fixture` run, reported by a green broker job; it is never a
broker failure. Of the last 54 completed broker jobs, 4 failed (7.4%), all
on timing, none on a product or test defect, and each rerun passed:

| Broker job | Failure | Cause |
|---|---|---|
| 125121991667 (#110), 125145322936 (#113) | `dispatched_unlocated` | the dispatched run was not yet visible through the API |
| 125475693770 (#121), 125475715229 (#123) | `still_running` where `passed`/`failed` was expected | the fixture's job was still queued: five stacked PRs saturated the runners |

The six cancelled broker jobs in the same window were superseded by a newer
push (cancel-in-progress), not failures. Impact: under load a broker job
can turn a slice red for timing alone. A wider wait window, or locating the
run by its correlation id over a longer period, would remove it; that is a
change to the subagents tests and is proposed, not made, here.

## Row 34 — D7-1: memory through one shared operation set

- `lightagent memory` worked on the active profile's memory file directly
  and the API had no memory route. One implementation now
  (`crates/lightagent/src/memory.rs`): list, add, update, forget, clear,
  search (hybrid recall with the configured semantic embedder), reflect,
  candidates and promote, returning typed results. The CLI prints them as
  before; `memory::perform` answers the API with exactly the CLI's
  `--json`, through `ops::MemoryRequest` and `RunFactory::memory` (the
  profiles pattern; the default refuses). `MemoryStore` locks and
  re-reads before each change, so the CLI and the API never overwrite each
  other. New: an empty text is refused for add and update.
- API (operator) under `/memories` — not `/memory`, which the inventory
  and contract checkers' self-tests inject as their unlisted probe.
  Contract `memory.*` with every query listed; the host refuses the memory
  writes during a profile switch (memory belongs to the active profile).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `094`; branch `desktop/d7a-memory-ops`, draft PR #128 | `f681980` / `4f03211` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41558246059 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24, first run | `f681980` | https://github.com/dlroqa/Lightagent/actions/runs/41558340948, https://github.com/dlroqa/Lightagent/actions/runs/41558341043, https://github.com/dlroqa/Lightagent/actions/runs/41558341167, https://github.com/dlroqa/Lightagent/actions/runs/41558341185 |

## Row 35 — D7-2: the Memory screen

- A Memory screen (account menus, `/memory`): remember with kind and
  tags, search with a result count, edit, forget (asks first), forget
  everything (asks first and states the count), the knowledge page with a
  topic, and from a saved conversation the suggested facts and keeping any
  user message by its number, optionally edited, with kind and tags.
- Browser `e2e/memory.mjs` (real harness) and host `runs.py memory` (every
  operation through the host, matched with the CLI).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `095`; branch `desktop/d7b-memory-ui`, draft PR #129 | `d691c90` / `7dd2106` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41559560433 |
| 2026-11-03 | render **failure**, two test causes: "Search" also named the workspace rail's button; and the harness remembers durable statements automatically (`memory.auto_capture`), so promoting the seeded message's own text merged into that memory | `d691c90` | https://github.com/dlroqa/Lightagent/actions/runs/41559630729 |
| 2026-11-03 | `096` (scope Search to the screen; promote an edited text, the CLI's `--text`) → `f43378f`: 24 of 24 | `f43378f` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41560232731 |
| 2026-11-03 | `097`: the host scenario adds reflect, update, promote and clear, so every memory operation has host evidence → `630988a`: **success**, 24 of 24 | `630988a` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41561924601; https://github.com/dlroqa/Lightagent/actions/runs/41561977615, https://github.com/dlroqa/Lightagent/actions/runs/41561977631, https://github.com/dlroqa/Lightagent/actions/runs/41561977677, https://github.com/dlroqa/Lightagent/actions/runs/41561977752 |

Coverage: the 24 `cli.memory.*` rows → **92 of 227**.

## Row 36 — D7-3: knowledge (RAG) through one shared operation set

- **Integrity fix in `lightagent-rag`.** `RagStore` wrote its in-memory
  copy back with `std::fs::write`: two writers (the CLI and the API) lost
  each other's documents, and a reader could see a half-written index.
  `add` now embeds first, then, under the index's lock file, reads the
  index again, replaces that source's chunks and writes it atomically and
  owner-only (`paths::write_private`); `clear` likewise. A unit test
  opens two stores on one index; both sources remain.
- One implementation (`crates/lightagent/src/rag.rs`): add (a file or a
  folder's files; the remote index is still synced and its failure never
  drops the local document), search (remote first when configured, else
  hybrid local), list, clear. The CLI prints as before; `--json` gains
  `skipped` and `notes` (what it printed to stderr). API (operator):
  `GET|POST|DELETE /knowledge`, `GET /knowledge/search` (`q`, `top_k`);
  POST takes an absolute path. Contract `knowledge.*`; the host refuses
  add and clear during a profile switch.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `099`; branch `desktop/d7c-knowledge-ops`, draft PR #130 | `49109b6` / `fdba802` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41564121251 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24, first run | `49109b6` | https://github.com/dlroqa/Lightagent/actions/runs/41564214555, https://github.com/dlroqa/Lightagent/actions/runs/41564214582, https://github.com/dlroqa/Lightagent/actions/runs/41564214604, https://github.com/dlroqa/Lightagent/actions/runs/41564214607 |

## Row 37 — D7-4: the Knowledge screen

- Index a file or folder (optional source name), see indexed, skipped (with
  the reason) and notes; search with a result count; the indexed sources
  with chunk counts; clear (asks first, states the count). Account menus
  and the render route list.
- Browser `e2e/knowledge.mjs` (clears only when the harness index was
  empty before the check) and host `runs.py knowledge`.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `100`; branch `desktop/d7d-knowledge-ui`, draft PR #131 | `c65132f` / `e82caba` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41564945835 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24, first run | `c65132f` | https://github.com/dlroqa/Lightagent/actions/runs/41565023807, https://github.com/dlroqa/Lightagent/actions/runs/41565023826, https://github.com/dlroqa/Lightagent/actions/runs/41565023863, https://github.com/dlroqa/Lightagent/actions/runs/41565023903 |

Coverage: seven `cli.rag.*` rows → **99 of 227** (`cli.rag.add.opt.source`
not counted: no test sets the source name yet).

## Row 38 — D7-5: sessions across CLI, API and desktop; Delete asks first

- `lightagent sessions list|show|delete` and the API's session routes
  already shared the active profile's `SessionStore` (row 10's lock).
  Proved at every level: backend `desktop_sessions.rs`, host `runs.py
  sessions`, browser `e2e/sessions.mjs`.
- Fixed: the Agent screen deleted a conversation as soon as Delete was
  chosen; it now asks first (Confirm / Keep it). `sessions delete`
  ignored `--json`; it prints `{"id", "deleted"}` now.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `102`; draft PR #132 | `0696551` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41567060355 |
| 2026-11-03 | `check` **failure**: `sessions delete --json` printed text; `104` → `c481a69` | `c481a69` | https://github.com/dlroqa/Lightagent/actions/runs/41567158114 |
| 2026-11-03 | host **failure**, switch.py `pending` (the race, row 40); the same job rerun **passed** on unchanged code (intermittent; attempt 1 kept) | `c481a69` | https://github.com/dlroqa/Lightagent/actions/runs/41567976364 (job 125694325184) |
| 2026-11-03 | render **failure** in the sessions check, narrowed step by step (`107`/`108` named steps; `109`/`110`): the row's actions show on hover or focus (keyboard users reach them by Tab) and take no pointer events while hidden; after "Keep it" the pointer was below the row. The check now hovers the row first → `c0b5ed3` | `c0b5ed3` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41572147184 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24 | `c0b5ed3` | https://github.com/dlroqa/Lightagent/actions/runs/41572984591, https://github.com/dlroqa/Lightagent/actions/runs/41572984624, https://github.com/dlroqa/Lightagent/actions/runs/41572984660, https://github.com/dlroqa/Lightagent/actions/runs/41572984678 |

## Row 39 — D7-6: worker assignments through one shared operation

- `worker_assignment` (crate `lightagent`): assign (a profile or the
  built-in worker) and revoke, for the active or a named profile, with the
  CLI's checks; the CLI prints as before and `RunFactory::workers_change`
  answers the API with the same JSON. API: `PUT|DELETE /workers/{role}`;
  `GET /workers` takes `profile` in the contract. The Specialists card:
  whose workers, and per role Assign to (built-in or a profile) / Revoke.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `103`; draft PR #133; row 38's fixes merged in server-side | `85df9f7` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41567463900 |
| 2026-11-03 | `check` **failure**: the test expected the profile name in readiness, which reports `assigned:<assignment id>` (API and CLI agreed); `106` fixed the assertion | `b9944df` | https://github.com/dlroqa/Lightagent/actions/runs/41567977741 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24 | `4afe907` | https://github.com/dlroqa/Lightagent/actions/runs/41572984218, https://github.com/dlroqa/Lightagent/actions/runs/41572984227, https://github.com/dlroqa/Lightagent/actions/runs/41572984266, https://github.com/dlroqa/Lightagent/actions/runs/41572984319 |

## Row 40 — the profile switch's rejections before its cancels (D5 race)

- Found by the host run of row 38: `stop_runs` (row 21) rejected each
  pending approval and then cancelled every run at once. The run waits on
  a `tokio::select!` (not biased) over the decision and the cancel token;
  when both were ready the cancel could win, and the run ended
  `Cancelled` with no rejection recorded. The D5 acceptance item "a
  profile switch with a pending approval records an explicit rejection"
  was therefore not guaranteed.
- Fix: after rejecting, the host waits (within its drain timeout) until
  each rejected run has left `awaiting_approval`, then cancels. Evidence:
  the host `switch.py pending` scenario.
- D10 decision: rows 21-39 carry the race (see MERGE_ORDER).

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-03 | `105`; draft PR #134 | `bb8241e` | source-edit https://github.com/dlroqa/Lightagent/actions/runs/41568476519 |
| 2026-11-03 | `check`, `desktop`, `jev evaluation`, `subagents`: **success**, 24 of 24 | `627e899` | https://github.com/dlroqa/Lightagent/actions/runs/41572990604, https://github.com/dlroqa/Lightagent/actions/runs/41572990675, https://github.com/dlroqa/Lightagent/actions/runs/41572990742, https://github.com/dlroqa/Lightagent/actions/runs/41572990743 |

Coverage: the five `cli.sessions.*` rows and six `cli.workers.*` rows →
**110 of 227**.

## Rows 41-50 — D7: identity, remote credentials, diagnostics, setup, configuration, update check, security

Implemented by session `978317c3` (owner from 2026-11-03T05:5xZ, claim on
#134), then session `445d73de` (owner from 09:0xZ, claim on #144, handoff
confirmed by the previous owner). Every head below is the row's current
head; every gate named passed on that exact commit.

| Row | PR | Branch | Head / tree | Gates |
|---|---|---|---|---|
| 41 the `rag --source` and workers `--worker`/`--profile` gaps at host and browser | #135 | `desktop/d7g-evidence-gaps` | `7cb8bfa` / `090da94` | check 41587423780, desktop 41587423781, jev 41587423772, subagents 41587423775 |
| 42 identity through one shared operation (`identity_admin::perform`, local-operator gate) | #136 | `desktop/d7h-identity-ops` | `a0b82e8` / `5bedc8c` | check 41588561713, desktop 41588561702, jev 41588561690, subagents 41588561683 |
| 43 the Identity screen | #137 | `desktop/d7i-identity-ui` | `2d572cc` / `2249fe8` | check 41589056316, desktop 41589056278, jev 41589056276, subagents 41589056380 |
| 44 remote credentials (keygen needs no key, fix `121`) | #138 | `desktop/d7j-remote-credentials` | `b24d10f` / `1abc992` | check 41592699058, desktop 41592698681, jev 41592698713, subagents 41592698833 |
| 45 diagnostics: doctor, architecture, the allowlisted bundle | #139 | `desktop/d7k-diagnostics` | `443df6e` / `bdbf16f` | check 41596482239, desktop 41596482116, jev 41596482297, subagents 41596482376 |
| 46 setup: init and the Hermes import | #140 | `desktop/d7l-setup` | `3fde996` / `f6b220f` | check 41601200056, desktop 41601199989, jev 41601200159; subagents 41601199941 failed twice on hosted queueing (below) |
| 47 one key of every settable configuration section | #141 | `desktop/d7m-config-sections` | `8f2496b` / `c2747be` | check 41608931747, desktop 41608931818, jev 41608931918; subagents 41608931679 hosted queueing |
| 48 evidence completion (host and browser gaps for identity, remote credentials, setup) | #142 | `desktop/d7n-evidence-completion` | `9013dd3` / `f7dac7e` | check 41608933114, desktop 41608933059, jev 41608933107; subagents 41608933089 hosted queueing |
| 49 `update --check`, read-only | #143 | `desktop/d7o-update-check` | `2b79e05` / `48deb34` | check 41608937007, desktop 41608937124, jev 41608936951, live 41608936949 (and 41601568852 at `78a4536`); subagents 41608936930 hosted queueing |
| 50 the Security card; parity-test hardening F2-F4 | #144 | `desktop/d7p-security` | `f17115c` / `a53d052` | check 41608939464, desktop 41608939285 (attempt 2), jev 41608939327; subagents 41608939492 hosted queueing |

Rows 51 and 52 (below) passed all four workflows, subagents included,
and their trees contain rows 46-50, as row 17's did for row 16.

**Failures and fixes.**
- `122` (row 47): an unused SECTIONS table became a debug assertion.
  `125`: `POST /sessions` answers 201. `126`: `lightagent --json config
  show` prints the configuration itself; only `GET /config` wraps it in
  `config`. `130` (row 46): a clippy borrow; `profile list --json` prints
  `active` (not `active_profile`); a persona lives in `SOUL.md`, and only
  the text `profile show` prints it.
- Row 48's conflict with row 47 was resolved by re-applying its edits on
  the fixed base (`127`, `131`) under two-parent merge commits. The
  resolution was reviewed by content, not by ancestry: the earlier fixes
  above are present at `f17115c`.
- `132` (row 47, 2026-11-03): rows 47-50 failed render and the desktop
  runs job on row 47's tests, not on product code. The browser check read
  keys before they had a value, and it restored each key with an external
  PUT, which made the card's revision stale (its next Set was refused,
  409, by design). The host scenario read originals with `config get`,
  which refuses an optional key with no value. Both now read through the
  API (a 404 is an unset key, refused alike by the CLI), reload the card
  before each key, and check every key is restored. Carried up by
  server-side merges (`POST /merges`, no force): row 48 `9013dd3`, row 49
  `2b79e05`, row 50 `f17115c`; each merge's change against its first
  parent is exactly `132`'s.

**Parity-test audit (review findings).** F1: `desktop_setup.rs` compared
a missing `settings()` field with a missing one (null equals null); fixed
in `126`. F2: `desktop_identity.rs` compared a restriction without its
content; hardened (row 50). F3: `desktop_setup.rs` init comparisons had no
non-null guards; hardened (row 50). F4: `desktop_knowledge.rs` chunk
counts had no number check; hardened (row 50). Rows 51-52 assert known
values, required fields and their types.

## Row 51 — D7: conversations

Draft PR #145, `desktop/d7q-conversations`, head `9f4dbc5` (tree
`f0df67b`), edit request `133`. Rename now edits the title in place
(Enter saves it trimmed, Escape keeps the old one). It used
`window.prompt`, which not every desktop WebView implements, so rename
could do nothing in the app. **Create project…** still uses
`window.prompt`; it is a D8 native item. Evidence: a conversation
continues between the desktop and `lightagent chat --session` both ways,
with the earlier turn sent to the model; search by message and by title;
rename (an empty title refused); an attachment saved byte for byte,
owner-only, under `attachments/<session>`; a file name that is not plain
refused by the runtime and by the host itself (the e2e driver's fetch now
passes raw bodies, as `api_fetch` does); Enter sends, Shift+Enter is a new
line; New chat starts a fresh session in the same profile. Gates: check
41611764860, desktop 41611764757, jev 41611764715, subagents 41611764652.

## Row 52 — D7: the setup sections in Settings

Draft PR #146, `desktop/d7r-setup-sections`, head `cc2829d` (tree
`d802e1b`), edit requests `134` and `135`. `lightagent_core::WebMode`
(`of`, `apply`) is the one web-mode operation for both setup variants and
`PUT /settings`; Settings has a Web access selector with setup's four
choices. `135` fixed a defect before CI: every Settings save carries the
mode it read, and re-applying it would reset realtime knowledge set by
hand. The mode now applies only as a change, and the backend test proves
an unrelated save keeps `rag.realtime_enabled`. Gates: check 41613505978,
desktop 41613505967, jev 41613506002, subagents 41613506030.

Coverage after rows 41-52 and the inventory correction: **204 of 222**.

## Inventory correction: test-only environment names (2026-11-03)

Owner decision: an env row is reclassified only when a source audit
confirms that no supported product path reads it. Audit at `12582a0` and
`cc2829d`: every reference in crates, scripts, workflows, packaging and
docs, plus indirect reads (`${env:NAME}` secret references take
user-chosen names; `serve --key-env` reads the variable it is given; the
only other runtime reads are `CI`, `GITHUB_ACTIONS` and the excluded
evaluation names).

| Row | References | Finding |
|---|---|---|
| `env.LIGHTAGENT_PROVIDER_KEY` | `crates/lightagent/src/lib.rs` (config_keys unit test), `tests/desktop_diagnostics.rs` | test only → not-applicable |
| `env.LIGHTAGENT_QDRANT_URL` | `tests/platform_services.rs`, `check.yml`, `scripts/test-platform-integration.sh`, `docs/platform-integration.md` | test only → not-applicable |
| `env.LIGHTAGENT_INFINITY_URL` | same as Qdrant | test only → not-applicable |
| `env.LIGHTAGENT_OPEN_TERMINAL_URL` | ignored live test in `builtins/open_terminal.rs`, `check.yml`, `docs/platform-integration.md` | test only → not-applicable |
| `env.LIGHTAGENT_OPEN_TERMINAL_TOKEN` | same, and a redaction probe in `tests/desktop_diagnostics.rs` | test only → not-applicable |
| `env.LIGHTAGENT_API_KEY` | `packaging/systemd/lightagent.env.example` and `lightagent.service` (`serve --key-env LIGHTAGENT_API_KEY`), sandbox tests, `check.yml` | **product** (the packaged service) → stays applicable (remote mode, D8) |

The denominator moves 227 → 222 as an inventory correction; nothing is
counted for it. Each row keeps its id, with the audit as its reason.

## Hosted-runner reliability, continued (2026-11-03), and the broker fix

| Job | Cause | Action |
|---|---|---|
| `desktop shell (macos-x64)` 125825666277 (row 50) | `hdiutil: couldn't eject "disk2" - Resource busy`; the bundling retry matches only `create failed - Resource busy` | job rerun once (attempt 2 passed); widening the retry pattern is a follow-up |
| `Tester broker (live)` 125800084898, 125800085405, 125801304410, 125825276049 (#140 attempt 2), 125825640433, 125825644829, 125825659185, 125825667194 | a child fixture's job waited 11-19 minutes for a hosted runner (for example run 41609058709: queued 09:11:57Z, started 09:28:06Z), past the broker's 600 s wait; every child, once it ran, passed or failed as intended | not rerun en masse; fixed in #147 |

**#147** (`fix/broker-queue-wait`, base `master` `12582a0`, head `e0e3ab4`,
owner-authorized): `actions.status` waits for a runner (`queue_wait`,
default 600 s, at most 3600 s) and for the executing run (`max_wait`, at
most 900 s) separately, under an overall cap. A run that has not completed
is `still_queued` or `still_running`, never passed, and the report names
the wait's outcome, the time queued and running, and the limits. A
cancelled wait stops at once. The live checks allow the observed queue
(30 minutes) and cancel a child they gave up on. Deterministic tests cover
queued → running → passed, a queue that never yields a runner, an
execution timeout, cancellation, an unexpected and a startup failure, no
wait, and the overall cap. Its evidence covers #147's own head until the
D10 candidate tree includes it.

## Artifacts and retention

| Artifact | Run | Retention |
|---|---|---|
| `prepared-015-desktop-d0-audit.txt`, `prepared-016-…` (commit, tree, diff) | 40995818991, 40996341547 | 30 days |
| `spike-tauri-<target>`: measure.json, screenshot, Cargo.lock, dmg/msi/nsis/deb/AppImage | 40997420599 (also 40996571854) | 30 days |
| `spike-electron-<target>`: measure.json, screenshot | 40997420599 | 30 days |
| `lightagent-web-screens` (existing `check` job) | 40996392105 | repository default |
| `desktop-shell-<target>`: e2e JSON, report, screenshots (ready and mismatch), dmg/nsis/msi/deb/AppImage | 40999470324 | 30 days |
| `desktop-cargo-lock` | 40999470324 | 30 days |
| `desktop-switch-e2e`: `switch-e2e/` (switch.json, reports, `driver-switch-pending.log`) and `runs-e2e/` (runs.json with the provider log, `report-runs.txt`, `driver-runs.log`) | 41431492257 (row 21), 41434134035 (row 22) | 30 days |
| `prepared-062…`, `prepared-063…`, `prepared-064…` (commit, tree, diff) | 41429457355, 41431358008, 41433988154 | 30 days |
| `desktop-switch-e2e` (runs.json with the continue and steering scenarios) | 41445389192 (row 23), 41451624678 (row 24) | 30 days |
| `lightagent-web-screens`: `runs/` (approval, paused, steering screenshots, record.json of what each control sent) | 41451624446 | repository default |

## Local backup

- **What is backed up:** a full git clone at `~/Repositories/LAcode/Lightagent`
  on the owner's machine (origin `https://github.com/dlroqa/Lightagent.git`).
  Its checked-out working tree is `master` (`12582a0`). Every `desktop/*`
  branch and `tools/source-edit` are kept as **fetched remote-tracking refs**
  (`refs/remotes/origin/…`), with all their objects, but are **not** checked
  out as files.
- **How it is synchronized:** `git fetch` only, after every stage. No reset,
  clean, checkout, or force; local work is never overwritten.
- **Local progress copy:** `~/Repositories/LAcode/desktop-progress/DESKTOP_PROGRESS.md`,
  refreshed from the authoritative GitHub version after each stage.
- **Synchronization record:**

  | Time (UTC) | Backed-up `desktop/*` heads | Notes |
  |---|---|---|
  | 2026-10-30T03:33:59Z | `d0-audit` `bc01426`, `d1-spike` `82896bc` | first sync |
  | 2026-10-30T13:01:04Z | through `d4b-profile-ui` `8c27f93` | D4b first gate |
  | 2026-10-30T22:37:46Z | `d0-audit` `bc01426`, `d1-architecture` `350b48b`, `d2-shell` `003a71c`, `d3a-runtime` `130dde9`, `d3b-lifecycle` `1f0ed69`, `d4a-profiles` `c544d7a`, `d4b-profile-ui` `8c27f93` | baseline re-verified before the D4b review fix; working tree clean |
  | 2026-11-01T22:27:58Z | `d4b-profile-ui` `5f364e2`, `feat/response-action-bar` `987c6db`, `integration/desktop-d4b-response-actions` `b4a61c9`, `tools/source-edit` `df11eac` (lower slices unchanged) | before recording #98's integration; fetch only, working tree `master` `12582a0` clean |
  | 2026-11-01T23:07:09Z | `d4b-profile-ui` `7c3e988`, `d4c-config` `0e46892`, `feat/response-action-bar` `987c6db`, `integration/desktop-d4b-response-actions` `b4a61c9`, `tools/source-edit` `a9f91a3` | D4c validated; fetch only, working tree clean |
  | 2026-11-02T00:57:09Z | `d4c-config` `551e73a`, `d4c1-response-actions` `fbd008c`, `d4c2-session-writes` `aebd5d6`, `tools/source-edit` `b9cacc0` (lower slices unchanged) | row 10 validated; fetch only, working tree `master` `12582a0` clean |
  | 2026-11-02T06:42:55Z | `d4c3-config-acceptance` `597604d`, `d4d1-credentials` `8f0717d`, `d4d2-endpoint-check` `2bed55c`, `d4d2b-key-binding` `8e3919b`, `d4d2b2-saved-providers` `108dac6`, `d4d2b3-providers-ui` `bb76e74`, `d4d3-fail-closed` `c15e249`, `tools/source-edit` `4cc32f9` (lower slices unchanged) | rows 14-17 validated; fetch only (remote-tracking refs), working tree `master` `12582a0` clean |
  | 2026-11-02T07:45:56Z | `d4d3-fail-closed` `bf5e56d`, `d4e-workspaces` `ec1fc4e`, `d5b-run-admission` `1e657e5`, `tools/source-edit` `c082884` (lower slices unchanged) | rows 18-19 validated; fetch only, working tree `master` `12582a0` clean. Local copies of the governing documents refreshed from `bf5e56d` at 07:08:20Z, earlier copies kept as `*.prev-2026-11-02c.*` |

  | 2026-11-02T11:24:27Z | `d5b-run-admission` `db44a46`, `d5b2-lease-release` `e5363fc`, `d5a1-host-runs` `64d632f`, `d5a2-interrupted` `c6fd6ac`, `tools/source-edit` `aef2d2e` (lower slices unchanged) | rows 20-22 validated; fetch only (remote-tracking refs), working tree `master` `12582a0` clean |

  | 2026-11-02T13:26:47Z | `d5a2-interrupted` `da950d1`, `d5a3-continue` `0d37299`, `d5a4-run-controls-ui` `e164dde`, `tools/source-edit` `4c05f6e` (lower slices unchanged) | rows 23-24 validated; fetch only (remote-tracking refs), working tree `master` `12582a0` clean |

  | 2026-11-02T14:32:30Z | `d5c-paused-discovery` `ed0ae47`, `d6a-extension-ops` `3e558b8`, `d6b-extensions-ui` `c89c989`, `tools/source-edit` `e1144b6` (lower slices unchanged) | rows 25-27 validated; fetch only (remote-tracking refs), working tree `master` `12582a0` clean |

  | 2026-11-02T20:06:04Z | `d6b-extensions-ui` `5cc2dad`, `d6c-mcp-ops` `7ddb855`, `d6d-mcp-check` `f9b5be6`, `d6e-tools-skills` `831d05f`, `d6f-skill-import` `62e87e6`, `tools/source-edit` `f5537ff` (lower slices unchanged) | rows 28-31 and the row 27 fix; fetch only (remote-tracking refs), working tree `master` `12582a0` clean |

  | 2026-11-03T00:54:57Z | `d6b-extensions-ui` `5cc2dad`, `d6c-mcp-ops` `7ddb855`, `d6d-mcp-check` `f9b5be6`, `d6e-tools-skills` `831d05f`, `d6f-skill-import` `5c98b3e`, `d6g-close-out` `c70c2f9`, `d6h-git-fixture` `a753206`, `tools/source-edit` `ef7b5a3` (lower slices unchanged) | rows 27-33 (fixes merged forward, row 32, row 33); fetch only (remote-tracking refs), working tree `master` `12582a0` clean |

  A documentation commit cannot record itself. Each row names the heads
  that existed when the sync ran. The commit that carries a row is
  backed up by the next sync.

## Rows 53-56 (2026-11-03, owner `a0fb357c`)

Ownership: session `a0fb357c` (lightagent-c7) took over from
`445d73de`, which had no live process and had ended its last turn at
12:44Z (claim: #144 issuecomment-6604428555).

Failures diagnosed (each a product or test defect, fixed by source edit;
no rerun was needed):
- #149: the secrets gate rejected a home-directory placeholder, and the
  contract check read a backticked `/onboard` in an agent.ts comment as a
  client path. Request 143 → 940b1f5.
- #150: `Vec<Value>` without `Value` in scope, and the same contract
  issue for `/help`. Request 144 → 7315635, then the two-parent merge
  46abd19 (tree 7423c97, row 54's fix carried up; POST /merges
  conflicted).
- #153: the sidebar shortens long titles (request 148), and the stale
  check first counted model requests, which the server can make after
  a saved run (request 149). It now asserts the 409 and unchanged runs.

| Row | PR | Head | Evidence |
|---|---|---|---|
| 53 | #148 | `67b8124` | rotate --expires-in, rc list --profile, init --profile/--base-url, LIGHTAGENT_HOME |
| 54 | #149 | `940b1f5` | /onboard, /onboard remove |
| 55 | #150 | `46abd19` | /help, /reload |
| 56a | #151 | `4e1aa34` | Jev status and check (stub evidence; desktop_jev.rs, runs.py jev, jev.mjs) |
| 56b | #152 | `03f4cfe` | LIGHTAGENT_CREDENTIAL_KEY, HERMES_HOME (second host, second server) |
| 56c | #153 | `0e53dd3` | regenerate on the real backend in the browser |

Coverage: 218 of 222. Remaining, with the stage and the criterion:

| ID | Missing | Stage | Done when |
|---|---|---|---|
| `cli.remote-credential.add.opt.secret-stdin` | host-side secret input | D8 | a native picker or host-owned input; no secret byte in WebView state, IPC answers, logs, diagnostics, screenshots or artifacts (seeded-secret tests); cancel, unreadable and invalid fail safely; the source file is never deleted |
| `cli.remote-credential.rotate.opt.secret-stdin` | the same | D8 | the same, for rotate |
| `env.LIGHTAGENT_API_KEY` | remote mode | D8 | the desktop against a `serve --key-env LIGHTAGENT_API_KEY` server: the key held by the host, never in the renderer; a failing remote is an error with no local execution |
| `cli.chat.opt.profile` | an owner decision | D7/D8 | the desktop's profile switch persists the active profile (DESKTOP_ARCHITECTURE.md §7); `chat --profile` does not. Either accept the switch as the equivalent and document the difference, or build a non-persisting per-conversation profile; then evidence |

## Row 57 (2026-11-04, owner `a0fb357c`)

Owner decision (2026-11-04): `lightagent chat --profile` is matched by a
conversation-scoped profile, prepared for later account integration;
the profile switch stays the only way to change the active profile.
#154 `desktop/d7y-conversation-profile`: request 151 (3583a5a), then 152
(c54b905). The first check run, 41780671146, found two defects:
- Product: a run that continues no conversation looked up an operator
  conversation store, which an embedding without a home refused with
  500. Thirteen ownership tests showed it.
- Test: the browser check waited on a hidden element; its screenshot
  showed the feature working.
Green at c54b905: check 41782010579, desktop 41782010641. Coverage 219
of 222. Contract and account mapping:
DESKTOP_CONVERSATION_PROFILES.md.

## Next

The owner's order of 2026-11-03 (sole owner `a0fb357c`):

1. The owner's decision on `cli.chat.opt.profile`.
2. D8: native WebView verification (an e2e-hooks evaluation channel into
   the real WebView, or tauri-driver where it exists, with unavailable
   platforms stated), host-side secret input for the two
   `--secret-stdin` rows, and remote mode with `LIGHTAGENT_API_KEY`.
3. Before merge approval: the backport or reordered delivery for the
   rows 15-31 and 21-39 defect windows, and #147's placement.


The owner's order of 2026-11-03 (sole owner `445d73de`):

1. Rows 47-52 gates are green (check, desktop, jev evaluation; rows 51-52
   subagents too). The rows 46-50 subagents parents failed on hosted
   queueing only; #147 is the fix, validated on its own head.
2. **Row 53 (next):** close the five evidence gaps found in the audit:
   rotate `--expires-in` and `remote-credential list --profile` in the
   browser, `init --profile`/`--base-url` asserted in the request, and
   `LIGHTAGENT_HOME` asserted equal to the home shown (host and browser).
3. Then the rest of D7: `slash.help`, `slash.reload`, `slash.onboard`,
   `slash.onboard-remove`; `api.get.jev` and `api.post.jev.verify` on the
   real backend; regenerate on the real backend in the browser;
   `cli.chat.opt.profile`; `env.HERMES_HOME`; `env.LIGHTAGENT_CREDENTIAL_KEY`.
4. D8: native WebView verification; host-side secret input for the two
   `--secret-stdin` rows (no secret byte in the renderer; see
   DESKTOP_ACCEPTANCE.md); remote mode with `LIGHTAGENT_API_KEY` held by
   the host; the remaining native items (Create project…, R2-R5). Then D9
   installers, and D10 parity, the candidate merge sequence on hosted
   runners, and the review package.

The owner's order of 2026-11-02 (fourth continuation, DESKTOP_CLAUDE_PROMPT.md §6):

1. The D5b review follow-through is done (row 20).
2. D5a is split by dependency: row 21 (host relay resume plus run
   scenarios, R1) and row 22 (sidecar crash: interrupted, never rerun) are
   done.
3. Row 23 (Continue over the API, steering semantics, locks) and row 24
   (Continue card, Stop in the conversation, browser coverage) are done. The
   D5 gate is met at backend, host and browser level; native WebView is D8.
4. D6 has started. Row 25 closed D5 (a paused run is reachable from its
   session; the lifecycle reviewed). Rows 26-27 (extensions: shared
   operations, then the card) are done, at 53 of 227.
5. Rows 28-31 are done (MCP configuration, the MCP check and card, tool
   permissions and the MCP-free catalog, skills with their source, and
   skill import as new scope).
6. Row 32 closed D6's acceptance items (host GET /skills, the MCP
   auth/profile fields, the extensions master switch, every client query
   in the contract) and row 33 ran the Git import path against a Git
   fixture: 68 of 227. Remaining D6 gap: a real remote Git host over HTTPS
   (not exercised; local fixture only).
7. D7 has started: rows 34-35 (memory: shared operations, then the
   Memory screen), at 92 of 227.
8. Rows 36-37 (knowledge: shared operations with the index lock fix, then
   the Knowledge screen) are done, at 99 of 227.
9. Rows 38-40 (sessions, workers, the switch race) are done, at 110 of
   227.
10. **Next in D7:** identity and remote credentials, init, import and
   chat, the remaining slash, setup, env and config rows, the remaining
   workers and rag options, and the diagnostics bundle with its redaction
   checks. Then D8-D10 in dependency order. D8 keeps the native WebView
   checks (Settings, profile switching, close with active runs, run
   controls, response actions R2-R5, Extensions, MCP, the tool catalog,
   skill import, Memory, Knowledge, sessions and workers). Before merge
   approval (D10): the `providers.remove` and switch-race window
   decisions, then the exact candidate merge sequence simulated on hosted
   runners and every candidate tree validated.

Previous order (DESKTOP_CLAUDE_PROMPT.md §5), kept for reference:

1. D4d-2b and the fail-closed conversion are done (rows 14-17).
2. D4e, workspaces, is done (row 18).
3. D5b (#105), cross-process run admission, is done (row 19).
4. **D5a (next):** the rest of the D5 gate and R1: a deterministic
   fake-provider run through the desktop host (streaming, a tool call,
   approval granted and denied, cancel, continue after a time-budget
   pause, steering); relay reconnect with no duplicated or missing event
   ids; a profile switch with a pending approval records an explicit
   rejection; a sidecar crash mid-run is visible as failed and never
   re-executed; regenerate through the host keeps the saved answer on
   failure or cancel.
5. D6-D10 in dependency order.

Earlier order (DESKTOP_CLAUDE_PROMPT.md §4), kept for reference:

1. **Rows 11 and 12** are done (see "Row 11" and "Row 12"). D4d continues
   with D4d-2, listed under "Row 12".
   The next slice is **D4d**: providers, models, protected credentials
   (including the terminal chat's key resolution) and endpoint checks.

   The original row 11 scope, for reference:
   - CLI and desktop configuration and profile files compared byte for byte
     in isolated homes.
   - Ordered concurrency: a Settings save against a revision loaded before a
     CLI write gets 409 and writes nothing. A CLI write after a Settings save
     keeps it.
   - An unreadable configured secret file fails closed, with a redacted
     error, before any request is made.
   - No secret in the renderer's state (the DOM, browser storage, and the
     loaded settings).
   - `jev-routing-report` as one shared operation, an API route, and Settings
     UI.
   - A Settings conflict keeps the unsaved input and offers review and
     refresh.

   The diagnostics bundle does not exist until D7 (`/diagnostics`), so its
   redaction test moves there.
2. **D4d–e:** providers, models, protected credentials and endpoint checks;
   then workspaces.
3. **D5–D10,** in the plan's dependency order.

## Rows 58-63 — D8, D9 and the D10 review (2026-11-04, owner `a0fb357c`)

The results, per target, with every gap: [DESKTOP_D8_D9_REPORT.md](DESKTOP_D8_D9_REPORT.md).
Heads and the merge strategy: [DESKTOP_MERGE_ORDER.md](DESKTOP_MERGE_ORDER.md) rows 58-63.

| Date | Step | Commit | Evidence |
|---|---|---|---|
| 2026-11-04 | D8a native: first runs drove the real WebViews; found the Settings labels, contrast and the close-question remount (fixed) | `0c75cda` | desktop https://github.com/dlroqa/Lightagent/actions/runs/41846351836, check https://github.com/dlroqa/Lightagent/actions/runs/41846351597 |
| 2026-11-04 | D8b secret input; the real pickers answered by OS input on every target | `a85d6fa` | check https://github.com/dlroqa/Lightagent/actions/runs/41847402711 |
| 2026-11-04 | D8c remote mode (host TLS via the source-edit `=== lock` directive; 32 crates added, none removed); D8a-D8c all green, 75 native checks per target | `71a929c` | desktop https://github.com/dlroqa/Lightagent/actions/runs/41847409624, check https://github.com/dlroqa/Lightagent/actions/runs/41847409491 |
| 2026-11-04 | D8d browser keyboard and axe | `248e1db` | check https://github.com/dlroqa/Lightagent/actions/runs/41847413678 |
| 2026-11-04 | D9 installers and supply chain; 15 advisories in Cargo.lock updated (`=== update`) | `cf1b2b3` | check https://github.com/dlroqa/Lightagent/actions/runs/41848860574, desktop https://github.com/dlroqa/Lightagent/actions/runs/41848860903 |
