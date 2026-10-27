# Specialist selection

Specialist selection decides, before a run starts, whether a task should be split among specialist workers, which roles, and in what order. Jev recommends; Lightagent decides. It is off by default, and nothing about a run changes unless an operator turns it to `active` for named profiles.

Code: `crates/lightagent/src/coordinator/selection.rs` (questions, rules, record), `crates/lightagent-tools/src/plan.rs` (the order `agent.delegate` enforces), wiring in `serve.rs` (API and ACP) and `chat.rs` (terminal).

## What Jev is asked

Jev's API answers typed questions only: `choice`, `score` and `noul` (a yes-probability). It does not generate plans, and none is asked of it. With selection on, six questions join the **existing** classification request, which is still one request per turn, over the same sanitized task description:

| Id | Type | Question |
| --- | --- | --- |
| `specialists` | noul | Would the task clearly benefit from being split into separate specialist work? |
| `role_researcher`, `role_coder`, `role_reviewer`, `role_tester` | noul | Does the task need a distinct piece of that kind of work? |
| `structure` | choice | `single`, `independent` or `sequential` |

So selection costs no extra Jev request and discloses nothing classification does not already send, and the run's existing Jev budget (`platform.jev.classification.max_calls_per_run`) counts it once. A malformed answer to any question invalidates the whole classification, which then falls back like any other failure.

Jev never supplies a profile, worker, tool name, command, endpoint, credential, model or permission, and no text it returns reaches the run.

## How Lightagent decides

The plan is built by fixed rules over the requesting profile's own checks, in this order. The first rule that applies ends with the lead keeping the task, under the code shown.

| Rule | Code |
| --- | --- |
| The run was cancelled | `cancelled` |
| Classification failed or was skipped | its own code, such as `timeout`, `profile_not_permitted` or `malformed` |
| The run cannot delegate: no `agent.delegate` (or, for an untrusted run, no `agent.research`), or its approval policy refuses it | `delegation_unavailable` |
| The task is simple | `simple_task` |
| The task is conversation | `conversation` |
| Jev's `specialists` answer is a confident no: at or below 100 − `selection_threshold_percent` (30% by default). An uncertain answer does not end selection; the role answers decide. | `delegation_not_useful` |
| No role is recommended (each needs at least `selection_threshold_percent`), or none fits the task. Coder and tester fit coding or multi-step tasks; reviewer fits those and analysis; researcher fits any. A role fits when the accepted category is one it serves, or when the categories it serves hold at least the category threshold of the category distribution. | `no_role_needed` |
| As above, but a recommended role's fit could not be established because the category is uncertain | `low_confidence` |
| Only a researcher, and the task is not complex: a straightforward search | `single_step_research` |
| No recommended role is ready for this profile | `no_ready_role` |
| The lead's limits leave no room to brief and read the workers | `insufficient_budget` |

The category's confidence is not a gate of its own. A task with several parts is often split between categories that admit the same workers (for example 76% coding and 23% multi-step), and selection uses the category only to check that a role fits. The `specialists` question asks whether the task would *clearly* benefit from being split among *different* assistants, a stronger question than the role questions and one a single-worker plan does not need; only a confident no ends selection on it. Both changes follow the diagnosis of the first live run, which kept every task with the lead: see [selection-diagnosis.md](selection-diagnosis.md).

Readiness is the existing per-profile check (`workers::readiness`): the role's assignment and its ownership, the role skill, the model route, the tools the profile's policy leaves, and the run's trust. A role that is not ready is listed under `restricted` with its reason (for example `assignment_refused` or `requires_isolation`) and is **never replaced** by another worker. A tester or reviewer of a change is dropped (`needs_coder`) when the coder it would check is not available.

A plan names the fewest useful workers: the coder first, then the most strongly recommended roles, up to `selection_max_workers` (1 to 3, default 2, and never above `max_workers_per_run`). It never names all four. The rest are listed as `worker_cap`.

The order is fixed:
- research runs before the code it informs, unless Jev confidently answered `independent`;
- the tester runs after the coder, because tests target the coder's patch or commit;
- the reviewer runs after the coder and the tester, because review targets the final change.

