# Jev evaluation, calibration and security hardening

Status: implemented in PR 6 (`jev-coordination/pr6-evaluation-hardening`).
Jev determines task requirements. Lightagent evaluates actual capabilities,
permissions and workflow eligibility. Lightweight owns inference routing.
Empirical evaluation decides whether classification should influence
execution, and this PR adds that evidence without changing any default.

## Open privacy and authorization issues (not solved by this work)

Evaluation measures these; it does not fix them. The separately scoped
proposal is
[`jev-routing-permission-and-profile-authorization-proposal.md`](jev-routing-permission-and-profile-authorization-proposal.md).

1. **Legacy Jev model routing has no per-profile external-processing
   permission.** Only classification is profile-gated.
2. **`legacy_raw` routing may transmit private-key material, attachments and
   sensitive message content** (up to 4,096 characters).
3. **Sanitization cannot detect every kind of private prose.** It is
   minimization, not a privacy guarantee.
4. **Admin-only configuration is not per-user profile ownership.** No API
   key is bound to a profile.
5. **`AgentProfile.toolsets` is not enforced at runtime.**

For sensitive or untrusted profiles, disable external legacy Jev routing
until a routing permission exists: set both `platform.jev.allowed_models`
and `platform.jev.allowed_profiles` to empty. Routing then sends nothing,
and classification keeps its per-profile gate (tested). No configuration is
migrated automatically.

## What each kind of evidence measures

| Evidence | Measures | Does not measure |
| --- | --- | --- |
| Deterministic pipeline tests (`mock_contract`) | That the runner, gates and arithmetic behave as specified | Jev's accuracy |
| Live contract tests (`live-contract`) | That the real endpoint accepts the schema, parses, authenticates and fails safely | Classification quality |
| Classification-quality measurements (`classification-live`) | Jev's answers against single-author labels on 101 synthetic cases. **Exploratory** until the labels are independently reviewed | Real user tasks, agent outcomes |
| Privacy regression tests | That known sensitive patterns never reach Jev under sanitized input, and refusals send nothing | Unknown patterns, prose |
| Real-world agent outcome measurements | Not collected | — |

No claim of improved agent accuracy, lower inference cost or better routing
is made from any of these.

## Audit: integration points

| Concern | Source | How PR 6 uses it |
| --- | --- | --- |
| Shared Jev client | `lightagent-jev::JevClient` (`evaluate`, `Limits`, `RetryPolicy::none`, `JevError::kind`) | Unchanged. Every evaluation, adversarial and live request goes through it |
| Classification coordinator | `coordinator::preflight` (routing and classification concurrently), `classify::evaluate` (all disclosure gates) | Driven directly by the runner. No new entry point |
| Workflow policy | `coordinator::workflow::evaluate` (pure: registry, approval policy, limits) | Labels are checked against it; predictions are scored through it; the terminal now calls it per turn |
| Sanitizer | `coordinator::sanitize::summarize` | Privacy labels and markers are checked against it offline and on the wire |
| Routing-input policies | `jev::route` → new `jev::routing_state` (`legacy_raw` / `sanitized`) | State preparation extracted unchanged so the benchmark measures exactly what is sent. `RouteReport` gains `fallback`, `usage` and `model_version` (additive) |
| Classification events | `ClassificationDiagnostics` schema 2 | Unchanged |
| Run-level Jev budgets | `JevBudget` (`max_calls_per_run`) | Unchanged; adversarial tests prove injected text cannot spend more |
| Tool registration and approvals | `ToolRegistry`, `PolicyEngine::evaluate`, `add_explicit_tool` | Unchanged; inspected read-only |
| Terminal run lifecycle | `chat::run` turn loop: `turn_preflight` → route → `model_history` + memory catalog → `run_streaming_with_history` | New per-turn workflow decision and per-turn guidance message (below) |
| Test harnesses | `classify::tests::{jev_stub, read_request}`, `serve::routing_tests::{gateway_stub, run_through_factory}` | Reused; two made `pub(crate)` inside `#[cfg(test)]` |
| GitHub Actions | `check.yml` (8 gates), `release.yml` | Linux gates pinned to `ubuntu-24.04`; new `jev-evaluation.yml` and `jev-live.yml` |

Nothing in `lightweight-*`, the provider routing authority, the tool
registry, the approval engine or the run limits changed.

