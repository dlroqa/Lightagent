# Jev model-routing permission

Status: **implemented** (item 1 of
[`jev-security-implementation-plan.md`](jev-security-implementation-plan.md)).
It closes issue 1 of
[`jev-routing-permission-and-profile-authorization-proposal.md`](jev-routing-permission-and-profile-authorization-proposal.md)
for configurations that opt in to `strict`, and for every new configuration.

Three separate questions concern profiles and Jev. This change answers only
the second:

| Question | Setting | Meaning |
| --- | --- | --- |
| Which routes may Jev choose? | `platform.jev.allowed_models`, `platform.jev.allowed_profiles` | Route options offered to Jev. `allowed_profiles` lists profiles whose *inference model* Jev may pick; it grants no permission. Unchanged. |
| **May this profile send task content to Jev for model routing?** | **`platform.jev.routing_permission`, `platform.jev.routing_allowed_profiles`** | **New.** Checked before any routing input is prepared. |
| May this profile send a sanitized task description for classification? | `platform.jev.classification.allowed_profiles` | Unchanged. Independent of the routing permission. |
| Which users may act through a profile? | none | Not implemented. See [Future work](#future-work). |

## Policy and defaults

`routing_permission` takes one of two values:

- `strict`: a profile may route only if its id is in
  `routing_allowed_profiles`. A missing or empty list permits no profile.
  The match is exact: no case folding, trimming or prefixes, and an invalid
  id matches nothing.
- `legacy_all_profiles`: the **compatibility policy**. Every profile may
  route, including profiles created later. This is the behavior from before
  the permission existed. It is kept only for configurations that predate
  the setting, and it is reported as compatibility everywhere routing is
  shown.

| Configuration | `routing_permission` | `routing_allowed_profiles` | Effect |
| --- | --- | --- | --- |
| New (`lightagent init`, no file, or a file without a `platform.jev` section) | `strict` | `[]` | No profile routes until granted |
| Existing file with a `platform.jev` section written before this release | `legacy_all_profiles` (read from the missing field) | `[]` | Unchanged: every profile routes |
| Unknown policy value, wrong type, or an invalid profile id | n/a | n/a | Refused: the file does not load, or the save is rejected. Never read as allow-all |
| Terminal turn whose configuration file cannot be re-read | `strict` | `[]` | Denied for that turn |

This follows the precedent `routing_input` set: the field-level default
applies only where the field is missing from an existing Jev section, while
`Default` (new configurations) is the conservative value. Saving a
configuration writes `routing_permission` explicitly, so the compatibility
policy becomes visible in `config.json` and `lightagent config show`.

A routing request is made only when all of these hold, checked in this order
in `jev::route` (`crates/lightagent/src/jev.rs`):

1. No model was chosen by the caller (`explicit_model`).
2. The effective model is not `Auto` (trimmed, any case). An `Auto` model
   never reaches the permission check: Lightweight receives `model: "Auto"`
   whatever the permission says.
3. Jev is enabled.
4. **The run's resolved profile has the routing permission.** On denial
   the fallback is `profile_not_permitted`. Nothing has been read from the
   message yet.
5. A route exists (`no_routes`), the endpoint is configured
   (`no_endpoint`), and the route count is within bounds.
6. The run is not cancelled (`cancelled`). This is checked before the input
   is prepared. The adapter still stops waiting for a request that is
   cancelled in flight.
7. The input policy produces safe input (`sanitized` refusals are
   unchanged; `legacy_raw` is unchanged).

Every fallback keeps the configured model. That is the existing routing
fallback, unchanged.

## Source-grounded integration

```
API  POST /runs ─┐                         (lightagent-api manager)
ACP  session/prompt ─┤  LightweightRunFactory::run          serve.rs
                     │    ConfigStore::load()   ← per run: grants and revocations apply to the next run
                     │    resolve_profile()     ← the server resolves the profile; the permission binds to profile.id
                     │    inference_for_request() ─┐
Terminal turn ───────┼─ turn_preflight()           │        chat.rs
                     │    with_current_routing_permission()  ← re-reads only the permission per turn
                     ▼                              ▼
             coordinator::preflight(PreflightRequest { config, profile, … })      coordinator/mod.rs
                ├─ jev::route_for_turn(&config.platform.jev, profile.id, …)        jev.rs
                │     route(): explicit → Auto → enabled → routing_grant(profile) ─✗→ profile_not_permitted (no input, no request)
                │              → routes/endpoint → cancelled? → routing_state() → JevClient::evaluate()
                └─ classify::classify(config, profile.id, …)  (own gate: classification.permits_profile)
             Preflight { model, routing_status, routing_permission, routing_fallback, … }
                ├─ serve.rs:  AgentEvent::RouteSelected { …, permission, fallback } → SSE routing.selected
                └─ chat.rs:   one terminal line on the first denied turn of a session
JevConfig::routing_grant(profile) → Allowed | Compatibility | Denied        lightagent-core/src/config.rs
```

What changed, by function:

| Function | Change |
| --- | --- |
| `lightagent_core::JevConfig` | New fields `routing_permission` and `routing_allowed_profiles`; `routing_grant()` |
| `Config::validate` | Rejects invalid ids in `routing_allowed_profiles` |
| `jev::route_for_turn`, `jev::route` | Take the profile id; check permission after the Auto guard and enablement, before anything is derived from the message; check cancellation before preparing input; report `permission` |
| `coordinator::preflight` | Passes `profile.id` and returns `routing_permission` and `routing_fallback` |
| `AgentEvent::RouteSelected`, `sse::data` | Optional `permission` and `fallback` fields. The payload carries them only when set, so the original three fields are unchanged |
| `serve.rs` run factory | Emits them |
| `chat.rs` terminal loop | `with_current_routing_permission()` before each turn; denial notice |
| `coordinator::status` (`GET /jev`) | New `routing` object: `permission_policy`, `compatibility`, `profile_permission`, `allowed_profiles` |
| `lightagent-api` settings | `routing_permission` and `routing_allowed_profiles` in `GET`/`PUT /settings`, still `Admin` only; an older client that omits them leaves them unchanged |
| CLI | `config get`/`set` for both keys; `config jev-routing-report` (dry run) |
| Web UI | A Settings section "Model routing permission" beside "Task classification"; a row in the Tools card; the Agent screen's routing line |

Unchanged: the shared adapter (`lightagent-jev`), classification, the
sanitizer, `routing_input`, the tool registry, approvals, the
`jev.classify_task` tool, thresholds, classification `off` by default, and
Lightweight.

## Privacy

- Denial happens before `routing_state()`. No routing input is built,
  serialized or sent, under `legacy_raw` and `sanitized` alike.
- Denial diagnostics carry fixed vocabulary only: `denied`,
  `profile_not_permitted` and the profile id. They never carry message text,
  keys, attachments or sanitized payloads.
- **A permitted profile under `legacy_raw` still sends the message itself,
  up to 4,096 characters, including anything sensitive in it.** For
  sensitive deployments, use `sanitized` where it suits the workload, or do
  not grant routing to sensitive profiles, or disable routing. Sanitization
  is pattern-based minimization and does not guarantee privacy: prose such
  as a health condition is not recognized (`priv-010`).
- A profile permission is not user identity. Anyone who can run a profile
  can use its permission. Today that means any API caller on loopback or
  holding the single `Admin` key.

## Compatibility and migration

**Effect on existing installations.** None at upgrade. An existing routing
configuration loads as `legacy_all_profiles`, and every profile routes
exactly as before. Settings shows a compatibility notice. `GET /jev` reports
`compatibility: true` and `profile_permission: "compatibility"`.
`routing.selected` carries `permission: "compatibility"`.

The compatibility policy is not meant to be permanent. It exists so the
upgrade breaks nothing. The operator moves to `strict` when ready.

**Migration (operator-controlled, never automatic):**

1. Dry run. It reads only the configuration and the profile ids on disk,
   and changes nothing:

   ```bash
   lightagent config jev-routing-report        # or --json
   ```

2. Grant the profiles that should keep routing:

   ```bash
   lightagent config set platform.jev.routing_allowed_profiles lead,research
   ```

   Under the compatibility policy this changes nothing yet. The report's
   "Under strict" column shows the result.

3. Enforce:

   ```bash
   lightagent config set platform.jev.routing_permission strict
   ```

   In the Web UI: Settings → Jev → Model routing permission. Check the
   profiles, then choose Strict.

API and ACP runs read the configuration at each run, and the terminal
re-reads the permission before each turn. A grant or revocation therefore
applies to the next routing request without a restart.

**Dry-run output for the synthetic fixture** used in
`routing_report_lists_who_strict_would_deny_and_changes_nothing`
(`crates/lightagent/src/lib.rs`). The fixture is an existing routing
configuration with profiles `lead`, `research` and `sensitive` on disk,
`lead` granted ahead of the switch, and a stale grant for `retired`. The
test asserts this exact text:

```
Jev model-routing permission (dry run; nothing was changed)

Policy: legacy_all_profiles (compatibility: every profile may send task content for routing)
Routing active: yes
Routing input: legacy_raw

Profile                        Now            Under strict
default                        compatibility  denied
lead                           compatibility  allowed
research                       compatibility  denied
sensitive                      compatibility  denied

Listed but not found on disk: retired

Denied under strict: default, research, sensitive

To enforce, grant the profiles that should keep routing, then switch the policy:
  lightagent config set platform.jev.routing_allowed_profiles <profile,...>
  lightagent config set platform.jev.routing_permission strict
```

The report holds profile ids and fixed vocabulary only. It does not read
sessions or messages, and does not print the endpoint or key.

**Trade-off.** Privacy by default for existing installations would mean
denying routing at upgrade, which silently changes the model that existing
profiles get. This change keeps existing behavior under an explicitly named,
reported policy and makes `strict` the default for everything new. The Web
UI can move an installation from compatibility to strict. It never offers
the way back: re-widening disclosure to every profile requires an explicit
`lightagent config set platform.jev.routing_permission legacy_all_profiles`.

## Tests

| Requirement | Test |
| --- | --- |
| New profile denied by default under strict | `fresh_configuration_grants_no_profile_routing_permission` (core); `an_unpermitted_profile_makes_no_routing_request_under_either_input_policy` (jev) |
| Explicitly permitted profile routes | `a_permitted_profile_routes_as_before_and_a_revoked_one_stops` |
| Unpermitted profile, zero routing requests | the above; `api_and_acp_runs_route_only_for_a_currently_permitted_profile` (serve); `strict_routing_permission_stops_profile_gated_markers_under_both_input_policies` (benchmark) |
| Revocation blocks the next request | jev, serve (on-disk revocation between runs), `terminal_routing_obeys_the_current_profile_permission` (chat) |
| Invalid configuration cannot allow routing | `invalid_routing_permission_configuration_never_allows_routing` (core), `routing_permission_settings_round_trip_and_need_the_administrator` (API), `strict_routing_permission_allows_only_exact_listed_profiles`, unreadable file in the terminal test, render state 18 |
| Legacy compatibility documented and tested | `existing_routing_configuration_loads_under_the_identified_compatibility_policy`, `the_compatibility_policy_routes_every_profile_and_says_so`, the unchanged benchmark under `legacy_all_profiles` |
| Sanitized and legacy raw obey permission | the jev and benchmark tests run both input policies |
| Classification and routing permissions independent | `classification_permission_never_grants_routing`, `routing_permission_and_classification_permission_are_independent` (coordinator), core and status tests |
| Auto unchanged | `auto_and_explicit_models_are_unchanged_by_the_routing_permission`; existing Auto tests, including the API/ACP run test |
| API, ACP and CLI share the check | serve test (API and ACP through the run factory), chat test (terminal), all through `coordinator::preflight` |
| No prompt or secret in denial diagnostics | serve test (no marker in any event), `RouteSelected` payload test, the dry-run report test |
| Cancellation before dispatch | `cancellation_before_dispatch_prepares_and_sends_nothing`; existing in-flight cancellation tests |
| Authorized non-Auto routing unchanged | every existing routing test, now run under an explicit grant |
| Tool registry, approvals, explicit tool unchanged | unchanged code paths; existing tool, workflow and approval tests |
| Settings API rejects unauthorized modification | `routing_permission_settings_round_trip_and_need_the_administrator` |
| Existing configuration loads and migrates predictably | core compatibility test, CLI report and key tests |
| Rendered UI | `e2e/jev.mjs` states 15–19: denied, allowed (granted with the keyboard-operable checkbox), compatibility to strict, configuration errors, revoked |

## Rollback

- **Configuration rollback:** set `routing_permission` back, or edit the
  list. Revocation is always a single setting.
- **Code rollback** (reverting this change): older builds ignore the two
  unknown fields, and routing returns to the old behavior with no
  per-profile gate, which is what `legacy_all_profiles` does today. Before
  reverting, an operator on `strict` with sensitive profiles must apply the
  interim mitigation from the proposal: clear `platform.jev.allowed_models`
  and `platform.jev.allowed_profiles`, or disable Jev. Otherwise the revert
  re-enables external routing for every profile. Nothing in this change
  re-enables disclosure automatically.

## Known limitations

- The permission is per profile, not per user (issue 4 of the proposal).
- Under `legacy_raw`, a permitted profile's raw message is sent (issue 2).
- The compatibility policy keeps today's exposure until the operator
  switches. The dry run and the Settings notice make it visible; nothing
  forces the switch.
- The terminal's denial notice is printed once per session, and the API's
  on every run (`routing.selected`).
- Revocation does not cancel a routing request already in flight. It
  applies from the next request.

## Future work

Each item is a separate PR:

1. **User identity and profile ownership** (plan item 3): configurable API
   keys bound to profiles, checked where the manager resolves
   `StartRun.profile`, so that a profile permission means per-user
   protection.
2. **Runtime toolset enforcement** (plan item 4,
   `profile-toolset-enforcement-proposal.md`): report-only, then opt-in,
   then default-on.
3. Safer `legacy_raw` handling (plan item 2) and tenant-safe diagnostics
   (plan item 5).
