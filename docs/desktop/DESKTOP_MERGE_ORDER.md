# Lightagent Desktop — proposed merge order

**Nothing here is approved, and nothing has been merged.** Merging any PR
below needs the repository owner's separate, explicit approval. The approval
covers only the exact head SHA named at that time. If a head changes, the
approval lapses. Tagging, publishing, and releasing are separate permissions.

Each PR is stacked on the previous one, so its diff shows only its own
slice. Before merge approval is requested (D10), the combined tree of every
head, in this order, is validated on GitHub-hosted runners, and its SHA and
tree are recorded here.

| Order | Slice | PR | Branch | Base | Current head | Last tested head (run) |
|---|---|---|---|---|---|---|
| 1 | D0 audit | https://github.com/dlroqa/Lightagent/pull/90 | `desktop/d0-audit` | `master` | see DESKTOP_PROGRESS.md | see DESKTOP_PROGRESS.md |
| 2 | D1 architecture | (this branch's PR) | `desktop/d1-architecture` | `desktop/d0-audit` | — | — |
| 3+ | D2–D9 | to be opened in dependency order | `desktop/d2-…` | previous slice | — | — |

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
