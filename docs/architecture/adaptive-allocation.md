# Adaptive output allocation (opt-in)

Status: draft PR, stacked on #66 (`fix/pool-turn-reservation`). Not merged.
Disabled by default.

This divides a run's output pool (`generation.max_run_output_tokens`)
between the lead and the specialist workers of the run's plan. Jev's part is
advisory and indirect. Every number comes from a deterministic allocator in
Lightagent, inside the operator's pool, ceilings and limits.

Everything here was validated with scripted providers on GitHub-hosted
Actions. That proves control logic only. It says nothing about real-model
quality, the best split for any task, speed or memory use, or whether it
resolves the original cutoff incident. Live validation is outstanding.

## Before this change

With a pool configured (#61 → #66), each pool-backed turn reserves:

1. its explicit or inherited `max_output_tokens`, else
2. the room its known window leaves (`remaining_context`), else
3. `generation.unknown_capacity_turn_tokens` (default 2048).

The reservation is sent as `max_tokens`. The lead, its workers,
continuations and repair rounds draw on **one undivided pool**
(`crates/lightagent-core/src/generation.rs`, `OutputPool`; the policy is in
`loop_.rs`). Nothing sets part of it aside:

- a worker that keeps working can drain the pool, and the lead then has
  nothing left to answer with (it ends `OutputBudget` without a request; see
  the test `workers_cannot_starve_the_leads_answer_with_a_plan_and_could_without`);
- the specialist plan Jev helped choose (`coordinator/selection.rs`) orders
  workers but has no say in how much each may generate.

The 2048 default stops one unknown-capacity turn from reserving everything.
It is not meant as everyone's allowance, and this change leaves it as the
per-turn bound for unknown capacity only.

## What Jev contributes, and what it does not

Jev already answers, on the **same** classification request that routing and
selection use:

- whether splitting helps, and a yes-probability per role (selection);
- how the parts relate (single, independent, sequential);
- the task category distribution and its **complexity** (simple, moderate,
  complex) with a confidence.

This change **asks Jev nothing new**. A per-role demand question was
considered and rejected: the adapter refuses a whole response when any asked
question is unanswered (`lightagent-jev/src/answer.rs`), so one more question
would put routing, workflow guidance and selection at risk for a signal no
evaluation has measured.

What reaches the allocator from Jev, indirectly:

| Signal | Used for | Condition |
|---|---|---|
| The roles of the run's specialist plan | which roles get a share | the plan was **applied**: `subagents.selection = active` and the profile is listed |
| Task complexity `complex` | raises the researcher's and coder's demand class one step | `adaptive_allocation = jev` **and** complexity confidence ≥ the classification's own threshold |

Jev never supplies a token count, a model capacity, a ceiling, a permission
or a setting. Its confidence is not treated as proof that a share is enough.

## The planning contract (`output-plan.v1`)

`lightagent_core::allocation::OutputPlan`:

| Field | Meaning |
|---|---|
| `shares` | Each planned role once, with a `Demand` class: `light` (weight 1), `standard` (2), `heavy` (3). At most 4. |
| `source` | `role_defaults`, `jev_complexity` or `no_plan`. |
| `fallback_reason` | Why a richer source was not used: `no_selection`, `lead_keeps_task`, `plan_not_applied`, `complexity_low_confidence`, `classification_unavailable`. |
| `lead_floor` | The lead's own per-turn reservation: its `max_output_tokens`, else `unknown_capacity_turn_tokens`. |
| `follow_up` | Whether a follow-up reserve is kept: continuations allowed, or repair rounds possible (a coder with a tester or reviewer, `max_repair_rounds > 0`). |

`OutputPlan::validate` refuses more than 4 shares, a duplicated role, or a
role that is not a plain lowercase code. A refused plan is not installed and
the pool stays undivided, exactly as when the feature is off.

Lightagent's fixed demand classes (illustrative, not measured):

| Role | Class | Raised by `jev` complexity |
|---|---|---|
| coder | heavy | stays heavy |
| researcher | standard | heavy |
| reviewer | standard | no |
| tester | light | no |

## Deterministic allocation rules

`lightagent_core::allocation::allocate(available, plan)`:

1. **Lead reserve first.** `max(25% of available, min(lead_floor, 50%))`.
   It covers the lead's coordination turns and its final answer. Only the
   lead may reserve from it.
2. **Follow-up reserve.** 10% when `follow_up`. Continuations of any run, and
   repeat starts of a planned role (repairs, re-checks), may use it.
3. **Worker part.** The rest is split between the planned roles in
   proportion to their weights, rounding down. One role gets all of it.
   Equal classes get equal shares. Rounding leftovers stay unallocated.

Every part adds up to exactly what was available. A randomized invariant test
checks that, across 200 seeded interleavings.

## How the pool enforces it

The pool keeps one account per part, inside the same atomic pool:

| Account (`allocation.account`) | Who | May reserve from |
|---|---|---|
| `lead` | the lead's own turns | lead reserve, then the free rest |
| `share` | the **first** worker of a planned role, while it runs | its share, then the free rest |
| `follow_up` | a repeat start of a planned role | follow-up reserve, then the free rest |
| `unplanned` | any other worker (a role not in the plan, a profile worker, a nested worker) | the free rest only |

Any continuation may also use the follow-up reserve.

Per request, nothing about the policy changes. The ask is still the ceiling,
else the known room, else the per-turn default. The account and the pool can
only **lower** it, and the granted amount is what is sent as `max_tokens`.
So a worker's request is bounded by all of:

- its task share (its account);
- its own verified remaining context, when known;
- its own model's ceiling, when configured: `generation.models.<model>.max_output_tokens`,
  never above the lead's;
- the operator's per-request ceiling;
- what the run's pool has left.

A context window is per-request input+output capacity. It is used only to
compute the room one request leaves, never as an entitlement. A worker on
another model is never budgeted with the lead's window (#65).

Lifecycle of a share:

- **Planned.** Held for its role from installation, reachable by no one.
- **Running.** Reachable by its worker once it starts. At start it is capped
  at what the worker can use: its per-turn capacity (ceiling, else its window
  less the 2% margin, else the per-turn default) × the turns it was granted.
  The excess is split between roles that have **not started**, by weight. A
  running share is never cut back later.
- **Released.** When its worker ends (completed, failed, cancelled, revoked,
  or a parked worker expires), what is left becomes free for later work.
  Released tokens never extend a generation already running. They can fund a
  later, permitted request.

Accounting stays as in #66:

- Reservations are atomic.
- A turn is settled against reported `completion_tokens`, which already
  include any reasoning the backend counts, charged once.
- Missing usage, cancellation and errors are charged the whole reservation
  (`output_estimated`).
- What a turn did not use goes back to the account it came from. A released
  share gets nothing back: those tokens stay free.
- An overrun (usage above the grant) is charged, and then the pool grants
  nothing more to anyone.

Turn, time, tree, concurrency, approval, plan-order, ownership, cancellation
and revocation checks are untouched. An allocation is never a permission: a
planned role the plan says must wait is still refused, and its share stays
untouched (test `an_allocation_grants_no_permission`).

### Contention

Nothing waits for tokens. A turn whose account cannot grant at least 16
tokens is not taken, and the run ends `OutputBudget`. The run is told why
first when it has tool results to answer from.

Accounts isolate planned roles from each other and from the lead. So the
only contention left is:

- between unplanned workers, over the free rest;
- between continuations and repeat starts, over the follow-up reserve.

### The 2048 default, precisely

- It is still the **per-turn** reservation of a turn with no ceiling and an
  unknown window. That is consumption control only: the window stays
  unknown, there is no context guard and no continuation, and nothing shows
  the input fits.
- It does **not** cap a worker whose window is known. That worker asks for
  the room its window leaves, bounded by its share (test
  `a_known_window_worker_is_bounded_by_its_share_not_the_unknown_default`).
- An unknown-capacity worker is never granted a larger per-turn allowance
  because its share is large, or because another model's window is. Its
  share is capped at `2048 × granted turns`, and the excess goes to roles not
  yet started.
- Ways to supersede it, all explicit operator assertions, not verified by
  Lightagent:
  - `generation.models.<model>.max_output_tokens` for that model (a
    ceiling, so the `ceiling` basis applies);
  - a global `max_output_tokens` (a routed worker gets the smaller of its own
    and the lead's);
  - `generation.unknown_capacity_turn_tokens` for every unknown-capacity
    turn.

## Configuration

```json
{
  "generation": {
    "max_run_output_tokens": 32000,
    "adaptive_allocation": "plan"
  },
  "subagents": {
    "enabled": true,
    "selection": "active",
    "selection_profiles": ["default"]
  }
}
```

| `generation.adaptive_allocation` | Effect |
|---|---|
| `off` (default) | One undivided pool, exactly as #66. No report field. |
| `plan` | Lead reserve, follow-up reserve, and a share per role of an **applied** plan, by the fixed classes. No classification signal changes a class. |
| `jev` | As `plan`, and a confident `complex` classification raises the researcher and coder one class. This value is the explicit opt-in for Jev's classification to influence budgets. It sends nothing more to Jev. |

Requirements and fallbacks:

- **No pool, no allocation.** Without `max_run_output_tokens` the setting is
  inert and says so (`Output allocation (…): inactive`). No implicit pool,
  unlimited or otherwise, is created.
- **Nothing is enabled automatically.** Specialist selection, classification
  and Jev requests keep their own settings and permissions. No new content
  is sent anywhere.
- **No plan.** With selection off, in shadow, for a profile not listed, or
  when the lead keeps the task, there are no worker shares. The lead reserve
  and follow-up reserve still apply (`source: no_plan`), so an ad-hoc worker
  cannot drain the lead's answer.
- **Jev unavailable.** With Jev unreachable, cancelled, malformed or
  low-confidence, the fixed classes apply to whatever plan exists, and the
  reason is recorded. There is no remote dependency: `plan` never needs Jev
  beyond the selection that already chose the roles.
- **CLI.** `lightagent config set generation.adaptive_allocation plan`. An
  empty value means `off`.

Intentional default changes: none. With `off`, requests, events and SSE
payloads are byte-for-byte as before (the `allocation` key is added only when
a plan is installed). One behavior changed whether or not the option is on:
a worker's handle on the pool is now a worker account, never the lead's.
Without a plan, that has no effect.

## Diagnostics

The terminal prints one line per turn when the option is on:

```
· Output allocation (plan, role defaults): lead 8000, follow-up 3200, coder 15600 (heavy), tester 5200 (light), unallocated 0 of 32000
```

`serve`/ACP log the same line to stderr.

Each `generation.report` event (SSE) gains an `allocation` object when a plan
is installed:

- `version`, `source`, `fallback_reason`;
- the turn's `account`, and its `role` for a share;
- `lead_reserve` / `lead_reserve_left`, and `follow_up_reserve` / `follow_up_left`;
- `shares[]`, each with:
  - `role`, `demand`, `weight`;
  - `planned`, after any cap or redistribution;
  - `capacity_cap`: the cap its worker's own capacity set;
  - `left`;
  - `state`: `planned`, `running` or `released`;
- `unallocated_available`;
- `asked` and `granted` for this turn. A grant below the ask was reduced by
  the account or the pool.

What is verified and what is estimated is unchanged from #61–#66:

- `context_source` says where a window came from;
- `prompt_basis` says whether a prompt figure was measured;
- `output_estimated` says whether a charge stood in for missing usage.

A refused turn ends the run `OutputBudget`.

Only fixed codes, role names and token counts appear: no prompt, tool text,
model output or reasoning.

## Illustrative examples

All examples are **illustrative**: what the rules compute, not claims about
efficient or sufficient budgets. Each uses a 32,000-token pool, no
per-request ceiling (lead floor 2048) and one continuation allowed
(follow-up 10%).

| Case | Before (`off`) | After (`plan`, or `jev` where noted) |
|---|---|---|
| Simple request, no subagent | One pool of 32,000; the lead's turns reserve per #66. | `no_plan` (`lead_keeps_task`): lead 8,000, follow-up 3,200, free 20,800. The lead can use 28,800. An ad-hoc worker can use only the free 20,800. |
| One specialist (researcher) | The worker can draw the whole pool. | Lead 8,000, follow-up 3,200, researcher 20,800. |
| Two equal specialists (researcher, reviewer) | First come, first served. | 10,400 each. |
| Unequal: coder + tester | First come, first served. | Coder 15,600 (heavy), tester 5,200 (light). |
| Researcher + coder + reviewer, `plan` (or `jev` with moderate complexity) | — | 5,942 / 8,914 / 5,942 (2 : 3 : 2), 2 unallocated by rounding. |
| Same, `jev`, confident `complex` | — | 7,800 / 7,800 / 5,200 (3 : 3 : 2). |
| Mixed capacities: coder (unknown window, `max_turns` 3) + tester (16k window) | Coder asks 2,048 per turn; the tester its window's room; one pool. | The coder's share is capped at 3 × 2,048 = 6,144, and 9,456 moves to the tester: 14,656. Coder requests stay at 2,048. Tester requests: its window's room, bounded by 14,656. |
| Missing Jev or unknown capacity | As above. | `jev` with a failed classification: no applied plan, so `no_plan`. With a plan but low-confidence complexity: `role_defaults` (`complexity_low_confidence`). Unknown capacity: as in the row above. |
| Continuation and repair | Every continuation and repair draws on one pool. | A cut-off answer's continuation may use the follow-up 3,200 even when its share is spent. A second coder start (repair) is `follow_up` and draws on the follow-up reserve, then the free rest (including what earlier workers released). |

## Limitations

- **Demand classes are fixed guesses.** Jev's only demand signal is task-wide
  complexity. Nothing measures whether a split suits a task or model.
- **Unknown capacity stays unknown.** The share cap for such a worker
  derives from the per-turn default, not from the model.
- **Provider compliance is assumed.** A provider that ignores `max_tokens`
  can overspend by the turns in flight. The pool then refuses everything
  (`output_overrun`).
- **Reasoning shares the budget.** Where reasoning and answer share
  `max_tokens`, a share bounds both. No visible-answer allowance is
  guaranteed, for the lead or any worker.
- **The lead reserve is a floor, not a guarantee.** It covers coordination
  turns too. A lead that spends it on research has less for the answer.
- **No waiting.** Refusal is immediate. There is no queueing for released
  tokens.
- **Plans don't change mid-run.** A plan is fixed when the run starts (the
  terminal plans afresh each turn, with a fresh pool). Roles outside it are
  unplanned.
- **Live validation is outstanding:** real models, real Jev, real endpoints.
