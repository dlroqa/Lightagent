# Lightagent Desktop — merge record

## Completed delivery

| Step | PR | Merge commit | Resulting tree | Post-merge Actions |
|---|---|---|---|---|
| 1 | [#147](https://github.com/dlroqa/Lightagent/pull/147) | `4cada790ceaa2154050cf34450b059480e39b6dd` | `c73557e834e5996c3b37574c5a9e41628b10fed1` | [Check](https://github.com/dlroqa/Lightagent/actions/runs/42148383320), [Jev](https://github.com/dlroqa/Lightagent/actions/runs/42148383289), [Subagents](https://github.com/dlroqa/Lightagent/actions/runs/42148441908) |
| 2 | [#161](https://github.com/dlroqa/Lightagent/pull/161) | `4f6b1f64ca84235f4e2682a2b5f3f65df96e6937` | `16161ba52f6cd4a4d8680708535f67448781cf0d` | [Check](https://github.com/dlroqa/Lightagent/actions/runs/42150133524), [Desktop](https://github.com/dlroqa/Lightagent/actions/runs/42150140810), [Jev](https://github.com/dlroqa/Lightagent/actions/runs/42150133551), [Subagents](https://github.com/dlroqa/Lightagent/actions/runs/42150144071) |

Both trees matched the reviewed simulation and all listed workflows passed. The combined candidate incorporated the Desktop stack, including #162. Remaining draft cleanup requires separate approval. Release readiness is tracked in [DESKTOP_FINAL_ACCEPTANCE.md](DESKTOP_FINAL_ACCEPTANCE.md).

## Historical development sequence

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
| 20 | D5b follow-through: a dropped run lease is free at once | https://github.com/dlroqa/Lightagent/pull/114 | `desktop/d5b2-lease-release` | `desktop/d5b-run-admission` (`db44a46`) | `e5363fc` | `e5363fc` |
| 21 | D5a-1 the host relay resumes a broken stream; runs through the host (R1) | https://github.com/dlroqa/Lightagent/pull/115 | `desktop/d5a1-host-runs` | `desktop/d5b2-lease-release` | `64d632f` | `64d632f` |
| 22 | D5a-2 a turn its runtime did not live to record is Interrupted, never rerun | https://github.com/dlroqa/Lightagent/pull/116 | `desktop/d5a2-interrupted` | `desktop/d5a1-host-runs` | `c6fd6ac` | `c6fd6ac` |
| 23 | D5a-3 time-budget pause and Continue over the API; steering semantics; every OS lock unlocks on drop | https://github.com/dlroqa/Lightagent/pull/117 | `desktop/d5a3-continue` | `desktop/d5a2-interrupted` (`da950d1`) | `0d37299` | `0d37299` |
| 24 | D5a-4 Continue card, Stop in the conversation, browser coverage of the run controls | https://github.com/dlroqa/Lightagent/pull/118 | `desktop/d5a4-run-controls-ui` | `desktop/d5a3-continue` (via merge commit `3bd001c`) | `e164dde` | `e164dde` |
| 25 | D5 close-out: a paused run is found from its session (`active_run`); the lifecycle reviewed | https://github.com/dlroqa/Lightagent/pull/119 | `desktop/d5c-paused-discovery` | `desktop/d5a4-run-controls-ui` (`8dc7d5b`) | `ed0ae47` | `ed0ae47` |
| 26 | D6-1 extension operations shared by the CLI, the API and the desktop | https://github.com/dlroqa/Lightagent/pull/120 | `desktop/d6a-extension-ops` | `desktop/d5c-paused-discovery` (via merge commit `63f1fba`) | `3e558b8` | `3e558b8` |
| 27 | D6-2 the Extensions card on the Tools screen | https://github.com/dlroqa/Lightagent/pull/121 | `desktop/d6b-extensions-ui` | `desktop/d6a-extension-ops` (via merge commit `c89c989`) | `c89c989` | `c89c989` |
| 28 | D6-3 MCP server configuration through one shared operation set | https://github.com/dlroqa/Lightagent/pull/122 | `desktop/d6c-mcp-ops` | `desktop/d6b-extensions-ui` (via merge commit `7ddb855`) | `7ddb855` | `7ddb855` |
| 29 | D6-4 an explicit MCP server check, and the MCP card | https://github.com/dlroqa/Lightagent/pull/123 | `desktop/d6d-mcp-check` | `desktop/d6c-mcp-ops` (via merge commit `f9b5be6`) | `f9b5be6` | `f9b5be6` (macos-arm64 jobs waiting for runner capacity when this was written) |
| 30 | D6-5 effective tool permissions, an MCP-free catalog, skills with their source | https://github.com/dlroqa/Lightagent/pull/124 | `desktop/d6e-tools-skills` | `desktop/d6d-mcp-check` (via merge commit `ef1984d`) | `831d05f` | `831d05f` |
| 31 | D6-6 (new scope) import skills from a repository or folder | https://github.com/dlroqa/Lightagent/pull/125 | `desktop/d6f-skill-import` | `desktop/d6e-tools-skills` (via merge commit `62e87e6`) | `62e87e6` | `62e87e6` |
| 32 | D6 close-out: GET /skills through the host, MCP auth/profile fields, the extensions master switch, every client query in the contract | https://github.com/dlroqa/Lightagent/pull/126 | `desktop/d6g-close-out` | `desktop/d6f-skill-import` (`5c98b3e`) | `c70c2f9` | `c70c2f9` |
| 33 | D6-6 the Git import path against a Git repository built on the runner | https://github.com/dlroqa/Lightagent/pull/127 | `desktop/d6h-git-fixture` | `desktop/d6g-close-out` (`c70c2f9`) | `a753206` | `a753206` |
| 34 | D7-1 memory through one shared operation set for the CLI, the API and the desktop | https://github.com/dlroqa/Lightagent/pull/128 | `desktop/d7a-memory-ops` | `desktop/d6h-git-fixture` (`dd83001`) | `f681980` | `f681980` |
| 35 | D7-2 the Memory screen | https://github.com/dlroqa/Lightagent/pull/129 | `desktop/d7b-memory-ui` | `desktop/d7a-memory-ops` (`f681980`) | `630988a` | `630988a` |
| 36 | D7-3 knowledge (RAG) through one shared operation set; the index written under a lock | https://github.com/dlroqa/Lightagent/pull/130 | `desktop/d7c-knowledge-ops` | `desktop/d7b-memory-ui` (`67ed55e`) | `49109b6` | `49109b6` |
| 37 | D7-4 the Knowledge screen | https://github.com/dlroqa/Lightagent/pull/131 | `desktop/d7d-knowledge-ui` | `desktop/d7c-knowledge-ops` (`49109b6`) | `c65132f` | `c65132f` |
| 38 | D7-5 sessions proved across CLI, API and desktop; Delete asks first; `sessions delete --json` | https://github.com/dlroqa/Lightagent/pull/132 | `desktop/d7e-sessions` | `desktop/d7d-knowledge-ui` (`d5ed51f`) | `c0b5ed3` | `c0b5ed3` |
| 39 | D7-6 worker assignments through one shared operation; Specialists assign and revoke | https://github.com/dlroqa/Lightagent/pull/133 | `desktop/d7f-workers` | `desktop/d7e-sessions` (via merge commit `4afe907`) | `4afe907` | `4afe907` |
| 40 | A profile switch's rejections are applied before its runs are cancelled (D5 race) | https://github.com/dlroqa/Lightagent/pull/134 | `desktop/d5d-switch-rejections` | `desktop/d7f-workers` (via merge commit `627e899`) | `627e899` | `627e899` |
| 41 | D7 evidence gaps: `rag --source`, workers `--worker`/`--profile` | https://github.com/dlroqa/Lightagent/pull/135 | `desktop/d7g-evidence-gaps` | `desktop/d5d-switch-rejections` (`59a4998`) | `7cb8bfa` | `7cb8bfa` |
| 42 | D7 identity through one shared operation | https://github.com/dlroqa/Lightagent/pull/136 | `desktop/d7h-identity-ops` | `desktop/d7g-evidence-gaps` | `a0b82e8` | `a0b82e8` |
| 43 | D7 the Identity screen | https://github.com/dlroqa/Lightagent/pull/137 | `desktop/d7i-identity-ui` | `desktop/d7h-identity-ops` | `2d572cc` | `2d572cc` |
| 44 | D7 remote credentials | https://github.com/dlroqa/Lightagent/pull/138 | `desktop/d7j-remote-credentials` | `desktop/d7i-identity-ui` | `b24d10f` | `b24d10f` |
| 45 | D7 diagnostics and the allowlisted bundle | https://github.com/dlroqa/Lightagent/pull/139 | `desktop/d7k-diagnostics` | `desktop/d7j-remote-credentials` | `443df6e` | `443df6e` |
| 46 | D7 setup: init and import | https://github.com/dlroqa/Lightagent/pull/140 | `desktop/d7l-setup` | `desktop/d7k-diagnostics` | `3fde996` | `3fde996` (subagents: hosted queueing) |
| 47 | D7 configuration sections | https://github.com/dlroqa/Lightagent/pull/141 | `desktop/d7m-config-sections` | `desktop/d7l-setup` | `8f2496b` | `8f2496b` (subagents: hosted queueing) |
| 48 | D7 evidence completion | https://github.com/dlroqa/Lightagent/pull/142 | `desktop/d7n-evidence-completion` | `desktop/d7m-config-sections` (via merge commit `9013dd3`) | `9013dd3` | `9013dd3` (subagents: hosted queueing) |
| 49 | D7 `update --check`, read-only | https://github.com/dlroqa/Lightagent/pull/143 | `desktop/d7o-update-check` | `desktop/d7n-evidence-completion` (via merge commit `2b79e05`) | `2b79e05` | `2b79e05` (subagents: hosted queueing) |
| 50 | D7 the Security card | https://github.com/dlroqa/Lightagent/pull/144 | `desktop/d7p-security` | `desktop/d7o-update-check` (via merge commit `f17115c`) | `f17115c` | `f17115c` (subagents: hosted queueing) |
| 51 | D7 conversations | https://github.com/dlroqa/Lightagent/pull/145 | `desktop/d7q-conversations` | `desktop/d7p-security` (`f17115c`) | `9f4dbc5` | `9f4dbc5` |
| 52 | D7 the setup sections in Settings (shared WebMode) | https://github.com/dlroqa/Lightagent/pull/146 | `desktop/d7r-setup-sections` | `desktop/d7q-conversations` (`9f4dbc5`) | `cc2829d` | `cc2829d` |
| 53 | D7 the audit's evidence gaps (test-only) | https://github.com/dlroqa/Lightagent/pull/148 | `desktop/d7s-evidence-gaps-2` | `desktop/d7r-setup-sections` | `67b8124` | `67b8124` |
| 54 | D7 profile guidance (/onboard) | https://github.com/dlroqa/Lightagent/pull/149 | `desktop/d7t-onboarding` | `desktop/d7s-evidence-gaps-2` | `940b1f5` | `940b1f5` |
| 55 | D7 /help and /reload | https://github.com/dlroqa/Lightagent/pull/150 | `desktop/d7u-help-reload` | `desktop/d7t-onboarding` (via merge commit `46abd19`) | `46abd19` | `46abd19` |
| 56a | D7 Jev status and connection check (stubs) | https://github.com/dlroqa/Lightagent/pull/151 | `desktop/d7v-jev` | `desktop/d7u-help-reload` | `4e1aa34` | `4e1aa34` |
| 56b | D7 LIGHTAGENT_CREDENTIAL_KEY and HERMES_HOME | https://github.com/dlroqa/Lightagent/pull/152 | `desktop/d7w-env-rows` | `desktop/d7v-jev` | `03f4cfe` | `03f4cfe` |
| 56c | D7 Try again on the real backend | https://github.com/dlroqa/Lightagent/pull/153 | `desktop/d7x-regenerate-backend` | `desktop/d7w-env-rows` | `7e4827f` | `7e4827f` |
| 57 | D7 conversation-scoped profile (chat --profile) | https://github.com/dlroqa/Lightagent/pull/154 | `desktop/d7y-conversation-profile` | `desktop/d7x-regenerate-backend` | `703196a` | `703196a` |
| 58 | D8a native WebView verification | https://github.com/dlroqa/Lightagent/pull/155 | `desktop/d8a-native` | `desktop/d7y-conversation-profile` | `0c75cda` | `0c75cda` |
| 59 | D8b host-side secret input | https://github.com/dlroqa/Lightagent/pull/156 | `desktop/d8b-secret-input` | `desktop/d8a-native` (via merge commits) | `a85d6fa` | `71a929c` (row 60 holds it) |
| 60 | D8c remote mode | https://github.com/dlroqa/Lightagent/pull/157 | `desktop/d8c-remote-mode` | `desktop/d8b-secret-input` (via merge commits) | `71a929c` | `71a929c` |
| 61 | D8d keyboard and axe in the browser | https://github.com/dlroqa/Lightagent/pull/159 | `desktop/d8d-accessibility` | `desktop/d8c-remote-mode` (via merge commit) | `248e1db` | see DESKTOP_D8_D9_REPORT.md |
| 62 | D9 installers and supply chain (and the runtime's advisory updates) | https://github.com/dlroqa/Lightagent/pull/158 | `desktop/d9-installers` | `desktop/d8d-accessibility` (via merge commit) | `cf1b2b3` | see DESKTOP_D8_D9_REPORT.md |
| 63 | D10 review: the last rows, the report and this merge strategy | https://github.com/dlroqa/Lightagent/pull/160 | `desktop/d10-review` | `desktop/d9-installers` | `a19f0d9` | `a19f0d9` |
| 64 | Final acceptance: Windows OS keys, lifecycle with a run's processes, the AppImage's environment (fix), user data across reinstall and uninstall, notices and MPL sources, reviewed advisories, tokio 1.54.1 | https://github.com/dlroqa/Lightagent/pull/162 | `desktop/d11-acceptance` | `desktop/d10-review` (`a19f0d9`) | `4e940fb` (code) | `4e940fb`; see DESKTOP_FINAL_ACCEPTANCE.md |

**Outside the stack: #147** (`fix/broker-queue-wait`, base `master`): the
broker's separate queue and run waits. It changes merged subagents code
and the subagents workflow, so it merges before the stack. Rows 46-50's
subagents parents failed only on hosted queueing (their children behaved
as intended), and rows 51-52, whose trees contain them, passed. The D10
candidate sequence validates the combined tree with #147.

**The two intermediate defect windows** (not accepted by the owner): rows
21-39 carry the switch rejection race (fixed at row 40), and rows 15-31
lack `providers.remove`'s `expected_revision` in the contract (fixed at
row 32).

**Proposed strategy (D10, needs the owner's separate approval for each
step): one validated integration merge instead of merging the rows one by
one.** Backporting into rows 15 and 21 would change about 30 heads, each to
be re-validated, and master would still pass through each of them. Instead:

1. Merge #147 (`fix/broker-queue-wait`, base `master`) at its exact head.
   It changes merged subagents code and its workflow, which the stack does
   not touch.
2. Merge one integration branch into `master` with a single merge commit
   (never a fast-forward, never a squash): the final stack head (row 64)
   with #147 merged in. Its combined tree, the exact tree `master` would
   get, is validated beforehand on all four platforms (check, desktop, Jev
   evaluation, subagents) through a draft pull request into `master` that is
   never merged by itself.

So `master` goes from `12582a0` to #147's tested head, then to the
validated combined tree. It never points at a commit inside either window.
Every slice's commits remain in history behind the merge's second parent,
and `git bisect --first-parent` walks only validated states. The stacked
draft PRs are then closed as superseded; #98 shows as merged (its head is in
the tree), #99 and #91 are closed unmerged, as planned below.

Row 27's head is now `5cc2dad` (the uninstall `scope` contract fix, `088`); its last fully tested head is recorded in DESKTOP_PROGRESS.md when its macos-arm64 jobs finish.

D5b (rows 19-20) comes before D5a (rows 21-23) because D5a depends on it,
not the reverse. D5b uses no D5a operation, and the owner ordered it next
(DESKTOP_CLAUDE_PROMPT.md §5). D5a's scenarios rely on D5b: a cancelled run
or a crashed runtime must release the session for the next run (rows 21 and
22 check that it does), and row 22's recovery runs under the run lease.
Rows 21 and 22 are separate PRs because row 22 changes the shared store and
API, while row 21 changes only the desktop host and its tests.

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
