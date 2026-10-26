# Specialist subagents: build and merge plan

**This is the authoritative merge plan.** `merge-plan.md` now only points here. Nothing described below has been merged, tagged, published, deployed or activated. Each merge, the tag, every paid evaluation and every activation needs its own explicit authorization from the owner.

Recorded 2026-10-22. Every build, format check, lint, test, render and evaluation named here ran on GitHub-hosted Actions runners. Source changes were applied through the GitHub Git Data API. Evidence links have the form `https://github.com/dlroqa/Lightagent/actions/runs/<id>`.

## 1. What was built, and why

Before this work, Lightagent's `agent.delegate` could start a "worker" only by cloning the lead into a profile named after a role. That worker had the lead's full tools, its own full budget, no result contract and no approval path. Research runs could also stop silently. The concrete problems and what each now does:

| Problem (verified at baseline) | Resulting behavior |
| --- | --- |
| A research run that hit a provider EOF, a token limit or its last turn ended as if complete. SSE reconnects duplicated the transcript. `rag.realtime` and embeddings had no deadline. | Truthful stop reasons: `Incomplete` and `MaxTokens`. A reserved tool-less wrap-up turn. SSE event ids with `Last-Event-ID` / `?after=` resume. A 45 s `rag.realtime` deadline and a 30 s embedding timeout. Progress is shown in the transcript (#30). |
| Roles were cloned profiles: a worker inherited everything, nested past `max_depth`, could widen `tool_scope`, and spent its own budget. | Four typed built-in roles, each with a versioned skill, a tool ceiling and an output contract. Each worker gets only its lead's effective tools ∩ the role ceiling ∩ the profile's toolsets ∩ the caller's scope. The depth limit is exact. A worker draws from one budget shared with the lead (#32, #34). |
| A profile could not use its own worker, and a missing assignment silently fell back. | `lightagent workers assign <role> --worker <profile>` assigns a private profile. It is ownership-checked on every turn and tool call. A refusal is reported, never replaced (#32, #37). |
| A worker reaching an approval just failed. | It is parked under a one-time handle bound to the worker, tool, arguments and risk. `agent.resume_worker` always asks the person, and `agent.decline_worker` declines (#38). |
| A coder could not change code safely. | `patch.propose` produces a scoped patch tied to its base. `patch.apply` and `repo.commit` (fast-forward of exactly the base, allowlisted branch only) each need their own approval. Both re-check proposer, run authority, files and base immediately before mutating (#39, #45). |
| A tester had nothing to run tests with, and running them locally is not acceptable. | A GitHub Actions workflow broker: allowlisted workflows, refs and inputs at an exact SHA, correlated runs, logs without the token (#33). |
| Workers ran without fairness or visibility. | An installation-wide pool and a per-profile pool with FIFO admission and a queue timeout. Worker lifecycle stages appear in SSE, the terminal and the Web UI. Readiness is shown per role (`GET /api/lightagent/v1/workers`, the Tools screen) (#34, #40). |
| Untrusted (multi-user) runs could reach generic delegation. | They get only `agent.research`: read-only, egress-guarded, under the profile's own credentials, and off by default (#35). |
| Nothing chose specialists; the lead guessed. | Jev-assisted selection (`off` / `shadow` / `active`) adds six typed questions to the existing classification request. Lightagent's fixed rules turn the answers into an order of at most `selection_max_workers` roles (#42). |
| Tool names with dots broke OpenAI-style endpoints. | `inference.tool_names = "portable"` applies a reversible, collision-checked map per request (#44). |
| Killing bubblewrap within ~1 ms of its start leaked a process. | Wait for bwrap to stop before ending its tree. On the A/B stress test, the base leaked in 15/15 runs and the fix in 0/30 (#41). |
| A tester's or reviewer's result could outlive the change it checked. | Each check is bound to the latest patch by id, content and base. A new patch, file drift or a moved checkout makes it stale, and a dependent check is refused until it runs again (#47). |
| A coder → tester → reviewer cycle used the whole child allowance, so a re-check after a fix was refused, silently. | Bounded repair rounds, separate from concurrency and the initial allowance. A check that cannot run reports **Verification BLOCKED … unverified** (#50, §4). |
| A workflow that skipped an evaluation still looked green. | An unavailable evaluation reports **BLOCKED — NOT EVALUATED**, with `evaluated: false`, in four places (#48). |

### Components touched

- `crates/lightagent-tools`: `builtins/delegate.rs`, `plan.rs`, `budget.rs`, `builtins/patch.rs`, `builtins/actions.rs`, `worker.rs`, `context.rs`, `sandbox*`.
- `crates/lightagent`: `workers*.rs`, `coordinator/selection.rs`, `chat.rs`, `serve.rs`, evaluation harnesses.
- `crates/lightagent-core`: config, `workers`, the provider tool-name map.
- `crates/lightagent-api`: the readiness endpoint and SSE ids.
- Web UI: the transcript, worker rows and the Tools card.
- Workflows: `subagents.yml`, `subagent-fixture.yml`, `sandbox-stress.yml`, `jev-selection-live.yml`, `jev-selection-pinned.yml`.
- Docs and evaluation sets: `docs/subagents/*`, `evaluation/subagents/*`.
- Unchanged across the whole stack: `crates/lightagent/src/jev.rs` and `crates/lightagent-jev` (`git diff 506340b dd4d47f` on those paths is empty). Lightweight `Auto` routing is therefore intact.

## 2. The four roles

| Role | Tools (ceiling) | Operational evidence | Competence evidence |
| --- | --- | --- | --- |
| researcher | web search and fetch (egress-guarded), RAG, memory read | Scripted model with real fetches to loopback fixtures; sources and citations; unverified citations flagged; private egress refused | **none** (BLOCKED) |
| coder | workspace read, `patch.propose` (scope- and base-bound) | Scripted model with a real workspace; a live commit verified on Actions (new SHA passes, base fails) | **none** (BLOCKED) |
| reviewer | workspace read only | Write attempts refused; findings bound to the change | **none** (BLOCKED) |
| tester | `actions.dispatch`, `status`, `logs`, `cancel` through the broker | Live broker against real hosted runs, pass and fail, on every head | **none** (BLOCKED) |

Authority:
- Every role runs under the **requesting profile's** authority and approval policy, never a looser one.
- Workers never approve anything themselves, and a plan never grants anything.
- Role model routes (`subagents.role_models`) take effect only for models in `approved_models`.

## 3. Jev specialist selection

- **Requests.** One classification request per root task (or terminal turn) carries 11 typed questions: the 5 existing ones plus 6 selection questions. It is counted once against the per-run Jev budget. Workers never classify, and no tool call triggers classification.
- **What Jev answers.** Jev answers only probabilities and one fixed choice:
  - whether delegation materially helps;
  - each role's usefulness;
  - the structure: `single`, `independent` or `sequential`.

  Complexity comes from the existing classification.
- **What Jev never supplies.** It never gives a worker identity, command, endpoint, credential or grant.
- **Lightagent decides** with fixed rules (`coordinator::selection::decide`):
  - simple tasks and single-step searches stay with the lead;
  - at most `selection_max_workers` (1–3, default 2) roles, never all four, and never more than `max_workers_per_run`;
  - research before code unless confidently independent;
  - the tester after the coder; the reviewer after what it reviews;
  - each role must be **ready** (`workers::readiness`: ownership, assignment, skills, model route, tool policy, trust). A refused role is reported in `restricted` and never replaced, and a role that depends on it is dropped too.
- **Modes.**
  - `off` (default) asks nothing.
  - `shadow` records the plan it would build and applies nothing.
  - `active` gives the plan to the lead, only for profiles in `selection_profiles`. Classification must be on for that profile.
- **Fallback.** Unavailable Jev, malformed answers, low confidence (below `selection_threshold_percent`, default 70), a small budget or an untrusted run without `agent.research` all fall back to the existing path, with a fallback code.
- **Enforcement.** `agent.delegate` enforces the order: a planned role waits for the roles before it. Checks are bound to the exact change (§4).

Tests: `coordinator::selection` covers rules, the one request, malformed answers, modes, untrusted research-only, and refused roles that are never replaced. `a_planned_tester_waits_for_the_coder…` uses the real `agent.delegate`.

## 4. Freshness, the child allowance and bounded repair

### Freshness (#47)

A planned tester or reviewer checks the run's latest patch: id, content and base, plus the commit it became. Its brief names that change, and a completed result is recorded against it. A new patch, files changed outside the patch, or a moved checkout makes earlier checks stale. A dependent check of a stale result is refused (`the tester result is for an earlier change …`).

### Three separate limits (#50)

| Limit | Key | Default | Bounds |
| --- | --- | --- | --- |
| Concurrency | `max_active_workers` / `_per_profile` | 4 / 1 | how many workers run **at once** (unchanged) |
| Initial allowance | `max_children_per_agent` | 3 | how many workers one execution starts **before any repair** (1–3, unchanged) |
| Repair | `max_repair_rounds` | 1 | extra starts **only** for a lead with a specialist plan (0–2) |
| Run total | `max_workers_per_run` | 6 | every worker in the tree, with the shared turns, tool calls and time (unchanged) |

A repair round, in `plan.rs` `admit_repair`:
- the coder may start past the allowance only when a check **completed on its current change**, at most once per round;
- then each planned check may start once per round, **never on a change it already verified** (the duplicate is refused with "reuse its result").

A round therefore cannot loop: the coder cannot revise a change nobody checked, and no check repeats. The default cycle plus one repair round uses exactly 6 workers, which is `max_workers_per_run`.

Without a plan (selection `off` or `shadow`, or an unlisted profile) there is no repair allowance and no change binding, so the old limit applies exactly as before. The refusal now says why.

### When verification cannot run

A check refused past the limits returns:

> Verification BLOCKED: the reviewer cannot check patch … within this run's limits. Report this change as unverified; an earlier result was for a different change and must not be reused.

It also emits the progress event `verification.blocked`. The plan instruction tells the lead never to present an earlier check as covering the final change.

### Operation boundaries

- `agent.delegate` enforces order and freshness for every dependent check.
- `patch.apply` and `repo.commit` re-check, immediately before mutating: proposer, assignment, run authority, files, base, branch allowlist and fast-forward.
- No operation in Lightagent merges or promotes a change on the strength of a check. The tester verifies the commit *after* `repo.commit`. So there is no further operation that consumes test evidence, and none is claimed.

Tests:
- `plan.rs`: `repairs_are_bounded_and_never_repeat_a_check`, `checks_are_bound_to_one_change_and_go_stale_with_it`;
- `coder_workflow.rs`:
  - `dependent_checks_are_bound_to_the_change_and_go_stale_with_it` runs a full cycle plus a re-check cycle under the production defaults;
  - `repairs_are_bounded_and_a_check_that_cannot_run_leaves_the_change_unverified`;
  - `without_a_plan_the_allowance_is_the_limit`.

## 5. Isolation, approvals and freshness review

Two owners' leads (Alice and Bob, plus Carol with no grant) delegate through the real `workers::delegation` and `agent.delegate` paths (`crates/lightagent/src/workers/isolation.rs`, #43 and #47):

| Surface | Test |
| --- | --- |
| Memory, sessions (another profile's exact session id) | `a_workers_memory_and_sessions_are_its_own_leads_only` |
| RAG index and remote Qdrant namespace | `a_workers_rag_search_reads_only_its_leads_index`, `a_workers_remote_rag_queries_carry_only_its_leads_namespace` |
| Vault credentials, revoked mid-run | `a_workers_search_presents_only_its_own_profiles_vault_grant` |
| Skills and assignments: guessed ids, tampered assignment, legacy arguments | `another_owners_profile_is_never_a_worker_and_is_never_named` |
| Workspaces (absolute path, `../`) | `a_workers_workspace_is_its_leads_and_stays_confined` |
| Patches and parked-approval handles used by the wrong caller | `patches_and_parked_approvals_belong_to_the_run_that_made_them` |
| Ownership change during execution | `an_ownership_change_mid_run_stops_the_assigned_worker_and_withholds_it` |
| Credential or run revocation during execution | `a_revoked_run_stops_its_worker_mid_run_and_withholds_it` |
| Sessions and SSE events across profiles | the profile-scoped API tests (API layer; not re-run through workers) |

**Child approvals.** A handle is one-time and bound to the exact worker, tool, arguments and risk. Resume re-checks expiry, cancellation, run authorization and assignment validity. Replay, wrong caller, expiry, decline, cancellation and revocation are tested in `worker_approvals.rs`, `isolation.rs` and `coder_workflow.rs`.

**Before code mutation**, these are re-checked: repository broker, patch identity (patch ids are never reused, and the content id is shown), write scope, base and file state, branch allowlist, approval, assignment, proposer and ownership.

**Invalidation.** Changed patches, files or bases invalidate dependent tester and reviewer evidence (§4).

## 6. Provider tool names, Web provisioning, defaults

- **Tool names** (#44). OpenAI Chat Completions (`^[a-zA-Z0-9_-]{1,64}$`), Anthropic and Gemini all reject dots.
  - `portable` maps declarations, history, tool results, follow-up and resumed requests, and streamed calls.
  - Unknown names are refused, and ambiguous maps are refused before sending.
  - The default stays `native`.
- **Web provisioning** (#40, `docs/integrations/web/subagent-presets-handoff.md`).
  - No provisioning code changed, and LA-WEB-PROVISIONING/1 stays at r4: its fixtures are green on every head.
  - A single operator default preset needs no wire change. r5 is needed only when the website chooses between several grantable presets per user.
  - Website preset selection is **not implemented** in the Web repository, and is not claimed.
- **Defaults** (nothing activates by itself):
  - `subagents.selection = "off"`;
  - `inference.tool_names = "native"`;
  - `subagents.remote_research = false`;
  - `subagents.max_repair_rounds = 1` (inert without an active plan);
  - multi-user mode, provisioning and the sandbox backend are not activated.
- **Migration.** Existing configs load unchanged. Generated role clones are superseded, while customized ones are kept as `legacy_profile`.
- **Rollback switches:**
  - `subagents.resolution = "legacy_profiles"` (operator runs);
  - `subagents.selection = "off"`;
  - `subagents.max_repair_rounds = 0`;
  - `inference.tool_names = "native"`.
- **Restrictions.**
  - No tenant execution sandbox: coder, reviewer and tester stay `requires_isolation` for untrusted users.
  - Workers run one at a time within a lead run.

## 7. Prepared 0.5.0 release

Prepared in #49 and amended by #50: the version is 0.5.0 in the four version files, the `CHANGELOG.md` section `[0.5.0] - Unreleased`, and `docs/releases/v0.5.0.md`. Not tagged. The tag, the changelog date and publication are separate decisions, made after the final merge and its post-merge checks.

## 8. The pull requests

`master` = `506340b8…` (merge of #29), tree `bf5fdee4…`. The stack is **21 PRs, #30 to #50**, one strictly linear chain. Every head contains its base's head (`behind_by = 0`), and every PR's GitHub test-merge tree equals its head tree (inspected 2026-10-22).

**Inventory note.** Earlier records said "21 PRs" while listing #30–#49, which is 20. That was a miscount. With #50 (added in this pass) the stack is 21. `jev-selection-pinned.yml` is additionally proposed to `master` on its own as draft [#51](https://github.com/dlroqa/Lightagent/pull/51) (branch `jev/selection-pinned-workflow`, head `e4a306c6d52603976aebadafc9c0c9d1748d77e4`, parent `506340b`). It is independent of the stack and is not one of the 21 (§9). Its checks: check 39714022711 ✓ (the macOS job passed on its one re-run after the known flake below), jev evaluation 39714022737 ✓, pinned `prepare` 39714022743 ✓ ("nothing to export" on `master`'s code; `send` and `score` skipped).

Every PR is a draft, targets the branch of the PR below it, and is unmerged. "Own" is the number of its own commits (`ahead_by` against its base). The check columns give run ids at the exact head: `check` has 11 jobs, `subagents` 3, `jev` is jev evaluation, and ✓ means success.

| # | Branch → base | Head | Own | Concrete change | check | subagents | jev | Other |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [30](https://github.com/dlroqa/Lightagent/pull/30) | `fix/rag-incomplete-delivery` → `master` | `1c12cf4c9d30a871b5dbaf5fb094c891c60bb98f` | 9 | RAG/output delivery: truthful stops, wrap-up turn, SSE ids/resume, bounded `rag.realtime`/embeddings, transcript progress | 39653621567 ✓ | — | 39653621493 ✓ | |
| [31](https://github.com/dlroqa/Lightagent/pull/31) | `subagents/a-baseline` → #30 | `b2c9a46f4bde390088385251c561ec29314026c0` | 2 | Baseline record; seeded (ignored) regression tests for each gap | 39694595355 ✓ | 39694595437 ✓ | 39694595439 ✓ | |
| [32](https://github.com/dlroqa/Lightagent/pull/32) | `subagents/b-catalog` → #31 | `4fdfc629d4fe898829978f7008a3173bde9ec544` | 5 | Typed role catalog, skills, ceilings; private assignments; scoped worker runtime; depth and scope fixes | 39694598100 ✓ | 39694598227 ✓ | 39694598086 ✓ | |
| [33](https://github.com/dlroqa/Lightagent/pull/33) | `subagents/d-broker` → #32 | `521d914800bd9b3207250e5e9cfe2715c2979037` | 4 | Tester GitHub Actions broker; `subagent-fixture.yml`; live broker job | 39694600128 ✓ | 39694600072 ✓ | 39694600142 ✓ | fixture 39694600104 ✓ |
| [34](https://github.com/dlroqa/Lightagent/pull/34) | `subagents/e-budget-events` → #33 | `7c78393b1ee52b91eef0c264a292e3b16948344a` | 5 | Shared tree budget, FIFO admission pools, worker lifecycle in SSE/UI | 39694603535 ✓ | 39694603536 ✓ | 39694603553 ✓ | |
| [35](https://github.com/dlroqa/Lightagent/pull/35) | `subagents/f-remote-research` → #34 | `67ab0f4fb93c4517e1d52cf0a4d7dbf51ac6078f` | 2 | `agent.research` for untrusted runs (off by default); capability presets | 39694608310 ✓ | 39694608374 ✓ | 39694608318 ✓ | |
| [36](https://github.com/dlroqa/Lightagent/pull/36) | `subagents/g-evidence` → #35 | `e4ba72ab11d5b88e516b1f508c2d8217d6eff1a2` | 7 | Role evaluation set, live role harness (BLOCKED without endpoint), docs | 39694609839 ✓ | 39694609845 ✓ | 39694609827 ✓ | |
| [37](https://github.com/dlroqa/Lightagent/pull/37) | `subagents/h-resolution` → #36 | `e1dc459a2ed60023d96a23d30334c721dacebed8` | 2 | Resolution checks: model routes, approval floor, limits; legacy refused to untrusted runs | 39694612967 ✓ | 39694613011 ✓ | 39694612827 ✓ | |
| [38](https://github.com/dlroqa/Lightagent/pull/38) | `subagents/i-approvals` → #37 | `7ea119a9211d3d50c09d0231fcb4803ebcb671ad` | 3 | Parked worker approvals: `agent.resume_worker` / `agent.decline_worker` | 39694616834 ✓ | 39694617020 ✓ | 39694616818 ✓ | |
| [39](https://github.com/dlroqa/Lightagent/pull/39) | `subagents/j-coder` → #38 | `e63be232e58e24f7b81321098671ae2f441b3339` | 3 | `patch.propose`/`apply`, `repo.commit` (fast-forward, allowlisted); live coder verify | 39694619276 ✓ | 39694619307 ✓ | 39694619298 ✓ | fixture 39694619275 ✓ |
| [40](https://github.com/dlroqa/Lightagent/pull/40) | `subagents/k-handoff` → #39 | `30f5b2b91a5652c4180bfb950b29b16400639826` | 3 | Readiness API and Tools card; live-eval proposal; Web handoff; acceptance matrix; changelog | 39697856690 ✓ | 39697856501 ✓ | 39697856655 ✓ | |
| [41](https://github.com/dlroqa/Lightagent/pull/41) | `subagents/s-sandbox-teardown` → #40 | `2fdacb1d11a4db09385b932236cc93791df4ba9f` | 2 | bwrap startup-cancellation teardown fix | 39701803527 ✓ | 39701803517 ✓ | 39701803540 ✓ | sandbox stress 39701803597 ✓ (0/10) |
| [42](https://github.com/dlroqa/Lightagent/pull/42) | `subagents/l-selection` → #41 | `ea898cefb38debc6a03a4d009f1cd564be9c39f5` | 1 | Jev specialist selection (off/shadow/active), plan order in `agent.delegate` | 39701835107 ✓ | 39701835098 ✓ | 39701835125 ✓ | |
| [43](https://github.com/dlroqa/Lightagent/pull/43) | `subagents/m-isolation` → #42 | `030e855a1bb685507e69fda7a750329e9b2da837` | 1 | Cross-profile isolation tests through delegated workers | 39703188046 ✓ | 39703188214 ✓ | 39703188027 ✓ | |
| [44](https://github.com/dlroqa/Lightagent/pull/44) | `subagents/n-tool-names` → #43 | `12b0a9e740f767a5ea3759557ae7103d138ac43b` | 1 | `inference.tool_names` portable mapping | 39703579583 ✓ | 39703579603 ✓ | 39703579727 ✓ | |
| [45](https://github.com/dlroqa/Lightagent/pull/45) | `subagents/o-boundaries` → #44 | `bf2a89db97376ecc1c24526bd473f83398ef335c` | 1 | Proposer and authority re-checked right before mutation; immutable patch ids | 39703589937 ✓ | 39703589837 ✓ | 39703589936 ✓ | |
| [46](https://github.com/dlroqa/Lightagent/pull/46) | `subagents/p-evidence` → #45 | `068230ab234780bbb80e3e68e15e4b7cad1930db` | 3 | Staged live-evaluation docs, `selection.v1.json`, `jev-selection-live.yml`, pilot bounds | 39705190983 ✓ | 39705190879 ✓ | 39705190906 ✓ | |
| [47](https://github.com/dlroqa/Lightagent/pull/47) | `subagents/q-freshness` → #46 | `759ec446a274bc3f9d08356e7446b882a3007f99` | 1 | Runtime check freshness; vault and RAG isolation through workers | 39710127402 ✓ | 39710127405 ✓ | 39710127408 ✓ | |
| [48](https://github.com/dlroqa/Lightagent/pull/48) | `subagents/r-eval-reporting` → #47 | `9d1901a4d1639199a4d531ff5fbb08e7a63cde72` | 1 | BLOCKED/NOT EVALUATED reporting; `jev-selection-pinned.yml` | 39710140105 ✓ | 39710140014 ✓ | 39710140022 ✓ | pinned prepare 39710140018 ✓ |
| [49](https://github.com/dlroqa/Lightagent/pull/49) | `subagents/t-release-prep` → #48 | `dd4d47f8623925aa26230ce3eac8cd60c15ef5cc` | 2 | 0.5.0 versions, changelog, release notes, merge plan | 39711436031 ✓ | 39711436033 ✓ | 39711436055 ✓ | |
| [50](https://github.com/dlroqa/Lightagent/pull/50) | `subagents/u-repair-rounds` → #49 | see §10 | 4 | Bounded repair rounds; blocked-verification reporting; pinned-workflow hardening (24/dispatch, digest); this plan | see §10 | see §10 | see §10 | |

**Live checks on dispatch.** The live broker and coder checks also ran as `workflow_dispatch` runs of `subagent fixture` at each head. The `fail` and base-`verify` dispatches fail **by design**: they are the negative halves of those checks.

**Acceptance per PR.** Each PR is accepted on its own head's runs above. Its unresolved items are the cross-cutting blockers in §11, none specific to one PR. The test-merge tree equals the head tree for every PR, so the tested tree is the candidate integration tree against its current base.

## 9. Pinned live Jev evaluation (prepared; not authorized)

The goal is to measure Jev's selection accuracy on the **final pinned PR head** before merging the stack, without the key ever reaching unmerged code.

- **Workflow.** `.github/workflows/jev-selection-pinned.yml`, as hardened in #50. The standalone proposal is draft [#51](https://github.com/dlroqa/Lightagent/pull/51): one file, byte-identical to #50's copy (source commit `e4a306c6d52603976aebadafc9c0c9d1748d77e4` on `506340b`). The exact diff is that whole file. Merging it is the owner's decision.
- **Code under test.** The reviewed head of the top PR (#50; see §10 for its SHA), passed as `sha=<40 hex>`. `prepare` refuses a SHA that is not the current head of an open PR from this repository, and refuses any dispatch ref other than `master`.

| Job | Environment | Permissions | Runs PR code | Has key | Does |
| --- | --- | --- | --- | --- | --- |
| `prepare` | none | `contents: read`, `pull-requests: read` | yes, pinned | **no** | Exports the 24 request bodies. Publishes the SHA-256 of each and of the set (summary plus job output `digest`). Uploads the `selection-requests` artifact |
| `send` | `jev-live`: required reviewer, `master` only, the owner approves in the UI | `{}` | **no** (no checkout) | yes | Requires the downloaded set to match `digest`, files `sel-NNN.json` (+ unsent `manifest.json`), at most 24. Validates each body with `jq` (only `model`, `state` ≤ 2000 chars, ≤ 11 choice/noul/score questions; ≤ 16 KiB). Sends with `curl --proto =https --max-redirs 0 --max-time 10 --max-filesize 64 KiB` to the fixed `https://api.typesafe.ai/v1/systemone`. Keeps a response only if it is a JSON object that does not contain the key |
| `score` | none | default read | yes, pinned | **no** | Replays the recorded responses through the pinned commit's own rules and scores them |

- **Provenance and integrity.** Artifacts come from the same workflow run, and the digest is checked between `prepare` and `send`. The reviewer can download `selection-requests` and compare digests before approving.
- **Secrets.**
  - Only `send` receives `TYPESAFE_API_KEY`, and it is masked by Actions.
  - No step echoes it, and responses that contain it are discarded.
  - `send` runs no repository code.
  - The key is used only for Jev's classification endpoint, never as a general inference credential.
- **Limits.**
  - `max_requests` is 1–24 per dispatch: one request per prompt, **no retries**.
  - It stops on 401/403, on 3 consecutive failures, or at the ceiling.
  - Input is about 15–24k tokens (estimated; measured usage is reported). Cost is not known: no verified price is configured.
  - **A second pass is a separate dispatch with a separate approval.**
- **Proposed first pass.** 24 labeled prompts (`evaluation/subagents/selection.v1.json`), `max_requests=24`.
- **Pass rule (fixed in the file).** Decision accuracy ≥ 0.80, and unnecessary delegation ≤ 0.10 of the 12 lead cases.
- **What it does not measure:** specialist competence, task success, latency or cost savings.
- **General-model evaluation stays BLOCKED** until an approved endpoint, model and usage budget exist (`live-evaluation-proposal.md`).

Preparing this changes nothing that needs authorization. Each of these does:
- merging the workflow to `master`;
- the `jev-live` approval;
- any paid request.

## 10. Final head and its evidence

- PR #50 code-tested head: ``6870d3f222c48b8b149a213b8661b69e5d737830``.
  - check 39715297275 ✓;
  - subagents 39715297283 ✓ (broker live; seeded; role evaluation BLOCKED);
  - jev evaluation 39715297263 ✓;
  - pinned prepare 39715297237 ✓.
- Earlier U commits: `8aecce0` (fmt only) and `8fe4da0` (green: check 39713543849, subagents 39713543570, jev 39713543847, pinned 39713543527). Both were superseded by the macOS temp-dir test fix in `6870d3f`.
- The documentation commit that adds this file is docs-only. Its runs are recorded in the progress file and on the PR.
- Docs head `10fe1ac894b432451e73121a8ff76afa88b9668c`:
  - check 39716160243 ✓, 11 of 11 jobs;
  - subagents 39716160247 ✓;
  - jev evaluation 39716160240 ✓;
  - pinned prepare 39716160265 ✓.
- **Frozen candidate.** The round-6 verification commit on top of `10fe1ac` is the candidate head of #50. Its SHA and runs are recorded in a PR #50 comment, not here.
- What those runs prove, by kind:

  | Kind | Status |
  | --- | --- |
  | Seeded regression tests | pass |
  | Scripted-model checks with real tools | pass |
  | Real GitHub Actions broker and coder checks | pass |
  | Live Jev selection accuracy | **NOT EVALUATED** |
  | Live general-model competence | **NOT EVALUATED** |

  The `subagents` run's "Role evaluation" job succeeds only by reporting BLOCKED. It is not evidence of competence.

## 11. Remaining blockers and owner decisions

1. **Merge authorization**, per PR, following §12.
2. **Live Jev selection.**
   - Route A: merge only #51 (the standalone `jev-selection-pinned.yml`), then dispatch with `sha=<#50 head>` and `max_requests=24`, and approve `send`.
   - Route B: merge the stack, then run `jev-selection-live.yml` (24 per dispatch).
3. **General-model evaluation.** Provide or decline an endpoint, model and budget. Until then it stays BLOCKED.
4. **Repair rounds outside a plan.** Decided for this release (owner, 2026-10-22): extra repair rounds stay limited to runs with a specialist plan. A lead without an active plan has no change binding, so it gets no repair allowance. Extending this to plan-less runs would be a separate, later change.
5. **Tenant execution sandbox.** None is verified, so coder, reviewer and tester stay unavailable to untrusted users.
6. **Website preset selection.** Not implemented. It needs Web repository work, and r5 only when there is more than one grantable preset.
7. **0.5.0 tag and publication**, after the final merge.

## 12. Merge procedure (prepared; not authorized)

### Method: merge commits

The repository allows all three methods. `master` has no branch protection and no rulesets, and `delete_branch_on_merge` is off. Every PR in this stack builds on the commits of the one below it.
- **Squash or rebase** would put new commits on `master` that the next PR does not contain. That PR's diff against `master` would then repeat its parent's changes and could conflict, and its tested tree would no longer be what merges.
- **A merge commit** whose first parent is current `master` and whose second parent is the PR head keeps every later head a descendant of `master`. When `master` is an ancestor of the head, the merge commit's tree is exactly the head's tree. That is still verified on every step, never assumed.

### Before the first merge

1. Record `master` SHA and tree.
2. Confirm the 21 heads still equal the table in §8 and §10. If any head differs, stop: that PR needs review and fresh checks.
3. Keep every stack branch until the end. GitHub **closes, without merging, an open PR whose base branch is deleted**; this is what happened to #28.

### For each PR, in order #30, #31, … #50, one at a time

1. **Identity.** `gh pr view N --json headRefOid,state,isDraft,baseRefName`. The PR must be open, and its head must equal the approved SHA.
2. **Retarget** (from #31 on, once the PR below has merged): `gh pr edit N --base master`.
   - Re-read the PR: base `master`.
   - `compare master...<head>`: `behind_by = 0`, and `ahead_by` must equal the "Own" column. Otherwise stop.
   - Do not rely on GitHub to retarget automatically.
3. **Intended diff.** `master`'s tree must equal the tree of the previous PR's head (checked in step 7 of the previous PR). Then `git diff master <head>` is exactly the PR's own change. Compare its file list with `git diff <old base head> <head> --stat`. A mismatch is a stop.
4. **Candidate integration tree.** Wait until GitHub reports `mergeable` (not null) for the new base, then read `merge_commit_sha`.
   - Its tree must equal the head's tree, which is the tree the PR's checks tested.
   - If it differs, **stop**. Do not merge. Bring the branch up to date with a merge from `master` (no force-push), re-run `check`, `subagents` and `jev evaluation` on the new head, and get the new head approved before continuing.
5. **Checks at the exact head.** `check` (11 jobs), `subagents` (3) and `jev evaluation` must be green at the head SHA, plus any path-triggered run listed. Retargeting does not re-run `pull_request` workflows. If a run is missing or red, re-run it with `gh workflow run <file> --ref <branch>` and wait.
6. **Merge.** `gh pr ready N`, then `gh pr merge N --merge --match-head-commit <approved head>`. `--match-head-commit` refuses the merge if the head moved. Do not delete the branch.
7. **Record and verify the result.**
   - Record the new `master` SHA.
   - Verify its parents are (previous `master`, head) and its tree equals the head's tree.
   - Record both in the progress file.
8. **Post-merge checks.** `check` and `jev evaluation` run on the push to `master`. From #33 on, also dispatch `gh workflow run subagents.yml --ref master` and wait for the live broker. All must be green before the next PR.
9. **Next PR.** Inspect the next PR's base and diff (steps 1–3) before doing anything else.

### Known flake on the PRs below #50

On macOS, `serve::routing_tests` (`an_untrusted_run_never_receives_unisolated_local_execution`, `an_enforced_profile_policy_shapes_the_provider_request`) can fail with `Unwritable … lightagent-workflow-… No such file or directory`. Two tests that start in parallel read the same clock value and share one temporary home. It is present on `master` (seen on #51, run 39714022711) and in every head below #50, and is fixed in #50 (`6870d3f`). For this exact signature only, re-run the failed job **once** (`gh run rerun <id> --failed`). Any other failure, or a second failure, is a stop.

### Stop conditions

Stop on any of:
- a failed or missing check;
- a head that moved;
- `ahead_by` or file list not as expected;
- a test-merge tree that differs from the head tree;
- a PR closed or retargeted by GitHub unexpectedly;
- any change of scope.

### Recovery, without discarding work or bypassing protection

- **A PR was closed because its base branch was deleted.** Recreate the branch at its recorded head SHA (`git/refs` API), reopen the PR, retarget it, and re-verify steps 1–5.
- **A post-merge check failed.**
  - Do not continue the stack, and do not force-push `master`.
  - Investigate on a branch.
  - Either fix it forward through a new PR, or revert the merge commit through a PR (`git revert -m 1 <merge sha>`). After a revert, the reverted PR's branch must be re-merged with a revert of the revert, not merged again as is.
- **A head changed after approval.** It needs a new review and a new run of every required check.
- **Branch cleanup** comes only after #50 merges: delete `fix/rag-incomplete-delivery` and `subagents/*` (the `subagents/live-coder-*` scratch branches are already removed).

### After the final merge (#50)

Validate the final `master` commit: `check`, `jev evaluation` and `subagents` (dispatched) must be green, and the tree must equal #50's head tree. Then report release readiness. The changelog date, the `v0.5.0` tag and publication are separately authorized steps (`release-preparation.md`).
