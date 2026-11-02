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
| 8 | D4c configuration: one locked, revision-checked writer | https://github.com/dlroqa/Lightagent/pull/101 | `desktop/d4c-config` | `desktop/d4b-profile-ui` | `0e46892` | `0e46892` |
| 9 | Response actions delivery: #98's commits plus their desktop adoption, as one PR | https://github.com/dlroqa/Lightagent/pull/102 | `desktop/d4c1-response-actions` | `desktop/d4c-config` | `a51449c` | `a51449c` |
| 10 | Session writes: one locked writer (#100) | https://github.com/dlroqa/Lightagent/pull/103 | `desktop/d4c2-session-writes` | `desktop/d4c1-response-actions` | `aebd5d6` | `aebd5d6` |
| 11 | D4 acceptance: isolated-home golden files, ordered concurrency, secrets, Jev routing report, Settings conflict | https://github.com/dlroqa/Lightagent/pull/104 | `desktop/d4c3-config-acceptance` | `desktop/d4c2-session-writes` | `205f01b` | `205f01b` |
| 12 | D4d-1 provider credentials: one shared write-only operation; unusable keys fail closed | https://github.com/dlroqa/Lightagent/pull/106 | `desktop/d4d1-credentials` | `desktop/d4c3-config-acceptance` | `6988f6c` | `6988f6c` |
| 13 | D4d-2a endpoint check: explicit, bounded, no redirect with the key | https://github.com/dlroqa/Lightagent/pull/107 | `desktop/d4d2-endpoint-check` | `desktop/d4d1-credentials` | `e6f4506` | `e6f4506` |
| 14 | D4d-2b-1 a provider key goes only to the endpoint it was saved for | https://github.com/dlroqa/Lightagent/pull/108 | `desktop/d4d2b-key-binding` | `desktop/d4d2-endpoint-check` | `8e3919b` | `8e3919b` |
| 15 | D4d-2b-2 saved providers and the model through one shared operation set | https://github.com/dlroqa/Lightagent/pull/109 | `desktop/d4d2b2-saved-providers` | `desktop/d4d2b-key-binding` | `108dac6` | `108dac6` |
| 16 | D4d-2b-3 Providers and models card in Settings | https://github.com/dlroqa/Lightagent/pull/110 | `desktop/d4d2b3-providers-ui` | `desktop/d4d2b2-saved-providers` | `bb76e74` | `bb76e74` (subagents: one live-broker job failed, not re-run; see DESKTOP_PROGRESS.md) |
| 17 | D4d-3 every configured remote credential fails closed | https://github.com/dlroqa/Lightagent/pull/111 | `desktop/d4d3-fail-closed` | `desktop/d4d2b3-providers-ui` | `c15e249` | `c15e249` |
| 18 | D4e the tools workspace through one shared, validated operation | https://github.com/dlroqa/Lightagent/pull/112 | `desktop/d4e-workspaces` | `desktop/d4d3-fail-closed` (`bf5e56d`) | `ec1fc4e` | `ec1fc4e` |
| 19 | D5b one turn at a time per session across every process (https://github.com/dlroqa/Lightagent/issues/105) | https://github.com/dlroqa/Lightagent/pull/113 | `desktop/d5b-run-admission` | `desktop/d4e-workspaces` | `1e657e5` | `1e657e5` |
| 20+ | D5a (deterministic run, relay reconnect, approvals across a profile switch, sidecar crash, R1), then D6 | opened in dependency order, each based on the previous slice's branch | — | — | — | — |

D5b is row 19, before D5a: it uses no D5a operation, and the owner ordered
it next (DESKTOP_CLAUDE_PROMPT.md §5).

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
- **Exact sequence (replaces "merge #98 together with its adoption"):**
  1. D0–D4c merge in order (rows 1–8). Each PR is based on the one before
     it, so after merge *k*, `master` is exactly head *k*'s tested tree. #98
     is not merged at any of these steps.
  2. **Row 9, the response actions delivery,** is a new PR,
     `desktop/d4c1-response-actions`, based on `desktop/d4c-config`. It
     holds:
     - a server-side merge of #98's head, which brings #98's own commits
       with no copies;
     - **one** adoption commit on top: the contract operation
       `sessions.regenerate`, its refusal during a profile switch, and its
       inventory and parity row, re-applied to the counts at D4c.

     It is validated on hosted runners as its own head. When it merges,
     `master` gets #98 and its adoption together, so no `master` state
     between rows 8 and 9 or after row 9 fails a desktop gate. #98 is then
     shown as merged, because its head is in `master`, and it is never
     merged on its own.
  3. **Row 10, the session-write fix** (#100), is based on row 9, because
     it converts #98's regenerate writer and reuses D4c's lock.
  4. D4d and later slices continue from row 10.
- **#99 keeps its evidence role only.** It proves the combination at D4b
  (`b4a61c9`). Its adoption commit is not merged anywhere; row 9
  re-applies the same change to D4c's counts. #99 is closed unmerged once
  row 9 passes.
- **If the owner wants #98 or #100 later,** rows 9 and 10 move to the end
  of the stack: each is rebuilt on the last desktop head, re-validated, and
  approved as an exact head. The rule stays the same: never #98 without
  its adoption.

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
