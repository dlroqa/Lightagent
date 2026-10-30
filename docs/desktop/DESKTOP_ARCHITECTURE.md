# Lightagent Desktop — architecture decision (D1)

Status: **decided for implementation; not yet validated by any desktop build.**
Inputs:
- the D0 audit ([DESKTOP_PROGRESS.md](DESKTOP_PROGRESS.md),
  [desktop-capabilities.json](desktop-capabilities.json));
- the governing plan ([DESKTOP_BUILD_PLAN.md](DESKTOP_BUILD_PLAN.md));
- the hosted-runner spike in draft PR #91 (evidence only, never merged).

Every number below is a **GitHub-hosted runner measurement**, named by its run.

## 1. Decision summary

| Question | Decision |
|---|---|
| Desktop framework | **Tauri 2** (`tauri` 2.12.x, CLI 2.12.x), in its own Cargo workspace under `desktop/` |
| Runtime placement | **Owned sidecar process**: the bundled `lightagent` binary of the same version and commit, started as `lightagent serve --desktop …` |
| Renderer ↔ runtime boundary | **Typed Tauri IPC to the host only.** The host is the sole HTTP client of the sidecar. The renderer never holds the launch credential and never opens a socket |
| UI | The existing React WebUI (`frontend/`) behind a transport interface. One codebase, built in `web` and `desktop` modes |
| Shared service layer | A new `lightagent-ops` crate of typed operations. The CLI, `lightagent-api`, and through the API the desktop all call it, extracted incrementally (D3–D7) |
| Profile switching | **Start-new-then-commit** sidecar replacement (§7) |
| Local-server security | Per-launch key over stdin, exact Host check, Origin refusal, and Bearer auth on every route including SSE. A key alone is not the design (§6) |
| Updates | Manual only. The desktop never runs `lightagent update` (§11, D1-Q1) |

## 2. Framework comparison

| Criterion | Tauri 2 | Electron | Weight / evidence |
|---|---|---|---|
| **macOS minimum** | Bundle default "macOS 10.13 and above"; `bundle.macOS.minimumSystemVersion` sets `LSMinimumSystemVersion` and `MACOSX_DEPLOYMENT_TARGET` (v2.tauri.app, macOS application bundle). The spike sets **10.14** and records the binary's `LC_BUILD_VERSION minos` (§3) | Electron 45 README: "macOS (Ventura and up)". Electron 42.11.14 (newest release at least 14 days old) declares the `LSMinimumSystemVersion` recorded in §3. No supported Electron reaches 10.14 | **Decisive.** The CLI's Intel floor is 10.14 (README, `release.yml`) |
| Intel architecture | x86_64 build is native on `macos-15-intel` | x64 binaries are published | Both fine. Architecture ≠ OS floor (§9) |
| Repository dependency policy | Own workspace (`desktop/`), so the root `Cargo.lock`, `check-deps.sh`, and "no sudo" root build are untouched. The spike applies the banned-crate list (`aws-lc-*`, `openssl-sys`, `native-tls`, `bindgen`, `cmake`) to the desktop graph for all targets (§3). Linux builds need `libwebkit2gtk-4.1-dev` (sudo apt): allowed on hosted runners, documented as a desktop-only build dependency | No Rust in the host. npm graph instead | Tauri keeps one language for host and runtime and passes the policy (§3) |
| Existing React frontend | Loads the Vite build from bundled assets | Same | Equal. WebView engines differ (WKWebView, WebView2, WebKitGTK), so render tests run on every OS |
| WebView engine risk | The system WebKit on macOS 10.14 is Safari 12–14 era. Vite 6 builds for `safari14` by default. **10.14 therefore also requires the Safari 14 system-WebKit update** (unverified, §9) | Bundled Chromium: one engine | Tauri risk, made explicit |
| Process lifecycle | The Rust host spawns and owns the sidecar (job object on Windows, process group on Unix) | The Node main process does the same | Equal |
| Package size / idle memory | Runner measurements, §3 | Runner measurements, §3 | Measured, not assumed |
| Security surface | Capability-scoped IPC; no Node in the renderer; strict CSP | Context isolation + sandbox are available; Node in the main process | Both adequate when configured; Tauri's default is narrower |
| Accessibility | Platform WebView accessibility trees (NSAccessibility, UIA, ATK) | Chromium accessibility on all OSes | Equal for an ARIA-correct React app; checked in D8 |
| Maintenance | Tauri 2 stable major; a 3.0 alpha exists, so a pinned 2.x line is required | 8-week majors; old majors lose support quickly | Tauri |

