# Release preparation: 0.5.0

**Status: prepared in PR #49, not performed.** The version files say 0.5.0, and the release notes and merge plan are written. Nothing has been merged, tagged, published or deployed.

## Version: 0.5.0 (owner's decision, 2026-10-22)

The repository has no written release policy. What it encodes:

| Source | What it fixes |
| --- | --- |
| `scripts/check-versions.sh` | `Cargo.toml` `[workspace.package] version` and `frontend/package.json` must agree, and a tag must be `v<version>` |
| `.github/workflows/release.yml` | `v*` tags build, smoke-test and **draft** a release with GitHub-generated notes. `CHANGELOG.md` is not read. |
| Commit `647758d` (v0.4.1) | A release commit changes `Cargo.toml`, `Cargo.lock` (workspace members only), `frontend/package.json` and `frontend/package-lock.json` |
| Tags | `v0.4.0` was a structural change (extraction from Lightweight); `v0.4.1` was a patch |

A minor bump fits because this release changes behavior by default. Those changes are listed first in `docs/releases/v0.5.0.md`:
- workers share the lead's budget;
- role profiles are superseded;
- research runs end truthfully;
- egress is hardened;
- patch mutations are re-checked.

The earlier record recommended 0.4.2. That recommendation is superseded by the owner's choice.

## What PR #49 changes

- `Cargo.toml`: `version = "0.5.0"`.
- `Cargo.lock`: the 15 workspace members only. The third-party crates also at 0.4.1 (`jni-sys`, `rustc_version`, …) are untouched. CI's "Cargo.lock is current" step (`cargo metadata --locked`) verifies the lockfile on all three operating systems.
- `frontend/package.json` and `frontend/package-lock.json`: the package's own version, two places in the lockfile.
- `CHANGELOG.md`: `## [0.5.0] - Unreleased (prepared 2026-10-22; set the date when tagging)`, with every entry since v0.4.1. `[Unreleased]` above it is empty.
- `docs/releases/v0.5.0.md`: the release body. It covers behavior changes, opt-in features, configuration, migration, restrictions and verification.
- `docs/subagents/merge-plan.md`: the merge order.

## After authorization (not now)

1. Merge in the order of `merge-plan.md`, ending with #49.
2. Set the release date in `CHANGELOG.md` on `master`, and wait for `check`.
3. `git tag -a v0.5.0 -m "Release 0.5.0"` on that commit, then push the tag. `release.yml` builds and drafts.
4. Paste `docs/releases/v0.5.0.md` above the generated notes, review, then publish with `gh release edit v0.5.0 --draft=false --latest`.
