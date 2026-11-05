# Lightagent Desktop — final acceptance review (row 64)

Owner of this work: Claude Code session `7cd9d8a9` (claim:
https://github.com/dlroqa/Lightagent/pull/161#issuecomment-6656887743),
2026-11-05. Nothing here is merged, tagged or published, and nothing here
is a release approval.

Row 64 is https://github.com/dlroqa/Lightagent/pull/162
(`desktop/d11-acceptance`, stacked on row 63 `a19f0d9`). Its code head is
`4e940fb` (tree `4a0b960`), green on every workflow:

| Workflow | Run | Result |
|---|---|---|
| check (4 platforms, 13 jobs) | https://github.com/dlroqa/Lightagent/actions/runs/42034765893 | success |
| desktop (inventory, host, 4 shells, 4 native, supply chain, host runs) | https://github.com/dlroqa/Lightagent/actions/runs/42034766013 | success |
| jev evaluation | https://github.com/dlroqa/Lightagent/actions/runs/42034765887 | success |
| subagents | https://github.com/dlroqa/Lightagent/actions/runs/42034765932 | success |

Earlier runs of row 64, kept as evidence of what each step found:
42024652486 (`12d14fe`), 42028198409 (`6f9988f`), 42030840705 (`71f5e21`),
42032516273 (`3739abb`). Artifacts (installers, SHA256SUMS, notices, SBOMs,
MPL sources, native and lifecycle reports, screenshots) are kept 30 days.

Parity is unchanged: **222 of 222** applicable rows; row 64 adds no
capability, and the inventory job (drift check, clap tree, self-test)
passed on `4e940fb`.

## 1. Windows native keyboard

**Proven on the actual WebView2 app (gated):** at launch, Win32 keyboard
focus is inside WebView2 (`Chrome_WidgetWin_1 < Chrome_WidgetWin_0 <
WRY_WEBVIEW < Tauri Window`, owned by `msedgewebview2.exe`), the page has
focus, and three real OS Tab presses sent with `SendInput` move focus
composer → "Model for this conversation" → "Summarize this project" →
"Plan next steps". This is the native.py `keyboard` mode on a host of its
own, gated since `3739abb` and passing on `4e940fb`.

**Why the earlier evidence said "no":** the old check sent keys with .NET
`SendKeys` after about twenty scenarios. By then Win32 focus was on the
host's top-level window (`Tauri Window`), not on the WebView, so the
presses never reached the page. JavaScript focus (`element.focus()`) does
not move Win32 focus, and the old check did not look at Win32 focus. That
was an evidence gap, not proof of either outcome.

**Not verified (stated, not waived):** whether keyboard focus returns to
the page after the Windows share flyout. In the full run the share
scenario opens the system flyout (`ShellExperienceHost`); the OS Escape
does not close it, and it is still on screen at the end. After it, Win32
focus stays on `Tauri Window`, and none of these put it back in the page:
bringing the window forward, Tauri's `Webview::set_focus` (WebView2
`MoveFocus`), or a direct Win32 `SetFocus` on the `WRY_WEBVIEW` container
(runs 42028198409, 42030840705). Bringing a second window in front and
returning left the page unable to answer the automation, so that stage
was dropped. Whether a person, who dismisses the flyout by clicking,
meets the same loss of focus is not established. It needs an
interactive Windows check or a reproduction outside the share flyout
before any fix is chosen. Browser-tier keyboard tests (D8d) are separate
evidence and are not counted as native.

## 2. Lifecycle: quitting and crashes, AppImage included

Every check follows the actual processes: the host, the runtime it
started, a real program left running by a run's approved `terminal.run`
call, and the run's stdio MCP server. The MCP fixture
(desktop/e2e/stubborn_mcp.py) ignores the end of its input on purpose, so
only being stopped ends it.

| Case | Native build, all 4 targets (e2e hooks) | Shipped AppImage (Linux, smoke account, OS input) |
|---|---|---|
| Quit with nothing working | host and runtime exit | WM_DELETE_WINDOW: host, runtime and the AppImage's wrapper exit |
| Quit with active work | the close question; Stop runs and quit ends host, runtime, program and MCP server | close request keeps everything and asks; Tab, Enter on Stop runs and quit ends host, runtime and MCP server; wrapper exits |
| Host crash (killed) | runtime exits; no program or MCP server outlives it | SIGKILL of the actual host: runtime exits; MCP server does not outlive it; wrapper exits |

**Product defect found and fixed:** the AppImage's AppRun points
`PYTHONHOME`, `PYTHONPATH`, `LD_LIBRARY_PATH` and `PATH` at the image, and
the host passed them to its runtime, so every program a run started
inherited them. A Python MCP server started from the AppImage died at
start ("No module named 'encodings'", run 42032516273). Fixed in
`4e940fb` (desktop/src-tauri/src/sidecar.rs, `appimage_env_changes`, with
unit tests): inside an AppImage the runtime gets the person's own
environment. Confirmed on the shipped AppImage by the lifecycle check in
run 42034766013. The .deb, macOS and Windows packages were not affected.

