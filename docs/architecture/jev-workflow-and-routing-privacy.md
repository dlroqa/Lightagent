# Jev workflow coordination and privacy-safe routing

Status: implemented in PR 5 (`jev-coordination/pr5-workflow-integration`).
Jev classifies. Lightagent decides eligibility and coordinates. Existing tools
perform authorized actions. Lightweight routes and runs inference.

## Audit: existing capabilities and entry points

| Capability | Source entry point | Notes |
| --- | --- | --- |
| Run lifecycle | `serve.rs::LightweightRunFactory::run` (API + ACP), `chat.rs` REPL | Preflight, then registry, then executor, then `AgentLoop::from_profile` → `manager::drive` |
| Tool registry | `chat::configured_registry`, `chat::configured_builtin_registry` | Built from global settings; profile `toolsets` are not consulted |
| Approval policy | `lightagent_core::PolicyEngine::evaluate(&ApprovalRequest, now)` | `&self`, no side effects; `deny` by risk class; mutating/executable always ask; privileged blocked |
| Retrieval | `web.search`, `web.fetch` (`config.web`), `rag.realtime`, `rag.search` (`rag.rs`); `chat::web_research_instructions` | Exists only as tools and the existing research instructions |
| Coding / terminal | `fs.read`, `fs.list`, `fs.write`, `terminal.run`, `open_terminal.run` | Workspace confinement and approvals in `BoundedExecutor` |
| Analysis | none | No dedicated capability; the default agent loop |
| Delegation | `agent.delegate` (`RiskClass::Executable`), `Delegation`, `SubagentPolicy` | Bounded by `subagents.max_depth`, `max_children_per_agent`, `delegate_timeout_secs`; workers get `ToolRegistry::worker_default()` |
| Task or plan execution | none | No planner or sequencer exists, and none is added |
| Run budgets | `RunLimits` (`max_turns`, `max_tool_calls`, `wall_clock_secs`), `classification.max_calls_per_run` | |
| Cancellation | `RunState::cancel_token`, threaded into the preflight | |
| Classification | `coordinator::{preflight, classify, sanitize, tool, state}` | |
| Provider routing | `jev::route_for_turn` → `jev::inference_route`; `LightweightProvider` (`model` field only) | The Auto guard is in `jev::route` |
| Profile authorization | `ApprovalPolicy` (per profile), `Scope` (API keys) | No per-user ownership |

## Architecture

```text
preflight (route_for_turn ∥ classify)                          coordinator
   │
   ├─ routing_input: legacy_raw │ sanitized ─► Jev route question   (Auto guard first)
   └─ classification (sanitized) ─► Jev classification question
          │
          ▼
configured_registry (+ jev.classify_task when permitted)        unchanged
          │
          ▼
workflow::evaluate(report, &registry, &PolicyEngine, &RunLimits, &Config, cancelled)
   │   reads only — nothing granted, raised, remembered or resized
   ├─ cancelled / not classified / uncertain     ─► no change
   ├─ conversation (default) / analysis (unsupported) ─► no change
   ├─ capability absent ─► not_configured         ─► no change
   ├─ every supporting tool denied ─► unauthorized ─► no change
   ├─ limits too small ─► insufficient_budget     ─► no change
   ├─ automation ─► advisory_only                 ─► no change
   └─ eligible ─► mode=active ∧ workflow_guidance ? append fixed guidance : advisory
          │
          ▼
BoundedExecutor(registry, PolicyEngine) → AgentLoop → provider    unchanged
          │
          ▼
classification.completed (schema 2) + per-profile record
```

## Decision types

`coordinator::workflow`:

- `Workflow`: `conversation`, `research`, `coding`, `analysis`, `automation`,
  `multi_step`. Each maps one-to-one from the classification category.
- `Eligibility`: `eligible`, `default_behavior`, `advisory_only`,
  `unsupported`, `not_configured`, `unauthorized`, `insufficient_budget`,
  `uncertain`, `not_classified`, `cancelled`.
- `WorkflowDecision`: `{ workflow, eligibility, available_tools,
  approval_required, guidance: Option<&'static str>, applied }`. Guidance is
  one of three compile-time constants; nothing from Jev or the message is
  interpolated.

## Capability mapping

| Workflow | Existing capability | Configured when the run has | Usable when the run's policy | Minimum limits (turns, tool calls) | Guidance | Fallback |
| --- | --- | --- | --- | --- | --- | --- |
| conversation | agent loop | always | n/a | n/a | none | default behavior |
| research | retrieval tools | `rag.realtime`, `web.search`, `web.fetch` or `rag.search` | does not deny all of them | 2, 1 | gather and cite evidence with existing retrieval tools | not_configured / unauthorized / insufficient_budget |
| coding | workspace tools | `fs.*`, `terminal.run` or `open_terminal.run` | allows a read tool (`fs.read`/`fs.list`) | 2, 1 | inspect files first; commands and changes need the usual approval | as above |
| analysis | none | — | — | — | none | **unsupported** |
| automation | action tools (any mutating or executable tool, MCP included) | any | does not deny all of them | 2, 1 | none | **advisory only**: never applied |
| multi_step | `agent.delegate` | `agent.delegate`, with bounded delegation (subagents off, or depth ≥ 1 and fan-out ≥ 1) | does not deny it | 3, 2 | outline steps; delegate only where permitted | not_configured / unauthorized / insufficient_budget |

Classification confidence must clear its (version-aware) threshold first, or
the result is `uncertain`. No path enables a tool, raises a limit, relaxes an
approval or adds network access.

