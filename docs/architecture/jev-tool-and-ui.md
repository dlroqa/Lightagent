# Jev in the tool catalog, explicit tool, and Settings

Status: implemented in PR 4 (`jev-coordination/pr4-tool-ui`). Builds on the
shared `lightagent-jev` adapter (PR 2) and the coordinator (PR 3). There is
no second Jev client, classifier, or frontend.

## Integration points

| Concern | Source |
| --- | --- |
| Tool trait, schema validation, risk/approval | `lightagent-tools`: `Tool`, `ToolDefinition`, `validate`, `BoundedExecutor` + `PolicyEngine` |
| Registry per run | `chat::configured_registry`, then `coordinator::add_explicit_tool` in `serve.rs` (`tools()`, `run()`), `chat.rs::build_chat_runtime`, and `lib.rs::tools_list` |
| Delegated workers | `Delegation.worker_registry = ToolRegistry::worker_default()` (builtins only), so workers never receive the Jev tool |
| Classification and sanitizer | `coordinator::classify::evaluate` (shared by preflight and tool), `coordinator::sanitize` |
| Budget and diagnostics | `coordinator::state::{JevBudget, JevState}` |
| HTTP | `GET /api/lightagent/v1/jev` (`Scope::ToolsRead`), `POST /api/lightagent/v1/jev/verify` (`Scope::Admin`), and `GET/PUT /settings` (`Scope::Admin`, existing) |
| Transport-agnostic API | `RunFactory::{jev_status, verify_jev}`; default implementations return `503` |
| Web UI | `frontend/src/components/Jev.tsx`, used by `screens/AgentTools.tsx` (Decision services card) and `screens/SettingsScreen.tsx` (inside the Jev endpoint row) |
| Rendering | `e2e/jev.mjs`, called from `e2e/render.mjs` in the existing `standalone Web UI render` job |

```text
                     ┌──────────── coordinator (crates/lightagent/src/coordinator) ────────────┐
 API / ACP run ──────┤ preflight ── classify::classify ──┐                                     │
 terminal turn ──────┤                                   ├─► classify::evaluate ─► lightagent-jev│
                     │ tool::ClassifyTaskTool ───────────┘    (gates, sanitizer,   (one client)  │
                     │   registered by add_explicit_tool       bounds)                          │
                     │ state::JevBudget  (one per run/turn: preflight + explicit calls)         │
                     │ state::JevState   (per-profile last record, connection check)            │
                     └──────────────────────────────────────────────────────────────────────────┘
 configured_registry ─► add_explicit_tool ─► BoundedExecutor(PolicyEngine) ─► AgentLoop
 GET /jev ─► RunFactory::jev_status ─► coordinator::status   POST /jev/verify ─► JevState::verify
```

## Explicit versus automatic classification

| | Automatic preflight | `jev.classify_task` |
| --- | --- | --- |
| Switch | `classification.mode` (`off`, `shadow`, `active`) | `classification.explicit_tool` |
| Profile permission | `classification.allowed_profiles` | same |
| `platform.jev.enabled = false` | disabled | disabled (not registered) |
| Sanitizer and input bound | yes | same code; the schema `maxLength` equals `max_input_chars` |
| Approval | not a tool call | the run's existing approval policy for `RiskClass::External` (scope `jev:classify`) |
| Calls | once per run or turn (≤ 1 classification + ≤ 1 routing) | one per call, while the run budget lasts |
| Effect on the run | `active` + accepted + mapped may add one fixed advisory instruction | none; advisory JSON returned to the model |
| Diagnostics | `classification.completed` event + per-profile record (`trigger: preflight`) | per-profile record (`trigger: explicit`) |

`mode = off` does not disable the explicit tool, and enabling the tool does
not enable automatic classification. A tool result never activates workflow
changes. The tool never calls the preflight, never classifies its own output,
and is absent from worker registries, so it cannot recurse.

### Run-level budget

`classification.max_calls_per_run` (default 4, valid 2–16) bounds every Jev
request a run (or terminal turn) makes. The preflight records its requests
first; at most two, which the lower bound always accommodates, so the
once-per-run preflight is never refused. Each explicit call reserves one
request; a call that sends nothing (refused input, disabled) returns its
reservation. When the budget is spent, the tool returns `budget_exhausted`.

## Status, verification and diagnostics

`GET /jev` reports, for the server's active profile only:
- `endpoint_configured`, `api_key_configured` (a reference exists),
  `api_key_resolvable` (the reference yields a value), and `configured`
  (both of the latter);
- `enabled`;
- `verification`: the last connection check, only while the endpoint, model
  and key it checked are unchanged. A changed key, endpoint or model returns
  `null`, never a stale "verified";
- the classification settings and the explicit tool's `enabled`/`available`;
- `last_classification`: the profile's latest sanitized record.

Entering an endpoint never shows as "verified". Only `POST /jev/verify`
(administrator scope, one synthetic request with no profile or message data)
can set it. Records are kept in server memory, one per profile, and never
returned for another profile. Neither endpoint returns a credential, a secret
reference, a request payload or message text.

## Authorization contract

Lightagent has no per-user identities or profile ownership. One server serves
one active profile, and API keys carry scopes (`auth.rs`). Within that model:

- Changing any Jev setting, including which profiles may send data, requires
  `Scope::Admin` (the existing settings endpoint), enforced by the server.
  Hidden UI controls are not relied on.
- A connection check spends a Jev request and therefore also requires
  `Scope::Admin`.
- Reading the status needs `Scope::ToolsRead`, like the tool catalog.
- Diagnostics are per profile, and the server answers only for its active
  profile.

"A user enabling external classification for another user's profile" cannot
be expressed, because users do not own profiles. Per-user ownership would
need an authorization contract Lightagent does not have, and none is invented
here. The administrator-only gate is the enforced boundary.

## Settings and compatibility

The settings payload gains an optional `jev.classification` object. A client
that omits it leaves the saved values unchanged (tested). Invalid values are
refused with `400` and nothing is saved (tested). Routing settings are
untouched. The new config fields `explicit_tool` (default `false`) and
`max_calls_per_run` (default 4) are additive.

The diagnostics record gains `trigger`. This is an additive field, and the
schema version stays 1.

## Rendering evidence

The `standalone Web UI render` job runs `e2e/jev.mjs` against the real
harness and a deterministic local SystemOne stub. It needs no credentials.
Screenshots and a Playwright trace are written to `e2e/screens/jev/` and
uploaded in the `lightagent-web-screens` artifact. The states:

1. not configured
2. missing credential
3. configured but disabled
4. shadow, verified
5. active
6. profile not permitted
7. explicit tool disabled
8. explicit tool enabled
9. invalid credential
10. timeout
11. classified run
12. low-confidence fallback

The job also exercises the controls themselves: the mode select, inline
threshold validation, and a keyboard-triggered connection check. It captures
light and dark themes and a 390 px width, checking for horizontal overflow.

## Reserved for PR 5

Workflow integration beyond the fixed advisory instructions (planning or
delegation policy using existing mechanisms), any change to legacy routing's
raw-message input, and profile toolset enforcement (separate proposal).
