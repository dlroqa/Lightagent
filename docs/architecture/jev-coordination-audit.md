# Jev classification coordination — Phase 0 source audit

Status: **audit only.** This document changes no runtime behavior. It records
what the source actually does today, so that later Jev work extends the
existing design instead of adding a second, conflicting decision system.

| | |
| --- | --- |
| Repository | `dlroqa/Lightagent` |
| Baseline branch | `master` (clean, in sync with `origin/master`) |
| Baseline commit | `7d952322c3dece8c6492d329d69fa902a957f630` ("Await terminal cleanup on cancellation") |
| Workspace version | `0.4.1` |
| Baseline CI | `check` run [37141663183](https://github.com/dlroqa/Lightagent/actions/runs/37141663183): all 8 jobs green |
| Lightweight reference | frozen tag `v0.7.0`; paired E2E pins `c380bddafedb27adc7044a5482bb91ddf04e4e33` |
| TypeSafe contract | `docs.typesafe.ai/api` and `/models`, fetched 2026-10-12 |

## 1. Headline findings

1. **Jev is already integrated in Lightagent, but as a model router, not a
   task classifier.** `crates/lightagent/src/jev.rs` asks Jev one `choice`
   question, `route`, whose options are `default`, `model:<id>` and
   `profile:<id>` from `platform.jev.allowed_models` and
   `platform.jev.allowed_profiles`. The answer can change the provider
   `model` string for a run. Nothing in it classifies the *task* (conversation,
   research, coding, …) or influences workflow selection.
2. **Lightweight v0.7.0 already ships its own Jev classifier.**
   `lightweight-router/src/classifier/jev.rs` sends the same SystemOne Choice
   request when a client sends `model: "Auto"` and an `Auto` rule says
   `"classify": true`. That module owns "which logical route", which is the
   provider-side responsibility this plan assigns to Lightweight.
3. **Consequence: the two layers can double-classify.** If Lightagent's
   configured model is `Auto` and Lightagent's Jev is enabled with a non-empty
   allowlist, then Lightagent's Jev may replace `Auto` with a concrete model or
   route name. That pre-empts Lightweight's own classification. This
   duplicates the routing intelligence this plan says must stay in
   Lightweight. No code path detects or prevents it today.
4. **The route-hint contract already exists, and it is the `model` field.**
   Lightweight v0.7.0 reads a logical route name (`Coder`, `Auto`, …) from
   the OpenAI-compatible `model` field
   (`lightweight-router/src/auto_route.rs`, `AUTO_ROUTE = "Auto"`). It has no
   separate route-hint header or JSON field, so none may be invented. Route
   names are operator-configured, not a fixed enum. The proposed `Coder`,
   `Research` and `General` labels exist only if the operator defined them.
5. **No task classifier, planner or workflow selector exists in Lightagent.**
   A source search for `classif`, `planner` and `workflow` finds only the tool
   risk classifier (`BoundedExecutor::classify`, which is authorization) and
   the persona instruction blocks described below. Workflow "selection" today
   is the model choosing among the tools it is offered.
6. **Profile `toolsets` are not enforced at runtime.** `AgentProfile.toolsets`
   is declared (`lightagent-core/src/profile.rs`) but no non-test code reads
   it. Tool eligibility is config-wide (`configured_registry`) plus the
   profile's `ApprovalPolicy` through `PolicyEngine`. Any "authorized workflow"
   filter must build on those two gates, not on `toolsets`.

## 2. Verified TypeSafe Jev contract

Source: `https://docs.typesafe.ai/api.md` and `https://docs.typesafe.ai/models.md`.

- `POST https://api.typesafe.ai/v1/systemone`, `Authorization: Bearer <key>`.
- Request: `model` (required; `jev-latest` alias, currently resolving to
  `jev-1.13.0`, plus `jev-preview`), `state` (string, object or array), and
  `questions` (a map from caller-chosen id to question).
- Question types, all with `type` and `instructions`:
  - `choice`: `criteria` is a map from option to description or `null`, with
    at most 255 options.
  - `score`: `criteria` is an ordered array of 2 to 10 levels.
  - `noul`: optional `criteria.true` and `criteria.false`.
- Answers come back under the same ids:
  - `choice`: `choice`, `probabilities` (sums to 1) and `confidence`.
  - `score`: `score`, `legend`, `probabilities` and `confidence`.
  - `noul`: `noul` only, a probability in [0, 1] with **no confidence**.
- The response also carries `model` (the versioned id that answered) and
  `usage.input_tokens` / `usage.output_tokens`. Pricing is per input token
  only.
- Errors: `401`, `422`, `429` (rate limit) and `529` (overloaded). The SDKs
  retry `408`/`429`/`5xx` with backoff and honor `retry-after` and
  `retry-after-ms`.
- Limits: 64k tokens per request; 32k for `state` plus the longest question.
  Rate limits are documented as changing dynamically.
- TypeSafe recommends batching every question into one call. It also warns
  that the `jev-latest` alias moves, so thresholds should be tuned against a
  **pinned** version.

Implications for later phases:

- A task classification fits in one request: one `choice` for the primary
  category, `noul`s for planning, retrieval and tool needs, and one `score`
  for complexity. Noul answers have no `confidence`, so the classification
  confidence must come from the Choice/Score answers. A raw noul probability
  must never be read as permission.
- The existing parser (`jev.rs::parse_decision`) already validates `choice`
  answers to this contract. It checks the probability range, that the chosen
  option is the maximum, that probabilities sum to 1 ± 0.02, and that the
  option keys exactly match the criteria. It ignores `usage` and the
  answering `model` id.

## 3. Current execution sequence (source-grounded)

```text
HTTP POST /api/lightagent/v1/runs            ACP session/prompt           CLI `lightagent chat`
lightagent-api/src/lib.rs::create_run        lightagent-acp/src/server.rs  lightagent/src/chat.rs (REPL loop)
  deny(Scope::RunsWrite)  ── auth             StartRun{model: None}          |
  session load + busy lock + profile match      |                            |
  model_history(...)                            |                            |
        \                                      /                             |
         RunManager::start (lightagent-api/src/manager.rs)                   |
           new RunState + CancellationToken                                  |
           tokio::spawn(factory.run(...))                                    |
                 |                                                           |
  LightweightRunFactory::run (lightagent/src/serve.rs)                       |
    ConfigStore::load  ─ per-run config reload                               |
    resolve_profile    ─ profile boundary                                    |
    ensure_subagent_profiles                                                 |
    inference_for_request ───────────► jev::select_route  ◄──────────────── jev::select_route (fresh turn only)
      (skipped when request.model is set)   POST /v1/systemone (choice "route")
                                       jev::inference_route → (model, base_url)
    emit AgentEvent::RouteSelected  → SSE "routing.selected" → Agent.tsx status
    LightweightProvider::new(ProviderConfig{base_url, model})
    Delegation / configured_registry / BoundedExecutor(PolicyEngine(profile.approval_policy))
      .with_web / .with_workspace / .with_open_terminal / .with_skills
    persona += skills catalog, extension instructions,
               web_research_instructions, autonomous_subagent_instructions,
               memory::relevant_catalog
    memory::capture
    AgentLoop::from_profile(provider, executor, &profile)
    manager::drive(...) → run_streaming_with_history → resume on approvals
```

### Integration points

| # | Concern | Verified symbol(s) |
| - | --- | --- |
| 1 | Receiving tasks | `lightagent-api/src/lib.rs::create_run` (`CreateRunBody{message, profile, session_id, model}`); `lightagent-acp/src/server.rs` (`StartRun{model: None}`); `lightagent/src/chat.rs` REPL |
| 2 | Creating/resuming runs | `manager::RunManager::start`, `manager::drive`, `AgentLoop::resume`; session continuity via `SessionStore::load` + `model_history` |
| 3 | Agent-loop init | `lightagent-core/src/loop_.rs::AgentLoop::from_profile`, `RunConfig`, `set_inference_route` |
| 4 | Tool selection | `lightagent/src/chat.rs::configured_registry` / `configured_builtin_registry`; `ToolRegistry::{builtin, worker_default, scoped}`; enforcement in `BoundedExecutor::classify` + `PolicyEngine` |
| 5 | Planning/retrieval workflow | No selector exists. Persona blocks act as workflow hints: `web_research_instructions`, `autonomous_subagent_instructions`, `memory::relevant_catalog`. Retrieval tools: `rag::rag_tool`, `rag::realtime_rag_tool`. Delegation: `Delegation` + `agent.delegate` |
| 6 | Provider requests | `lightagent-provider-lightweight/src/provider.rs` body: `model, messages, stream, stream_options.include_usage, tools, temperature, max_tokens` |
| 7 | Cancellation/deadlines/limits | `RunState::cancel_token`; `AgentLoop::run_streaming_with_history(.., cancel, ..)`; `RunLimits` (`lightagent-core/src/limits.rs`); `BoundedExecutor` 60 s per-tool timeout and 262 144-byte output cap; `WallClockPolicy` |
| 8 | SSE/agent events | `lightagent-core/src/event.rs::AgentEvent` (incl. `RouteSelected`); `lightagent-api/src/sse.rs` maps it to `routing.selected` |
| 9 | Tool registration | `ToolRegistry::insert`; MCP via `mcp_tools`; RAG and memory tools inserted in `configured_registry` |
| 10 | Sessions/per-profile config | `lightagent-store/src/session.rs::SessionStore` (per profile via `at_profile`); `ProfileStore`; `ConfigStore` (global `Config`, incl. `platform.jev`) |

## 4. Existing Jev implementation in detail

| Area | Location | Notes |
| --- | --- | --- |
| Config | `lightagent-core/src/config.rs::JevConfig` under `platform.jev` | `enabled`, `base_url`, `api_key: SecretRef` (flattened `PlatformEndpointConfig`), `model` (default `jev-latest`), `confidence_threshold` (0.85), `allowed_models`, `allowed_profiles`, `timeout_secs` (3, validated 1..=300). Validation: threshold 0–1 when enabled, ≤254 alternatives, valid profile ids |
| Adapter | `lightagent/src/jev.rs::select_route`, `parse_decision`, `inference_route`, `systemone_endpoint`, `bounded_message` | No redirects, whole-request timeout, 64 KiB response cap, bearer key resolved from `SecretRef`, fail-closed to default |
| API call sites | `serve.rs::inference_for_request` (API + ACP), `chat.rs` REPL | An explicit `request.model` bypasses Jev before any outbound call |
| Event | `AgentEvent::RouteSelected{source, model, kept_default}` | Not persisted in `Session::record_run_events` (that only records content and tool events) |
| CLI | `lightagent/src/lib.rs` config get/set for `platform.jev.*`; status tree "Jev: routing and confidence decisions" | |
| API settings | `lightagent-api/src/lib.rs::UiJevSettings`, `apply_jev_api_key` | The key is written to `provider_key_file("jev")` and stored as `SecretRef::File`; `GET` returns only `api_key_configured` |
| Web UI | `frontend/src/screens/SettingsScreen.tsx::JevSettingsRow`, `JevModelPicker`; `frontend/src/screens/Agent.tsx` reads `routing.selected` | Settings → platform endpoints. No Tools-screen entry |
| Docs | `docs/architecture.md` (topology), `docs/platform-integration.md` ("Advisory routing and execution settings") | |
| Tests | 13 unit tests in `jev.rs`; `serve.rs::saved_session_identity_profile_keeps_jev_active_but_explicit_model_bypasses_it`; config validation tests; settings round-trip in `lightagent-api/src/lib.rs` | All use loopback mock HTTP; no live key in CI |

### Gaps against this plan, found in the existing adapter

These are hazards to fix in later PRs, not in PR 1.

- **No data minimization.** `state` is the raw user message, truncated to
  4 096 chars. The plan requires a minimized, policy-approved representation,
  or skipping Jev entirely.
- **No cancellation propagation.** `select_route` is not raced against the
  run's `CancellationToken`. A cancelled run can still wait up to
  `timeout_secs` (default 3 s) before stopping.
- **No typed errors or fallback reasons.** Every failure collapses to
  "default", so telemetry cannot tell timeout, 401, 429, malformed and
  low-confidence apart.
- **No retry handling.** No `retry-after` handling for 429/529. This is safe
  today because there are no retries, but nothing is recorded either.
- **`usage` and the answering `model` id are discarded.** Cost and version
  pinning cannot be observed.
- **Two duplicated call sites** (`serve.rs` and `chat.rs`) each compose
  `select_route` + `inference_route`. A coordinator should own this once.
- **No shadow mode.** Enabling Jev changes routing immediately.
- **The legacy `output` envelope parser** accepts non-TypeSafe shapes for
  "local adapters". This widens the attack surface of a fail-closed parser,
  so retaining it should be an explicit decision.
- **Interaction with `Auto`** (finding 3): not detected.

## 5. Profile, policy and isolation boundaries

- **Auth:** `lightagent-api/src/auth.rs` uses `AuthConfig` with scopes
  (`RunsRead`, `RunsWrite`, `SessionsRead`, `SessionsWrite`, `ToolsRead`,
  `ApprovalsWrite`, `Admin`). A loopback bind is open; a non-loopback bind
  requires `--key-env`. There is a single operator, with no tenants: one
  Lightagent home serves one active profile per `serve` process.
  `create_run` rejects a session whose profile does not match the server's
  profile.
- **Profiles** (`AgentProfile`): `routing{model, base_url}`, `approval_policy`,
  `limits`, `persona` (from `SOUL.md`) and `toolsets` (not enforced, see
  finding 6). `jev::inference_route` takes a routed profile's
  `routing.model` only. It refuses a profile whose `base_url` differs from
  the run's, which prevents credential cross-sending.
- **Approvals:** `PolicyEngine::new(profile.approval_policy)`. Mutating and
  executable tools ask on every call; privileged tools always block
  (`docs/permissions.md`). Nothing in the Jev path touches the executor or
  the policy.
- **Secrets:** `SecretRef` (env or file); `scripts/check-secrets.sh` gates CI.
  The Jev key never reaches the browser (a write-only field with an
  `api_key_configured` flag).
- **Cross-profile cache:** none exists, because there is no Jev cache at all.

## 6. Provider contract

- Lightagent talks to Lightweight (or any OpenAI-compatible gateway) through
  `LightweightProvider` in `lightagent-provider-lightweight`, over
  streaming `/v1/chat/completions`, `/v1/models` and the optional runtime
  catalog through `lightagent-runtime`.
- The `model` field is the **only** routing input Lightagent sends.
  Lightweight resolves it as a route name, `Auto`, or an alias. No headers
  carry routing intent. `x-lightweight-session` (affinity) exists in
  Lightweight but is not sent by Lightagent.
- The workspace dependency policy (root `Cargo.toml`) bans cmake, OpenSSL,
  libclang and heavy crates. Lightagent has no Lightweight workspace
  dependency; the paired E2E builds Lightweight separately.

## 7. Existing GitHub Actions workflows

`.github/workflows/check.yml` runs on push to `master`/`main`, on
`pull_request`, and on `workflow_dispatch`:

| Job | Runner | Covers |
| --- | --- | --- |
| `check (macos-arm64 / windows-x64 / linux-x64)` | macos-15, windows-latest, ubuntu-latest | `scripts/check.sh`: fmt, clippy `-D warnings`, `cargo test --workspace`, frontend `tsc` + `vite build`, versions, deps policy, secrets scan; CLI smoke |
| `Open Terminal live contract` | ubuntu-latest | Provisions Open Terminal and tests the adapter |
| `standalone Web UI render` | ubuntu-latest | `scripts/render-panel.sh` + Playwright (`e2e/render.mjs`) → artifact `lightagent-web-screens` |
| `paired Lightweight contract E2E` | ubuntu-latest | Builds the Lightweight mock gateway at the pinned SHA and runs `scripts/paired-e2e.py` |
| `Qdrant + Infinity live contract` | ubuntu-latest | `compose.platform-integration.yml`, `scripts/platform-contract-test.py`, `scripts/test-platform-integration.sh` |
| `package smoke` | ubuntu-latest | Release build + `scripts/smoke-lightagent-package.sh` |

`.github/workflows/release.yml` (tags `v*`, dispatch) builds and packages for
macOS ARM64, Windows x64 and Linux x64 (ubuntu-22.04), then drafts a release.

Gaps relevant to Jev: the render harness has no Jev states. The settings
screenshot shows Jev only in its default disabled form. There is no Jev live
contract job, and no GitHub Environment for a Jev secret.

## 8. Tests that must stay passing

Everything in `scripts/check.sh` on all three OSes, plus the five
single-runner jobs above. Jev-adjacent tests to preserve verbatim in
behavior:

- `crates/lightagent/src/jev.rs` tests: allowlist enforcement, ignoring
  authority fields, timeout bound, secret only in `Authorization`, response
  cap, Unicode bound, endpoint normalization, explicit-model bypass.
- `crates/lightagent/src/serve.rs::saved_session_identity_profile_keeps_jev_active_but_explicit_model_bypasses_it`.
- `crates/lightagent-core/src/config.rs`: Jev threshold, timeout and
  allowlist validation.
- `crates/lightagent-api/src/lib.rs` settings round-trip (key never echoed).
- `crates/lightagent-api/tests/api.rs`: run, SSE, sessions, settings, scoped
  auth.
- `scripts/paired-e2e.py`: the public HTTP/SSE contract against Lightweight.

## 9. Backward-compatibility hazards

1. **`platform.jev` config shape.** Existing `config.toml` files carry
   `enabled`, `base_url`, `api_key`, `model`, `confidence_threshold`,
   `allowed_models`, `allowed_profiles` and `timeout_secs`. New fields
   (e.g. `mode`, `classification`) must be `#[serde(default)]` and additive.
   `timeout_secs` must stay accepted even if a `timeout_ms` is added.
2. **`enabled = true` already means "route models".** A new "shadow" default
   must not silently turn existing users' active routing into shadow mode, or
   the reverse. This needs an explicit migration rule (see §11).
3. **The SSE `routing.selected` event and its fields** are consumed by
   `Agent.tsx`. New events must be new types, not reshaped existing ones.
4. **The settings API (`UiJevSettings`)** uses optional fields for
   round-trip compatibility. New fields must follow the same `Option` pattern.
5. **The tool catalog is model-visible.** Adding `jev.classify_task` to
   `configured_registry` changes the tool list every model sees and the
   render harness's `agent-tools` screen. It must be off by default.
6. **The CLI REPL path** (`chat.rs`) has different semantics: it routes once
   per fresh turn and keeps executor and approvals across model changes. A
   coordinator must preserve this.
7. **Sessions:** `RunRecord` / `Session` JSON must stay readable. Any new
   classification record must be an optional field.

## 10. Proposed architecture (source-grounded)

The principle stays: Lightagent decides *what*, Lightweight decides *where*,
and Jev is advisory to both and authoritative for neither.

```text
create_run / ACP / chat REPL
  └─ LightweightRunFactory::run / chat loop             (existing)
       ConfigStore::load, resolve_profile               (existing boundary)
       ┌──────────────── NEW: coordinator (one place, replaces the two call sites) ──────────┐
       │ eligibility: platform.jev.enabled, mode, explicit model, profile allowed, input ok   │
       │ minimized state (policy-gated; skip if unsafe)                                       │
       │ ONE SystemOne request, raced with CancellationToken, deadline ≤ timeout:             │
       │   route?      (existing choice — only when model routing is enabled AND the          │
       │                configured model is not Lightweight `Auto`)                           │
       │   task        choice  conversation|research|coding|analysis|automation|multi_step    │
       │   planning / retrieval / tools   noul (advisory, never permission)                   │
       │   complexity  score                                                                  │
       │ typed outcome + fallback reason + usage + answering model id                         │
       └──────────────────────────────────────────────────────────────────────────────────────┘
       mode=off     → unchanged path
       mode=shadow  → emit sanitized classification event only; nothing changes
       mode=active  → workflow hint selects among EXISTING persona blocks /
                      tool emphasis that configured_registry + PolicyEngine already allow
       jev::inference_route → ProviderConfig{model}                        (existing)
       configured_registry → BoundedExecutor(PolicyEngine)                  (unchanged)
       AgentLoop::from_profile → manager::drive                             (unchanged)
                                   │
                                   ▼
                    LightweightProvider (model = configured | allowlisted | "Auto")
                                   │
                                   ▼
                    Lightweight v0.7.0 router: Auto rules / its own Jev classifier
```

### Placement

- **Adapter:** the plan suggests a new `crates/lightagent-jev`. The audit
  supports a **new crate** for these reasons:
  - the adapter is needed by `lightagent` (coordinator) and by the tool layer
    (`jev.classify_task` would live with `lightagent-tools`-style `Tool`
    impls), so a crate avoids making `lightagent-tools` depend on the binary
    crate;
  - its dependencies (`reqwest` + rustls-ring, `serde_json`, `tokio`) are
    already workspace dependencies, so it adds no new external crate;
  - existing provider crates follow the same one-crate-per-external-service
    pattern (`lightagent-provider-lightweight`, `lightagent-runtime`,
    `lightagent-mcp`).

  `crates/lightagent/src/jev.rs` would shrink to the coordinator glue. Its
  existing tests move with the code they test.
- **Coordinator:** `crates/lightagent/src/jev.rs` (renamed or kept) owns the
  single entry point, called from `serve.rs::inference_for_request` and the
  `chat.rs` turn start.
- **Tool:** `jev.classify_task`, which follows the dotted naming of
  `rag.realtime`, `web.search` and `agent.delegate`. It is registered in
  `configured_registry` only when explicitly enabled. Its risk class is
  `External` (it sends data off-host), so the existing policy decides whether
  it asks. It is never added to `worker_default`.

## 11. Decisions needed before PR 2–3 (blockers)

1. **Double classification with Lightweight `Auto`.** Proposed rule: when
   the effective model is `Auto` (case-insensitive, as Lightweight matches
   it), Lightagent's Jev does **not** ask the `route` question. It may still
   classify the task for workflow purposes. Is that acceptable, or should
   Lightagent's model routing be retired in favor of Lightweight's?
2. **Meaning of the existing `enabled = true`.** Proposed rule: keep current
   behavior (active model routing) for configs that already have a non-empty
   allowlist. Introduce `mode = off | shadow | active` for the new task
   classification, defaulting to `shadow` only when explicitly turned on, so
   no existing user's behavior changes on upgrade.
3. **Data minimization policy.** What may leave the machine by default? The
   proposal is the user's current message only, after a redaction pass,
   bounded to a much smaller limit than 4 096 chars. Never history, memory,
   tool output, persona or file content. Plus a per-profile
   `external_classification = deny` switch.
4. **Legacy `output` envelope.** Keep or remove? Removing it tightens the
   parser but could break an unknown local adapter.
5. **Live contract:** a protected GitHub Environment (e.g. `jev-live`) with a
   `TYPESAFE_API_KEY` secret and required reviewers must be created by the
   repository owner. The agent cannot create or approve it.

## 12. Files proposed for modification (later PRs)

| PR | Files |
| --- | --- |
| 2 – adapter | `Cargo.toml` (workspace member), new `crates/lightagent-jev/{Cargo.toml,src/lib.rs,src/wire.rs,src/error.rs,tests/*}`, `crates/lightagent/Cargo.toml`, `crates/lightagent/src/jev.rs` (delegate to crate, behavior-identical) |
| 3 – coordinator | `crates/lightagent/src/jev.rs`, `serve.rs`, `chat.rs`, `lightagent-core/src/config.rs` (additive `mode`, `classification` sub-table), `lightagent-core/src/event.rs` (new `TaskClassified` event), `lightagent-api/src/sse.rs` |
| 4 – tool/CLI/UI | `crates/lightagent/src/chat.rs::configured_registry`, `lightagent/src/lib.rs` (config keys), `lightagent-api/src/lib.rs` (`UiJevSettings` + diagnostic endpoint), `frontend/src/api/agent.ts`, `frontend/src/screens/{SettingsScreen,AgentTools}.tsx`, `e2e/render.mjs`, `scripts/render-panel.sh` |
| 5 – workflows/hints | `serve.rs` / `chat.rs` persona-block selection, `jev.rs::inference_route` (`Auto` guard) |
| 6 – security/telemetry/live | adversarial tests, evaluation fixture (`fixtures/jev-eval/*.jsonl`, synthetic only), `.github/workflows/jev-live.yml` (dispatch-only, environment-protected), docs |

## 13. PR 1 scope (this change)

- Add this audit (`docs/architecture/jev-coordination-audit.md`).
- No Rust, frontend, script or workflow changes. The existing
  `check.yml` gates validate the branch on all three hosted OSes, plus the
  render, paired-E2E, live-contract and package jobs, unchanged.
- Rollback: revert the single commit.
