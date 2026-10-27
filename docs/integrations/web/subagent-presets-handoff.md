# Web handoff: selecting a capability preset at provisioning

**Status: PENDING.** This is a proposal for the next revision of LA-WEB-PROVISIONING/1, after r4. The Web repository owns that spec and its fixtures. **Nothing here is implemented or activated.**

What stays unchanged:
- The `assistant` template and the r4 wire contract. Lightagent still accepts exactly r4.
- Provisioning stays inactive.

Today an operator gives a profile a preset with `lightagent profile preset <id> research@1`. No website can select one. There is also no per-tenant execution sandbox, so untrusted users can only ever be given the read-only researcher (`agent.research`). The coder, reviewer and tester stay `permission_denied` (`requires_isolation`) for them.

## Is a protocol extension necessary? (reviewed 2026-10-21)

Only if the **website** must choose a preset per profile. There are three ways to give provisioned profiles the read-only researcher:

| Option | Wire change | Who decides | Fit |
| --- | --- | --- | --- |
| A. The operator runs `lightagent profile preset <id> research@1` after provisioning | none | the operator, per profile | Works today, but manual: it does not scale and leaves a window with no preset |
| B. An operator-set **default preset in the provisioner registration**, applied by `materialize` in the create transaction | none on the wire. The ledger records it, which needs a ledger schema bump and an L1 continuity check. | the operator, per provisioner | Every profile a provisioner creates gets the same capabilities. No Web change. |
| C. A `capabilityPreset` member, r5 (below) | yes: new member, codes and fixtures | the website, per request, within the operator's allowlist | Needed only when different users of one provisioner need different presets |

There is one preset today (`research@1`), and untrusted users cannot be given more than research until a tenant execution sandbox exists. So **option B covers every case that can currently be granted, without a protocol change**. Option C, r5, is necessary only once a second grantable preset exists and the website must choose between them per user. The r5 design below is kept so it does not have to be reinvented, but it should not be frozen before then.

Either way, Lightagent needs:
- **Authorization**: a preset is applied only from the operator-set registration (B) or allowlist (C). The provisioner credential stays restricted to exactly `{provision}`, and nothing on the wire can name toolsets, approval, models, endpoints or credentials.
- **Migration**: the provisioning ledger's operation records `preset`. That is a schema bump with an L1 continuity check, the old schema read as "no preset", and no rewrite of existing records. The registration gains `default_preset` (B) or `presets` (C), absent by default, which keeps current behavior.
- **Paired tests** (Lightagent with Web, on the frozen protocol bytes): a create with a preset materializes it in the same transaction; replay and status report the **recorded** preset; revoke still removes ownership and leaves the preset nothing to grant; a preset never widens beyond `research@1` or weakens approval below balanced; an untrusted run gets `agent.research` only when `subagents.remote_research` is on, and never `agent.delegate`.
- **Specialist selection** (`subagents.selection`) does not depend on the website. An untrusted run's plan can only name the researcher, through `agent.research`, because its other roles are `requires_isolation`. Activating it for provisioned profiles is an operator configuration (`selection_profiles`), not a wire member. Listing many tenant profiles there does not scale. A later preset could carry an operator-owned "selection active" flag, which would be option B or C again.

None of this is implemented, and nothing was changed in the Web repository. Lightagent-only tests do not show website integration.

## Why a separate member, not a new template

`template` decides a profile's identity defaults: persona, strict approval, limits. A preset decides capabilities: toolsets and approval, from an operator-owned, versioned registry (`provisioning::PRESETS`). Keeping them separate:
- leaves `assistant` and every r4 vector valid;
- lets status report the preset;
- lets an operator withdraw a preset from a provisioner without touching templates.

## Proposed wire change (r5)

### `profile.create` payload: one new optional member

| Member | Rule | Error (class) |
| --- | --- | --- |
| `capabilityPreset` | Optional. When present: a string of 1–32 characters matching `^[a-z0-9-]+@[0-9]+$`, naming a preset in the harness registry. Absent: exactly r4 behavior. | `INVALID_PRESET` (payload, `400`) when malformed or unknown to the harness |

- §8.4 (forbidden content) is unchanged. The preset is an identifier only. Tools, toolsets, approval policy, models, providers, endpoints, paths and credentials stay forbidden (`UNKNOWN_FIELD`).
- The member is covered by the signature (§6) and by the request fingerprint. A resend with a different preset is the existing idempotency conflict.

### Admission (§10.2): one new merits check, after the template check

- The provisioner registration gains `presets: [..]`. It is operator-set and **empty by default**.
- A preset that is not in the registration fails with `PRESET_NOT_ALLOWED` (merits, final), in the same step as `ISSUER_NOT_ALLOWED` and the template check (step 16).

### Execution (§10.3)

- `materialize` applies the preset **inside the same create transaction** that writes the template profile, so the profile never exists without it.
- The ledger operation records `preset`.
- A preset that has disappeared from the registry by execution time records the failure code `invalid_preset` (new in §11), mirroring `invalid_template`.

### Responses (§12.2)

- The operation result gains `"preset": "<id>" | null`. It is `null` when none was requested, which r4 Web must ignore as an unknown result member.
- A status or replay answer reports the **recorded** preset, not the profile's current toolsets.

### New fixtures (Web)

- `valid-create-preset-research`
- `create-preset-malformed` (`INVALID_PRESET`)
- `create-preset-unknown` (`INVALID_PRESET`)
- `create-preset-not-allowed` (`PRESET_NOT_ALLOWED`)
- `create-preset-forbidden-sibling`: a `toolsets` member next to the preset, answered `UNKNOWN_FIELD`
- `create-200-failed-invalid-preset`
- status vectors carrying `preset`
- a redelivery vector where only the preset differs (idempotency conflict)
- an updated MANIFEST digest

## Lightagent work, once Web freezes r5

None of these steps is done.

1. Mirror the r5 bytes. Update `COMPATIBILITY_DIGEST` and the `lightagent-protocol-conformance` verifier for the new member, codes and vectors.
2. Make these core provisioning changes:
   - `ProvisionRequest.preset: Option<String>`;
   - the registration's `presets`;
   - the ledger operation's `preset` with a schema bump;
   - in `materialize`, `apply_preset` within the transaction;
   - the `invalid_preset` failure;
   - `status_of` reporting it;
   - an L1 continuity check, since the ledger schema changes.
3. In the gateway parser (L2), accept `capabilityPreset` and keep every other §8.4 field `UNKNOWN_FIELD`.
4. Add tests:
   - every new vector on real stores;
   - a preset never widens beyond `research@1`'s toolsets, and approval is never weaker than balanced;
   - an untrusted run of the profile is offered `agent.research` only when `subagents.remote_research` is on, and never `agent.delegate`;
   - revoke still removes ownership, and a preset grants nothing to a revoked profile.
5. Get operator authorization to add a preset to a provisioner registration. It is a separate, explicit action.

## What the website must never do

- Send tools, toolsets, approval policy, model ids, provider URLs, paths or secrets.
- Assume a preset makes coding, review or testing available to its users. Those need a tenant sandbox that does not exist.
- Infer harness state from this document. Only signed operation responses count.

## Readiness for the website

`GET /api/lightagent/v1/workers` (scope `tools_read`) reports, for the caller's own profile as an untrusted run of it, each role's state and reason codes. It never names another profile. A website can show these, for example "Research: ready" or "Coding: not permitted (requires isolation)", but must treat them as a snapshot: every run rechecks.
