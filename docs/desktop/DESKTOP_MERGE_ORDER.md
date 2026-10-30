# Lightagent Desktop — proposed merge order

**Nothing here is approved, and nothing has been merged.** Merging any PR
below needs the repository owner's separate, explicit approval. The approval
covers only the exact head SHA named at that time. If a head changes, the
approval lapses. Tagging, publishing, and releasing are separate permissions.

Each PR is stacked on the previous one, so its diff shows only its own
slice. Before merge approval is requested (D10), the combined tree of every
head, in this order, is validated on GitHub-hosted runners, and its SHA and
tree are recorded here.

| Order | Slice | PR | Branch | Base (prerequisite) | Head at this commit's base | Last fully tested head |
|---|---|---|---|---|---|---|
| 1 | D0 audit and drift check | https://github.com/dlroqa/Lightagent/pull/90 | `desktop/d0-audit` | `master` `12582a0` | `bc01426` | `bc01426` |
| 2 | D1 architecture decision | https://github.com/dlroqa/Lightagent/pull/92 | `desktop/d1-architecture` | `desktop/d0-audit` | `350b48b` | `350b48b` |
| 3 | D2 desktop shell and contract | https://github.com/dlroqa/Lightagent/pull/93 | `desktop/d2-shell` | `desktop/d1-architecture` | `003a71c` | `003a71c` |
| 4 | D3a real runtime (`serve --desktop`) | https://github.com/dlroqa/Lightagent/pull/94 | `desktop/d3a-runtime` | `desktop/d2-shell` | `130dde9` | `130dde9` |
| 5 | D3b lifecycle | https://github.com/dlroqa/Lightagent/pull/95 | `desktop/d3b-lifecycle` | `desktop/d3a-runtime` | `1f0ed69` | `1f0ed69` |
| 6 | D4a shared profile operations | https://github.com/dlroqa/Lightagent/pull/96 | `desktop/d4a-profiles` | `desktop/d3b-lifecycle` | `c544d7a` | `c544d7a` |
| 7 | D4b profile switch and Profiles screen | https://github.com/dlroqa/Lightagent/pull/97 | `desktop/d4b-profile-ui` | `desktop/d4a-profiles` | `8c27f93` (review correction follows) | `8c27f93` (before the correction) |
| 8+ | D4c onwards | opened in dependency order, each based on the previous slice's branch | — | — | — | — |

Heads change as slices receive fixes. DESKTOP_PROGRESS.md records every
head with its runs; the D10 review package fixes the final heads and the
validated combined tree.

Not in the merge order:
- https://github.com/dlroqa/Lightagent/pull/91 (`desktop/d1-spike`), the D1
  framework evidence. Close it unmerged once D1 is accepted.
- `tools/source-edit`, the editing tool branch (never merged).

## Recovery notes

- Every desktop slice is additive until D3a: new files, a test-only module,
  and new workflows. Reverting a slice is a plain `git revert` of its merge
  commit.
- From D3a on, the CLI's printed output is pinned by regression tests. A revert
  of an operations-extraction PR restores the previous handlers unchanged.
- No slice migrates user data. Any later migration must ship with a backup and
  a documented rollback before its PR is proposed for merge.
