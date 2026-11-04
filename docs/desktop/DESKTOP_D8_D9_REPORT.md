# Lightagent Desktop — D8 and D9 report

Owner of the work: session `a0fb357c`. Nothing here is merged, tagged or
published. Every result below names the GitHub-hosted run it comes from.

## Where it is

| Row | Slice | PR | Head | Evidence |
|---|---|---|---|---|
| 58 | D8a native WebView verification | #155 | `0c75cda` | desktop [41846351836](https://github.com/dlroqa/Lightagent/actions/runs/41846351836), check [41846351597](https://github.com/dlroqa/Lightagent/actions/runs/41846351597) |
| 59 | D8b host-side secret input | #156 | `a85d6fa` | check [41847402711](https://github.com/dlroqa/Lightagent/actions/runs/41847402711); desktop [41847402725](https://github.com/dlroqa/Lightagent/actions/runs/41847402725) |
| 60 | D8c remote mode | #157 | `71a929c` | desktop [41847409624](https://github.com/dlroqa/Lightagent/actions/runs/41847409624), check [41847409491](https://github.com/dlroqa/Lightagent/actions/runs/41847409491) |
| 61 | D8d keyboard and axe in the browser | #159 | `248e1db` | check [41847413678](https://github.com/dlroqa/Lightagent/actions/runs/41847413678); desktop [41853963632](https://github.com/dlroqa/Lightagent/actions/runs/41853963632) |
| 62 | D9 installers and supply chain | #158 | `cf1b2b3` | check [41848860574](https://github.com/dlroqa/Lightagent/actions/runs/41848860574), desktop [41848860903](https://github.com/dlroqa/Lightagent/actions/runs/41848860903) |

Parity: **222 of 222** applicable rows implemented (rows 59-60 complete it).

## D8: the rendered app on every target

A test-only channel (`--features e2e-hooks`, desktop/src-tauri/src/e2e_driver.rs)
evaluates scripts in the main window's real page; the page answers through
`e2e_reply` with a single-use token the host issued. The shell job greps every
packaged host binary (the bundle, the .app, the unpacked .deb) for the
channel's markers and fails on any; the native job's test build is the
positive control. desktop/e2e/native.py drives the UI by role and accessible
name, as a person sees it, and reads the OS clipboard with the platform's tool.

Covered on every target (full mode, 75 checks at `71a929c`): streaming;
approvals allowed and denied; Stop; Continue after a time-budget pause and a
paused run found again after a reload; a working run shown again after a
reload; every screen opened from the account menu with an axe scan (no
critical violation); the Memory and MCP forms; response actions R2-R5;
keyboard; the minimum window size; a conversation-scoped profile; the profile
switch; closing with a working run (ask, keep working); crash recovery; quit
with a working run; host-side secret input with the real picker; remote mode.
Offline mode: the bundled UI, local conversations and Settings work, and a
model call and an MCP check against unreachable services end with the error
shown. On Linux the offline run also runs inside a network namespace with
only loopback, checked first.

| Target | WebView | OS key input reaches the page | Clipboard (R2) | Read aloud (R3) | Share (R4) | Native picker |
|---|---|---|---|---|---|---|
| macOS x64 (`macos-15-intel`) | WKWebView | yes (System Events) | OS clipboard holds the answer | speech, 223 voices | share sheet opens | NSOpenPanel answered by OS input |
| macOS arm64 (`macos-15`) | WKWebView | yes (System Events) | yes | speech, 223 voices | share sheet opens | NSOpenPanel answered by OS input |
| Windows x64 | WebView2 (Edge 156) | **no**: the window becomes foreground but Tab does not move focus in the page | yes | speech, 3 voices | share sheet opens | common item dialog answered by SendKeys |
| Linux x64 (Xvfb) | WebKitGTK | yes (xdotool) | yes (xclip) | none: the button shows unsupported | none: copies and says so | GTK dialog answered by xdotool |

Product defects the native runs found, fixed in D8a: four Settings inputs
whose label named no input (axe critical); the avatar and the chat list's
section label below 4.5:1; the close question remounting the app so a
conversation in progress lost its run.

### Host-side secret input (the two `--secret-stdin` rows)

The window sends only the non-secret fields. The host opens the platform's
picker, reads the chosen file (at most 16 KiB, UTF-8, one line, at most
8 KiB of secret), and sends the secret to its runtime in the request body,
as the CLI pipes standard input. The API accepts an inline secret only from
the desktop host; any other caller must name a secret file. The contract
marks `secret` host-only, so the window cannot send one through the host.
What persists: the secret, sealed by the vault, exactly as the CLI stores it.
The host keeps no copy after the request and writes nothing. **The chosen
file is never deleted or changed**; the message says so and leaves deleting
it to the person.

