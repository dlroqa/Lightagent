# Lightagent Desktop: release acceptance, 0.6.0 candidate

**Prepared for the owner's review. Nothing here is approved, merged, tagged
or published.** Pull request https://github.com/dlroqa/Lightagent/pull/164
(`release/v0.6.0`, draft). Revised 2026-11-06: owner-approved unsigned v0.6.0 release path.

| | Commit | Tree |
|---|---|---|
| Base (`master`, the merged Desktop delivery) | `4f6b1f64ca84235f4e2682a2b5f3f65df96e6937` | `16161ba52f6cd4a4d8680708535f67448781cf0d` |
| **Release candidate (the final dry run built it)** | `22ad4fe60be6124d14c3f8b77dc8d9bb4c4b2cde` | `9b764e5c2cf41732402f471b9a94dfbce0bd1300` |
| Code candidate before the unsigned release path | `692dcece27b20590747507ae3f02ee765b0a97fa` | `804fb3606842b9221e73ab3d9403371ed6e68464` |
| Previous code candidate (license fix) | `55f4b96b733a254748369f037a128da08caf94b7` | `dc966a88333f8e362e7252a369b035797d40d894` |
| Earlier code candidate (superseded) | `2e1623d5facc567407dcdd8bfeeffb0f8a8ac445` | `971d5cfb8c0f4e02bf935a61328181eb0a5ea213` |

The commit that adds this revision changes documentation only. No package
input differs from the code candidate.

What changed since `2e1623d`, in order:

