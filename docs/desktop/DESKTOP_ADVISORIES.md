# Lightagent Desktop — reviewed dependency advisories

Reviewed 2026-11-05 on row 64 (`4e940fb`), cargo-audit 0.22.2 against
both lockfiles and npm audit against the UI's production packages
(desktop workflow run https://github.com/dlroqa/Lightagent/actions/runs/42034766013).
Vulnerabilities: **0** (Cargo.lock), **0** (desktop/Cargo.lock), **0** (npm).

Warnings are not vulnerabilities, but they are not ignored either: the
audit step now fails on any warning that is not listed in
`REVIEWED_WARNINGS` in scripts/desktop-sbom.py, which points here. Paths
and call sites come from `desktop-sbom.py --advisory-paths`, which scans
the source of every crate each binary is built from at the exact locked
versions.

| Advisory | Crate and version | Kind | Where | Fix available | Status |
|---|---|---|---|---|---|
| RUSTSEC-2026-0386 | tokio 1.53.1 (Cargo.lock) | unsound: `AsyncFd::new` and related constructors violate I/O safety | the bundled runtime | yes: 1.51.6, ≥1.53.3 | **fixed**: Cargo.lock to tokio 1.54.1 (desktop/Cargo.lock already had 1.54.1). Call sites of the affected constructors in the linked crates: 0 |
| RUSTSEC-2024-0370 | proc-macro-error 1.0.4 (desktop/Cargo.lock) | unmaintained | Linux host: lightagent-desktop > gtk 0.18.2 > gtk3-macros 0.18.2 > proc-macro-error | none (no maintained release; alternatives need the GTK 3 bindings to move) | **reviewed**: build time only, a procedural macro; none of its code is in a package |
| RUSTSEC-2024-0429 | glib 0.18.5 (desktop/Cargo.lock) | unsound: `VariantStrIter` iterator implementations | Linux host: lightagent-desktop > gtk 0.18.2 > glib 0.18.5 (in the binary) | only in glib ≥0.20, which the GTK 3 bindings (gtk 0.18, used by Tauri 2 on Linux through webkit2gtk) cannot take | **reviewed**: call sites of `VariantStrIter` / `Variant::array_iter_str` in every crate the host is built from: 0. The affected code is linked but not called |

Residual risk: for glib, a future dependency update could start calling
the affected iterator; the scan runs on every desktop workflow run and
lists any call site. For proc-macro-error, none at run time. Moving off
both depends on Tauri's Linux stack moving off GTK 3.