## The dataset

`evaluation/jev/benchmark.v1.json`: 101 synthetic cases (75 benchmark, 26
calibration), twelve groups, independent labels, acceptable alternatives
fixed at labeling time, six capability fixtures, privacy expectations and
sensitive markers. The rubric, field meanings and provenance are in
[`evaluation/jev/README.md`](../../evaluation/jev/README.md). Labels were
written without consulting Jev, by one labeler (an AI assistant); a second,
human review is recommended before live figures drive any decision.

## The runner

The evaluation is test code inside the `lightagent` crate
(`coordinator::evaluation`, `#[cfg(test)]`), so it drives the production
classifier, sanitizer, workflow policy and router, not copies of them, and
adds nothing to the shipped binary.

| Run | Jev | Where | What it measures |
| --- | --- | --- | --- |
| `mock_contract` | Local stub answering from `mock-responses.v1.json` | `jev-evaluation.yml`, every PR | The pipeline, privacy gates and arithmetic. **Not** model accuracy |
| `live_jev` | The real TypeSafe endpoint | `jev-live.yml`, manual dispatch only | Contract compatibility and, on a bounded synthetic sample, classification quality |

Every report names its `measurement` and carries a disclaimer. A mock
number is never presented as accuracy.

### Metrics

All rates carry their numerator, denominator and a 95% Wilson interval. A
rate over an empty denominator is reported as absent, never as zero.

- **Category:** accuracy over classified cases (a prediction is correct when
  it is any acceptable category); per-category precision, recall and F1;
  macro F1; a confusion matrix (a correct prediction is recorded under the
  predicted category, a wrong one under the primary label); abstention
  (no classification) and fallback (not accepted) rates over cases that
  should be sent; accepted coverage; coverage-adjusted (selective) accuracy;
  high-confidence misclassifications.
- **Requirements:** planning, retrieval and tools as probabilities at a 0.5
  cut-off against labeled cases only: TP/FP/TN/FN, false-positive and
  false-negative rates, Brier score, reliability bins.
- **Workflow:** agreement between the predicted and labeled eligibility;
  eligible false positives (made eligible when the label says not) and false
  negatives; eligibility counts; how often active mode with guidance would
  apply.
- **Privacy:** per case, whether a refusal sent nothing and whether any
  marker reached Jev.
- **Cost:** Jev calls, latency percentiles, token usage. Cost is estimated
  only from operator-verified pricing (`JEV_PRICE_INPUT_PER_MTOK`,
  `JEV_PRICE_OUTPUT_PER_MTOK`, `JEV_PRICING_SOURCE`); without it the report
  says "pricing unverified" and estimates nothing.

### Reports

`LIGHTAGENT_JEV_EVAL_OUT` receives, as JSON plus a Markdown summary:
`classification-{mock,live}`, `confusion-matrix-{mock,live}`,
`threshold-analysis-{mock,live}`, `routing-{mock,live}`,
`privacy-regression`, `performance-simulation` and `live-contract`. Reports
hold fixed vocabulary, numbers, case ids and the dataset's synthetic marker
strings; never a credential or user data.

## Confidence threshold policy

Four quantities are kept apart:

| Quantity | Source | Calibrated how |
| --- | --- | --- |
| Category confidence | `choice` answer | Reliability curve, ECE, accuracy-coverage curve, per model version |
| Yes/no probability | `noul` answer | Brier score and reliability against labels. **No confidence exists and none is derived** |
| Score confidence | `score` answer (complexity) | Reported only; complexity is unlabeled |
| Empirical accuracy | Labels | The measure the others are judged against |

Rules:

1. The default category threshold stays **0.9**. Version thresholds stay as
   configured. No evaluation run writes a threshold.
2. A threshold is proposed only from `live_jev` data. Mock confidences are
   scripted and the report marks calibration **pending**.
3. A proposal comes from the calibration split and is checked on the
   benchmark split. It is the lowest grid threshold reached by walking down
   from the highest qualifying one, where *qualifying* means at least 30
   accepted samples and a Wilson lower bound of selective accuracy of at
   least 0.9. The walk settles lower only when the lower threshold accepts
   more samples, so it never proposes a threshold below the observed
   evidence. Otherwise the result is `insufficient_evidence`.