| Commit | Change |
|---|---|
| `248262e` | the upstream license texts, `desktop/licenses/upstream/` (git data API; each blob id equals upstream's) |
| `1a5936b` | edit request 193: the notices use them and fail without a license text; upstream re-fetch check; checksums verification |
| `f1bac64` | edit request 194: the signing proofs (quarantined download, Gatekeeper, timestamps) |
| `55f4b96` | edit request 195: the signature step no longer stops at a rejected assessment (it did on `f1bac64`, below) |
| `2a78bcb` | edit request 196: documentation |
| `8f35230` | edit request 199: the owner-approved unsigned path for v0.6.0 only (exact tag list in the gate; that tag publishes unsigned from its tag push only; a manual publishing run on it is refused; unsigned publication of any other tag is refused); one run per ref (concurrency); the publish job refuses an existing release for the tag, verifies SHA256SUMS and drafts with the release notes; the release notes |
| `22ad4fe` | edit request 200: the shape test forbids computing checksums in the publish job, not verifying them (its earlier wording failed check on `8f35230`) |
| `692dcec` | edit request 197: the `signing` input (unsigned by default; a tag push signs); only a signed run's macOS and Windows jobs use the protected `release-signing` environment; a signed run fails without every credential or verification; the .dmg files notarized and stapled; the Windows runtime's signature checked as built and as installed |

## 1. Evidence

| Run | On | Result |
|---|---|---|
| **final release dry run (artifact-only, `signing=unsigned`, `publish` skipped)** | `22ad4fe` | https://github.com/dlroqa/Lightagent/actions/runs/42268737686: **every job green**; every installer installed, launched, reinstalled, uninstalled with user data unchanged; notices identical to the generated ones (the same digests as every run since the license fix); 0 shipped crates without a license text; SHA256SUMS 11 of 11 OK when downloaded; MPL archive `8dca80a1…cc0e8`; afterwards no `v0.6.*` tag and no v0.6.0 release exist, Latest still v0.5.0 |
| release dry run | `8f35230` | 42268526429: cancelled once check failed on the shape test (fixed in `22ad4fe`) |
| unsigned release dry run (artifact-only, `signing=unsigned`, `publish` skipped) | `692dcec` | https://github.com/dlroqa/Lightagent/actions/runs/42221449510: **every job green**: gate (`signing=unsigned`), 4 CLI builds, 4 Desktop targets, licenses and sources, Windows Server 2022, checksums. No job used an environment; none was created |
| check | `692dcec` | https://github.com/dlroqa/Lightagent/actions/runs/42221456703 green, 13/13 (the gate's signing tests included) |
| desktop | `692dcec` | https://github.com/dlroqa/Lightagent/actions/runs/42221456705 green, 12/12 |
| jev evaluation | `692dcec` | https://github.com/dlroqa/Lightagent/actions/runs/42221456725 green |
| subagents | `692dcec` | https://github.com/dlroqa/Lightagent/actions/runs/42221456760 green |
| **signed release dry run** | — | **not run**: `release-signing` does not exist yet (owner action, DESKTOP_SIGNING_HANDOFF.md §1). Dispatching one before would create the environment unprotected |
| release dry run (license fix) | `55f4b96` | 42209236320: every job green; superseded |
| release dry run | `f1bac64` | 42207800464: **failed** in both macOS jobs' "Signature state of what ships". The job shell runs with `-e`, and `spctl` exits 3 on its (expected) rejection of the unsigned app. Fixed in `55f4b96`; the run was cancelled |
| release dry run | `1a5936b` | 42207340531: cancelled before it started (superseded by `f1bac64`) |
| desktop supply chain | `1a5936b` | 42207306326: its supply-chain job passed with the new notices and the upstream re-fetch; the rest of the run was cancelled as superseded |
| release dry run 2 | `2e1623d` | 42160144740: green, superseded (notices without the upstream texts) |

Every job of the unsigned dry run records the commit and tree it built:
`692dcec` / `804fb36`.

### Artifacts of the unsigned dry run (run 42221449510)

| Artifact | What | Kept until |
|---|---|---|
| `lightagent-<label>` (4) | CLI archives | 2027-02-04 (90 days) |
| `release-SHA256SUMS` | the release's checksum file | 2027-02-04 |
| `release-desktop-<label>` (4) | the 6 Desktop installers | 2026-12-06 (30 days) |
| `release-desktop-mpl-sources` | `desktop-mpl-sources-0.6.0.tar.gz` | 2026-12-06 |
| `desktop-licenses` | SBOMs, license inventories, notices per target, MPL sources, audits | 2026-12-06 |
| `desktop-evidence-<label>` (4) and `desktop-evidence-windows-2022` | install, launch, data, uninstall and notices results per installer (each packaged notices file's SHA-256), signature state, notices against the binaries, bundling logs | 2026-12-06 |
| `desktop-smoke-kit-windows-x64` | the Windows installers for the Server 2022 job | 2026-11-13 (7 days) |

The repository is public, so any signed-in GitHub user can download these
artifacts. They are unsigned candidate packages, not a release.

### SHA256SUMS (unsigned dry run)

```
163b69dc2a996ce4286c59af751930788313abbf7a4b827ae486daff4344ee34  lightagent-0.6.0-aarch64-apple-darwin.tar.gz
1fa099ef4a6219db98129b92b9663062db577197af84a34ed33778b7d8b97e98  lightagent-0.6.0-x86_64-apple-darwin.tar.gz
e093870fa6a0e9019de189ae0fd0298205d716a4ab6771dd5dc8d73be33221dc  lightagent-0.6.0-x86_64-pc-windows-msvc.zip
c080d7453a43a71341257d2b603267ec0e99a17e27acc9c42603362d3f413797  lightagent-0.6.0-x86_64-unknown-linux-gnu.tar.gz
9094a7bbc45ff237f76520900379d1cd47ef9c9c4741b156da38fb5c0d191e4d  Lightagent_0.6.0_aarch64.dmg
4434212166fbe31815e14f5c2654ad78daa5b5f78495c789a4d8ef879c1a3f71  Lightagent_0.6.0_amd64.AppImage
4c16f75de8ecd856c8b5ea1e5cdf97b3630887846ba5adf8be0f55b14700d4b1  Lightagent_0.6.0_amd64.deb
090256b7549431dfb06330bbcef5d86128caa2d6ae85e673480611002a5f0fa5  Lightagent_0.6.0_x64-setup.exe
d3bc8f03abc605bac614d1065b66a1edd342f51c045bb8ab6171e1864f45b8fd  Lightagent_0.6.0_x64.dmg
edcbeced0937757b3132c7cce1d48d3394a5fd9d7ebbbeb704872b4071eb9541  Lightagent_0.6.0_x64_en-US.msi
8dca80a16fa2e9f1f7d2d7d12e45564690d0b218012501393706b56284ccc0e8  desktop-mpl-sources-0.6.0.tar.gz
```

A signed run regenerates this file over the signed packages.

The checksums job verified the file (`sha256sum -c`: all 11 OK). It also
unpacked the MPL archive as the release attaches it and verified its own
`SHA256SUMS`, 5 of 5 OK. The CLI archives and installers are not
bit-for-bit reproducible between runs. The MPL archive is: the same digest
in all four dry runs.

### Install, launch and uninstall (every installer, its own runner)

Every step of every installer passed, with no failed step in any result
file. The steps: install; launch (bundled runtime ready, UI loaded, first
API call, no launch key in any process's arguments or environment);
packaged notices; the user data checks; a **same-version reinstall** (the
same installer over its own installation); uninstall. Windows Server 2022
repeated it for both Windows installers. The Linux AppImage lifecycle passed
too. This is not an upgrade test: no earlier Desktop release exists.

## 2. Signing and platform trust

| Platform | State in the unsigned dry run | What a person sees today |
|---|---|---|
| macOS arm64 | only the linker's ad-hoc signature; `codesign --verify --deep --strict` fails; not notarized. **Quarantined download:** disk image rejected ("no usable signature"), app from it rejected | a downloaded app is blocked; it opens only through System Settings › Privacy & Security › Open Anyway, and macOS may call it damaged |
| macOS x64 | no signature; not notarized. Quarantined download: disk image and app rejected | as above |
| Windows x64 | NSIS, MSI, `lightagent-desktop.exe` and the bundled runtime `lightagent-sidecar.exe`, as built and as installed by each installer: `NotSigned`, no signer, no timestamp | SmartScreen "Windows protected your PC" › More info › Run anyway |
| Linux x64 | .deb and AppImage unsigned | no OS prompt; integrity only through SHA256SUMS |

**Signed and unsigned runs are separate.** A manual run is unsigned unless
its `signing` input is `signed`; a tag push always signs. Only a signed
run's macOS and Windows jobs use the protected `release-signing`
environment. A signed run fails when:

- any credential is missing;
- Apple's notary service does not accept a disk image;
- the app, either .dmg, or the app copied out of a quarantined .dmg is not
  accepted, or any of them lacks a stapled ticket;
- on Windows, the NSIS setup, the MSI, the app or the runtime (as built,
  and the app and runtime as installed) is not validly signed and
  timestamped.

The signed dry run has **not** been run: the environment does not exist
yet. Setup, secret names, the proofs and the order of runs:
`DESKTOP_SIGNING_HANDOFF.md`. **macOS stays open until a signed dry run
passes every macOS proof for both .dmg files.**

## 3. Licenses

**Fixed: the 19 permissive components that were published without a license
file** (earlier counted as 18; the table had 19 rows). Each now carries its
upstream license files, from the commit its crates.io version was published
from: the `git.sha1` in the crate's `.cargo_vcs_info.json`, none dirty.

| Components | License | In | Upstream files (repository @ publishing commit) |
|---|---|---|---|
| alloc-stdlib 0.3.0 | BSD-3-Clause | all targets | `LICENSE` (Copyright (c) 2016 Dropbox, Inc.), dropbox/rust-alloc-no-stdlib @ `0a81fd69` |
| block2 0.6.2, objc2 0.6.5, objc2-encode 4.1.0, objc2-foundation 0.3.2 | MIT | macOS | `LICENSE.md`, madsmtm/objc2 @ `b4167b58`, `d7d2fa23`, `8d214f54`, `7b1abfd7`; plus the SPDX MIT text |
| dispatch2 0.3.1, objc2-exception-helper 0.1.1, objc2-app-kit, -core-foundation, -core-graphics, -io-surface, -quartz-core, -web-kit 0.3.2 | Zlib OR Apache-2.0 OR MIT | macOS | `LICENSE.md`, madsmtm/objc2 @ `8852b424`, `8d214f54`, `7b1abfd7`; plus the SPDX Zlib, Apache-2.0 and MIT texts |
| webview2-com 0.39.1, webview2-com-sys 0.39.1 | MIT | Windows | `LICENSE` (Copyright (c) 2021 Bill Avery), wravery/webview2-rs @ `edc2caf8` |
| webview2-com-macros 0.8.1 | MIT | Windows, **build time only** | the same `LICENSE`, @ `dffa41a8` |
| dlopen2 0.8.2 | MIT | Linux | `LICENSE` (Szymon Wieloch, Ahmed Masud, OpenByte), OpenByteDev/dlopen2 @ `cc80e4a0` |
| dlopen2_derive 0.4.3 | MIT | Linux, **build time only** | the same |
| libappindicator-sys 0.9.0 | Apache-2.0 OR MIT | Linux | `LICENSE-APACHE`, `LICENSE-MIT` (qDot), tauri-apps/libappindicator-rs @ `eafd1e36` |

- **objc2 family:** upstream publishes no license text file and **no
  copyright notice** at any of these commits. Its only file, `LICENSE.md`,
  names the licenses. So the notices reproduce `LICENSE.md` verbatim and add
  the standard texts of the named licenses, from the SPDX License List
  v3.29.0 (spdx/license-list-data @ `31ba1a50`). The MIT text keeps SPDX's
  `<year> <copyright holders>` placeholder, and each entry says why. No
  copyright line was invented. `LICENSE.md` also says the crates are derived
  from Apple SDKs. Owner or legal review (item 13).
- **Correction:** libappindicator-sys's crate metadata names *no*
  repository. The earlier record said it named the wrong one; that was a
  misreading of the neighbouring entry. Its repository and commit come from
  its `.cargo_vcs_info.json`.
- **Stored and checked:** the 9 distinct files are in
  `desktop/licenses/upstream/blobs/` under their git blob ids, marked
  `-text`. Each one was created through the git data API and its id came
  back equal to upstream's. `SOURCES.json` records repository, commit, path
  and blob for each crate, plus the shipped-or-build-time distinction the
  generator computes per target.
- **Enforced:** `desktop-sbom.py --notices` fails when a component that is
  not build time only has no license text. It also fails when a stored text
  does not hash to its id, or when a recorded commit differs from the
  crate's own `.cargo_vcs_info.json`.
  `desktop-upstream-licenses.py` fetches every file again from
  `raw.githubusercontent.com` at its commit and compares blob ids. In the
  every licenses job since: 19 crates, 14 upstream files, 9 distinct, no difference.
- **Result, dry runs 42209236320 and 42221449510 (identical notices digests):**

| Target | Without a license text | With upstream texts | Notices SHA-256 (generated = in each installer) |
|---|---|---|---|
| macOS arm64 | 0 | 13 | `37fbf47d72bd9643…` (.dmg) |
| macOS x64 | 0 | 13 | `46d3f3d9e35bd05b…` (.dmg) |
| Windows x64 | 0 | 4 | `f2e854ea02999dd8…` (NSIS and MSI) |
| Linux x64 | 0 | 4 | `9a7446e0e73b5462…` (.deb and AppImage) |

  **In every packaged installer:** the install smoke test found
  THIRD_PARTY_NOTICES.txt and LICENSE.txt in the installed app, identical to
  the generated ones. The digests above also equal the notices generated
  separately by the licenses job. The check against the shipped binaries
  passed on every target: every crate version found in the binaries is in
  the notices at that version, with a license text (0 missing, 0 without).
  Every listed crate, build-time ones included, now has a text.
- **MPL-2.0 Source Code Form:** `desktop-mpl-sources-0.6.0.tar.gz`, SHA-256
  `8dca80a16fa2e9f1f7d2d7d12e45564690d0b218012501393706b56284ccc0e8`,
  156,645 bytes. It holds cssparser 0.37.0, cssparser-macros 0.7.1,
  dtoa-short 0.3.5, option-ext 0.2.0 and selectors 0.38.0, each equal to
  Cargo.lock's checksum, exactly the archives the four notices name, with
  MPL-2.0.txt. Verified in the licenses job and again in the checksums job.
  selectors 0.38.0 still has no license file of its own; it is pointed to the
  identical MPL-2.0 text (cssparser's).

## 4. Platforms: tested versus declared

### Tested (hosted runners, unsigned dry run 42221449510; the same in 42209236320)

| Target | OS actually run | Image | Account |
|---|---|---|---|
| macOS Apple silicon | macOS 15.8.2 (24H232) | `macos-15-arm64` | runner account, seeded home |
| macOS Intel | macOS 15.7.9 (24G830) | `macos-15` (Intel) | runner account, seeded home |
| Windows x64 | Windows Server 2025 (build 26100), every check | `windows-2025-vs2026` | runner account, seeded home |
| Windows x64 | Windows Server 2022 (build 20348), install, launch, data, uninstall | `windows-2022` | runner account, seeded home |
| Linux x64 | Ubuntu 22.04, glibc 2.35 | `ubuntu-22.04` | a new account `la-smoke`, its own home |

### Declared (what the packages claim; not all tested)

| Target | Declared minimum | Where it is declared | Tested at that minimum? |
|---|---|---|---|
| macOS Apple silicon | macOS 11.0 | `LSMinimumSystemVersion` (release matrix `macos_min`) | no, 15.8 only |
| macOS Intel | macOS 10.14 | `LSMinimumSystemVersion` (`tauri.conf.json`, matrix) | no, 15.7 only |
| Windows x64 | **none declared** by the packages (the PE header says 6.0); WebView2 Runtime required, which supports Windows 10 and 11 | — | no client edition; Server 2025 and 2022 only |
| Linux x64 | glibc 2.34; `libwebkit2gtk-4.1-0`, `libgtk-3-0` | the binaries' highest GLIBC symbol; the .deb's Depends | no, glibc 2.35 |

Hosted runners cannot close these gaps: no macOS image older than 15 runs
(macos-14 is refused), hosted Windows is Server only, and there is no glibc
2.34 image. Decision 7.

### Windows keyboard focus after the share panel: PENDING

Hosted runs cannot show the system share panel (desktop 42160150122,
42158154084). The manual check is `DESKTOP_WINDOWS_SHARE_FOCUS_CHECK.md`,
about 10 minutes, keyboard only, on Windows 11 (version and OS build recorded). **It stays pending until someone runs
it.** If focus is lost but the keyboard can recover it, the item is a
documented limitation with the recovery key. If the keyboard cannot recover
it, that is an **unrecoverable keyboard trap and a release blocker**.

## 5. Live evidence and fixtures

| Area | Live | Fixture only |
|---|---|---|
| Jev | one live CLI run before the Desktop existed (jev-live 38311407479, 2026-10-12; protected environment) | every Desktop Jev status and connection check (stubs) |
| Model providers | none (no provider secrets in the repository) | every chat, run, approval and regenerate check (FakeProvider) |
| MCP | none against a third-party service | real processes: stdio and HTTP fixture servers, the stubborn server for lifecycle |
| Update check | read-only against github.com (update-check-live, last on the delivery candidate 42038415262; release-update unchanged since) | loopback release API fixtures |
| Upstream license texts | fetched live from GitHub at their commits in every licenses job | — |
| Qdrant and Infinity | real software in service containers (check), not a hosted service | — |

## 6. Runtime cleanup backstop and accessibility

Unchanged since the earlier record:

- **Backstop:** when the host goes away, the runtime shuts down gracefully.
  The thread that saw standard input close calls `std::process::exit(0)`
  after 5 s. A child process would outlive the runtime only if the graceful
  path were still running at that point. No case reached it. The Job Object
  and process-group teardown is not implemented.
- **Accessibility:** axe reports two moderate rules, `page-has-heading-one`
  and `heading-order`, and nothing serious or critical. Keyboard navigation
  at launch passes on every target.

## 7. Open items, classified

| # | Item | Class | Status |
|---|---|---|---|
| 1 | Permissive notices without license texts (19) | was a release blocker | **resolved**: §3 |
| 2 | macOS not signed or notarized | **accepted by the owner for v0.6.0** (2026-11-06; Gatekeeper behaviour documented in the release notes); a blocker again for later versions unless approved | later versions: a signed dry run passing every macOS proof (handoff §5) |
| 3 | Windows not signed | **accepted by the owner for v0.6.0** (SmartScreen behaviour documented) | handoff §2 for later versions |
| 4 | Linux not signed | documented limitation | optional |
| 5 | Windows focus after the share panel | **pending manual check on Windows 11**; an unrecoverable keyboard trap is a blocker and must be fixed before release | `DESKTOP_WINDOWS_SHARE_FOCUS_CHECK.md` |
| 6 | Declared minimum OS versions untested | documented limitation | decision 7 |
| 7 | Second OS account on macOS and Windows | documented limitation | — |
| 8 | Live Jev, providers and MCP through the Desktop | documented limitation | optional live runs |
| 9 | Runtime exit backstop before child teardown | documented limitation (residual risk) | later |
| 10 | Two moderate axe rules | documented limitation | later |
| 11 | Same-version reinstall only, no upgrade | documented limitation | test from 0.6.0 next |
| 12 | MPL-2.0 sources attachment | resolved in the workflow; done when published | — |
| 13 | objc2 family: no upstream copyright notice, SPDX texts used, Apple SDK statement; build-time entries listed; notice wording | owner or legal review | §3 |
| 14 | Windows bundled runtime's signature not reported | was a gap in the signing proof | **resolved**: reported as built and in each installation (unsigned run 42221449510); a signed run requires it valid and timestamped |
| 15 | Signed dry run, and the negative check that a signed request without credentials fails | not needed for v0.6.0 (unsigned); open for later versions, owner action first | handoff §1 and §6 |

## 8. Proposed version, channel and assets

- **Version 0.6.0, one tag `v0.6.0`** for the CLI and the Desktop.
  `lightagent update` and the Desktop's update check both read
  `releases/latest`.
- **Assets (12):** the 4 CLI archives under their unchanged names, the 6
  installers, `desktop-mpl-sources-0.6.0.tar.gz`, and `SHA256SUMS` covering
  the other 11.
- **Channel: pending (the owner's decision).**
  - (a) A normal release marked Latest. CLI users get the update.
  - (b) A pre-release. `lightagent update` would not offer 0.6.0.
  - (c) A CLI-only 0.6.0. This needs a workflow change.
- The Desktop does not update itself.

## 9. Verdict

**The owner approved (2026-11-06) unsigned macOS and Windows installers for
v0.6.0, publication without marking it Latest, the merge of #164, the tag
and publication, once every gate passes.** The unsigned path is in the
candidate and tested, and the final artifact-only dry run on `22ad4fe` is
green with every artifact verified.

**Publication is held on one gate: the Windows 11 share-panel keyboard
check (item 5) has not been run.** No result has been supplied, and hosted
runners offer no Windows 11. Until it is run and recorded
(`DESKTOP_WINDOWS_SHARE_FOCUS_CHECK.md`), #164 is not merged, no tag is
created and nothing is published. Item 13 (objc2 treatment) remains
documented for review.

## 10. Process record

- **Local `git merge-tree` exception (2026-11-06):** during the read-only
  verification before the #147 and #161 merges, `git merge-tree` was run
  twice on the local backup to simulate the merges. The execution policy
  allows only backup, synchronization, static reading and progress records
  locally, and counts this as validation. It was disclosed at the time,
  changed no repository state, and the merges were verified afterwards on
  GitHub-hosted runners. It has not recurred; every later validation in
  this record ran on GitHub-hosted runners.
- Edit request 185 was void (a stray line). Request 186 was refused by the
  applier and resubmitted as 187. Request 194 introduced the errexit defect
  that 195 fixes; the defect was caught by the `f1bac64` dry run before any
  report relied on it.

## 11. Release path for v0.6.0 (approved unsigned)

- **Allowed:** the tag push of `v0.6.0`. The gate prints `signing=unsigned
  publish=true`, no job uses `release-signing`, and the publish job drafts
  one release with `docs/releases/v0.6.0.md` as its body.
- **Refused, so nothing can race the tag push or widen the approval:**
  - a manual publishing run on `v0.6.0` (signed or unsigned);
  - a manual unsigned publication of any other tag;
  - any listed tag that is not the exact tag;
  - the workflow setting the gate's test override.
- **Guards:** runs on one ref run one at a time, and the publish job refuses
  to draft when the tag already has a release.
- **Unchanged for other versions:** a tag push signs; a manual publishing
  run must be signed.
- **Publication:** the draft is published with `make_latest` false, so
  v0.5.0 stays Latest.
  - `lightagent update` on 0.5.x keeps reporting it is up to date.
  - 0.6.0 reports itself up to date.
  - The release notes say so.

## 12. Remaining before publication

1. The Windows 11 share-panel check (item 5), run and recorded by the
   owner or a tester.
2. Then, under the owner's approval:
   - merge #164 at its reviewed head;
   - verify the master tree and the post-merge checks;
   - tag that commit `v0.6.0`;
   - verify the draft's 12 assets and checksums;
   - publish without Latest;
   - verify the public release.
3. Outside this approval:
   - #163;
   - superseded-draft cleanup;
   - signing setup for later versions (handoff);
   - minimum-OS declarations;
   - the objc2 review.