**Choice: Tauri 2.** Electron cannot satisfy the declared Intel macOS floor in
any supported release. Tauri can at least declare it, though runtime evidence
on 10.14 is unavailable (§9). The final framework commitment also depends on
the spike's §3 results. If a Tauri build fails a gate there, this section
records it and the decision is revisited before D2.

## 3. Runner evidence (spike, PR #91)

Filled from the spike's run. See [DESKTOP_PROGRESS.md](DESKTOP_PROGRESS.md) for links and artifacts.

Spike head `82896bcd63f3bd6786dcea12b603241e50a3a9ea` (tree `d9cde26`),
run https://github.com/dlroqa/Lightagent/actions/runs/40997420599: all 8 jobs
succeeded.

The spike's Tauri code was first measured at `cf5cb85` (run 40996571854). That
run's Electron jobs failed before measuring because npm had not fetched the
Electron binary; the fix is spike commit `82896bc`. Artifacts are
`spike-tauri-<target>` and `spike-electron-<target>`, retained for 30 days.

**These are runner measurements of an empty window, not user-machine claims.**
Timing is from process spawn to the page's load event. Memory is the summed
resident set of the app, its children, and WebView helpers started after
launch, sampled 10 s after load.

| Target (runner image) | Tauri: page load | Tauri: idle RSS | Tauri: package | Electron 42.11.14: page load | Electron: idle RSS | Electron: runtime size (zip, proxy) |
|---|---|---|---|---|---|---|
| macOS x86_64 (`macos-15-intel`, macOS 15.7.9) | 3.6 s | 94 MB | dmg 3.2 MB | 4.1 s | 215 MB | 207 MB |
| macOS arm64 (`macos-15`, macOS 15.8.2) | 2.1 s | 127 MB | dmg 3.0 MB | 1.8 s | 285 MB | 195 MB |
| Windows x64 (`windows-latest`, Server 2025) | 3.0 s | 300 MB | nsis 1.3 MB, msi 1.9 MB | 1.2 s | 259 MB | 142 MB |
| Linux x64 (`ubuntu-22.04`, Xvfb) | **30.3 s** | 404 MB | deb 1.9 MB, AppImage 76.8 MB | 0.5 s | 636 MB | 119 MB |

Other facts recorded on the runners:

- **Intel macOS deployment target:**
  - the Tauri binary's `LC_BUILD_VERSION` reads `minos 10.14, sdk 15.5`, and `LSMinimumSystemVersion` is `10.14`;
  - the arm64 binary reads `minos 11.0`, but its plist says `10.14`. This inconsistency must not ship: D2 sets the plist per architecture (11.0 on arm64);
  - the Electron 42 runtime's own `LSMinimumSystemVersion` is **12.0**.
- **Dependency policy:** clean for the Tauri graph on all four targets
  (`cargo tree --target all`; none of `aws-lc-sys`, `aws-lc-rs`,
  `openssl-sys`, `native-tls`, `bindgen`, `cmake`). The graph has 407 crates in
  its own lockfile.
- **Rendering:** screenshots of the Tauri window show the page rendered on
  macOS x86_64, Windows, and Linux.
