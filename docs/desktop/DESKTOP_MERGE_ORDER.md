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
| 7 | D4b profile switch and Profiles screen | https://github.com/dlroqa/Lightagent/pull/97 | `desktop/d4b-profile-ui` | `desktop/d4a-profiles` | `5f364e2` | `5f364e2` |
| 8+ | D4c onwards | opened in dependency order, each based on the previous slice's branch | — | — | — | — |

Heads change as slices receive fixes. DESKTOP_PROGRESS.md records every
head with its runs; the D10 review package fixes the final heads and the
validated combined tree.

## Outside the stack: #98 (response actions and in-place regenerate)

https://github.com/dlroqa/Lightagent/pull/98 (`feat/response-action-bar`,
based on `master` `12582a0`) is **not a desktop slice and does not replace
D4c**. D4c and later slices stay stacked on D4b, independent of #98.

- **Head:** `987c6db`. `check` passed on it
  ([41354677426](https://github.com/dlroqa/Lightagent/actions/runs/41354677426)).
- **Merging it with the stack:** a server-side merge with D4b (`5f364e2`)
  is clean (`3b64f25`). On its own, though, #98 fails the desktop drift
  checks: the inventory, the contract against the server, and the contract
  against the client
  ([41354730506](https://github.com/dlroqa/Lightagent/actions/runs/41354730506)).
  The desktop host would refuse its route.
- **The adoption commit:** `b4a61c9` (tree `4d4cedb`) on the evidence branch
  `integration/desktop-d4b-response-actions` (draft
  https://github.com/dlroqa/Lightagent/pull/99, do not merge). It adds:
  - the contract operation `sessions.regenerate`;
  - the host's refusal of it during a profile switch;
  - one not-started inventory and parity row.

  `check`
  ([41356206521](https://github.com/dlroqa/Lightagent/actions/runs/41356206521))
  and `desktop`
  ([41356206421](https://github.com/dlroqa/Lightagent/actions/runs/41356206421))
  passed on it.
- **Proposed placement:** after whichever desktop slices are merged before
  it, and only **together with its adoption**.
  - Once D2 is in `master`, merging #98 alone would leave `master` failing
    the desktop gates.
  - Merging #98 before D2 would need the operation inside D2's contract.
    That rewrites the stack, so it is not proposed.
- **How the merge would go:** the integration branch is rebuilt with a
  server-side merge on the latest stack head merged by then. The adoption
  counts are refreshed, because later slices change the same inventory
  totals. The combined tree is validated on hosted runners, and the owner
  approves that exact head.

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
