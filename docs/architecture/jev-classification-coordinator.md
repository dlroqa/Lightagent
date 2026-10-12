# Jev task-classification coordinator

Status: implemented in PR 3 (`jev-coordination/pr3-classification`).
Classification is **off by default**. Turning it off restores the previous
behavior exactly, with no rebuild and no data migration.

## Responsibilities

| Component | Decides | Never decides |
| --- | --- | --- |
| Lightagent | What the task requires; which advisory instruction, if any, an accepted classification adds | Where inference runs once the model is `Auto` |
| Jev | Typed, probabilistic answers about the task (and, for legacy routing, an allowlisted model) | Tools, approvals, permissions, profiles, workflows |
| Lightweight | The route behind `Auto`, model eligibility, inference budgets, inference | Anything about Lightagent's tools or sessions |

Task classification describes requirements. Model routing chooses a model.
They are separate decisions with separate settings, and both use the one
shared `lightagent-jev` adapter.

## Integration points

`crates/lightagent/src/coordinator/` is the single pre-run decision boundary:

| Caller | Entry | Notes |
| --- | --- | --- |
| HTTP API run | `serve.rs::inference_for_request` → `coordinator::preflight` | inside `LightweightRunFactory::run` |
| ACP prompt | same factory as the API (`StartRun { model: None, cwd }`) | |
| Terminal turn | `chat.rs::turn_preflight` → `coordinator::preflight` | once per fresh turn; a dropped file marks the turn as an attachment |

Neither tool calls, approval resumes, nor delegated workers call the
preflight. Classification therefore happens at most once per run or turn, and
never recursively.

## Run lifecycle

```text
create_run / ACP prompt / terminal line
  └─ resolve profile (existing)
     └─ coordinator::preflight ─┬─ jev::select_route   (model routing, legacy)
        (tokio::join!, shared   │     explicit model → keep it
         cancellation token)    │     Auto           → keep it, no request   ← Auto guard
                                │     otherwise      → one route request (unchanged)
                                └─ classify::classify (task classification)
                                      off            → nothing at all
                                      not permitted / unsafe / attachment → skipped, no request
                                      otherwise      → one batched request over a sanitized state
     ├─ routing.selected event (unchanged contract)
     ├─ configured_registry + PolicyEngine (unchanged)
     ├─ active + accepted + mapped + tools present → one fixed advisory instruction appended to the persona
     ├─ classification.completed event (only when classification is not off)
     └─ AgentLoop → provider (model = configured | allowlisted | "Auto")
```

The two Jev requests run concurrently, so the preflight lasts as long as the
slower one, which is bounded by `platform.jev.timeout_secs`. Cancelling the
run ends both requests at once; the configured model is kept and the agent
loop observes the same cancellation.

### Why routing and classification are not one request

TypeSafe evaluates every question in a request against one shared `state`.
Routing's state is the original message, cut to 4,096 characters, which is
the legacy behavior this PR must not change. Classification's state is the
sanitized description. Merging them would either send the raw message for
classification or change routing's decision inputs. Both decisions stay
correct only as separate requests. When both are enabled for a non-`Auto`
model, a run makes **two** Jev calls (tested). An `Auto` model makes at most
one (tested). Diagnostics report the count as `jev_calls`.

## The Auto guard