4. A proposal is for review. Activating it is a configuration change made by
   an operator (`platform.jev.classification.version_thresholds`) for the
   exact model version measured. A new version falls back to the default
   until it is measured (tested).
5. ECE is reported with its sample size and flagged as descriptive below 50
   classified samples.

**Current finding:** no live Jev data has been collected (the `jev-live`
environment is not configured), so calibration is **pending** and the 0.9
default is retained.

## Routing: `legacy_raw` versus `sanitized`

`evaluation::routing` routes every case under both policies through
`route_for_turn`, with two allowlisted models, and records what each policy
would send (`routing_state`), what reached Jev, fallbacks, routes, latency
and tokens. Model routing is measured separately from classification.

Routing **accuracy is not measured**: the route options carry only model
names ("Use provider model fast"), so no independent label can say which
route is right for a task. The report states this rather than inventing
labels.

Deterministic findings (stand-in router; see Results for the numbers):

- `legacy_raw` sends every case as written, including private-key material,
  an attachment's contents and the first 4,096 characters of an over-long
  paste. `sanitized` refuses those seven cases and transmits no
  pattern-protected marker.
- **Routing has no per-profile gate.** The case protected only by the
  classification profile permission is routed, under both policies. This
  PR preserves that behavior and records it.
- Pattern redaction does not recognize sensitive prose (a health condition);
  it reaches Jev under both policies when routing is enabled.
- Minimization changes some routes for a content-sensitive stand-in. Whether
  it changes Jev's routes is a live question.

### Recommendation for a future privacy-default migration

Evidence supports making `sanitized` the default for existing
configurations in a separately approved change, together with:

1. a per-profile permission for routing, matching classification's;
2. an opt-in prompt for operators still on `legacy_raw`, showing what it
   sends (key material, attachments, long pastes);
3. a live raw-versus-sanitized comparison on this dataset first, to quantify
   any route change.

Nothing is migrated in PR 6.

## Adversarial suite (`coordinator::adversarial`)

Each test names its requirement and fails on the first violation.

| Area | Requirement | Test |
| --- | --- | --- |
| Privacy | No marker reaches Jev through classification or sanitized routing, for every marker-bearing case | `privacy_no_marker_leaves_…` |
| Privacy | Sensitive prose escapes redaction; the profile gate is the control | `privacy_sensitive_prose_…` |
| Privacy | Attachments and key material are never sent under sanitized input | `privacy_attachments_and_key_material_…` |
| Injection | Injected text cannot change the request schema, model or questions, and is sent only as the sanitized state | `injection_cannot_change_the_classification_schema_…` |
| Injection | Answers naming an unsupported category are refused; nothing is applied | `injection_answers_naming_unsupported_categories_…` |
| Injection | A route outside the allowlist (typed or legacy) is never taken | `injection_cannot_force_a_route_…` |
| Injection | Text cannot disable sanitization or change settings | `injection_cannot_disable_sanitization_…` |
| Injection | The credential is never request content or status output | `injection_cannot_disclose_the_credential` |
| Injection | Text cannot cause extra Jev calls; the run budget holds | `injection_cannot_cause_extra_jev_calls` |
| Authorization | For every category × capability × confidence × mode × guidance (288 decisions), tools, approval answers and limits are unchanged, and guidance applies only when active, enabled and eligible | `authorization_no_classification_changes_…` |
| Authorization | A tool outside the registry is never suggested into it | `authorization_a_tool_outside_the_registry_…` |
| Authorization | Denied retrieval stays denied | `authorization_denied_retrieval_…` |
| Authorization | Coding never authorizes command execution | `authorization_coding_never_authorizes_…` |
| Authorization | Denied delegation is unauthorized | `authorization_denied_delegation_…` |
| Authorization | Profiles cannot classify for, or read the diagnostics of, another profile | `authorization_profiles_cannot_…` |
| Reliability | 401, 403, 400, 422, 429, 529, 500 and redirects fall back with a stable code and never echo the body | `reliability_every_error_status_…` |
| Reliability | A 429 with `retry-after` is not retried by the run | `reliability_rate_limits_…` |
| Reliability | Non-JSON, empty, legacy, oversized and confidence-less answers fall back | `reliability_malformed_legacy_oversized_…` |
| Reliability | An ambiguous (uniform) answer is low confidence and never applied | `reliability_ambiguous_answers_…` |
| Reliability | A new model version falls back to the default threshold | `reliability_a_new_model_version_…` |
| Reliability | Timeouts and cancellation keep the configured route | `reliability_timeouts_and_cancellation_…` |