Workers run **one at a time**, because the lead's tool calls do. The record says `concurrent: false` with `workers_run_one_at_a_time`, even for parts Jev called independent.

## Modes

`subagents.selection`:

| Mode | Questions asked | Plan given to the lead | Order enforced |
| --- | --- | --- | --- |
| `off` (default) | no | no | no |
| `shadow` | yes | no, recorded only | no |
| `active` | yes | only for profiles in `subagents.selection_profiles` | for those profiles |

Selection needs task classification on (`platform.jev.classification.mode` not `off`) and the profile permitted to send to Jev (`allowed_profiles`). It does not turn classification on.

When applied, the lead receives one fixed block of instructions built from role names only. It lists the order, asks for a bounded, self-contained brief per worker, says to reuse what a worker already returned and to test or review again after a later change, says to continue alone if a worker fails or is refused, and states that the plan grants no permissions. `agent.delegate` then refuses to start a planned role before the roles it follows have completed in that run. It names the missing role and suggests continuing without it. A role the plan does not name is unaffected. The user does not need to name specialists.

The plan grants nothing. Resolution, ownership, assignments, tool policy, approvals (`agent.delegate` still asks under the run's policy), budgets and revocation apply exactly as without it. Lightweight `Auto` routing and Jev model routing are separate decisions and are unchanged.

Classification runs once per root task: once per API or ACP run, and once per terminal turn. There is no automatic replanning. The only further Jev requests are explicit `jev.classify_task` calls, within the same per-run budget.

## What is reported

The `classification.completed` event gains an additive `specialists` object. It holds only fixed codes, role names, probabilities and worker labels:

```json
{
  "mode": "active", "outcome": "plan", "fallback_reason": null,
  "delegation_probability": 0.91, "structure": "sequential",
  "steps": [
    {"role": "coder", "reason": "needs_code_change", "after": [], "worker": "builtin:coder@2"},
    {"role": "tester", "reason": "needs_verification", "after": ["coder"], "worker": "builtin:tester@1"}
  ],
  "restricted": [{"role": "reviewer", "reason": "worker_cap"}],
  "concurrent": false, "concurrency": "workers_run_one_at_a_time",
  "tool": "agent.delegate", "applied": true
}
```

The terminal prints one line, for example `· Specialists (active): coder → tester, given to the lead`. The Web UI adds one sentence to the classification status. Worker progress is the existing `tool.progress` lifecycle.

## Configuration

In `config.json`:

```json
{
  "platform": { "jev": { "classification": {
    "mode": "shadow",
    "allowed_profiles": ["lead"]
  } } },
  "subagents": {
    "selection": "shadow",
    "selection_profiles": ["lead"],
    "selection_threshold_percent": 70,
    "selection_max_workers": 2
  }
}
```

- `platform.jev.classification.mode` must not be `off`. `allowed_profiles` lists the profiles that may send a sanitized description to Jev.
- `selection` is `off`, `shadow` or `active`. Active plans apply only to the profiles in `selection_profiles`.
- `selection_max_workers` is 1 to 3.

`lightagent config set subagents.selection shadow` and `subagents.selection_profiles` are also available. Rollback is `subagents.selection = off`, which asks Jev nothing extra and leaves every run as it was.

## Evidence

- Deterministic (CI, `check`): the rules (`coordinator::selection` tests), the one-request contract with 11 typed questions, malformed answers, order enforcement through the real `agent.delegate` (`specialists.rs`), the SSE payload, the CLI keys and validation.
- Live Jev selection accuracy: run 40503493185 (2026-10-27, `jev-1.14.0`, the earlier rules) was **BELOW_THRESHOLD**: 12/24 decisions, every task kept with the lead. Its answers are kept in `evaluation/subagents/runs/2026-10-27-40503493185/`. Replaying them through the corrected rules is development evidence only, since the rules were corrected after seeing them. A held-out set (`selection-heldout.v1.json`) is ready for a separately authorized live run. See [selection-diagnosis.md](selection-diagnosis.md).
- Whether selection makes runs better or cheaper than the lead choosing alone is **not measured**, and no such claim is made.
