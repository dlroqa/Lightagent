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
| D0 Baseline and feature audit | implemented; validation pending | `desktop/d0-audit` | pending | (this commit's parent chain; see the D0 log) | none yet |
| D1 Architecture and UI specification | not started | — | — | — | — |
| D2–D10 | not started | — | — | — | — |

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
- **Six open applicability decisions** (8 rows), resolved in D1:
  - D1-Q1: `update` and its `--check` and `--force` options;
  - D1-Q2: `/update`;
  - D1-Q3–Q5: the three feature-gated provisioning routes;
  - D1-Q6: `LIGHTAGENT_INSTALL_ROOT`.
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
| 2026-10-29 | Edit request `015-desktop-d0-audit` prepared commit (D0 files without the workflow) | recorded in the next update | source-edit run, recorded in the next update |

## Local backup

- Backup clone: `~/Repositories/LAcode/Lightagent` on the owner's machine
  (origin `https://github.com/dlroqa/Lightagent.git`). It is synchronized with
  `git fetch` only: no reset, no clean, no force.
- Local progress copy: `~/Repositories/LAcode/desktop-progress/`.
- Last synchronization and backed-up commit: recorded in each update below.

## Next

1. Record the D0 commit, branch, and draft PR, and get the `desktop` and `check` runs green on the exact head.
2. D1: DESKTOP_ARCHITECTURE.md (framework ADR, threat model, profile-switch
   protocol, local-server security design, screen map) and
   DESKTOP_ACCEPTANCE.md, with a hosted spike workflow for the framework
   measurements.