A classification never overrides runtime authorization. Redaction is
minimization; it does not guarantee privacy.

## Terminal workflow behavior

Before PR 6, terminal turns classified but never evaluated a workflow,
because the session's system prompt is fixed when the session starts.

Now each fresh terminal turn:

1. runs the same preflight as before;
2. evaluates the workflow policy against a read-only snapshot of the
   session's tool registry, with the profile's approval policy and limits
   (the same baseline an API run uses; a session-wide approval relaxation is
   not consulted, so eligibility is never wider than over the API);
3. prints one line, for example `· Workflow: research (eligible), advisory
   only, this turn is unchanged`;
4. in `active` mode with `workflow_guidance` on and an eligible decision,
   adds the fixed guidance as a system message after the turn's memory
   catalog and before the conversation.

This uses the mechanism the terminal already uses each turn for the memory
catalog. The history is rebuilt from the session at every turn, and the
session stores only user and assistant text. So the guidance reaches this
turn's requests and no other, the session prompt is untouched, and earlier
turns are never rewritten. No planning loop, tool grant, routing authority
or recursive classification is added. Cancellation, the Auto guard and every
CLI approval are as before. `off`, `shadow` and default-off
`workflow_guidance` behave exactly as in API runs. Continuing a paused run
does not re-classify.

Tests: `terminal_workflow_follows_each_mode_and_never_touches_the_session`
and `terminal_guidance_reaches_only_its_own_turn_and_keeps_the_session_prompt`
(two turns through a real `AgentLoop` against a stub gateway).

## Live contract procedure

1. In repository settings, create the GitHub Environment **`jev-live`**.
   Add required reviewers if the plan supports them, and restrict it to the
   default branch.
2. Add the environment secret **`TYPESAFE_API_KEY`**. Optionally add
   environment variables `JEV_PRICE_INPUT_PER_MTOK`,
   `JEV_PRICE_OUTPUT_PER_MTOK` and `JEV_PRICING_SOURCE` once pricing is
   verified.
3. Actions → **jev live contract** → *Run workflow*, with `max_requests`
   (default 40, hard ceiling 150), optionally `model` and `max_cost_usd`. The
   workflow file must be on the default branch to be dispatched.
4. The run builds without the credential, then runs
   `live_jev_contract_and_bounded_evaluation` once, single-threaded. It
   checks authentication; schema acceptance; typed parsing; model version
   (where reported); token usage (where reported); category confidence;
   `noul` without confidence; a deliberately invalid key's error staying
   secret-safe; adapter compatibility; and a bounded classification and
   routing evaluation on the synthetic dataset, interleaved across groups.
5. Without the secret the job reports **PENDING** and makes no request. A
   failed required check fails the job. Reports are uploaded as
   `jev-live-reports`.

Safeguards: manual dispatch only (forks cannot start it or read the secret);
GitHub-hosted runner; `environment: jev-live`; one run at a time
(`concurrency: jev-live`); request ceiling enforced in code; no retries; the
adapter's timeout per request and step and job timeouts; a cost cap when
pricing is verified; the key read through a secret reference and never
printed or written to a report.

### Readiness and dispatch

The `jev-live` environment exists with deployment allowed from `master`
only, and holds `TYPESAFE_API_KEY`. Recommended before the first run: add a
required reviewer to the environment, and add the pricing variables once
TypeSafe pricing is verified (without them no cost is estimated and the cost
cap does not apply; the request ceiling is the bound).

Dispatch, once a run is authorized:

```bash
gh workflow run jev-live.yml -R dlroqa/Lightagent --ref master \
  -f max_requests=40 -f model= -f max_cost_usd=
gh run watch -R dlroqa/Lightagent "$(gh run list -R dlroqa/Lightagent --workflow jev-live.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
gh run download -R dlroqa/Lightagent <run-id> -n jev-live-reports
```