- **Linux load time:** the 30.3 s Tauri page load reproduced in both runs
  (30.2 s and 30.3 s). It is consistent with WebKitGTK waiting on GPU or
  DMA-BUF initialization under Xvfb. It is **not** accepted as a property of
  the product. D2 must investigate (for example
  `WEBKIT_DISABLE_DMABUF_RENDERER=1`) and either fix it or record why it is
  specific to Xvfb, before Linux P1 baselines are set.

**How the decision uses the evidence:**
- Electron starts faster on Windows and Linux (Xvfb).
- Tauri uses less memory on macOS. It ships a package about 60–150× smaller,
  except the AppImage, which carries WebKitGTK.
- Tauri passes the repository dependency policy.
- Tauri is the only option that can declare the 10.14 Intel floor, which is
  decisive.

The Windows and Linux startup gap and the Linux anomaly are tracked as D2
risks with acceptance thresholds (DESKTOP_ACCEPTANCE.md P1–P4).

## 4. Process model and ownership

```
┌──────────── Lightagent Desktop (Tauri host, Rust) ─────────────┐
│ window + WebView ──IPC──▶ host commands ──HTTP(Bearer)──▶ sidecar│
│  (React UI, CSP,          (allowlisted ops,         lightagent   │
│   no network)              SSE relay, dialogs)       serve        │
│                                                     --desktop    │
└───────────── owns: sidecar pid/job, key, logs ───────────────────┘
```

- The host is a single instance (`tauri-plugin-single-instance`); a second launch focuses the first window.
- **Spawn**:
  - the bundled `lightagent` from the app bundle only (Tauri `externalBin`), never `PATH`;
  - argv: `serve --desktop --host 127.0.0.1 --port 0 [--profile <id>]`;
  - the key is the **first line of stdin**. The host then keeps stdin open as the runtime's **lifeline**: however the host ends, even killed, the OS closes the pipe and the runtime exits (graceful shutdown, at most 5 s);
  - the home is the user's normal Lightagent home, so CLI, WebUI, and desktop share state.
- **Readiness**:
  - the sidecar writes exactly one JSON line to stdout:
    `{"lightagent":"serve","protocol":1,"version":"0.5.x","commit":"<sha>","addr":"127.0.0.1:<port>","profile":"<id>","capabilities":[…]}`;
  - the host waits up to 20 s, then requires `version` and `commit` to equal its own bundle metadata;
  - a mismatch is an explicit error, and the host never falls back to another binary.
- **Ownership**:
  - the host records the child's pid and process start time;
  - on Windows the child runs in a Job Object (kill-on-close), on Unix in its own process group;
  - the stdin lifeline makes the sidecar exit when the host disappears, so a crashed host leaves no orphan. This replaced the earlier `--parent-pid` polling design in D3a: it needs no polling and no platform process APIs, and it cannot be fooled by PID reuse;
  - the host never discovers or stops any process it did not spawn: no port scans and no name matching.
- **Shutdown**:
  - app close with active runs shows a dialog listing them, with *Cancel runs and quit* or *Keep window open*;
  - with no runs, or after cancellation, the host sends SIGTERM (CTRL_BREAK on Windows), waits 5 s, then kills the owned job or group.
- **Crash**:
  - an unexpected sidecar exit marks every open run stream `runtime_exited`;
  - the UI shows the last 200 log lines (redacted) and offers *Restart runtime*;
  - restart is automatic at most once per 60 s.
  - Runs are not resumed blindly. After restart, the UI reconciles from `GET /sessions`, `GET /runs/{id}`, and `GET /approvals`.
- **Logs**:
  - sidecar stdout and stderr go to a bounded ring buffer (2 MiB) and a rotated file, 5 × 2 MiB, under the OS app-log directory;
  - the key never appears in them because it is never printed, never in argv, and never in env.

## 5. Renderer ↔ host contract

- The renderer has **no** `fs`, `shell`, `http`, or `process` plugin
  permissions. Its Tauri capability file grants only the host's own commands:
  - `api_call(op, params, body)`;
  - `api_subscribe(run, after) → Channel`, plus `api_unsubscribe`;
  - `upload_attachment`;
  - `pick_path(kind)`;
  - `runtime_status`;
  - `runtime_restart`;
  - `open_external(url)`.