When the effective model is `Auto` (trimmed, case-insensitive, matching
Lightweight's own rule), `jev::route` returns `RouteFallback::AutoRoute`
before any request is made. The provider receives `model: "Auto"`, and
Lightweight decides the route, including its own provider-side Jev
classification. This holds in every classification mode. Task classification
never chooses a model. An explicit caller-selected model was already exempt
from routing and remains so.

Evidence:
- `jev::tests::an_auto_default_is_never_replaced_or_sent_to_jev`;
- `coordinator::tests::auto_is_preserved_in_every_classification_mode`;
- `serve::routing_tests::api_and_acp_runs_send_auto_to_the_provider_unchanged`:
  a full factory run for API- and ACP-shaped requests against a stub
  OpenAI-compatible gateway that advertises `Auto`. The gateway receives
  `"model": "Auto"`.
- `chat::model_tests::terminal_turns_keep_auto_and_never_send_attachments`.

## Decision inputs and outputs

One request, five questions, all over the sanitized description:

| Id | Type | Output |
| --- | --- | --- |
| `task` | choice | `conversation`, `research`, `coding`, `analysis`, `automation`, `multi_step`, with probabilities and TypeSafe confidence |
| `planning` | noul | probability that an explicit plan helps (no confidence) |
| `retrieval` | noul | probability that information beyond the task is needed (no confidence) |
| `tools` | noul | probability that tools are needed (no confidence) |
| `complexity` | score | `simple` / `moderate` / `complex`, with its own confidence |

A Noul has no confidence in TypeSafe's API, and none is derived. Only the
category's confidence is compared with a threshold. Confidence is TypeSafe's
estimate, not measured accuracy; calibration against labeled cases is later
work.

The threshold is version-aware. `version_thresholds["jev-1.13.0"]` applies
when that version answered. Otherwise the conservative `default_threshold`
(0.9) applies. A classification below its threshold is recorded as
`low_confidence` and never influences the run.

## Modes

| Mode | Requests | Effect on the run |
| --- | --- | --- |
| `off` (default) | none | none; no event |
| `shadow` | one per run or turn, when permitted | none; a `classification.completed` event |
| `active` | as shadow | an accepted, mapped category whose tools the run already has adds one fixed advisory instruction; otherwise as shadow |

In `active` mode the mapping is explicit and closed:

| Category | Applied only if the registry already has | Instruction |
| --- | --- | --- |
| research | `rag.realtime`, `web.search` or `rag.search` | gather and cite evidence with existing retrieval tools |
| coding | `fs.read` or `fs.list` | inspect relevant workspace files first; changes still need approval |
| multi_step | `agent.delegate` | outline steps; delegate only where already permitted |
| conversation, analysis, automation | — | advisory only |

The instruction text is fixed: nothing from the message or from Jev is
interpolated into it. The tool registry, the approval policy and the
provider request are built exactly as without classification (tested). The
terminal keeps its persona for the session, so terminal classification is
always advisory and is shown as a single status line.

## Configuration

Additive, under `platform.jev.classification`:

| Key | Default | Meaning |
| --- | --- | --- |
| `mode` | `off` | `off`, `shadow` or `active` |
| `allowed_profiles` | `[]` | profiles permitted to send a sanitized description; every other profile is denied |
| `max_input_chars` | 400 | 32–2000 |
| `default_threshold` | 0.9 | used until a version is calibrated |
| `version_thresholds` | `{}` | e.g. `{"jev-1.13.0": 0.8}` |

`platform.jev.enabled` remains the endpoint's master switch, and the endpoint,
key, model and timeout are shared. Existing configurations load with
classification off. Enabling the endpoint or setting routing allowlists never
enables classification. CLI:
`lightagent config set platform.jev.classification.mode shadow` (and the other
keys above). Settings-UI controls are PR 4.

## Privacy boundary

- Input is the current user message only: never history, memory, persona,
  tool output, documents, attachments or another profile's data.
- The profile permission is checked before anything is derived from the
  message. Unlisted profiles never contact Jev for classification.
- Sanitization (`coordinator/sanitize.rs`):
  - replaces code blocks, URLs, e-mail addresses, file paths,
    credential-shaped tokens, values after credential words, and digit runs
    of six or more with placeholders;
  - refuses key material and messages over 8,000 characters;
  - refuses a result with no substantive words left;
  - bounds the output to `max_input_chars`.

  Redaction minimizes; it does not authorize.
- The request is capped at 16 KiB and the response at 16 KiB.
- No cache exists, so nothing is shared across profiles or runs.
- Diagnostics hold only fixed vocabulary, numbers and the Jev model version.
  The message, key and sanitized text are never recorded.
- The legacy routing path's raw-message exposure is unchanged and documented.
  Classification does not reproduce it.

## Events

`routing.selected` is unchanged. A new SSE event, `classification.completed`
(`AgentEvent::TaskClassified`, schema 1), is emitted only when classification
is not off. It reports:
- `mode` and `outcome`, plus `source`;
- the category with its confidence and `confidence_available`, and the
  `threshold` applied;
- the three noul probabilities;
- the complexity level with its confidence;
- `model_version` and the input/output token counts;
- `jev_calls`, the classification latency and the whole preflight latency;
- `fallback_reason` and `influenced`.

ACP and the session store ignore it, as they ignore other informational
events.

## Disabling versus rolling back

These are different operations with different effects.

**Operational disablement** keeps the code and changes configuration only. No
rebuild or data migration is needed:

| Setting | Effect |
| --- | --- |
| `platform.jev.classification.mode = off` | No automatic classification requests and no `classification.completed` events. Model routing is unaffected, and **the Auto guard stays in force**. |
| `platform.jev.classification.explicit_tool = false` | `jev.classify_task` leaves every profile's catalog. |
| `platform.jev.classification.allowed_profiles = []` | No profile may send anything for classification. |
| `platform.jev.enabled = false` | Disables the Jev endpoint for every path: routing, automatic classification, and the explicit tool. A configured `Auto` model is still sent unchanged. |

**Full code rollback** reverts the PRs. Only reverting PR 3 removes the Auto
guard, so legacy routing could again replace a configured `Auto` model
(the pre-PR-3 behavior). The configuration fields are additive and ignored by
older builds, and no persisted session or run data changes shape.

The single behavior change for existing users is the Auto guard: a
configured `Auto` model is no longer replaced by legacy routing. Non-`Auto`
routing is unchanged.

## Subsequent work

- PR 4: settings and Tools-screen controls, the explicit `jev.classify_task`
  tool, and recent sanitized results (see `jev-tool-and-ui.md`).
- PR 5: wider workflow integration (planning or delegation policy) built on
  existing mechanisms only.
- PR 6: the protected `jev-live` contract, labeled evaluation and
  calibration, and a separately approved migration of legacy routing's
  raw-message input.
- Profile toolset enforcement:
  `docs/architecture/profile-toolset-enforcement-proposal.md`.