Budget at `max_requests=40`: 3 contract probes; then up to 24 classification
requests (two thirds of the remainder), interleaved across the twelve groups
so both splits are represented; then routing pairs (two requests each) with
what remains. Every request is sequential, uses a 10-second timeout and is
never retried; the test step stops after 20 minutes and the job after 45.
An authentication failure stops the run after one request.

The contract report records the commit, the requested model, every resolved
model version, requests used, and each check's area (`credential`,
`api_contract`, `security`, `classification_quality`) with a failure class
(`transport`, `credential`, `api_contract`) when it failed. Classification
quality never fails the contract; it is reported as exploratory.

**Status:** implemented and ready to dispatch. No live run has been made.

## Performance methodology

`serve::routing_tests::preflight_overhead_by_mode_is_measured_under_fixed_stubs`
runs the full API run factory five times in each of `off`, `shadow`, and
`active` with guidance, against an instant gateway and a Jev stub with a
fixed 100 ms delay, with routing enabled. It records Jev requests per run,
preflight and end-to-end latency, fallback, eligibility and application
rates, provider requests, tool calls and Jev tokens, and asserts that the
tools offered to the provider are identical in every mode.
`routing_and_classification_run_concurrently_in_the_preflight` proves the
two requests are in flight together with a stub that answers only when both
have arrived.

These are **simulations**: stub latency, stub tokens, no real model. They
show the shape of the overhead (one extra concurrent request per run, and no
extra latency beyond the slower request); they say nothing about cost or
answer quality. No claim that Jev reduces cost or improves answers is made.
No learned or adaptive routing is added.

## Results (deterministic, `jev evaluation` workflow)

All from the mock-contract run and stand-ins: they validate the pipeline and
gates, not Jev.

| Classification contract (scripted answers) | Value |
| --- | --- |
| Cases run / expected sent / expected refused | 101 / 93 / 8 |
| Privacy failures (refusals that sent anything, markers on the wire) | 0 of 101 |
| Jev requests | 93 (one per sendable case, none for refusals) |
| Scripted failures falling back (429, 529, malformed, legacy) | 4 of 4, each with its own code |
| Accuracy / coverage / coverage-adjusted accuracy | 86/89, 85/93, 83/85 (exactly as scripted) |
| Workflow agreement; eligible false positives | 80/93; 0 |
| Calibration | pending; 0.9 retained; no threshold changed |

| Routing input (stand-in router) | `legacy_raw` | `sanitized` |
| --- | --- | --- |
| Requests | 101 | 94 |
| Sanitizer refusals | 0 | 7 (key material 1, attachment 1, over-long 1, empty 4) |
| Pattern-protected markers transmitted | 21 | 0 |
| Mean characters sent | 111.9 | 70.1 |
| Same route when both sent | 87/94 (Wilson 95%: 0.85 to 0.96) | |

| Preflight (simulation, 100 ms stub, 5 runs) | off | shadow | active + guidance |
| --- | --- | --- | --- |
| Jev requests per run | 1 (routing) | 2 | 2 |
| Preflight p50 | — | 114 ms | 114 ms |
| End-to-end p50 | 125 ms | 132 ms | 132 ms |
| Workflow eligible / applied | — | 5 / 0 | 5 / 5 |

Two requests in flight together cost about one stub delay, not two (also
proved by the barrier test). Tools offered to the provider were identical in
every mode.

Adversarial suite: every test passed on Linux, macOS and Windows. Privacy
regression: 14 marker-bearing cases through the full preflight, 0 leaks.

## Live baseline: run 38311407479 (2026-10-12)

