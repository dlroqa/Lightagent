# Profile provisioning for the account-control service

Status: **implemented, not activated.** The service layer
(`lightagent_core::provisioning`) is always compiled, but nothing in the
`lightagent` binary constructs it. The HTTP contract
(`lightagent_api::provisioning`) sits behind the cargo feature
`provisioning-api`. No release or production build enables that feature,
and `router()` never mounts the routes, so a running server answers every
`/api/lightagent/v1/provisioning/...` path with the ordinary
unknown-endpoint `404`. Multi-user mode is still not activatable.

The Cloudflare Workers control website manages external accounts.
Lightagent itself verifies the website's provisioning authority, establishes
profile ownership in its own identity store, enforces every runtime
permission, and protects credential access. **A provisioning permission
never becomes agent or operator authority.**

## 1. Source integration points (audit)

| Concern | Where it lives | How provisioning uses it |
|---|---|---|
| Durable identity authority | `identity::store::IdentityStore` (`store.rs:359` `transact`, `:332` `effective_ownership`) | Every ownership change is one `transact`, idempotent by operation. Authority is re-read on every call |
| Principals, roles, sources | `identity::{Principal, Role, PrincipalSource}` (`mod.rs:108`) | Only `PrincipalSource::Credential` is admitted. The legacy key, local process and compatibility sources are refused |
| Capabilities, restrictions | `identity::{Capability, Restriction, authorize_capability}` (`mod.rs:176`, `:319`) | `Provision` is operator-only. The provisioning credential must be restricted to exactly `{provision}` |
| Credentials | `identity::CredentialStore::caller_for_at` (`credential.rs:233`) | The presented credential is re-resolved by key id under the ledger lock, so revocation, rotation and expiry apply to the next call |
| Ownership records | `OwnershipRegistry`, `OWNER_DEFAULT` (`ownership.rs:32`) | `assign_owner` only, never `reassign_owner` (that needs `administer`). The owner gets the C6 minimum bundle |
| Profile marker and binding | `store::BINDING_FILE` (`store.rs:55`), `effective_ownership` | Ownership counts only while the binding marker matches. Provisioning adds its own `.provisioning-operation` marker |
| External identity | `store::ExternalIdentity` (`store.rs:72`), `trust_issuer` (`:714`, `administer`) | Issuers must be operator-registered. One external identity maps to at most one principal |
| Profile creation and deletion | `ProfileStore::create` (`profile.rs:310`) / `delete` (`:374`) | `create` is check-then-write, so provisioning stages the profile and renames it into place instead. `delete` leaves ownership records stale but inert (binding check) |
| Transactional updates | `store::{write_durable, open_private}` (temp file, `fsync`, rename, directory `fsync`) | Reused for the ledger. Its lock is taken before the identity lock |
| Audit | `identity::AuditRecord` (fixed codes) and the vault's audit pattern | The ledger has its own `audit.jsonl` (codes and identifiers only). Identity changes are audited by the store as before |
| Revocation supervisor | `RunAuthorization` (`revocation.rs:42`), `RunManager::revoke` (`manager.rs:933`) | Revoking access removes ownership in the store. Running runs stop at their next revalidation (PR #22/#23) |
| API auth and routing | `AuthConfig::{multi_user, caller, authorize}` (`auth.rs:434`, `:478`, `:528`), `router()` (`lib.rs:73`), `access()` (`lib.rs:320`) | Operator-role callers are checked by capability on every route, so a `{provision}`-only credential is refused everywhere (tested). The provisioning routes are a separate, feature-gated `Router` |
| Startup and configuration | `serve.rs` builds only `AuthConfig::keyed` or `open` | Unchanged. No startup path builds multi-user auth or a `ProvisioningService` |
| CLI | `identity_admin.rs` (`lightagent identity`), `lightagent profiles` | Unchanged. Provisioner registration is a library call only in this PR |
| Tool policy for users | `policy.rs:234` (`RemoteUser` is always `strict`) | Under `strict`, an empty `toolsets` grants nothing, which is the template default |

The earlier contract proposal (`profile-ownership-architecture.md`,
"Provisioning interface") defined the credential as role `operator` plus
restriction `{provision}`, server-derived principal ids,
`PUT /profiles/{profile_id}` with `Idempotency-Key`, server-defined
templates, and `409` on conflict. This implementation keeps all of those.
It narrows the proposal in four ways, which §11 lists.

## 2. Architecture

```text
 Cloudflare Workers control website                       Lightagent installation
 ──────────────────────────────────                       ─────────────────────────────────────────────
 signs the user in (its own IdP) ──┐
                                   │ HTTPS, Bearer la_<key>_<secret>      (feature `provisioning-api`;
                                   │ Idempotency-Key, JSON ≤ 8 KiB         not mounted in router())
                                   ▼
                          ┌─────────────────────────────┐
                          │ provisioning::routes        │ AuthConfig::caller ─► IdentityAuthority
                          │  PUT profiles/{id}          │   (persistent: re-reads identity/state.json)
                          │  GET operations/{op}        │
                          │  POST operations/{op}/revoke│
                          └──────────────┬──────────────┘
                                         ▼ spawn_blocking
                          ┌─────────────────────────────────────────────────────────────┐
                          │ ProvisioningService            ledger lock (cross-process)  │
                          │ 1 admit: credential re-read by key id; role operator;       │
                          │   restriction == {provision}; principal active;             │
                          │   registered, unrevoked provisioner                         │
                          │ 2 validate: issuer, subject, profile id, name, template     │
                          │ 3 idempotency: (provisioner, key) → op, by fingerprint      │
                          │ 4 begin: issuer allowed + trusted; template allowed; quota; │
                          │   profile id unknown everywhere; principal resolved/derived │
                          │ 5 materialize: staging/<op> ─rename─► profiles/<id>         │
                          │ 6 bind: ONE identity txn = create principal + assign owner  │
                          │   + binding marker (idempotent key provisioning:<op>)       │
                          └───────┬───────────────────────┬───────────────────┬─────────┘
                                  ▼                       ▼                   ▼
                     <home>/provisioning/       <home>/profiles/<id>/   <home>/identity/
                       state.json (ledger)        profile.json, SOUL.md   state.json (principals,
                       audit.jsonl                .provisioning-operation credentials, ownership,
                       staging/  quarantine/      .identity-binding       bindings, issuers)
                                                                          audit.jsonl
 Runtime (unchanged): user credential ─► authorize(capability, profile, effective ownership)
                      ─► RunAuthorization (revalidated) ─► strict tool policy, profile vault grants
```

## 3. Who may provision

A **provisioner** is an identity-store principal with:

1. role `operator` (only operators may hold `provision`; users cannot, by
   construction);
2. a credential whose restriction is **exactly** `{provision}`. An
   unrestricted operator credential, or one with any further capability,
   is refused (`credential_not_provisioning_only`), so a full operator key
   can never be pasted into the website by mistake;
3. status `active`;
4. a **registration** in the provisioning ledger, made by an operator holding
   `administer` (`register_provisioner`). The registration names the issuers
   it may assert subjects for (each also trusted in the identity store), the
   templates it may use, and a quota of live profiles.

All four are read from the durable stores on every call, under the ledger
lock. Refused: the legacy `--key-env` key, the terminal/ACP local operator,
the single-user compatibility caller, user credentials, revoked, expired or
rotated-away credentials, suspended principals, and withdrawn
registrations.

### Authorization matrix

| Action | Provisioning credential (`{provision}`, registered) | User (owner of P) | Operator (`administer`) |
|---|---|---|---|
| Provision a profile for a subject of an allowed issuer | **yes** (within quota, template, credential profile limit) | no | not through this boundary (CLI/library) |
| Read status of its own operations | **yes** | no | ledger file |
| Read another provisioner's operation | no (`404 unknown_operation`) | no | ledger file |
| Revoke access to a profile it provisioned | **yes**, while it still owns the binding | no | yes (`remove_profile`) |
| Revoke a profile an operator reassigned | no (`409 ownership_changed`) | no | yes |
| Reassign an existing profile | no | no | yes (`reassign_owner`) |
| Provision over an existing / operator-owned / stale-owned id | no (`409 profile_unavailable`) | no | n/a |
| Start runs, read run status or events, cancel | no (`403`) | own runs (C6) | yes |
| Read sessions, conversations | no (`403`) | own (C6) | yes |
| Tool catalog, invoke tools | no (`403`) | per profile policy (`strict`, no tools by default) | yes |
| Jev diagnostics / verify | no (`403`) | no without a grant | yes |
| Provider view, provider or remote credentials | no (`403`) | user view nulls; vault grants by operator only | yes |
| Settings, global configuration | no (`403`) | no | yes |
| Create operators, trust issuers, grant capabilities | no (`MissingCapability`) | no | yes |
| Register / withdraw provisioners, reconcile | no (`operator_required`) | no | yes |
| Issue user API credentials | not exposed by this boundary (§13) | no | yes (CLI) |

"`403`" rows are enforced by the existing production router and are tested
route by route with a `{provision}`-only credential
(`provisioning_boundary.rs`).

## 4. Identity mapping

```text
(issuer, subject)                         external identity (from the website)
   │  issuer ∈ identity.trusted_issuers   (operator: trust_issuer, administer)
   │  issuer ∈ provisioner.issuers        (operator: register_provisioner)
   ▼
existing principal with that external identity?  ── yes ─► that principal (must be an active user)
   │ no
   ▼
derived id = "ext-" + hex(SHA-256("lightagent-external-principal-v1\0" ‖ issuer ‖ 0 ‖ subject))[..40]
   │  must be free; created as role `user` with the external identity, in the binding transaction
   ▼
principal ─owns─► profile (assign_owner + .identity-binding)   ◄── operation <op> (ledger)
```

- **Never trusted from the body.** The request carries no principal id,
  owner, role or capability. Unknown fields are refused. A name such as
  `https://accounts.google.com` is not trusted unless an operator registered
  it, and only for provisioners allowed to use it.
- **Stable.** The same `(issuer, subject)` always resolves to the same
  principal. An existing mapping always wins, and a mapping is never
  re-pointed by provisioning. A different issuer with the same subject is a
  different person.
- **What Lightagent verifies.** It verifies the *website* (its credential,
  and its registration for that issuer). It does not verify the user's
  sign-in: the website does that and vouches for the subject. The user's
  website session token is never sent to Lightagent or to a profile.
- **Subjects** are 1–255 printable ASCII characters (an OIDC `sub`).
  Issuers are `https://` URLs of printable ASCII, at most 256 bytes, without
  userinfo, query or fragment, compared byte for byte.

## 5. API contract

All paths are under `/api/lightagent/v1/provisioning`. Authentication:
`Authorization: Bearer la_<16 hex>_<64 hex>`. Bodies: `application/json`,
at most 8 KiB, unknown fields refused. Errors:
`{"error": "<fixed text>", "code": "<code>"}`.

### `PUT /profiles/{profile_id}`

Headers: `Idempotency-Key` (required, 8–128 of `[A-Za-z0-9._:-]`).

```json
{
  "issuer": "https://accounts.control.example",
  "subject": "user-123",
  "display_name": "Research assistant",
  "template": "assistant"
}
```

| Field | Rule |
|---|---|
| `profile_id` (path) | `^[a-z0-9][a-z0-9_-]{0,63}$`, not `default`, and unknown to the profile store, the identity store and the ledger |
| `issuer` | registered and allowed (see §4) |
| `subject` | 1–255 printable ASCII |
| `display_name` | 1–64 characters, no control characters, no surrounding whitespace |
| `template` | `assistant` (the only server-defined template), allowed for the provisioner |

Response (`201` created, `200` replay of the same key and request, `409`
when the operation failed):

```json
{
  "replayed": false,
  "operation": {
    "operation_id": "<32 hex>",
    "state": "completed",
    "failure": null,
    "principal_id": "ext-<40 hex>",
    "profile_id": "alice-research",
    "template": "assistant",
    "owner_bound": true,
    "created_unix": 1760000000,
    "updated_unix": 1760000000
  }
}
```

A failed operation adds `"code": "<failure>"` and `"error"`. `state` is one
of `pending`, `profile_created`, `completed`, `failed`, `revoked`.

### `GET /operations/{operation_id}`

`200 {"operation": {...}}` for the caller's own operation. Another
provisioner's operation, a malformed id and a missing one are all
`404 unknown_operation`.

### `POST /operations/{operation_id}/revoke`

No body. `200 {"operation": {... "state": "revoked", "owner_bound": false}}`.
Repeating it returns the same answer.

### Error codes

| HTTP | Codes |
|---|---|
| 400 | `invalid_request` (malformed, missing or unknown field), `missing_idempotency_key`, `invalid_idempotency_key`, `invalid_issuer`, `invalid_subject`, `invalid_profile_id`, `reserved_profile_id`, `invalid_display_name`, `invalid_template` |
| 401 | `unauthenticated` (missing, malformed, unknown, revoked or expired credential; the legacy key; a suspended provisioner) |
| 403 | `not_a_provisioner`, `credential_not_provisioning_only`, `provisioner_inactive`, `provisioner_revoked`, `issuer_not_allowed`, `template_not_allowed`, `quota_exceeded`, `profile_outside_credential`, `ledger_full` |
| 404 | `unknown_operation` |
| 409 | `idempotency_conflict`, `profile_unavailable`, `principal_inactive`, `external_identity_conflict`, `ownership_changed`, `operation_in_progress`, `not_revocable`; failed operations: `issuer_not_trusted`, `principal_inactive`, `external_identity_conflict`, `profile_unavailable`, `ownership_changed`, `provisioner_revoked` |
| 413 | `payload_too_large` |
| 415 | `unsupported_media_type` |
| 503 | `storage_unavailable` (retry with the same key), `storage_corrupt`, `identity_unavailable` |

## 6. Idempotency, atomicity and recovery

- **Scope.** A key belongs to one provisioner: `(provisioner principal, key)`.
  Two provisioners using the same key never see each other's operation.
- **Same key, same request** (same fingerprint: domain-separated SHA-256 of
  issuer, subject, profile id, display name and template): the same
  operation. It is resumed if interrupted, and its outcome (including a
  failure) is returned again.
- **Same key, different request**: `409 idempotency_conflict`, and nothing
  changes.
- **Concurrency.** The ledger lock (`File::lock` plus an in-process mutex) is
  held for the whole call, so provisioning is serialized across processes.
  The second of two racing requests for one profile id sees the first
  operation and gets `profile_unavailable`. Racing retries of one key
  resolve to one operation (tested with 8 threads on separate service
  instances).
- **Not one transaction.** The ledger, the profile directory and the
  identity store are three files. The design does not pretend otherwise.

| Crash or failure after… | State left | What happens next |
|---|---|---|
| `pending` recorded | ledger only | retry or `reconcile`: create the profile |
| staging written | `staging/<op>` | the retry discards and rebuilds it; `reconcile` removes staging that no live operation owns |
| rename into `profiles/<id>` | profile with this operation's marker, no owner | retry: marker matches, continue. **Unowned means operator-only**, so no user can use it |
| `profile_created` recorded | as above | retry: bind |
| identity transaction committed | owner bound, ledger still `profile_created` | retry: the identity transaction replays under `provisioning:<op>`, then `completed` |
| identity store unreadable | `profile_created`, unowned | `503`, retry once restored |
| profile directory not creatable | `pending`, nothing on disk | `503`, retry |
| refusal on the merits during bind (principal suspended, issuer withdrawn, ownership changed) | `failed:<code>` | the unowned profile is **moved** to `quarantine/<op>`, never deleted |
| provisioner withdrawn with operations open | open | `reconcile` fails them (`provisioner_revoked`) and quarantines their profiles |

`ProvisioningService::reconcile(operator)` completes or fails every open
operation and clears orphan staging. Run it after a crash or a restore,
before serving requests.

- **No takeover, no stale ownership.** A profile id is provisioned only if
  no directory, no ownership record (even a stale one for a deleted
  profile) and no non-failed operation knows it. A replay never recreates a
  deleted profile. A profile an operator reassigned is not revocable by the
  provisioner.

## 7. Secure profile defaults (template `assistant`)

| Setting | Value | Why |
|---|---|---|
| `toolsets` | empty | An untrusted run is always `strict`, where empty grants **no tools** |
| `approval_policy` | `strict` | Prompt for anything above a bare read |
| `limits` | 8 turns, 16 tool calls, 1 parallel, 2 repeats, 64 KiB output, 300 s | Tighter than the defaults |
| `routing.model` | empty (installation default) | The website cannot pick a model or endpoint |
| persona | a fixed sentence | No caller-supplied prompt |
| remote credentials | none | Vault grants are operator-only (PR #25) |
| Jev | unchanged | Routing and classification permissions are operator settings |

The website chooses only the id (validated), the display name and a
server-defined template name. Only an operator can widen any setting.

## 8. Revocation

| Event | Effect |
|---|---|
| Provisioner revokes access (`/revoke`) | One identity transaction removes the ownership record and binding. The user's next request is refused (`profile_unowned`). Running runs stop at their next revalidation (bounded, PR #23). The profile stays for the operator |
| Operator revokes the provisioning credential, or it expires | Next provisioning call is `401` |
| Operator suspends the provisioner principal | All its credentials are revoked, so the next call is `401` |
| Operator withdraws the registration | Next call is `403 provisioner_revoked`; `reconcile` fails its open operations |
| Operator suspends a provisioned user | The user's credentials stop. New provisioning for that subject is `409 principal_inactive`; an in-flight one fails and is quarantined |

## 9. Security test evidence

Core (`crates/lightagent-core/src/provisioning/tests.rs`, all platforms):

| Requirement | Test |
|---|---|
| Restrictive defaults, derived principal, no token on disk | `provisions_a_restrictive_profile_owned_by_a_derived_principal` |
| Provisioning-only credential gets no run, session, tool, Jev, settings or admin capability | `provisioning_never_becomes_runtime_or_operator_authority` |
| Invalid credential, legacy key, over-broad operator, unregistered, user | `only_a_registered_provisioning_only_credential_is_admitted` |
| Unauthorized external issuer (including well-known names) | `only_operator_registered_issuers_are_accepted` |
| Malformed identities, unauthorized tool grants or configuration | `malformed_requests_and_configuration_are_refused` |
| Duplicate request, same key | `the_same_key_and_request_is_one_operation` |
| Same key, conflicting payloads | `a_reused_key_with_different_parameters_is_refused` |
| Keys scoped per provisioner; cross-provisioner isolation | `idempotency_keys_are_scoped_to_the_provisioner` |
| Concurrent profile creation | `concurrent_provisioning_never_creates_conflicting_ownership` |
| Partial filesystem failure | `a_filesystem_failure_leaves_a_retryable_operation_and_nothing_usable` |
| Partial identity-store failure, no usable orphan | `an_identity_store_failure_leaves_no_usable_orphan` |
| Crash and restart recovery (4 crash points × retry/reconcile) | `crash_and_restart_recovery_completes_every_interrupted_operation` |
| Compensation by quarantine | `a_failed_operation_quarantines_its_profile` |
| Profile name reuse, ownership takeover | `existing_and_reused_profiles_are_never_taken_over` |
| Access revocation via the durable authority | `revoking_access_is_enforced_by_the_identity_store` |
| Credential revocation, rotation, expiry; principal suspension; provisioning access revocation | `provisioning_authority_follows_the_current_stores`, `reconciliation_fails_operations_of_a_withdrawn_provisioner` |
| Credential profile limit, quota, template | `credential_profile_limits_quotas_and_templates_are_enforced` |
| Cross-profile access | `provisioned_users_reach_only_their_own_profiles` |
| Secret exposure in audit and errors | `audit_records_hold_metadata_and_codes_only` |
| No state for an unused installation | `an_unused_service_writes_nothing` |

API (`crates/lightagent-api/tests/`):

| Requirement | Test |
|---|---|
| Provisioning-only credential vs **every** production route (runs, settings, Jev, tools, sessions, approvals, profiles, provider): never 2xx, no run started, audited | `provisioning_boundary.rs` `a_provisioning_credential_reaches_no_runtime_or_operator_endpoint` |
| Provisioning routes unreachable in the production router (all callers) | `the_production_router_does_not_serve_provisioning` |
| Single-user compatibility | `single_user_mode_is_unchanged` |
| HTTP contract: create, replay, conflict, status, cross-provisioner 404, revoke, durable effect | `provisioning_api.rs` (feature) `provision_replay_status_and_revoke_over_http` |
| Auth over HTTP: none, garbage, legacy key, unrestricted operator, user, revoked | `invalid_and_over_privileged_credentials_are_refused` |
| Bounded body, unknown fields, media type, path traversal, issuer name | `requests_are_bounded_and_validated` |
| No identity authority, nothing served | `provisioning_requires_the_identity_store` |

Lightweight and Jev: no `lightweight-*`, Jev, routing or classification code
changed. The paired Lightweight E2E and the deterministic Jev evaluation
run unchanged.

## 10. Compatibility assumptions for the Cloudflare Workers website

1. **Server to server only.** The Worker calls Lightagent with its
   provisioning credential from a Worker secret, over HTTPS terminated by a
   trusted ingress (see blockers). Browsers never call these endpoints, and
   CORS is not offered.
2. **The website authenticates users.** It sends the issuer and subject of a
   user it signed in. It never forwards the user's session token, ID token or
   cookies.
3. **Issuers are agreed in advance.** The operator registers each issuer URL
   exactly as the Worker will send it (`trust_issuer` plus
   `register_provisioner`). Changing an issuer URL creates different
   principals.
4. **Retries reuse the key.** On a network error or `503`, retry with the
   same `Idempotency-Key` and the same body. Use a new key only for a new
   intent. Treat `409 idempotency_conflict` as a client bug.
5. **Ids.** The Worker picks `profile_id` from its own namespace (for example
   `u-<hash>-<n>`) and handles `409 profile_unavailable` by picking another.
   It stores `operation_id` and `principal_id` as opaque strings.
6. **Status, not callbacks.** Lightagent does not call the website. The
   Worker polls `GET /operations/{id}` if it needs to.
7. **No runtime access.** Provisioning does not give the user a Lightagent
   API credential, and does not let the website act as the user. How a user
   authenticates to the agent API is a separate, open decision (§13).
8. **Deletion is revocation.** The website cannot delete profiles. It revokes
   access, and the operator decides about the profile's data.
9. **Clock.** Credential expiry is judged by the Lightagent host's clock.

## 11. Differences from the earlier proposal

| Proposal | Now | Reason |
|---|---|---|
| `PUT /principals/{digest}` as a separate step | principal resolved inside `PUT /profiles/{id}`, in the binding transaction | No ownerless principals; one atomic binding |
| `owner: principal_id` in the body | `(issuer, subject)` in the body; principal derived | Body ids are never ownership claims |
| `POST /principals/{id}/credentials` (token to the website) | not offered | The website must not hold user tokens (open question resolved conservatively) |
| `DELETE /profiles/{id}/owner`, `POST /principals/{id}/disable` | `POST /operations/{op}/revoke`; principal suspension stays operator-only | Revocation limited to what this provisioner provisioned |
| any operator-role `{provision}` credential | plus an operator registration (issuers, templates, quota) | Issuer trust is per provisioner, not global |
| HMAC request signature | not implemented | Blocker: requires TLS ingress design (§13) |

## 12. Rollback and recovery

- **Code rollback.** Revert the PR. Nothing in the running server changes,
  because nothing served it. Do not delete `<home>/provisioning/`.
- **Data preserved.** Back up `<home>/provisioning/` and `<home>/identity/`
  together, with the server and every CLI stopped. Profiles a provisioner
  created are ordinary profiles. Their owners are ordinary identity records,
  which the operator CLI manages without this code.
- **Restore.** Restore both directories from the same backup point. Then
  re-apply every later revocation, before serving: `identity` audit
  `owner_removed`, `credential_revoked` and `principal_suspended`, and the
  ledger audit `revoke` and `provisioner_revoked`. Then run `reconcile`.
  Never restore only `identity/state.json`: an older copy can hold ownership
  a revocation removed.
- **Quarantine.** `<home>/provisioning/quarantine/<op>` holds profiles of
  failed operations. An operator may inspect, move back (and assign) or
  archive them. Nothing removes them automatically.

## 13. Remaining multi-user blockers

1. **Activation.** No startup path builds multi-user auth. Activating it,
   and mounting `provisioning::routes`, is a separate, reviewed change with
   its own flag.
2. **Ingress.** TLS termination at a trusted ingress, a loopback bind behind
   it, rate limiting of failed authentications, and (recommended) HMAC
   request signing with a replay window for the website.
3. **User runtime credentials.** How a provisioned user obtains an agent API
   credential (a website-mediated exchange, OIDC token exchange, or an
   operator-issued token) is undecided. Provisioning deliberately does not
   issue tokens.
4. **Operator tooling.** CLI commands for `register_provisioner`,
   `revoke_provisioner`, `reconcile` and quarantine review. Today these are
   library calls, deliberately: nothing to activate.
5. **Profile deletion hook.** `ProfileStore::delete` still does not release
   ownership. The stale record is inert (binding), and provisioning refuses
   the id, but the CLI should release it.
6. **Verified execution isolation.** Local tools remain withheld from
   untrusted runs. The bubblewrap backend stays experimental.
7. **Per-user rate limits** on runs and provisioning quotas beyond the
   per-provisioner profile count.
8. **Web UI** sign-in and a profile switcher limited to authorized profiles.

## 14. Threat model

| Threat | Mitigation |
|---|---|
| The website (or its stolen key) runs agents, reads conversations or settings | `{provision}`-only credential. Every runtime route checks capability (tested per route) |
| A full operator key used as the website key | Refused: `credential_not_provisioning_only` |
| Spoofed ownership via body ids | No principal, owner or role field. Unknown fields refused. Principal derived on the server |
| A provider name trusted by appearance | Exact operator-registered issuers, per provisioner |
| Profile takeover or name reuse | The id must be unknown to the profile store, the identity store and the ledger. No reassignment. Revoke only own, unchanged bindings |
| Replay or confused retries | Provisioner-scoped keys, request fingerprints, `idempotency_conflict` |
| Races | Cross-process ledger lock, one identity transaction per binding |
| Crash leaves a usable orphan | An unowned profile is operator-only. Recovery is explicit; failure means quarantine |
| Over-privileged new profiles | Server-defined template: no tools, strict policy, tight limits |
| Data leakage in logs | Fixed-code errors. The audit holds identifiers and codes only (no subject, issuer, display name or token). The request `Debug` hides the subject |
| Stale authority | Credential, principal and registration re-read every call. Ownership changes reach runs through the revocation supervisor |
