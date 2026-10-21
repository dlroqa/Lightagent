# Specialist subagents: implementation progress

The working record for making `researcher`, `coder`, `reviewer` and
`tester` operational specialists: functioning scoped tools, loaded role
skills, enforceable permissions and budgets, visible lifecycle, and evidence.
Every build, test, format check, render and evaluation runs on
GitHub-hosted Actions runners. The local checkout is only used to keep the
source and this record. The only local actions are text edits to the source,
which the user authorized on 2026-10-21, and git.

## Baseline

| Item | Value |
| --- | --- |
| Default branch | `master` at `506340b` (merge of PR #29) |
| Base of this work | PR #30 `fix/rag-incomplete-delivery` at `42c26ba`. PR #30 is open and not merged, and other work is still landing on it. |
| Why stacked on PR #30 | PR #30 changes `delegate.rs`, `context.rs`, `executor.rs`, `chat.rs` and `serve.rs`, and adds `ToolProgress`, which carries worker progress. A master base would conflict on every file. |
| Stack rule | Retarget each stacked PR to its new base **before** its base merges. GitHub closed PR #28 unmerged when its base merged. |
| Web repository | Not inspected. Its deployment state is not inferred from the Lightagent mirror. |

## Findings, rechecked against `42c26ba`

| # | Finding (spec hypothesis or new) | Status | Evidence |
| --- | --- | --- | --- |
| 1 | Built-in workers are ordinary profiles named after roles, cloned from the first lead, and same-named profiles are reused silently | **Confirmed** | `crates/lightagent/src/lib.rs` `ensure_subagent_profiles` runs on every chat and serve run. `SubagentsConfig::default().enabled` is **true** (`config.rs`), so a role profile is created on the first run. |
| 2 | One-line role instructions, copied routing, no skills, no readiness state | **Confirmed** | `lib.rs` `subagent_role_instruction`. Routing is copied from the lead clone. |
| 3 | Child executors have no web, workspace, skill or remote-terminal context | **Confirmed** | `delegate.rs` builds a bare `BoundedExecutor` and never reads `ctx.web`, `ctx.workspace`, `ctx.skills` or `ctx.open_terminal`. Workers are offered `web.*`, `fs.*` and `skill.read`, but those calls can only fail. |
| 4 | `worker_default` has no RAG, MCP or extension tools | **Confirmed** | `registry.rs` `worker_default` is `builtin()` without delegate. `serve.rs` and `chat.rs` build the worker registry from it, not from the run's configured registry. |
| 5 | Delegation ignores the worker profile's toolsets and installs no child policy guard | **Confirmed** | Only the parent's static `ToolPolicy::worker_registry` narrows the child. The child executor has no `call_guard`. |
| 6 | No single tree-wide budget | **Confirmed** | `within_ceiling` caps each child at the parent's *whole* limits, and siblings do not share a budget. |
| 7 | Worker events are flattened into text | **Confirmed** | `child.run(...)` collects events, and `worker_answer` returns text. Nothing reaches the parent's stream. |
| 8 | `RemoteUser` withholds delegation; `assistant` profiles have no tools and strict approval | **Confirmed** | `policy.rs` `ToolReach::Delegation.requires_isolation()`; `provisioning.rs` `template_profile`. |
| 9 | Mock-only tests | **Confirmed** | `crates/lightagent-tools/tests/delegate.rs` |
| N1 | **Depth off by one**: with `max_depth = 2`, a third generation can start | **Confirmed in code, latent** | Starting a worker never checks depth. The depth-2 context gets `agent.delegate` (`permits_nested_delegation` checks the parent's depth), so depth 3 is reachable. It is latent today because of N4. |
| N2 | A model-supplied `tool_scope` cannot remove `agent.delegate` | **Confirmed** | `delegate.rs` re-adds `AgentDelegate` after scoping. |
| N3 | Lead instructions advertise parallel specialist work, but tools run serially | **Confirmed** | `RunLimits::max_parallel_tools` is 1, and `AgentLoop::run_call` drives one call at a time. |
| N4 | A worker can never run a mutating or executable tool, including a nested `agent.delegate` | **Confirmed** | `PolicyEngine::evaluate` always requires approval for `Mutating`/`Executable`. A child's `AwaitingApproval` becomes an error, so the coder cannot edit and nested delegation never runs. |
| N5 | Worker routing is copied from the lead; the factory always uses the lead's endpoint and key | **Confirmed** | `LightweightFactory { base_url, api_key }` comes from the lead's preflight. |

Approval, cancellation and authorization in the current code:
- A worker shares the parent's cancellation token and `RunAuthorization`. A revocation stops it at its next model turn or tool call.
- A worker's approval policy is the stricter of its profile's policy and the parent's floor, but the floor applies only when the profile tool policy is enforced.
- The delegate call is bounded by `subagents.delegate_timeout_secs`, which defaults to 180 s and must be between 30 and 300.

## Decisions

| Date | Decision |
| --- | --- |
| 2026-10-21 | Source edits are text-only in the local checkout (user-authorized). Formatting, compilation and tests run only on Actions. Under this policy, rustfmt counts as execution. |
| 2026-10-21 | Stack on PR #30 and do not push to its branch. |
| 2026-10-21 | Seeded regressions are `#[ignore = "seeded: …"]` tests. The non-required `subagents` workflow reports them; a stage that closes a gap removes the `ignore` in the same change, and the required gate then runs it. |

## Stages

| Stage | Scope | Branch / PR | State |
| --- | --- | --- | --- |
| A | Baseline record, seeded regressions, report workflow | `subagents/a-baseline`, #31 | done, green |
| B+C | Typed catalog, role skills, explicit assignments, legacy resolution, one worker-runtime factory, scoped contexts, guards, readiness, routing validation, `lightagent workers` | `subagents/b-catalog`, #32 | done, green |
| D | GitHub Actions workflow broker; operational role fixtures | `subagents/d-broker`, #33 | see evidence |
| E | Tree budget, fair admission, lifecycle events, web UI, render scenarios | `subagents/e-budget-events`, #34 | see evidence |
| F | Confined `agent.research` for untrusted runs, capability presets | `subagents/f-remote-research`, #35 | see evidence |
| G | Role evaluation set and live harness, documentation | `subagents/g-evidence`, #36 | live evaluation BLOCKED |

Each stage branch is stacked on the previous one, and A is stacked on PR #30.

## Seeded gaps

| Gap | Test | Closed in |
| --- | --- | --- |
| 1 Built-in workers created as profiles | `seeded_built_in_workers_create_no_profiles` | B |
| 3 Child contexts missing | `seeded_a_worker_reads_the_skills_its_run_has` | B |
| 5 Worker toolsets ignored | `seeded_a_worker_profiles_toolsets_bound_its_tools` | B |
| N2 `tool_scope` re-widened | `seeded_a_tool_scope_is_never_widened` | B |
| N1 Depth off by one | `seeded_no_worker_starts_below_the_maximum_depth` | B |
| 7 No worker progress | `seeded_a_worker_reports_its_lifecycle_to_the_parent` | B |
| 6 No tree budget | `seeded_workers_share_one_tool_call_budget` | E |

Gaps not covered by a seeded test:
- Gap 2 (no skills) was closed by the role skills and is tested in `workers/tests.rs` and `specialists.rs`.
- Gap 4 (no RAG/MCP for workers) was closed because workers now draw from the lead's effective tools (`worker_base`).
- Gap 8 (untrusted runs) is partly closed by stage F: research only.
- Gap 9 (mock-only evidence): operational fixture tests were added and the live evaluation is BLOCKED.
- N3 (serial "parallelism"): rechecked. The lead's instructions never claimed parallel work; only the `SubagentsConfig` doc comment did, and it is corrected. Workers run one at a time per lead run, with one active worker per profile by default.
- N4 (mutations need approval) is reported as status `blocked`.
- N5 (routing) is handled by approved routes only.

## Actions evidence

| Stage | Tested SHA | Workflow runs | Result |
| --- | --- | --- | --- |
| A | `9be758b` | check 39653913064, subagents 39653912976, jev 39653912889 | all green. The seeded report showed 6 of 6 seeded tests failing, as designed. |
| B+C | `98eef17` | check 39658871077 (11/11 jobs), subagents 39658871113, jev 39658871184 | all green |

## Resume instructions

1. `git fetch origin`, then check out the latest stage branch in the table above.
2. Re-read this file's findings and decisions. Check whether PR #30 moved, and rebase the stack if it did.
3. Never run cargo, rustfmt, npm or project scripts locally. Push, then read the Actions logs.