The first live run against TypeSafe, dispatched from master `074cdb1` with
`max_requests=12` and approved at the `jev-live` gate by the repository
owner. Reports: artifact `jev-live-reports` of
[run 38311407479](https://github.com/dlroqa/Lightagent/actions/runs/38311407479).

**Verified: API compatibility.** These are contract facts, not estimates.

| Check | Result |
| --- | --- |
| Requests made | 11 of 12: 3 contract probes, 6 classifications, 1 routing pair |
| Resolved model | `jev-1.13.0` (requested `jev-latest`); the only version seen |
| Authentication | Passed (185 ms) |
| Schema acceptance and typed parsing | Passed: choice, noul and score answers parsed; no malformed or legacy answers |
| Noul answers | Probability only; no confidence reported or derived |
| Invalid-key error | `unauthorized`, with no key text in the error |
| Token usage | Reported on every answer |
| Failures | None: no credential, transport, contract or quality failure |

**Not established: model accuracy.** These are exploratory figures on six
benchmark cases with single-author, unreviewed labels.

| Measure | Value |
| --- | --- |
| Category accuracy | 6/6 (Wilson 95%: 0.61 to 1.00) |
| Accepted at 0.9 | 5/6; one correct answer below threshold (`amb-001`, confidence 0.87) |
| Abstention / fallback | 0/6 / 1/6 |
| Category confidence | min 0.87, median 0.99, max 1.0 |
| Requirement errors | planning: 1 false positive (of 5 labeled); retrieval and tools: none |
| Workflow agreement | 5/6; no eligible false positive |
| Classification latency | median 151 ms, p95 172 ms |
| Tokens | 4,084 in, 769 out; no cost estimated (pricing unverified) |
| Routing pair (`amb-001`) | Identical input under both policies; both kept the configured model |
| Calibration | Insufficient evidence (6 samples); the 0.9 default is unchanged |

The six cases covered six of the twelve groups and none of the privacy,
research, retrieval or calibration-split cases. Nothing here supports a
threshold, routing or default change.

## Privacy threat model: updates

| Threat | Control | Residual risk |
| --- | --- | --- |
| Credential, path, URL, e-mail, long identifier in task text | Sanitizer placeholders; offline and on-the-wire marker tests | Novel formats; prose |
| Sensitive prose (health, HR, legal) | **Profile permission only** | Sent when the profile is permitted |
| Key material, attachments, long pastes | Refusal (classification, sanitized routing) | Still sent by `legacy_raw` routing |
| Profile not permitted | Classification skipped before anything is derived | **Routing is not profile-gated** |
| Injection against the classifier | Fixed schema; content-only instructions; typed parsing; allowlists | Jev may still misclassify; misclassification is advisory only |
| Credential disclosure | Key only in the `Authorization` header; typed errors carry no body | — |
| Extra spend | Per-run budget; no retries; live request ceiling | — |

## Shadow-mode recommendation

Shadow mode is appropriate as the next step, under these conditions:

- only after the live contract passes;
- only for profiles listed in `classification.allowed_profiles` that are not
  sensitive or untrusted;
- with routing allowlists empty for any installation serving sensitive
  profiles, or `routing_input = sanitized` where routing must stay on;
- `workflow_guidance` left off and `mode` not `active` until labels are
  independently reviewed and live data supports a threshold.

Shadow mode changes no run; it adds one concurrent Jev request per run and
records diagnostics.

## Known limitations

- Labels come from one AI labeler; human review is pending.
- 101 cases give wide intervals; per-category live figures will be
  indicative only.
- Routing accuracy cannot be measured without capability-describing route
  options.
- Routing is not profile-gated, and `legacy_raw` sends sensitive content;
  both are preserved here by design.
- Sanitization is pattern-based and misses prose.
- Performance numbers are simulations.
- Live validation and calibration are pending until a run is authorized and
  dispatched. Live measurements are exploratory until the labels are
  independently reviewed.
- `release.yml` still uses `ubuntu-latest`; it is release machinery, not a
  gate, and is left for a release-time change.

## Rollback

- Operational: unchanged from PR 5. `workflow_guidance = false` stops
  guidance in API and terminal runs; `mode = off` stops classification and
  workflow decisions; `routing_input` is untouched by this PR.
- Terminal: with `mode = off`, or `workflow_guidance = false`, the terminal
  behaves exactly as in PR 5.
- CI: delete `jev-evaluation.yml` or `jev-live.yml` to stop them. The Linux
  pin is one line per job (`ubuntu-24.04` back to `ubuntu-latest`).
- Code: revert the PR. All changes are additive, test-only or read-only
  inspection; there is no data or configuration migration.

## Recommendations for a future optimization phase

Only after a live run on this dataset (and a human label review):

1. If category accuracy at 0.9 is high with adequate coverage, propose a
   version-specific threshold from the calibration split, as the rule above
   defines, for operator approval.
2. Decide the routing privacy default with live raw-versus-sanitized
   agreement data, and add a routing profile permission first.
3. Measure real agent outcomes (task success, tool calls, tokens) with
   guidance on and off before any claim of benefit. Without that, no
   adaptive or learned routing is justified.