- `op` is an id from the **operation contract**
  `docs/desktop/contract/operations.json` (D2). Each op lists its method, its
  path template, a parameter pattern for every path segment, and whether it
  is destructive. The host refuses unknown ops and malformed parameters before
  any HTTP request. CI keeps the contract equal to the server routes and the
  frontend client.
- `api_subscribe` relays SSE events `{id, event, data}` unchanged, so the
  WebUI's existing sequencing (`id` order, gap detection, `?after=` replay in
  `frontend/src/hooks/useRunEvents.ts`) works as it does in the browser. The
  host adds the terminal pseudo-events `stream_closed{reason}` (for
  `profile_switch`, `runtime_exited`, or `shutdown`).
- **CSP**:
  `default-src 'self'; connect-src ipc: http://ipc.localhost; img-src 'self' data: asset:; style-src 'self' 'unsafe-inline'; script-src 'self'`.
  Navigation away from the app origin is blocked. External links go through
  `open_external`, which allows only `https:` and only after a user gesture.
- The frontend selects its transport at build time (`vite build --mode desktop`).
  `web` mode is unchanged (same-origin `fetch` and `EventSource`).

## 6. Local-server security design (desktop mode)

All of the following apply together. Each closes a different attack path.

| Layer | Rule | Closes |
|---|---|---|
| Bind | `127.0.0.1`, ephemeral port; `--desktop` refuses any non-loopback `--host` | LAN exposure |
| Launch credential | 256-bit key from the OS CSPRNG, generated by the host per sidecar launch. Written once as the first line of the sidecar's **stdin**, which then stays open as the lifeline and carries nothing else. Held only in host memory and overwritten on drop. **Never** in argv, env, a URL, a file, browser storage, the renderer, logs, crash reports, diagnostics, or artifacts | Other local users and processes reading argv, env, or files; leaks into the UI or artifacts |
| Authentication | Every route, **including `GET /runs/{id}/events` and `/health`**, requires `Authorization: Bearer <key>` (the existing `AuthConfig::keyed` path; constant-time comparison verified in D3). No query-string tokens. No cookies | Unauthenticated local callers; token leakage through URLs |
| Host validation | `Host` must equal exactly `127.0.0.1:<bound port>`, otherwise `421`/`403 host_rejected` | DNS rebinding: a remote page resolving its own name to 127.0.0.1 |
| Origin validation | Any request carrying an `Origin` header is refused (`403 origin_rejected`): the only legitimate client is the host's `reqwest`, which sends none. `Sec-Fetch-Site` other than `none`/absent is refused too | Browser-driven CSRF and cross-origin reads, even with a guessed port |
| Event stream | Same three checks as above. The host relays it, so the renderer never needs `EventSource` credentials | SSE token-in-URL leakage |
| No static UI | `--desktop` disallows `--web-root`; the fallback returns 404 | Serving content to a browser at all |
| CORS | No CORS headers, ever | Cross-origin readable responses |

The browser WebUI mode (`serve` without `--desktop`) gets the same Host and
Origin layers in D8: loopback host names plus the bound port, `--allow-host`
for reverse proxies, and same-origin requirements for state-changing methods.
That is a documented behaviour change with regression tests. It closes the D0
finding that loopback is unauthenticated.

**Verification (D3/D8):**
- contract tests for each rejection (missing or wrong key, wrong Host, any Origin, SSE without auth);
- a packaged end-to-end test that scans process argv, environment, log files, diagnostics bundles, and every uploaded artifact for the key (the test harness receives the key through a test-only fd) and for any 64-hex run.

## 7. Profile switching (decided)

The sidecar serves one profile: `AppState.session_profile`
(`crates/lightagent/src/serve.rs:739-760`). Switching therefore replaces the
sidecar using **start-new-then-commit**:

1. **Preconditions.** The host asks the current sidecar for active runs
   (`GET /runs?state=active`, added in D3) and pending approvals
   (`GET /approvals`).
   - If any exist, the switch is refused with a dialog listing them. The user may choose
     *Cancel these runs and switch*: the host rejects each pending approval
     explicitly (`approve:false`, so it is recorded, not lost) and cancels each
     run.
   - It then waits up to 10 s for terminal states. If any run is still not terminal, the
     switch is aborted and nothing has changed.
2. **Start new.** The host spawns a second sidecar with `--profile <new>` and
   a fresh key, and waits for its handshake. The old sidecar is still running
   and authoritative.
3. **Failure before commit.** If the handshake fails or times out, the host
   kills the new owned process. The old sidecar, its key, and its streams are
   untouched, and the UI shows the redacted log excerpt. Nothing is persisted.
4. **Commit.**
   - The host switches IPC routing to the new sidecar.
   - It ends every relayed stream of the old one with `stream_closed{reason:"profile_switch"}`.
   - It persists the choice through the new sidecar's typed `profile use` operation (`ProfileStore::set_active`, already atomic: temp file plus rename, `paths.rs:173-185`). The CLI and WebUI then see the same active profile.
   - It stops the old sidecar (§4 shutdown).
5. **Failure after commit.** If persisting fails, the new sidecar stays in
   use and the UI warns that the CLI's active profile was not updated, with a
   retry. Old streams are not resumed across profiles. The UI reloads
   sessions, approvals, tools, and skills from the new sidecar.
6. **Credentials.** Each sidecar has its own key, and the old key dies with
   its process. Provider and MCP secrets are resolved by the sidecar from the
   profile's `SecretRef`s; the host and the renderer never read them.
7. **Locks and concurrent writers.**
   - The active-profile file is written atomically.
   - The CLI may switch profile while the desktop runs. The desktop sidecar stays bound to its `--profile`, and the status bar shows a *CLI active profile differs* notice.
   - Session transcripts get a cross-process advisory lock in D5, shared by the CLI chat and `serve`.
   - Configuration writes get a lock plus a generation/ETag check in D4. A stale `PUT /settings` returns `409`, and the UI reloads.
8. **Remote mode.** Switching is offered only if the remote server's
   capability list includes `profiles.scoped`. Otherwise the control is
   disabled with that reason.

## 8. Configuration, data paths, and credentials

- **Home and paths:**
  - one home, resolved by `LightagentPaths::resolve()` (honours `LIGHTAGENT_HOME`);
  - the desktop adds only `desktop/settings.json` (window state, last remote endpoints, UI preferences), written atomically, and holds no secrets.
- **Secret authority:**
  - provider keys are entered in the desktop, sent host → sidecar → the existing owner-only `~/.lightagent/secrets` store, and referenced by `SecretRef`;
  - the remote-credential vault and identity store are unchanged;
  - the renderer sees only redacted status (*set*, *env reference*, *missing*, or *unreadable*);
  - a secret-store read failure fails closed: the operation errors and no secret is substituted;
  - there is no second credential store.
- **Remote endpoints:** the key is saved through the same secrets store as a
  `SecretRef` named `desktop-remote-<id>`.
- **Migration:** none needed in D1. Any later schema change follows the plan:
  backup, atomic update, recovery, documented compatibility.

## 9. Targets: architecture versus minimum OS versus evidence

| Target | Runner | Build | Packaged launch evidence | Declared minimum | Minimum-OS evidence |
|---|---|---|---|---|---|
| macOS x86_64 (priority) | `macos-15-intel` | spike §3 | macOS 15 only | **10.14 declared, unverified.** Also needs the Safari 14 system-WebKit update | Binary `minos` and `LSMinimumSystemVersion` only. **Blocked:** no hosted runner runs macOS older than 15. No allowed hosted alternative exists |
| macOS arm64 | `macos-15` | spike §3 | macOS 15 only | 11.0 | Binary `minos` only |
| Windows x64 | `windows-latest` | spike §3 | Windows Server image | Windows 10 1803+ (WebView2 preinstalled); older systems need the WebView2 bootstrapper | none beyond the runner image |
| Linux x64 | `ubuntu-22.04` | spike §3 | Xvfb on 22.04 | glibc 2.35 + `libwebkit2gtk-4.1-0` | runner only |