## Configuration and rollout

| Setting | Default | Effect |
| --- | --- | --- |
| `platform.jev.classification.mode` | `off` | `off`: no classification and no workflow decision. `shadow`: decisions are evaluated and recorded, never applied. `active`: guidance may apply, if `workflow_guidance` is on. |
| `platform.jev.classification.workflow_guidance` | `false` | Required, together with `active`, for guidance to be applied. |
| `platform.jev.routing_input` | `legacy_raw` for an existing Jev configuration; `sanitized` for a new one | See below. |

Compared with PR 3, `active` alone no longer adds guidance. It needs
`workflow_guidance` as well. This narrows behavior that was never released;
nothing is activated on upgrade.

## Routing privacy migration

`platform.jev.routing_input` selects what legacy model routing sends:

- `legacy_raw`: the message cut to 4,096 characters, unchanged from before.
- `sanitized`: the classification sanitizer's output, bounded by
  `classification.max_input_chars`. If the message cannot be minimized
  safely, nothing is sent and routing keeps the configured model
  (`RouteFallback::UnsafeInput`).

A configuration file with a `platform.jev` block but no `routing_input` loads
as `legacy_raw`. A new configuration (`Config::default`, `lightagent init`)
writes `sanitized`. Changing an existing configuration is an explicit
operator choice:
- `lightagent config set platform.jev.routing_input sanitized`; or
- Settings → Jev → "What model routing sends to Jev".

| Message content | `legacy_raw` | `sanitized` |
| --- | --- | --- |
| Code blocks | sent | replaced by `[code]` |
| Local paths (`/…`, `~/…`, `C:\…`) | sent | `[path]` |
| URLs | sent | `[url]` |
| E-mail addresses | sent | `[email]` |
| Credential-shaped tokens, values after "password"/"token"/… | sent | `[secret]` |
| Digit runs of six or more (phones, IPs, accounts) | sent | `[number]` |
| Key or certificate material | sent | **not sent**: default route |
| Messages over 8,000 characters | first 4,096 sent | **not sent**: default route |
| Terminal-dropped file (attachment) | sent | **not sent**: default route |
| Other private prose | sent | sent if it survives redaction; sanitization is minimization, not authorization |

The two policies can route differently. With identical stand-ins, a decision
that depends on a path or code the sanitizer removes can change (tested in
`jev::tests::the_two_input_policies_can_route_differently`). No claim of
equivalent routing quality is made. Measuring it needs the labeled evaluation
reserved for PR 6.

The Auto guard is checked before any input is prepared, under both policies
(tested). `model: "Auto"` reaches Lightweight unchanged. No provider header
or field was added.

## Before and after

Research request, web tools enabled, Jev classifies `research` at 0.95:

| | System prompt | Tools offered | Event |
| --- | --- | --- | --- |
| mode `off` | baseline | baseline | none |
| `shadow` | baseline | baseline | `workflow_eligibility: eligible`, `workflow_applied: false` |
| `active`, guidance off | baseline | baseline | eligible, not applied |
| `active`, guidance on | baseline + one fixed "Task assessment (advisory)" paragraph | baseline | eligible, `workflow_applied: true` |
| `active`, web tools off | baseline | baseline (no web) | `not_configured`, not applied |

All five cases are asserted from the real provider request in
`serve::routing_tests::workflow_guidance_reaches_the_provider_only_when_active_eligible_and_enabled`.

## Telemetry (`classification.completed`, schema 2)

Schema-1 fields are unchanged. Additive fields:
- `accepted`;
- `workflow`, `workflow_eligibility`, `workflow_capability`;
- `workflow_applied`, which is the same value as `influenced`;
- `available_tools` (usable supporting tools) and `approval_required`;
- `workflow_selection_us` (the policy's own overhead);
- `routing_input` (the policy used when routing prepared an input).

Jev call count, latency and model version are carried as before. No message,
routing input, memory, tool output or secret is included. No accuracy score
is reported. `routing.selected` is unchanged. The per-profile record (`GET
/jev`) carries the same fields; there is no cross-profile access.

## Authorization boundary

Eligibility is an inspection of what the run already has, and is not
authorization. The policy reads `PolicyEngine::evaluate` and never calls
anything that grants, remembers or resizes. The executor is then built from
the same registry and the same policy as without Jev. Asserted by:
- the unchanged provider tool list;
- `workflow::tests::coding_reports_approval_and_never_relaxes_it`;
- `workflow::tests::the_registry_is_never_changed`.

Profile `toolsets` remain unenforced and are not used as evidence of
anything. See `profile-toolset-enforcement-proposal.md`.

## Rollback

- Operational:
  - `workflow_guidance = false` stops guidance;
  - `mode = off` stops classification and workflow decisions;
  - `routing_input = legacy_raw` restores the original routing input.

  None of these affects the Auto guard.
- Code: revert the PR. All fields are additive, and older builds ignore
  them.

## Remaining for PR 6

- A labeled, privacy-safe evaluation set: categories, mixed tasks, sensitive
  and ambiguous inputs. Use it for confidence calibration per Jev version,
  confusion matrices, and a raw-versus-sanitized routing comparison.
- The protected `jev-live` contract workflow (`TYPESAFE_API_KEY`), separate
  from PR checks.
- Adversarial suites beyond the current unit coverage (prompt injection
  through task text, provider-hint manipulation).
- Terminal workflow decisions: the terminal currently reports classification
  only.
- Enforced profile toolsets, if approved separately.
