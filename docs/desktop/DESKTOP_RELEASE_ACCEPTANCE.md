# Lightagent Desktop: release acceptance, 0.6.0 candidate

**Prepared for the owner's review. Nothing here is approved, merged, tagged
or published.** Pull request https://github.com/dlroqa/Lightagent/pull/164
(`release/v0.6.0`, draft). Revised 2026-11-06 after the license fix.

| | Commit | Tree |
|---|---|---|
| Base (`master`, the merged Desktop delivery) | `4f6b1f64ca84235f4e2682a2b5f3f65df96e6937` | `16161ba52f6cd4a4d8680708535f67448781cf0d` |
| **Code candidate (the final dry run built it)** | `55f4b96b733a254748369f037a128da08caf94b7` | `dc966a88333f8e362e7252a369b035797d40d894` |
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

## 1. Evidence

| Run | On | Result |
|---|---|---|
| **release dry run (final; artifact-only, `publish` skipped)** | `55f4b96` | https://github.com/dlroqa/Lightagent/actions/runs/42209236320: **every job green**: gate, 4 CLI builds, 4 Desktop targets, licenses and sources, Windows Server 2022, checksums |
| check | `55f4b96` | https://github.com/dlroqa/Lightagent/actions/runs/42209246445 green, 13/13 |
| desktop | `55f4b96` | https://github.com/dlroqa/Lightagent/actions/runs/42209246366 green, 12/12 (supply chain with the upstream re-fetch) |
| jev evaluation | `55f4b96` | https://github.com/dlroqa/Lightagent/actions/runs/42209246372 green |
| subagents | `55f4b96` | https://github.com/dlroqa/Lightagent/actions/runs/42209246343 green (its `fail` and `verify` fixture dispatches fail by design) |
| release dry run | `f1bac64` | 42207800464: **failed** in both macOS jobs' "Signature state of what ships". The job shell runs with `-e`, and `spctl` exits 3 on its (expected) rejection of the unsigned app. Fixed in `55f4b96`; the run was cancelled |
| release dry run | `1a5936b` | 42207340531: cancelled before it started (superseded by `f1bac64`) |
| desktop supply chain | `1a5936b` | 42207306326: its supply-chain job passed with the new notices and the upstream re-fetch; the rest of the run was cancelled as superseded |
| release dry run 2 | `2e1623d` | 42160144740: green, superseded (notices without the upstream texts) |

Every job of the final dry run records the commit and tree it built:
`55f4b96` / `dc966a8`.

### Artifacts of the final dry run

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

### SHA256SUMS (final dry run)

```
8106843e2572b4951e039be775578dcdd648e1820b5193292431b61af80186bf  lightagent-0.6.0-aarch64-apple-darwin.tar.gz
935409f80e6be66f58a671c8e2dd5cc914ea78c3a002e531d2a964534ada594e  lightagent-0.6.0-x86_64-apple-darwin.tar.gz
a99e04b924cd254a4a4a0a8ed73bac1e3c66e3822ad8664d7e041ce0abcc1b09  lightagent-0.6.0-x86_64-pc-windows-msvc.zip
66627e2987954a69aadb7eda672b9466d6b9b326c2b7326eec7b67fccad311ee  lightagent-0.6.0-x86_64-unknown-linux-gnu.tar.gz
3ad9333a0eb8cd651d01e3cf8bbdcc2b8483bf3eff8a35ae74d81c698e9cf3a6  Lightagent_0.6.0_aarch64.dmg
35efbaa86e93881223e20f3fcb8a2160e54b65e2d3ce11a24f626fa90f143f50  Lightagent_0.6.0_amd64.AppImage
361eb87db1aec05fcfb758234a1bd953fa10ddb93822e1ea9f36b479a7081c82  Lightagent_0.6.0_amd64.deb
aead1542e70596818676929787a34e9901a8a11c51f434e9348c615ce863e96b  Lightagent_0.6.0_x64-setup.exe
37a5d99ef05046ccbc3a74d8744ff6b1c074f9b98dad74dbe3f405c1bfe6fbce  Lightagent_0.6.0_x64.dmg
581ef92aa8a33f7652182bbf88613d706656faad316033714e899578b5877291  Lightagent_0.6.0_x64_en-US.msi
8dca80a16fa2e9f1f7d2d7d12e45564690d0b218012501393706b56284ccc0e8  desktop-mpl-sources-0.6.0.tar.gz
```

The checksums job verified the file (`sha256sum -c`: all 11 OK). It also
unpacked the MPL archive as the release attaches it and verified its own
`SHA256SUMS`, 5 of 5 OK. The CLI archives and installers are not
bit-for-bit reproducible between runs. The MPL archive is: the same digest
in all three dry runs.

### Install, launch and uninstall (every installer, its own runner)