A cross-compiled or deployment-targeted binary is **not** evidence that the
app runs on an older OS. Release notes must say "declared, runtime-tested on
macOS 15 only".

## 10. Screen map

The screen map reuses the WebUI shell (`frontend/src/components/Shell.tsx`)
and its language. A persistent **scope badge** in the top bar shows
`Local · profile <id> · workspace <path>` or
`Remote · <host> · profile <id>`. Unavailable capabilities stay visible with
their reason.

```
┌ Lightagent ──────────────────────────── [Local · default · ~/work] [⌘K] ┐
│ Conversations │  session list  │  transcript · tool calls · approvals     │
│ Runs/Agents   │                │  composer (enabled during a run → steer) │
│ Providers     │                │  [Stop] [Continue] (when paused)         │
│ Tools & MCP   ├────────────────┴──────────────────────────────────────────┤
│ Extensions    │ Settings / Diagnostics: doctor · architecture · config    │
│ Skills        │ keys (typed) · Jev routing report · runtime logs · admin: │
│ Memory        │ Identity · Remote credentials (operator only)             │
│ Profiles      │                                                           │
│ Settings      │ Command palette ⌘K: typed forms per inventory row         │
└───────────────┴───────────────────────────────────────────────────────────┘
```

| Area | Route | Inventory rows (examples) |
|---|---|---|
| Conversations | `/` (existing Agent screen) | `cli.chat*`, `cli.sessions.*`, `slash.new/approve/reject/stop/continue`, `key.steer`, `cli.memory.candidates/promote` |
| Runs/Agents | `/agents` | `cli.workers.*` |
| Providers/Models | `/providers` | `setup.provider.*`, `cli.models`, `config.inference` |
| Tools & MCP | `/tools` (existing) | `cli.tools.list`, `mcp.server.*`, `setup.tools.*`, `slash.reload` |
| Extensions | `/extensions` | `cli.extensions.*`, `extension.*` |
| Skills | `/skills` | `slash.skills`, `skills.roots` |
| Memory/Knowledge | `/memory` | `cli.memory.*`, `cli.rag.*` |
| Profiles/Workspaces | `/profiles` | `cli.profile.*`, `slash.onboard*`, `cli.import.hermes`, `setup.tools.workspace` |
| Settings/Diagnostics | `/settings` (existing), `/diagnostics` | `cli.config.*`, `cli.doctor`, `cli.architecture`, `env.*`, Jev |
| Admin | `/admin/identity`, `/admin/credentials` | `cli.identity.*`, `cli.remote-credential.*` |

The command palette counts toward parity only where it shows a typed form,
calls the real operation, and renders structured output (plan, CLI parity
contract).

## 11. Applicability decisions (D0 → resolved)

| Decision | Resolution | Effect on the denominator |
|---|---|---|
| D1-Q1 `lightagent update` | `cli.update` and `--force`: **not applicable**. Updating would replace a CLI installation the desktop does not own, and the plan forbids automatic updates of installed apps. `--check`: **applicable** as a read-only *Check for updates* in Diagnostics, using the same release feed and installing nothing | +1 (`cli.update.opt.check`) |
| D1-Q2 `/update` | Not applicable; duplicate of the D1-Q1 decision | 0 |
| D1-Q3–Q5 provisioning routes | Not applicable for the bundled runtime: compiled only with the `provisioning-api` feature, which no release build enables (`check.yml` comment). Remote mode lists them as a capability if a remote server advertises them; that is deferred, not parity | 0 |
| D1-Q6 `LIGHTAGENT_INSTALL_ROOT` | Not applicable: read only by the update installer | 0 |

