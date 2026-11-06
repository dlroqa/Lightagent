# Lightagent Desktop — release acceptance, 0.6.0 candidate

**Prepared for the owner's review. Nothing here is approved, merged, tagged
or published.** Pull request https://github.com/dlroqa/Lightagent/pull/164
(`release/v0.6.0`, draft), 2026-11-06.

| | Commit | Tree |
|---|---|---|
| Base (`master`, the merged Desktop delivery) | `4f6b1f64ca84235f4e2682a2b5f3f65df96e6937` | `16161ba52f6cd4a4d8680708535f67448781cf0d` |
| Code candidate (packages built from it) | `2e1623d5facc567407dcdd8bfeeffb0f8a8ac445` | `971d5cfb8c0f4e02bf935a61328181eb0a5ea213` |

The commit that adds this record changes documentation only; no package
input differs from the code candidate.

## 1. Evidence

| Run | On | Result |
|---|---|---|
| release dry run 2 (artifact-only; `publish` skipped) | `2e1623d` | https://github.com/dlroqa/Lightagent/actions/runs/42160144740: every job green (gate, 4 CLI builds, 4 Desktop targets, licenses and sources, Windows Server 2022, checksums) |
| check | `2e1623d` | https://github.com/dlroqa/Lightagent/actions/runs/42160150385 green |
| desktop | `2e1623d` | https://github.com/dlroqa/Lightagent/actions/runs/42160150122 green, 12/12 |
| jev evaluation | `2e1623d` | https://github.com/dlroqa/Lightagent/actions/runs/42160150274 green |
| subagents | `2e1623d` | https://github.com/dlroqa/Lightagent/actions/runs/42160150146 green (its `fail` and `verify` fixture dispatches fail by design) |
| release dry run 1 (superseded) | `697125b` | https://github.com/dlroqa/Lightagent/actions/runs/42155896523: all packages green; its macos-14 job was refused by GitHub ("The configuration 'macos-14-arm64-us-default' is not supported"), so that job was removed |

Every job records the commit and tree it built; all name `2e1623d` /
`971d5cf`.

### Artifacts of dry run 2

| Artifact | What | Kept until |
|---|---|---|
| `lightagent-<label>` (4) | CLI archives | 2027-02-04 (repository default, 90 days) |
| `release-SHA256SUMS` | the release's checksum file | 2027-02-04 |
| `release-desktop-<label>` (4) | the Desktop installers | 2026-12-06 (30 days) |
| `release-desktop-mpl-sources` | `desktop-mpl-sources-0.6.0.tar.gz` | 2026-12-06 |
| `desktop-licenses` | SBOMs (CycloneDX) and license inventories per target, notices per target, MPL sources, audit reports | 2026-12-06 |
| `desktop-evidence-<label>` (4) and `desktop-evidence-windows-2022` | install, launch, user data, uninstall and notices results per installer, AppImage lifecycle, signature state, notices against the binaries, bundling logs | 2026-12-06 |
| `desktop-smoke-kit-windows-x64` | the Windows installers for the Server 2022 job | 2026-11-13 (7 days) |

The repository is public: any signed-in GitHub user can download these
artifacts. They are unsigned candidate packages, not a release.

### SHA256SUMS (dry run 2)

