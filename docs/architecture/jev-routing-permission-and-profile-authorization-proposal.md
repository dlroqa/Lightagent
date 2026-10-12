# Proposal: routing permission and profile authorization

Status: **proposal only.** Nothing here is implemented. It must be approved
separately, as its own reviewable change. It is not part of the Jev
evaluation work, and the evaluation work does not solve any issue listed
here.

## Open issues

These are the current, unresolved boundaries. PR 6 measured and documented
them; it changed none of them.

| # | Issue | Evidence | Effect today |
| --- | --- | --- | --- |
| 1 | **Legacy Jev model routing has no per-profile external-processing permission.** Classification checks `classification.allowed_profiles`; routing checks nothing per profile. | `jev::route`; routing benchmark (`priv-009` is routed under both input policies); `privacy_*` tests | Any profile's message is sent to TypeSafe when routing is enabled and the model is not `Auto` or caller-chosen |
| 2 | **`legacy_raw` routing transmits private-key material, attachments and sensitive content**, up to 4,096 characters. | Routing benchmark: 21 pattern-protected markers sent under `legacy_raw`, 0 under `sanitized`; `priv-007` sends `OPENSSH` key text raw | Existing configurations keep `legacy_raw` (by design, until a migration is approved) |
| 3 | **Sanitization cannot detect every kind of private prose.** It is pattern-based minimization. | `priv-010` (a health condition) reaches Jev under both policies; `privacy_sensitive_prose_*` | The profile permission is the only control for prose |
| 4 | **Admin-only configuration is not per-user profile ownership.** `lightagent serve` is open on loopback; otherwise it takes one `--key-env` key, which always holds `Admin`. No key is bound to a profile, so any caller may start a run as any profile. | `lightagent-api/src/auth.rs` (`Scope`, `AuthConfig`); `serve.rs` (`AuthConfig::keyed(key, [Scope::Admin])`) | Profile-level Jev permissions protect a profile's data only as far as every caller who can run that profile is trusted |
| 5 | **`AgentProfile.toolsets` is not enforced at runtime.** | `profile-toolset-enforcement-proposal.md` | Every profile sees the globally configured tool list |

## Interim recommendation (no code change)

For sensitive or untrusted profiles, **disable external legacy Jev routing**
until a routing permission exists. Routing is global, so the operator choice
is per installation:

```bash
lightagent config set platform.jev.allowed_models ""
lightagent config set platform.jev.allowed_profiles ""
```

With both routing allowlists empty, routing takes its `no_routes` path and
sends nothing. Classification is unaffected and keeps its own per-profile
gate (`mitigation_empty_routing_allowlists_*` proves both). Where routing
must stay on, set `platform.jev.routing_input = sanitized`, accepting that
prose is still sent (issue 3). Lightagent does not change either setting
automatically.

## Proposed changes

Each step is separately reviewable, additive and reversible. No existing
configuration changes meaning at any step.

### Step 1: routing permission (issues 1, 2)

- Add `platform.jev.routing_profiles: Option<Vec<String>>`. `None`, the
  default for every existing configuration, means today's behavior (all
  profiles). `Some(list)` permits only the listed profiles; `Some([])`
  permits none.
- `jev::route` checks it **before** `routing_state`, so nothing is derived
  from a non-permitted message. The fallback code is `profile_not_permitted`,
  as for classification.
- `PreflightRequest` already carries the profile; no API or SSE contract
  change is needed. Diagnostics report the routing fallback code.
- The Settings screen shows routing permission beside the classification
  permission, and warns when `routing_profiles` is `None` and
  `routing_input` is `legacy_raw`.
- Tests: permitted and non-permitted profiles; `None` identical to today;
  checked before any request; the Auto guard unchanged.

### Step 2: routing privacy default (issue 2)

Only after Step 1 and a live raw-versus-sanitized comparison:

- report-only: show operators on `legacy_raw` what their input policy sends;
- opt-in migration command, never silent;
- a later release may default *new* configurations' `routing_profiles` to
  `Some([])` (routing off until profiles are chosen).

### Step 3: profile ownership (issue 4)

- Add configurable API keys with an optional profile binding (`profiles:
  [..]`). `--key-env` and loopback keep today's semantics. A bound key may
  start runs, read sessions and read Jev diagnostics only for its profiles;
  `Admin` remains unrestricted.
- The run manager checks the binding where it resolves `StartRun.profile`,
  before any preflight.
- Tests: a bound key cannot run as, read the sessions of, or read the Jev
  status of another profile; unbound keys unchanged.

This is the minimum needed for per-profile Jev permissions to mean
per-user protection. It is not a general access-control rewrite.

### Step 4: toolset enforcement (issue 5)

As specified in `profile-toolset-enforcement-proposal.md` (report-only, then
opt-in, then default-on).

### Not addressed by any step

Pattern redaction will remain incomplete (issue 3). The control for prose is
the permission boundary from Steps 1 and 3, not better regular expressions.

## Rollback

Every step is a new optional field or an opt-in setting. Removing it
restores today's behavior; reverting the change restores today's code.