## 12. Shared service extraction and refined slices

- `lightagent-ops` (new crate, D3a) depends on `lightagent-core`, `-store`,
  `-memory`, `-rag`, `-extensions`, and `-tools`. Operations are typed:
  `fn(&OpsContext, Input) -> Result<Output, OpsError>`, where
  `OpsError { category, message }` has stable categories (`not_found`,
  `invalid`, `conflict`, `denied`, `unavailable`, `internal`).
- CLI handlers become printers over these operations, keeping their exact
  current output; the CLI regression tests compare it.
- `lightagent-api` gains routes that call the same operations. No desktop
  feature scrapes CLI output or builds shell strings.

| PR | Slice | Contents | Depends on |
|---|---|---|---|
| #90 | D0 | Audit, inventory, drift check | — |
| D1 | D1 | This document, DESKTOP_ACCEPTANCE.md, DESKTOP_MERGE_ORDER.md, inventory decisions | D0 |
| D2 | D2 | `desktop/` Tauri shell, frontend transport + desktop build mode, operation contract + check, mock sidecar, render/screenshot CI | D1 |
| D3a | D3 | `lightagent-ops` skeleton; `serve --desktop` (key over stdin, stdin lifeline, Host/Origin/Bearer guard, handshake, `--profile`); the real runtime bundled. `GET /runs?state=active` is needed for the profile switch and moves to D4a | D2 |
| D3b | D3 | Host sidecar lifecycle: spawn, readiness, crash and restart, shutdown, single instance, logs | D3a |
| D4a–d | D4 | Profiles; config (lock + ETag); provider, models, key entry, endpoint test; workspaces | D3b |
| D5a–b | D5 | `continue` and steering operations; conversation relay, approvals, reconciliation, session lock | D4 |
| D6a–c | D6 | Extensions; MCP server configuration; skills, onboard, reload | D5 |
| D7a–f | D7 | Memory; RAG; workers assign/revoke; identity; remote credentials; import, doctor, architecture, Jev report, update check | D6 |
| D8 | D8 | Remote mode; WebUI Host/Origin hardening; accessibility, keyboard, regression | D7 |
| D9 | D9 | Installers on all four targets, checksums, SBOM, licenses, vulnerability gate, clean-user smoke | D8 |
| D10 | D10 | Review package and the merge-order validation tree | D9 |

## 13. Threat model

| Asset | Threat | Mitigation | Verified by |
|---|---|---|---|
| Tool execution (files, terminal) | A local process or web page drives the sidecar | §6: key + Host + Origin + loopback; host-only client | D3 contract tests; D8 |
| Launch key | Leak via argv, env, logs, URL, renderer, or artifacts | Stdin delivery; never printed; renderer never receives it | D3/D9 scans for the key and any 64-hex run |
| Provider and MCP secrets | Exposure to the renderer or diagnostics | Backend-only `SecretRef` resolution; redacted status only | D4 tests; diagnostics redaction test |
| Session and config data | Corruption by concurrent CLI and desktop writers | Atomic writes; config lock + ETag (D4); session lock (D5) | Concurrency tests with isolated homes |
| Approvals | Lost or duplicated across reconnect or switch | Explicit reject on switch; reconciliation via `GET /approvals` | D5 end-to-end |
| Remote mode | A failed remote silently falls back to local execution | No fallback; the scope badge; capability negotiation | D8 tests |
| Extensions and skills | Path traversal, untrusted text granting permissions | Existing install validation plus fixtures; permissions stay in the runtime | D6 fixtures |
| Renderer | Remote content or script injection | Bundled assets only; strict CSP; no Node; allowlisted IPC | D2 config test; D8 |
| Updates | Unattended binary replacement | Manual only; no updater plugin | Inventory D1-Q1; D9 |
| Other Lightagent servers | The desktop kills or uses a server it does not own | Owns only its own pid/job; no discovery | D3 lifecycle test |