```
cae31185405ecaed242bbb191ab657205eea79ec47e2f611401e6eea13fbd754  lightagent-0.6.0-aarch64-apple-darwin.tar.gz
04878eaefd2f4845ec123ed34dcee501d430cb90a04002a42d5240bb8f6d47e6  lightagent-0.6.0-x86_64-apple-darwin.tar.gz
652c9d8a37a1bf2bb722dbb55f09ebad51799e90718e357a98a3c91c5fcdcc77  lightagent-0.6.0-x86_64-pc-windows-msvc.zip
6e9ccaf47efd1b37be21645916f23ff31031f28964bc9f449d1187dff10d2041  lightagent-0.6.0-x86_64-unknown-linux-gnu.tar.gz
f35ace1539ce09a327b6f7a1a723911cd01db69c3f32f0fc0294f785ad2ca0e1  Lightagent_0.6.0_aarch64.dmg
7ff7a2d4c4a1e930209c2e2b6a5d0426e4b99bacc9b89cd64774dd2ba799612d  Lightagent_0.6.0_amd64.AppImage
3dade43769dc96bb8e269b6186c989b1bf9bc798b47b7499c72c80f716978570  Lightagent_0.6.0_amd64.deb
5ec903d1a607c27491c512461d2a05dbb2f7a8f9790f1de9752c81f88e6f1880  Lightagent_0.6.0_x64-setup.exe
8dff2a20ec6fa2d62d8550e31feab9e650dd13eea332cf666a0bada7163d1231  Lightagent_0.6.0_x64.dmg
c3c8577af2838b9d8225be9fb1a4abd6db8d3062d391e74e2b3e02d90aca40a5  Lightagent_0.6.0_x64_en-US.msi
8dca80a16fa2e9f1f7d2d7d12e45564690d0b218012501393706b56284ccc0e8  desktop-mpl-sources-0.6.0.tar.gz
```

The packages are not bit-for-bit reproducible between runs (the CLI
archives' digests differ from dry run 1); the MPL-2.0 source archive is
(identical digest in both runs).

### Install, launch and uninstall (every installer, its own runner)

Every step passed (no failed step in any result file): install; launch of
the installed app (bundled runtime ready, UI loaded, first API call, no
launch key in any process's arguments or environment, runtime gone with
the app); the bundled notices identical to the generated ones; the user
data checks; uninstall. On Linux the shipped AppImage's lifecycle (close,
close with a run working, SIGKILL of the host) passed as well.

**User data, labelled exactly:** one of each kind of data is seeded in the
account's own home; after the first launch, after a **same-version
reinstall** (the same installer run again over its own installation:
`apt-get install --reinstall`, the app replaced from the .dmg, NSIS `/S`
again, `msiexec /fvomus`) and after uninstall, all 14 files are unchanged
and every kind reads back, credentials included. This is not an upgrade:
no earlier Desktop release exists. An upgrade from 0.6.0 can first be
tested by the release after it.

## 2. Signing and platform trust

| Platform | State in dry run 2 | What a person sees today |
|---|---|---|
| macOS arm64 | only the linker's ad-hoc signature on the executable; the bundle is not signed (`codesign --verify --deep --strict` fails); not notarized; Gatekeeper rejects it | a downloaded (quarantined) app is blocked; it can be opened only through System Settings › Privacy & Security › Open Anyway, and macOS may call it damaged |
| macOS x64 | no signature; not notarized; Gatekeeper: "rejected source=no usable signature" | as above |
| Windows x64 | NSIS, MSI and the app: Authenticode `NotSigned` | SmartScreen "Windows protected your PC"; More info › Run anyway |
| Linux x64 | .deb and AppImage unsigned | no OS prompt; integrity only through SHA256SUMS over HTTPS |

**Wired, not exercised:** release.yml signs only when the signing secrets
or variables exist (none exist in the repository today), fails on an
incomplete set, and its "Signature state of what ships" step fails when
signing was configured but the result does not verify. The first run with
credentials must be another dry run on this branch.

**What each needs (owner actions; none can be done from here):**

