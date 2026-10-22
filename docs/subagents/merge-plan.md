# Merge plan: PR #30 and the specialist-subagent stack

**Prepared, not executed.** Nothing has been merged or retargeted. Each merge is the owner's action.

## What is actually there (inspected 2026-10-22)

- 21 open pull requests form one strictly linear chain. Each head contains its base's head (`compare`: `behind_by = 0` for every pair):

  ```text
  master (506340b)
    ← #30 fix/rag-incomplete-delivery
    ← #31 a ← #32 b ← #33 d ← #34 e ← #35 f ← #36 g ← #37 h ← #38 i ← #39 j ← #40 k
    ← #41 s-sandbox-teardown ← #42 l-selection ← #43 m-isolation ← #44 n-tool-names
    ← #45 o-boundaries ← #46 p-evidence ← #47 q-freshness ← #48 r-eval-reporting
    ← #49 t-release-prep (0.5.0)
  ```

- Repository settings: merge commits, squash and rebase are all allowed. `delete_branch_on_merge` is **off**.
- `master` has **no branch protection and no rulesets**. "Required" checks are a convention, not enforced: `check` (11 jobs), `subagents` (3 jobs) and `jev evaluation`.
- `check.yml` runs on pushes to `master`, so each merge gets a post-merge run.

## The rules this plan follows

1. **Merge commits only.** Squash or rebase would give `master` new commits the next PR does not contain. Its diff against `master` would then show the parent's changes again and could conflict. With a merge commit, the next PR's head still contains `master`, and its diff is only its own commits.
2. **Retarget each PR to `master` only after its base has merged**, and before anything deletes its old base branch. GitHub closes, without merging, an open PR whose base branch is deleted; that is what happened to #28. Retargeting early would put every lower PR's changes into its diff. Retargeting just in time keeps each diff reviewable.
3. **Never delete a stack branch while a PR still targets it.** Delete all of them at the end, or keep them.
4. **Revalidate before each merge.** A merge is safe to make when:
   - the PR's head contains the current `master` (`compare master...<head>`: `behind_by = 0`). The merge commit's tree is then exactly the head's tree, which its own CI already tested at that SHA;
   - and that CI is green at the current head SHA.

   If `master` moved for any other reason (`behind_by > 0`), stop. Update the PR from `master`, re-run `check`, `subagents` and `jev evaluation` on the new head, and only then merge.

## Sequence

For each PR in order, #30, #31, … #49:

1. **Check** the base: it is `master` (only #30 at the start) or its previous base, which has just merged. Then check `gh api repos/dlroqa/Lightagent/compare/master...<head sha>`, which must show `behind_by = 0`.
2. **Retarget** (from #31 on): `gh pr edit <n> --base master`. Then confirm on the PR page that the diff shows only that PR's own commits. Its commit count must match the `ahead_by` recorded for its old base (see the table).
3. **Confirm CI** at the exact head SHA. Retargeting does not re-run `pull_request` workflows, so if anything is not green at that SHA, re-run it with `gh workflow run check.yml --ref <branch>` (and `subagents.yml`, `jev-evaluation.yml`).
4. **Mark ready and merge with a merge commit:** `gh pr ready <n>`, then `gh pr merge <n> --merge`. Do not use `--delete-branch` until the end.
5. **Wait for the post-merge `check` run on `master`** to finish green before the next PR.

Expected own-commit counts, from the 2026-10-22 inspection. Recheck them, because they change only if a branch is updated:

| PR | own commits | PR | own commits | PR | own commits |
| --- | --- | --- | --- | --- | --- |
| #30 | 9 | #37 | 2 | #44 | 1 |
| #31 | 2 | #38 | 3 | #45 | 1 |
| #32 | 5 | #39 | 3 | #46 | 3 |
| #33 | 4 | #40 | 3 | #47 | 1 |
| #34 | 5 | #41 | 2 | #48 | 1 |
| #35 | 2 | #42 | 1 | #49 | 2 |
| #36 | 7 | #43 | 1 | | |

## Points to decide or watch

- **Partial merges are possible.** The order above is the only valid order. Stopping after any PR leaves a consistent `master`, because each head is a tested state. The release commit (#49) belongs last.
- **The pinned-commit Jev workflow** (`jev-selection-pinned.yml`) can be merged earlier, on its own, if a selection measurement is wanted **before** the stack merges. It is a single-file PR against `master`. It only adds a manual workflow, and it is the only thing that has to be on `master` to evaluate an unmerged head (`docs/subagents/jev-selection-review.md`).
- **#30 is not part of the subagent work.** It is the RAG delivery fix that the stack builds on, and it merges first either way.
- **Tagging is separate.** After #49 merges and its post-merge `check` is green:
  - set the date in `CHANGELOG.md` (a small commit);
  - tag `v0.5.0` on `master`;
  - the release workflow drafts the release with generated notes. Paste `docs/releases/v0.5.0.md` above them and publish.

  None of this is authorized yet.
- **Branch cleanup** comes after everything is merged: delete `fix/rag-incomplete-delivery` and `subagents/*`. The `subagents/live-coder-*` scratch branches are already deleted by the live coder checks.