Every step of every installer passed, with no failed step in any result
file. The steps: install; launch (bundled runtime ready, UI loaded, first
API call, no launch key in any process's arguments or environment);
packaged notices; the user data checks; a **same-version reinstall** (the
same installer over its own installation); uninstall. Windows Server 2022
repeated it for both Windows installers. The Linux AppImage lifecycle passed
too. This is not an upgrade test: no earlier Desktop release exists.

## 2. Signing and platform trust

| Platform | State in the final dry run | What a person sees today |
|---|---|---|
| macOS arm64 | only the linker's ad-hoc signature; `codesign --verify --deep --strict` fails; not notarized. **Quarantined download:** disk image rejected ("no usable signature"), app from it rejected | a downloaded app is blocked; it opens only through System Settings › Privacy & Security › Open Anyway, and macOS may call it damaged |
| macOS x64 | no signature; not notarized. Quarantined download: disk image and app rejected | as above |
| Windows x64 | NSIS, MSI and `lightagent-desktop.exe`: `NotSigned`, no signer, no timestamp | SmartScreen "Windows protected your PC" › More info › Run anyway |
| Linux x64 | .deb and AppImage unsigned | no OS prompt; integrity only through SHA256SUMS |

The workflow signs only when credentials exist, and none exist. With
credentials, the step "Signature state of what ships" fails unless:

- macOS: the quarantined .dmg and the app copied out of it are both
  accepted, the app as "Notarized Developer ID", with a stapled ticket;
- Windows: every signature is Valid and timestamped.

**What the owner needs to provide, how to store it in a protected
environment, and the full list of proofs:** `DESKTOP_SIGNING_HANDOFF.md`.
**macOS stays open until a credentialed dry run's quarantined-download
check passes for both .dmg files.**

Gap: the Windows bundled runtime (`lightagent.exe`) is not in the build
directory where the step looks, so its signature is not yet reported. The
first credentialed run must check it in the installed folder (handoff §3).

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
  final run: 19 crates, 14 upstream files, 9 distinct, no difference.
- **Result, final dry run:**

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

### Tested (hosted runners, final dry run)

| Target | OS actually run | Image | Account |
|---|---|---|---|
| macOS Apple silicon | macOS 15.8.2 (24H232) | `macos-15-arm64` 20261028.0489.1 | runner account, seeded home |
| macOS Intel | macOS 15.7.9 (24G830) | `macos-15` (Intel) 20260824.0482.1 | runner account, seeded home |
| Windows x64 | Windows Server 2025 (build 26100), every check | `windows-2025-vs2026` 20261103.298.1 | runner account, seeded home |
| Windows x64 | Windows Server 2022 (build 20348), install, launch, data, uninstall | `windows-2022` 20261101.353.1 | runner account, seeded home |
| Linux x64 | Ubuntu 22.04, glibc 2.35 | `ubuntu-22.04` 20261101.347.1 | a new account `la-smoke`, its own home |

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
about 10 minutes on Windows 10 or 11. **It stays pending until someone runs
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
| 1 | Permissive notices without license texts (19) | was a release blocker | **resolved**: §3, final dry run |
| 2 | macOS not signed or notarized | **release blocker for a general release**; acceptable only for an explicitly unsigned preview | open until a credentialed dry run's quarantined-download check passes (handoff) |
| 3 | Windows not signed | documented limitation | handoff §2 |
| 4 | Linux not signed | documented limitation | optional |
| 5 | Windows focus after the share panel | **pending manual check**; a blocker if focus cannot be recovered with the keyboard | `DESKTOP_WINDOWS_SHARE_FOCUS_CHECK.md` |
| 6 | Declared minimum OS versions untested | documented limitation | decision 7 |
| 7 | Second OS account on macOS and Windows | documented limitation | — |
| 8 | Live Jev, providers and MCP through the Desktop | documented limitation | optional live runs |
| 9 | Runtime exit backstop before child teardown | documented limitation (residual risk) | later |
| 10 | Two moderate axe rules | documented limitation | later |
| 11 | Same-version reinstall only, no upgrade | documented limitation | test from 0.6.0 next |
| 12 | MPL-2.0 sources attachment | resolved in the workflow; done when published | — |
| 13 | objc2 family: no upstream copyright notice, SPDX texts used, Apple SDK statement; build-time entries listed; notice wording | owner or legal review | §3 |
| 14 | Windows bundled runtime's signature not reported | gap in the signing proof | the first credentialed run (handoff §3) |

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

**Not ready for a general release.** The license blocker is resolved and
verified in every installer, and the final artifact-only dry run on
`55f4b96` is green on every job. One release blocker remains: **macOS
signing and notarization** (item 2), unless the owner chooses an
explicitly unsigned preview. Item 5 stays pending until someone runs the
manual Windows check. Item 13 needs the owner's or a legal review.

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

## 11. The owner's decisions

1. Channel: (a), (b) or (c) in §8. The version is 0.6.0, with tag `v0.6.0`.
2. macOS: provide the Apple credentials and the protected environment
   (handoff §1 and §4), or accept an unsigned preview and its Gatekeeper
   behaviour.
3. Windows: Azure Artifact Signing, a CA cloud signer, or unsigned with the
   SmartScreen warning (handoff §2).
4. Approve the one-line `environment: release-signing` change once the
   environment exists, then the credentialed dry run.
5. Windows share-panel check: who runs it, on which Windows versions.
6. Minimum OS declarations: keep, narrow to what was tested, or test
   elsewhere.
7. Item 13: accept the objc2 treatment and the notice wording, or obtain a
   legal review.
8. Separately, when ready: merging #164 and #163, the tag, marking Latest,
   publishing, and closing the superseded drafts.