- macOS: an Apple Developer Program membership (an organization account
  needs a D-U-N-S number); a **Developer ID Application** certificate
  exported with its private key as .p12; an App Store Connect API key for
  notarization. Secrets: `APPLE_CERTIFICATE` (the .p12, base64),
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
  (`Developer ID Application: <name> (<team id>)`), `APPLE_API_ISSUER`,
  `APPLE_API_KEY` (the key id), `APPLE_API_KEY_P8` (the key's contents).
  Verification after signing (already in the workflow): codesign strict
  verify, `spctl --assess` accepted as Notarized Developer ID, and the
  ticket stapled to each .dmg.
- Windows: a code-signing certificate whose private key is held in
  hardware or a cloud HSM (required for publicly trusted code-signing
  certificates since June 2023), so a .pfx in a secret is not an option for
  a new certificate. Candidates: Azure Artifact Signing (formerly Trusted
  Signing; eligibility and regions are set by Microsoft), or a CA's cloud
  signing service. Configuration: repository variable
  `WINDOWS_SIGN_COMMAND` (Tauri's `signCommand`, `%1` is the file), optional
  `WINDOWS_SIGN_TOOL` (`crate@version` installed before the build), and the
  service's credentials as secrets (`AZURE_CLIENT_ID`,
  `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID` are passed to the build).
  SmartScreen reputation builds with downloads; a signed installer can
  still warn at first.
- Linux: nothing is required. Optional: a detached GPG signature of
  SHA256SUMS (a key and a secret), or GitHub artifact attestations (these
  write to a public transparency log, which is a publication decision).

## 3. Licenses

- **Notices against what ships:** a new check reads, from the shipped
  binaries themselves, the crate versions whose code they contain (the
  registry paths a release build keeps) and requires each in the bundled
  notices at the same version. Result on every target: **0 missing**
  (host and runtime: macOS arm64 76 and 77 crate versions, macOS x64 75
  and 77, Windows 68 and 75, Linux 67 and 65). The notices also list
  crates that leave no path (no panicking code, or build time only).
- **MPL-2.0 Source Code Form:** `desktop-mpl-sources-0.6.0.tar.gz`
  (SHA-256 `8dca80a1…cc0e8`, 156,645 bytes) holds cssparser 0.37.0,
  cssparser-macros 0.7.1, dtoa-short 0.3.5, option-ext 0.2.0 and selectors
  0.38.0, each equal to Cargo.lock's checksum, with SHA256SUMS,
  MPL-2.0.txt and README.txt. Checked on the runner: its SHA256SUMS
  verifies, the MPL-2.0 text is present, and it holds exactly the archives
  the four targets' notices name. Packed reproducibly. The release workflow
  now attaches it with the installers and lists it in SHA256SUMS, so the
  notices' statement becomes true when a release is published.
- **Permissive components published without a license file** (18; their
  license is named in their metadata, but no copyright line is
  reproduced). Reviewed upstream:

| Components | License | Upstream text |
|---|---|---|
| alloc-stdlib 0.3.0 (all targets) | BSD-3-Clause | dropbox/rust-alloc-no-stdlib, tag `0.3.0`: LICENSE |
| block2 0.6.2, dispatch2 0.3.1, objc2 0.6.5, objc2-encode 4.1.0, objc2-exception-helper 0.1.1 (macOS) | MIT; Zlib OR Apache-2.0 OR MIT | madsmtm/objc2, tags of those versions: LICENSE.md, LICENSE-MIT.txt, LICENSE-APACHE.txt, LICENSE-ZLIB.txt |
| objc2-app-kit, -core-foundation, -core-graphics, -foundation, -io-surface, -quartz-core, -web-kit 0.3.2 (macOS) | Zlib OR Apache-2.0 OR MIT; MIT | madsmtm/objc2, no tag for these versions: the default branch's files |
| webview2-com 0.39.1, webview2-com-macros 0.8.1, webview2-com-sys 0.39.1 (Windows) | MIT | wravery/webview2-rs, no tags: LICENSE on the default branch |
| dlopen2 0.8.2, dlopen2_derive 0.4.3 (Linux) | MIT | OpenByteDev/dlopen2, no tag for 0.8.x: LICENSE on the default branch |
| libappindicator-sys 0.9.0 (Linux) | Apache-2.0 OR MIT | tauri-apps/libappindicator-rs, tag `libappindicator-sys-v0.9.0`: LICENSE-APACHE, LICENSE-MIT. Its crate metadata names the wrong repository (rust-windowing/keyboard-types) |

  MIT and BSD-3-Clause ask that the copyright and permission notice
  accompany copies, binary ones included, so a metadata-only entry does not
  meet them. Every text is available upstream; adding them (at the tag, or
  at a pinned commit where there is none) to the notices generator is a
  small change, not made here.

## 4. Platforms and accounts actually tested

| Target | Declared minimum | Tested by hosted runners |
|---|---|---|
| macOS Apple silicon | 11.0 (LSMinimumSystemVersion) | macOS 15.8.2 only |
| macOS Intel | 10.14 | macOS 15.7.9 only |
| Windows x64 | PE 6.0 header, WebView2 required | Windows Server 2025 (build 26100, every check) and Windows Server 2022 (build 20348, install, launch, data, uninstall) |
| Linux x64 | glibc 2.34 (highest symbol version needed) | Ubuntu 22.04, glibc 2.35 |

Gaps hosted runners cannot close: macOS 11-14 (no image older than 15
runs; macos-14 is refused), Windows 10 and 11 client editions (hosted
Windows is Server only), glibc 2.34 itself (no image; a container check
would be possible but is not done). Decision needed: declare only what was
tested, or keep the declared floors as untested claims, or test them
outside hosted runners.

**Second account:** Linux runs every package as a new account
(`la-smoke`, its own home). macOS and Windows run as the runner account
with a seeded home; a second GUI login is not available on hosted runners.

**Windows keyboard focus after the share flyout: still unverified.** A
mode of its own (native.py `share-focus`) now presses Share with a real
click and closes the flyout with a real Escape. On the hosted runner the
system flyout never became visible or took the foreground (desktop
42160150122: the window stayed active, the page kept focus, and real Tab
presses moved focus after Escape), while the full run's screenshot shows
the window losing activation when Share is pressed. A script click opens
nothing at all (no user activation, desktop 42158154084). So hosted runs
show focus is not lost when no flyout appears, and cannot show what
happens after a real flyout. Needed: one check on an interactive Windows
10 or 11 desktop: Share on an answer, close the flyout with Esc and, again,
by clicking the app; then Tab must move focus in the app.

## 5. Live evidence and fixtures

| Area | Live | Fixture only |
|---|---|---|
| Jev | one live CLI run before the Desktop existed (jev-live 38311407479, 2026-10-12; protected environment) | every Desktop Jev status and connection check (stubs) |
| Model providers | none (no provider secrets in the repository) | every chat, run, approval and regenerate check (FakeProvider) |
| MCP | none against a third-party service | real processes: stdio and HTTP fixture servers, the stubborn server for lifecycle |
| Update check | read-only against github.com (update-check-live, last on the delivery candidate 42038415262; release-update unchanged since) | loopback release API fixtures |
| Qdrant and Infinity | real software in service containers (check), not a hosted service | — |

## 6. Runtime cleanup backstop and accessibility

- Backstop (crates/lightagent/src/serve.rs, `run_desktop`): when the host
  goes away the lifeline cancels, the server shuts down gracefully, and
  dropping the runtime's tasks ends their children (`kill_on_drop`). The
  thread that saw standard input close then calls `std::process::exit(0)`
  after 5 s, which runs no destructors: a child would outlive the runtime
  only if the graceful path were still running after 5 s, for example
  behind a stuck blocking task. No case reached it (host kill on every
  target and on the shipped AppImage, final acceptance §2). The planned
  Job Object (Windows) and process-group teardown (DESKTOP_ARCHITECTURE.md
  §4) is not implemented.
- Accessibility: axe reports two moderate rules on the chat screen,
  `page-has-heading-one` and `heading-order`; no serious or critical
  finding (desktop 42150140810 on master). Keyboard navigation at launch
  passes on every target.

## 7. Open items, classified

| # | Item | Class | Evidence | Proposed resolution |
|---|---|---|---|---|
| 1 | Permissive notices without copyright texts (18 components) | **release blocker** (license compliance) | §3 | add the upstream texts to the notices generator, re-run the dry run |
| 2 | macOS not signed or notarized; Gatekeeper rejects both packages | **release blocker for a general release**; acceptable only for an explicitly unsigned preview | §2 | sign and notarize (owner's Apple account), or the owner accepts an unsigned preview with instructions |
| 3 | Windows not signed | documented limitation (SmartScreen warns; Run anyway works) | §2 | sign when a certificate exists |
| 4 | Linux not signed | documented limitation | §2 | optional signature of SHA256SUMS |
| 5 | Windows focus after the share flyout | documented limitation until the interactive check; a blocker if that check fails | §4 | one manual check |
| 6 | Minimum OS versions untested below the runners' | documented limitation | §4 | owner chooses which floors to declare |
| 7 | Second OS account on macOS and Windows | documented limitation | §4 | — |
| 8 | Live Jev, providers and MCP services through the Desktop | documented limitation | §5 | optional live runs with credentials |
| 9 | Runtime exit backstop before child teardown | documented limitation (residual risk) | §6 | Job Object / process group later |
| 10 | Two moderate axe rules | documented limitation | §6 | heading structure later |
| 11 | Same-version reinstall only, no upgrade | documented limitation (no earlier release) | §1 | test upgrades from 0.6.0 next |
| 12 | MPL-2.0 sources attachment | resolved in the workflow; done when the release is published | §3 | — |
| 13 | Permissive-license notice wording, build-time-only entries | owner review | §3 | — |

## 8. Proposed version, channel and assets

- **Version 0.6.0** (prepared in this candidate). The repository releases
  patch versions for features (0.3.x) and minor versions for significant
  behaviour (0.4.0, 0.5.0); the first Desktop is the latter. One version
  and **one tag `v0.6.0`** for the CLI and the Desktop: `lightagent update`
  and the Desktop's read-only update check both read `releases/latest`, so
  a separate Desktop tag would hide the CLI's update or offer the wrong
  one.
- **Assets (12):** the 4 CLI archives `lightagent-0.6.0-<triple>.tar.gz`
  or `.zip`, unchanged (the updater looks them up by these exact names),
  the 6 installers `Lightagent_0.6.0_<arch>.{dmg,-setup.exe,msi,deb,AppImage}`
  (Tauri's names, distinct from the CLI's), `desktop-mpl-sources-0.6.0.tar.gz`,
  and one `SHA256SUMS` covering all 11.
- **Channel options:** (a) a normal release, marked Latest, with the
  Desktop described as unsigned where it is: CLI users keep receiving
  updates; (b) a GitHub pre-release: `releases/latest` skips it, so
  `lightagent update` would not offer 0.6.0 to CLI users; (c) Desktop held
  back and a CLI-only 0.6.0, which needs a workflow change. Recommended:
  (a), after items 1 and 2 are settled.
- The Desktop does not update itself (the plan forbids automatic updates of
  installed apps); a new version is installed from its installer.

## 9. Verdict

**Not ready for release.** Packaging, installation, data preservation,
notices against the binaries and the MPL-2.0 sources are verified on all
four targets in an artifact-only dry run, and every workflow is green on
the candidate. Two items block a general release: the permissive license
texts (item 1) and macOS signing and notarization (item 2), unless the
owner chooses an explicitly unsigned preview. Item 5 needs one manual
check.

## 10. The owner's decisions

1. Version 0.6.0 and one shared tag `v0.6.0`, or another.
2. Channel: (a), (b) or (c) in §8.
3. macOS: provide signing and notarization credentials, or accept an
   unsigned preview and its Gatekeeper behaviour.
4. Windows: obtain a signing service now, or ship unsigned with the
   SmartScreen warning.
5. Permissive license texts: approve adding the upstream texts to the
   notices (then a new dry run).
6. Windows share-flyout focus: who runs the interactive check, and whether
   a failure blocks.
7. Minimum OS declarations: keep, narrow to what was tested, or test
   elsewhere.
8. Whether build-time-only components stay in the notices, and legal
   review of the notice wording.
9. Separately, when ready: merging #164 (and #163), the tag, and
   publishing the drafted release.