### Remote mode (`LIGHTAGENT_API_KEY`)

Settings → Remote server names an address; the key comes from this app's
`LIGHTAGENT_API_KEY` or a key file the host reads. The key stays in the
host's memory for the connection and is overwritten on disconnect; the bundled
runtime never receives it. It is sent only to that origin over https, or over
http to this computer only; no proxy; a redirect is refused, not followed.
The server must describe itself (`GET /api/lightagent/v1/server`) with the same
version and protocol. Connected, the bundled runtime is stopped and a scope
badge names the server; a failing server's requests fail with its error and
nothing runs on this computer. Profile switching and the runtime restart are
refused while remote. Not implemented: saving a remote endpoint's key in the
secrets store (DESKTOP_ARCHITECTURE.md §8 "desktop-remote-<id>"); each launch
names it again. The host's TLS is rustls with ring, as the runtime's.

### HERMES_HOME (owner decision 2026-11-04)

Unchanged behaviour, now pinned by tests (desktop_setup.rs): a folder given
to the import comes first; else `HERMES_HOME`; else `~/.hermes`. A
`HERMES_HOME` that is set but empty, names a missing folder or names a file is
refused ("no Hermes home at <value> (pass --from <dir>)"; for an empty value
the message reads "no Hermes home at  (pass --from <dir>)"), the CLI refuses
the same, nothing is written, `~/.hermes` is not tried instead, and a folder
given still comes first. The empty-value case runs on Unix only: Windows
cannot set an empty variable.

### Browser tier (D8d)

e2e/accessibility.mjs: every screen opened from the account menu with the
keyboard only (Tab, Enter), its controls walked with Tab, and an axe scan;
no critical violation. Moderate findings remain (heading order in the Jev
section, no level-one heading on the chat screen).

## D9: installers and supply chain

| Target | Installer | Install | Launch (ready check) | Uninstall | Signature | Declared minimum (not run on older systems) |
|---|---|---|---|---|---|---|
| Linux x64 | .deb | apt | as a new account `la-smoke`, its own home | apt remove | unsigned | glibc 2.34; libwebkit2gtk-4.1-0, libgtk-3-0 |
| Linux x64 | AppImage | copied, executable | as `la-smoke` | deleted | unsigned | as the .deb |
| Windows x64 | NSIS setup | silent, current user | runner account | silent | NotSigned | PE 6.0; WebView2 required |
| Windows x64 | MSI | silent, machine | runner account | silent | NotSigned | PE 6.0; WebView2 required |
| macOS x64 | .dmg | app copied to /Applications | fresh home | removed | not signed | LSMinimumSystemVersion 10.14 |
| macOS arm64 | .dmg | app copied to /Applications | fresh home | removed | not signed | 11.0 |

SHA-256 of each installer is in each shell job's summary and artifact
(`SHA256SUMS-<target>.txt`). The AppImage's orphan check is recorded, not
gated: run.py ends the AppImage's wrapper, not the host, which keeps running
by design; the .deb launch shows the host's lifeline.

Supply chain (job "desktop supply chain"): per target a CycloneDX 1.5 SBOM
and a license inventory of the crates the host and the bundled runtime link
through normal dependencies and the UI's production npm packages. No
third-party license is unknown; five crates carry MPL-2.0 (cssparser,
cssparser-macros, dtoa-short, option-ext, selectors) for the owner's review.
Vulnerability gate (cargo audit 0.22.2, npm audit): the first run found 15
advisories in Cargo.lock (rustls, rustls-webpki, webpki-root-certs, ctutils,
zstd, zstd-safe, zstd-sys, zip); D9 updates each to its patched release (zip
to 9). Now: Cargo.lock 0, desktop/Cargo.lock 0, the UI 0; warnings only
(tokio 1.53.1 unsound `AsyncFd`, proc-macro-error unmaintained, glib 0.18.5).

## Gaps, stated plainly

- Windows: OS key presses do not reach the page (the window is foreground);
  keyboard use there rests on the browser tier and the page's own handlers.
- A second user account is used only on Linux; macOS and Windows launch as the
  runner account (macOS with a fresh home). Windows resolves the app's data
  folder through the known-folder API, so it is the runner's.
- Installers are unsigned; Gatekeeper and SmartScreen are not exercised.
- Minimum OS versions are declarations; no hosted runner runs older systems.
- Remote mode's key is not persisted (named per launch); remote profile
  switching is refused.
- Live Jev, live providers and live MCP services are not used; stubs only.