**Suspected and disproved:** that a killed host leaves a run's programs
behind. When the host dies, the runtime's lifeline closes, its server
shuts down, and dropping the runtime's tasks ends every child
(`kill_on_drop`). The `std::process::exit` five seconds later is only a
backstop; if it ever fired first, children would outlive the runtime.
That residual risk is recorded; no case reached it.

## 3. Upgrade and user data

**No Lightagent Desktop release has been published** (releases v0.5.0,
v0.4.1 and v0.4.0 carry no desktop installer), so there is no upgrade
from a release to test. The evidence is a **simulated upgrade**:
desktop/e2e/user_data.py seeds the account's own home
(`~/.lightagent`) with one of each kind of data, then each installer is
launched on it, run again over its own installation, launched again and
uninstalled, with a check after each step.

Seeded: a second profile; a setting; a conversation saved by the runtime
with an attachment; a memory; an indexed knowledge source; a provider API
key in the secrets store; a sealed remote credential in the vault (its
key file outside the home). Checked: all 14 files present and
byte-identical, and every kind read back through the CLI and a keyed
`lightagent serve`, including that the provider key is usable and the
vault opens with its key (`remote-credential status`: key configured, one
active grant, none unopenable).

| Installer | After launch | Simulated upgrade | After uninstall |
|---|---|---|---|
| .deb (smoke account) | unchanged | `apt-get install --reinstall` | `apt-get remove`: unchanged |
| AppImage (smoke account) | unchanged | — | file deleted: unchanged |
| .dmg arm64, .dmg x64 | unchanged | app replaced from the image | app removed: unchanged |
| NSIS | unchanged | setup run again `/S` | `uninstall.exe /S`: unchanged |
| MSI | unchanged | `msiexec /fvomus` | `msiexec /x`: unchanged |

No installer owns the home, and no uninstaller removed any of it. Left
behind on Windows after either silent uninstall:
`%LOCALAPPDATA%\dev.lightagent.desktop` (the WebView2 profile: browser
storage such as the conversation-profile choice), not user data in the
home. The NSIS uninstaller's "delete app data" option, shown only
interactively and unticked by default, removes that folder and
`%APPDATA%\dev.lightagent.desktop`; it does not touch `~/.lightagent`
(not exercised interactively).

## 4. Dependency licenses

See [DESKTOP_LICENSES.md](DESKTOP_LICENSES.md). Notices are generated per
target, bundled in every package and verified inside each installed app;
the five MPL-2.0 crates are the exact published archives. Found: selectors
0.38.0 is published without a license file; the notices include the
MPL-2.0 text for it. Distribution is not approved by this review.

## 5. Dependency audit warnings

See [DESKTOP_ADVISORIES.md](DESKTOP_ADVISORIES.md). Vulnerabilities: 0 in
both lockfiles and in the UI's production npm packages. RUSTSEC-2026-0386
is fixed (tokio 1.54.1); RUSTSEC-2024-0370 and RUSTSEC-2024-0429 are
reviewed with their paths and call sites. A warning that is not reviewed
now fails the audit step. This is not an unqualified "audit clean".

## 6. Remaining limitations (each needs the owner's explicit acceptance)

| Item | Kind | State |
|---|---|---|
| Installers unsigned (Authenticode, Apple signing and notarization, deb/AppImage signatures) | product limitation | unchanged |
| Gatekeeper and SmartScreen behaviour | missing verification | not exercised |
| Declared minimum OS (glibc 2.34; macOS 10.14 x64, 11.0 arm64; Windows PE 6.0 + WebView2) | missing verification | declarations only; no older system run |
| A second, new OS account | missing verification on macOS and Windows | Linux only (`la-smoke`) |
| Remote server key | product limitation | named per launch; not saved in the secrets store |
| Remote profile switching | product limitation | refused while remote |
| Live Jev, providers, MCP services | missing verification | stubs and fixtures only |
| Windows keyboard focus after the share flyout | missing verification | §1 above |
| Upgrade from an earlier Desktop release | not applicable yet | no release exists; simulated upgrade only |
| Permissive components published without a license file | owner review | DESKTOP_LICENSES.md §5 |
| Runtime exit backstop before child teardown | residual risk | §2 above |
| Moderate axe findings (heading order, no level-one heading on chat) | product limitation | unchanged |

## Established behaviours, unchanged by row 64

Conversation-scoped profiles do not change the persisted active profile;
account, agent profile and conversation identities stay separate;
profile ids are selectors, not authorization; cloud login and sync remain
a later stage; offline start loads the bundled UI and local data (native
offline mode passed on every target); interrupted work is never replayed;
a failed regeneration keeps the saved answer; host-only secret input keeps
secrets out of renderer state. Row 64 changes only the runtime's
environment inside an AppImage, the test-only driver, tests, packaging
resources and documentation.
