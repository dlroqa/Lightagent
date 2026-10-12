# Changelog

All notable Lightagent changes are documented here.

## [Unreleased]

### Added

- Independent label-review package for the Jev benchmark: a blinded
  worksheet, author labels and key built in CI
  (`scripts/jev-review-package.py`, artifact `jev-label-review-package`),
  an annotation schema and a disagreement procedure
  (`evaluation/jev/review/`).
- The first live Jev baseline (run 38311407479: contract passed,
  `jev-1.13.0`, exploratory 6/6) and a prioritized security implementation
  plan (`docs/architecture/jev-security-implementation-plan.md`, plan only).
- Jev live-validation readiness: the live contract records the commit and
  every resolved model version, classes each failure as transport,
  credential or API contract, stops after a failed authentication, and runs
  only from `master`. Live classification reports are marked exploratory
  and score the benchmark and calibration splits separately.
- A routing-permission and profile-authorization proposal
  (`docs/architecture/jev-routing-permission-and-profile-authorization-proposal.md`)
  documenting five open privacy and authorization issues. Proposal only.
- Jev evaluation: a versioned, synthetic, independently labeled benchmark
  (`evaluation/jev/benchmark.v1.json`, 101 cases with a calibration split)
  and a test-only runner over the production classifier, sanitizer, workflow
  policy and router. It reports category accuracy, per-category precision,
  recall and F1, a confusion matrix, abstention, fallback and coverage,
  requirement error rates, workflow agreement, reliability curves, ECE and
  accuracy-coverage curves, with sample sizes and intervals. Mock-contract
  and live results are labeled separately; thresholds are only ever
  proposed, never applied.
- `legacy_raw` versus `sanitized` routing benchmark, an adversarial privacy,
  prompt-injection, authorization and reliability suite, and a preflight
  performance simulation.
- Terminal workflow parity: terminal turns now evaluate the workflow policy.
  In `active` mode with `workflow_guidance` on, eligible fixed guidance is
  added to that turn only, beside the memory catalog; the session prompt and
  stored history are unchanged.
- CI: `jev-evaluation.yml` (deterministic, every pull request, uploads
  reports) and `jev-live.yml` (manual dispatch, `jev-live` environment,
  `TYPESAFE_API_KEY`, bounded requests; reports pending without the secret).


- Jev workflow policy: a typed, read-only eligibility check that maps an
  accepted classification to an existing capability (retrieval, workspace,
  delegation) only when the run already has it, its approval policy permits
  it and its limits allow it. Analysis is unsupported and automation advisory
  only. Guidance is applied only with `active` mode and the new
  `classification.workflow_guidance` setting (off by default).
- `platform.jev.routing_input` (`legacy_raw` | `sanitized`): sanitized model
  routing sends the minimized task and never an attachment or key material.
  Existing Jev configurations keep `legacy_raw`; new configurations use
  `sanitized`.
- `classification.completed` schema 2: workflow eligibility, applied versus
  advisory, usable tools, approval, selection overhead and routing input.

- Jev in the Tools catalog and Settings: a Decision services card (configured,
  enabled and verified shown separately), classification controls, an
  administrator-only connection check, and the profile's latest sanitized
  classification. New endpoints: `GET /api/lightagent/v1/jev` and
  `POST /api/lightagent/v1/jev/verify`.
- Optional explicit `jev.classify_task` tool for permitted profiles
  (`platform.jev.classification.explicit_tool`, off by default), sharing the
  sanitizer and gates of automatic classification and bounded by a run-level
  Jev request budget (`max_calls_per_run`).

- Optional Jev task classification (`platform.jev.classification`, off by
  default): one sanitized, batched request per run or terminal turn, permitted
  per profile, reported through a new `classification.completed` event. In
  `active` mode an accepted category may add one fixed advisory instruction
  about tools the run already has; it never grants tools, approvals or models.

### Changed

- Linux CI jobs are pinned to `ubuntu-24.04` ahead of the `ubuntu-latest`
  move to 26.04. Required check names are unchanged.
- Lightagent's Jev model routing no longer replaces Lightweight's `Auto`
  model; `Auto` reaches the provider unchanged so Lightweight decides the route.
- `active` classification alone no longer adds advisory guidance; it also
  requires `classification.workflow_guidance` (unreleased behavior narrowed).

- Moved TypeSafe Jev transport, response validation and error handling into a
  new shared `lightagent-jev` crate used by the advisory model router. Routing
  decisions, allowlists, thresholds and settings are unchanged.
- A cancelled API or ACP run now stops waiting for Jev's routing answer at once
  instead of waiting up to the routing timeout; it keeps its configured model.

## [0.4.0] - 2026-09-18

### Changed

- Extracted Lightagent into its own repository and Cargo workspace.
- Removed every inference-engine, gateway, model-management, benchmark, and
  desktop-shell crate from the harness repository.
- Made the browser UI a standalone Lightagent surface served directly by
  `lightagent serve`; it no longer depends on a Lightweight reverse proxy.
- Split update, CI, packaging, and release workflows from Lightweight.
- Added the compiled Web UI to Lightagent release archives.

## [0.3.25] - 2026-09-17

- Added the responsive Agent Web UI with persisted sessions, live SSE runs,
  tool timelines, approvals, queued steering, runtime tool discovery, and
  harness settings.
- Kept dropdown menus within the viewport and made the render harness
  deterministic.

## [0.3.24] - 2026-09-15

- Added terminal reasoning visibility, session-scoped approvals, durable
  sessions, memory reflection, realtime retrieval, and guided setup refinements.

## [0.3.8] - 2026-09-12

- Added bounded run-time decisions, extensions, skills, MCP, ACP, RAG, durable
  memory, provider model discovery, and end-to-end tool execution coverage.

## [0.3.0] - 2026-09-09

- Introduced Lightagent as an agent harness with a provider-neutral loop, tools,
  approvals, profiles, persisted sessions, HTTP/SSE API, terminal CLI, and a
  protocol-only adapter for the Lightweight OpenAI-compatible endpoint.

Versions through 0.3.25 were originally tagged in the combined Lightweight
repository. Subsequent Lightagent releases are produced from this standalone
repository.

[Unreleased]: https://github.com/dlroqa/Lightagent/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/dlroqa/Lightagent/releases/tag/v0.4.0
[0.3.25]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.25
[0.3.24]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.24
[0.3.8]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.8
[0.3.0]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.0
