# User identity and profile ownership

Status: **foundation merged (#15); run-ownership enforcement in review.**
The foundation added the shared identity and authorization vocabulary
(`lightagent_core::identity`). The [enforcement slice](#enforcement-slice-runs)
connects it to API authentication and every run endpoint in **multi-user
mode, which stays off**: `lightagent serve` has no way to enable it yet, so
installations behave exactly as before. The compatibility contract (C1–C6)
was decided on 2026-10-12.

**Not production multi-tenant.** Sessions, Jev diagnostics, the tool catalog
and provisioning are operator-only in multi-user mode, not yet scoped to
profiles, and tools are configured installation-wide. See
[readiness](#multi-user-readiness-and-blockers).

This is item 3 of
[`jev-security-implementation-plan.md`](jev-security-implementation-plan.md)
and issue 4 of
[`jev-routing-permission-and-profile-authorization-proposal.md`](jev-routing-permission-and-profile-authorization-proposal.md).

## Target

```
Authenticated caller ─► identity resolution ─► capability check ─► profile ownership check ─► AuthorizedProfile ─► runtime
  (credential, loopback   (Caller: Principal     (authorize_          (authorize: owner,           (built only by      (tools/approvals, Jev,
   compatibility, local    + Restriction)         capability)          grant, or operator)          authorize)           memory/sessions, provider,
   process)                                                                                                             events/diagnostics)
```

The boundary sits **before** anything profile-specific is constructed:
before `RunManager::start`, before a session store is opened for a profile,
and before an event stream or diagnostic record is returned.

## Audit of the current implementation

| Area | Where | What it does today | Gap |
| --- | --- | --- | --- |
| API authentication | `lightagent-api/src/auth.rs`: `AuthConfig { key: Option<String>, scopes }`, `authorize(headers, Scope)`, `constant_time_eq` | Open (no key) or one bearer key with a scope set. 401 for a missing or wrong key, 403 for a missing scope. Constant-time comparison. | One identity. No users, no profile binding. The raw key is held in memory. `AuthConfig` derives `Debug`, which would print the key if it were ever logged; nothing logs it today. |
| Binding | `lightagent/src/serve.rs::run` | `is_loopback` = host string `127.0.0.1` / `localhost` / `::1`. Loopback without `--key-env` → `AuthConfig::open()`. Non-loopback without a key refuses to start. A `--key-env` key always gets `[Scope::Admin]`. | Every keyed caller is an administrator. Loopback is open to every local process and user. |
| Scopes per route | `lightagent-api/src/lib.rs`, `deny(&state, &headers, Scope)` at the top of each handler | Tools, skills, provider and `GET /jev` need `ToolsRead`. `/profiles` and session reads need `SessionsRead`. Session writes need `SessionsWrite`. Run create and cancel need `RunsWrite`; run reads and SSE need `RunsRead`. Approvals need `ApprovalsWrite`. `/settings` and `/jev/verify` need `Admin`. | Scopes are not tied to a profile. |
| Run creation | `create_run` → `RunManager::start(StartRun { profile, .. })` → `LightweightRunFactory::run` → `chat::resolve_profile(store, config, Some(name))` | `body.profile` is passed through unchecked. Any existing profile, or the built-in `default`, runs. With a `session_id`, the session must belong to `AppState.session_profile`. | **Any API caller can run as any profile.** |
| Runs, events, cancel | `manager.rs`: `RunManager.runs: HashMap<String, Arc<RunState>>`; `get_run`, `run_events`, `cancel_run` | Looked up by id. Ids are `run-` plus 128 random bits (`RunId::new`). `RunState` records neither profile nor caller. | Access is by knowledge of the id. No owner check is possible, because nothing records the owner. |
| Approvals | `list_approvals` (`awaiting_approval()` over all runs), `respond_approval(run)` | Lists **every** waiting run and lets the caller decide any of them. | Cross-profile visibility and decision. |
| Sessions | `AppState { sessions: SessionStore, session_profile }`, set once in `serve::run` from the active profile | Session endpoints serve only the server's active profile. | Single profile per server process. Fine for one user; for several, the store must follow the authorized profile. |
| Profiles | `list_profiles` (`SessionsRead`): every profile's id, name and model. `ProfileStore` under `<home>/profiles/<id>/` with an `active` file. | Lists all profiles. | Visibility should follow authorization. |
| Settings | `get_settings`/`save_settings` (`Admin`). They change global config and the **active profile's** approval policy and limits. | Administrator only. | Correct for an operator. Per-profile settings for owners are future work. |
| Jev | `GET /jev` (`ToolsRead`): status for the active profile. `POST /jev/verify` (`Admin`). `JevState` diagnostics are keyed by profile. Routing and classification permissions are per profile (`routing_grant`, `permits_profile`). | Profile-level permissions. | They describe what a profile may send. They do not authorize a caller (tested in this slice). |
| ACP | `lightagent/src/acp.rs`, `lightagent-acp/src/server.rs` | stdio, spawned by the local user's editor. `session/new` takes a Lightagent `profile` extension, checked to exist (`store_for`). | Trust is the local OS user, the same as the CLI. |
| Terminal / CLI | `chat.rs` (`--profile`), `lib.rs` commands | A local process acting on the user's own home. | Same OS-user boundary. |
| Secrets | `SecretRef` (env/file), `paths::write_private` (owner-only files), `provider_key_file` | Credentials stored as references or owner-only files. | Reused below for the identity store. |
| Provisioning | none | Profiles are created by `lightagent profile create`, `setup` and `init`. No API creates profiles or users. | New interface, specified below. |

## Identity and ownership schema

Added in this slice as types, not yet persisted.

| Type | Fields | Notes |
| --- | --- | --- |
| `PrincipalId` | `^[a-z0-9][a-z0-9_-]{0,63}$` | Server-assigned. Never derived from a profile id or name. |
| `Principal` | `id`, `role: Operator \| User`, `source: LocalCompatibility \| LocalProcess \| Credential { key_id }` | `source` is recorded for audit and never widens access. |
| `Capability` | `runs_read`, `runs_write`, `sessions_read`, `sessions_write`, `tools_read`, `approvals_write`, `administer`, `provision` | One per API scope, plus `provision`. `administer` and `provision` are operator-only. |
| `Restriction` | `capabilities: Option<Set>`, `profiles: Option<Set<ProfileId>>` | Carried by a credential. It only narrows. |
| `Caller` | `principal`, `restriction` | What an entry point resolves. |
| `CredentialRecord` | `key_id`, `principal`, `role`, `secret_sha256`, `restriction`, `created_unix`, `revoked_unix` | Token `la_<16 hex>_<64 hex>` (256-bit secret). Only the SHA-256 is stored, compared in constant time. Issuing fails if OS entropy is unavailable. |
| `OwnershipRegistry` | `profile → { owner, grants: principal → Set<Capability> }` | `assign`: idempotent for the same owner, `Conflict` for another. `reassign`: an operator action that drops grants. `grant`/`revoke`: never operator-only capabilities. `remove`: deleted profiles keep no owner. |
| `AuthorizedProfile` | private fields | Built only by `authorize`. |
| `AuditRecord` | `at_unix`, `principal`, `key_id`, `action`, `profile`, `outcome` | Ids and fixed codes only. No free-text field. |

Proposed persistence (next slice): `<home>/identity.json`, written with
`paths::write_private` (owner-only), holding principals, the
`CredentialStore` and the `OwnershipRegistry`. Authorization decisions are
appended as `AuditRecord` JSON lines to `<home>/logs/authz.jsonl`. The file
is read at each request, as `config.json` is, so revocation applies to the
next request without a restart.

## Decision rules (`identity::authorize`)

In order:

1. **Capability.** The role must allow it (users never hold `administer` or
   `provision`), and so must the credential restriction.
2. **Credential profile limit.** If the credential lists profiles, the
   target must be one of them.
3. **Ownership.** An operator may act on every profile. A user may act only
   on a profile it owns, or one whose owner granted it this capability. **A
   profile with no owner is refused to users** (fail closed).

Not consulted: Jev routing or classification permission, profile names, or
the caller knowing a profile id. Tested: a profile permitted to use Jev is
still refused to a caller who does not own it.

## Permission matrix

The target in multi-user mode, with C6's minimum owner bundle
(`OWNER_DEFAULT`: `runs_read`, `runs_write`, `sessions_read`,
`approvals_write`). "Grantee" means an operator-issued grant of that
capability. Run rows refer to runs the caller started itself. Rows marked †
are enforced by the enforcement slice; the rest are operator-only until their
own PR.

| Request | Operator | Owner | Grantee | Other user | No credential |
| --- | --- | --- | --- | --- | --- |
| Start a run as profile P (`runs_write`) † | ✓ | ✓ | if granted | 403 | 401 |
| Read or stream, or cancel, a run they started on P † | ✓ (any run) | ✓ | if granted (`runs_read` / `runs_write`) | 404 | 401 |
| List or answer approvals of runs they started on P † | ✓ (any run) | ✓ | if granted `approvals_write` | not listed / 404 | 401 |
| Read P's sessions | ✓ | ✓ | if granted | 403 | 401 |
| Change P's sessions (`sessions_write`) | ✓ | grant only | if granted | 403 | 401 |
| P's Jev status, diagnostics and tool catalog (`tools_read`) | ✓ | grant only | if granted | 403 | 401 |
| List profiles | all | owned + granted only | owned + granted only | own only | 401 |
| Settings, `jev/verify`, credentials, ownership (`administer`) | ✓ | 403 | 403 | 403 | 401 |
| Provisioning (`provision`) | ✓ (or a provisioning credential) | 403 | 403 | 403 | 401 |

In the compatibility mode every caller is the operator, so every row is ✓,
exactly as today.

## Compatibility contract (decided)

**Mode 1: local compatibility (default, unchanged).** Applies when no
identity store exists, which is every installation today. The API resolves
every admitted request to the local operator: the open loopback bind or the
single `--key-env` key. Behavior is identical to today. The non-loopback
refusal without a key stays. This slice's tests prove the seam agrees with
today's gate for every scope.

**Mode 2: multi-user (explicit opt-in).** Entered only by an operator command
(proposed: `lightagent identity enable`), which creates `identity.json` and
issues the first operator credential. Then:

- Every API request needs a credential, **including on loopback**. A new
  multi-user installation fails closed.
- Existing profiles start **unowned**: operator-only until assigned. Nothing
  is assigned automatically.
- Runs record their owner and profile. Run, event, cancel and approval
  access is checked against them. Session, `/jev` and `/profiles` responses
  follow the authorized profile.
- Revoking a credential or grant applies to the next request.

Decided on 2026-10-12:

| # | Decision |
| --- | --- |
| C1 | Multi-user mode authenticates every API request, loopback included. Bind address, forwarded headers and proxy origin never stand in for a credential. Loopback stays open only while multi-user mode is off. |
| C2 | The `--key-env` key stays as a **legacy operator credential**: its elevated authority is identified, a startup warning appears in multi-user mode, it is never logged and never converted into a user credential, and rotation and replacement are documented. |
| C3 | Existing profiles stay unowned and operator-only. Nothing is assigned automatically; the operator assigns owners explicitly. |
| C4 | Terminal and ACP stay operator-only local interfaces under the OS account boundary, with no network exposure. Any process running as that OS account holds local operator authority. |
| C5 | Runs from before activation are operator-only. New runs capture the initiating principal and authorized profile at creation, immutably. Read, status, events, cancel and approvals are checked against them. |
| C6 | Owners get a minimum bundle only: start runs; view, stream and cancel their runs; read sessions and events; answer approvals for their runs. Not administration, reassignment, provisioning, credential or membership management, cross-profile access, extra tools, server or provider configuration, or Jev controls. More only by operator grant. Tool and approval policies stay independent of ownership. |

## Enforcement slice: runs

Connects the foundation to the API in multi-user mode
(`AuthConfig::multi_user(identity, legacy_operator_key)`), in this order:

```
request ─► AuthConfig::caller           bearer credential required (C1); legacy key first, constant-time (C2);
                                        otherwise IdentityAuthority → CredentialStore::authenticate (SHA-256, constant-time)
        ─► authorize_capability          before any lookup, so the answer never depends on a run existing
        ─► create: ProfileId from the body, or the server's default; IdentityAuthority::authorize_profile → AuthorizedProfile
                   → StartRun.profile = the authorized profile → RunManager::start_as(RunOwner { principal, profile })
        ─► existing run: IdentityAuthority::authorize_run(caller, run.owner(), capability)
                   operator: any run · user: only a run it started, while still authorized on its profile
                   refused → 404, identical to a missing run
        ─► SSE: a guard re-reads the credential (CredentialStore::caller_for) and ownership before every event
                and every 2 s while idle; the stream ends when either is withdrawn
        ─► existing runtime, unchanged: the factory receives StartRun as before (profile, message, model, cwd, history)
```

Every other route goes through the existing `deny` gate, which in multi-user
mode admits **operators only** (`this endpoint is operator-only in
multi-user mode`). Those routes fail closed rather than being partially
protected.

| Source | Change |
| --- | --- |
| `lightagent-core/src/identity/ownership.rs` | `OWNER_DEFAULT` (C6). `permits` = bundle for the owner, plus grants. A grant to the owner outside the bundle is allowed. |
| `lightagent-core/src/identity/mod.rs` | `PrincipalSource::LegacyOperatorKey` (C2) |
| `lightagent-core/src/identity/credential.rs` | `caller_for(key_id)`: re-read a credential for long-lived connections |
| `lightagent-core/src/identity/audit.rs` | `AuditAction::Authenticate` |
| `lightagent-api/src/manager.rs` | `RunOwner` (immutable, private fields), `RunState::owner`, `RunManager::start_as`. `start` = operator-only owner. |
| `lightagent-api/src/auth.rs` | `IdentityState`, `IdentityAuthority` (`authorize_profile`, `authorize_run`, `refresh`, bounded audit log; a poisoned lock denies), `rejection`, `AuthConfig::multi_user`, `startup_notices`, a key-redacting `Debug` |
| `lightagent-api/src/lib.rs` | `create_run`, `get_run`, `run_events`, `cancel_run`, `list_approvals`, `respond_approval`. The guarded `event_stream`. Single-user paths are unchanged. |
| `lightagent-api/tests/ownership.rs` | Integration and adversarial tests |

**How C1–C6 are shown** (tests in `lightagent-api/tests/ownership.rs`
unless named otherwise):

| Decision | Behavior | Test |
| --- | --- | --- |
| C1 | 401 on every route without a valid credential on loopback, with `X-Forwarded-For`, `Forwarded` and `X-Real-IP` set | `multi_user_mode_requires_a_credential_on_loopback` |
| C1 (compat) | Single-user loopback unchanged | `single_user_loopback_compatibility_is_unchanged`, all existing API tests |
| C2 | The legacy key authenticates as an operator with `LegacyOperatorKey` source. The startup notice names its authority. `Debug` never prints it. | `the_legacy_key_is_an_operator_credential_and_never_a_user`, `the_policy_never_prints_a_key` |
| C3 | Unowned (`orphan`, `default`) and nonexistent profiles are refused to users, identically | `a_user_cannot_run_another_users_or_an_unowned_profile` |
| C4 | Terminal and ACP code unchanged. ACP-style `RunManager::start` runs are operator-only. | `runs_without_an_owner_are_operator_only` |
| C5 | Owner captured at creation; run id knowledge gives 404; historical runs operator-only; losing ownership ends access | `a_user_runs_its_own_profile_and_the_run_records_who_and_which`, `knowing_a_run_id_never_authorizes_access_to_it`, `runs_without_an_owner_are_operator_only`, `losing_ownership_ends_access_to_earlier_runs` |
| C6 | Owner bundle exact; tools and Jev status need a grant; operator-only never | `owning_a_profile_grants_only_the_minimum_bundle` (core), `users_never_reach_operator_or_unscoped_endpoints` |
| Revocation | Next request 401; open SSE ends; requests racing revocation never admitted afterwards | `a_revoked_credential_is_refused_at_the_next_request_and_stream`, `revocation_racing_requests_never_admits_afterwards` |
| Approvals | Listed and answerable only for authorized runs | `approvals_are_listed_and_answered_only_for_authorized_runs` |
| Restriction | A read-only credential cannot start or cancel | `a_credential_restriction_limits_what_its_holder_may_do` |
| Identity grants no tools | The factory receives the authorized profile and nothing else | `a_user_runs_its_own_profile_and_the_run_records_who_and_which` |
| Jev independent | A Jev-permitted profile is still refused to a non-owner | `a_jev_permission_never_authorizes_a_caller` (core) |
| Swapping and races | Profile-id swaps (other, unowned, case and whitespace variants, traversal) refused; 24 concurrent creations and cross-reads never cross | `a_user_cannot_run_another_users_or_an_unowned_profile`, `concurrent_requests_never_cross_profiles` |

### Remaining endpoints in multi-user mode

All of these are **operator-only** in multi-user mode (users get 403), so
none is reachable by an ordinary user until its own PR scopes it.

| Endpoint | Scope | Why not yet scoped |
| --- | --- | --- |
| `GET/POST /sessions`, `GET /sessions/search`, `GET/PATCH/DELETE /sessions/{id}`, `POST /sessions/{id}/attachments` | sessions | `AppState.sessions` serves one profile chosen at startup. It needs per-request stores for the authorized profile. Runs with a `session_id` are therefore operator-only too. |
| `GET /profiles` | sessions_read | Must list only owned and granted profiles |
| `GET /jev`, `GET /tools`, `GET /skills`, `GET /provider` | tools_read | They report the server's active profile. They need the authorized profile, and C6 grants. |
| `GET/PUT /settings`, `POST /jev/verify` | administer | Operator-only by design |
| Provisioning | provision | Not implemented |
| `GET /health` | none | Public liveness only |

### Multi-user readiness and blockers

Multi-user mode is **not activatable** in this slice: nothing in
`lightagent serve`, the CLI or the configuration constructs
`AuthConfig::multi_user`. Before it can be offered:

1. **Tool policy is installation-wide.** `tools.*` and `web.*` apply to every
   profile, and `AgentProfile.toolsets` is advisory. A user's run gets the same
   tools as the operator's, including local file and terminal tools that act
   as the OS account. Operator authority could be reached through a
   delegated tool, which C4 forbids. Runtime toolset enforcement (plan item 4)
   must land first, or multi-user mode must refuse profiles with local
   execution tools.
2. **Identity store and CLI**: persisted `identity.json` (owner-only), and
   `identity enable`, `credential issue/revoke/rotate`, `owner assign/reassign`
   and `grant`. Runtime state is in memory today.
3. **Sessions, `/profiles`, `/jev`, `/tools` scoped to the authorized
   profile** (the inventory above).
4. **Persistent audit log** (`logs/authz.jsonl`). The in-memory log keeps the
   last 4,096 decisions.
5. **Transport.** An externally reachable server must sit behind TLS, either
   a trusted ingress that terminates TLS and forwards to a loopback bind, or
   native TLS. Bearer credentials must never cross plain HTTP. Forwarded
   headers are never trusted for identity.
6. **Provisioning API** behind its own enable flag.
7. **Rate limiting** of failed authentications.

### Migration and rollback for this slice

- **Migration:** none. No installation can enter multi-user mode yet. The
  single-user API, ACP and terminal are unchanged, and runs they start are
  recorded as operator-only.
- **Rollback:** reverting removes the enforcement code. With multi-user mode
  never enabled, nothing changes for any installation.
- **Legacy key rotation (C2), once activation exists:** start the server with
  a new `--key-env` value (the old key stops at restart). Or issue an operator
  credential, move clients to it, and restart without `--key-env`. The key is
  read from the environment and never written or logged by Lightagent.

## Provisioning interface (contract proposal, not implemented)

For the separate Cloudflare Workers control website. It is narrow,
authenticated, idempotent and audited. It can never give the website the
harness's full privileges.

**One identity authority.** Provisioning uses this same identity store and
credential mechanism. It is not a second authority.

**Credential.** An operator issues the website one credential with role
`operator` and restriction `capabilities = {provision}`. That credential
cannot run agents, read sessions, change settings or reassign profiles. It
travels over HTTPS only. Recommended additionally: an HMAC request signature
over method, path, body digest and timestamp, with a ±5-minute replay window.

**Endpoints** (`/api/lightagent/v1/provisioning`, all requiring
`provision`):

| Method and path | Body | Result | Idempotency |
| --- | --- | --- | --- |
| `PUT /principals/{external_subject_digest}` | `{ "issuer": "...", "display": "..." }` | `{ principal_id }`. The server derives the id; the caller never chooses it. Role is always `user`. | Same subject → same principal. `display` changes are allowed. |
| `PUT /profiles/{profile_id}` | `{ "owner": principal_id, "name": "...", "template": "..." }` plus an `Idempotency-Key` header | `201` created, `200` unchanged, `409` if owned by someone else | `OwnershipRegistry::assign`. Reassignment is not available here: it is an operator `administer` action. |
| `GET /profiles/{profile_id}` | none | Owner and status. No sessions or content. | n/a |
| `POST /principals/{principal_id}/credentials` | `{ "restriction": {...} }` | Token once, for the user | Open question: whether the website should ever hold user tokens, or users should obtain them directly. |
| `DELETE /profiles/{profile_id}/owner` | none | `204`. The profile becomes unowned (operator-only), and the previous owner's access ends at its next request. | Idempotent. Re-assigning afterwards is a fresh `PUT`. |
| `DELETE /principals/{principal_id}/credentials/{key_id}` | none | `204` | Revocation is idempotent. |
| `POST /principals/{principal_id}/disable` | none | `204`. Every credential stops at the next request. | Idempotent. |

**Requirements.**

- Identity mapping is server-verified: the website proves who the user is
  (its own sign-in), and Lightagent verifies the website (credential and
  signature). A subject maps to one principal, and the mapping is never
  re-pointed without an operator.
- Every request writes an `AuditRecord`.
- Profile templates are server-defined names. They never carry tool, approval
  or Jev permissions; permissions stay with the operator. Per-profile request
  rate limits apply.
- Nothing in the contract is final until the website's contract is agreed.

## Threat model

| Threat | Today | With ownership (target) |
| --- | --- | --- |
| A caller runs as someone else's profile by naming it | Possible (`body.profile` unchecked) | `authorize(runs_write, P)` before `RunManager::start`. Knowing the id is not enough. |
| Reading another profile's run events, sessions or Jev diagnostics | Run events need the 128-bit id; sessions are single-profile | Run records hold profile and owner and are checked. Sessions and diagnostics follow the authorized profile. |
| Deciding another profile's tool approval | Possible: `GET /approvals` lists all runs | Filtered and checked per run profile. |
| A stolen or leaked token | The single admin key grants everything until rotated | Per-principal tokens, profile and capability restrictions, revocation at the next request, hashed at rest. |
| A token read from disk or logs | The key comes from the environment | Only SHA-256 digests are stored. `IssuedToken` `Debug` is redacted. Audit lines carry the key id only. |
| A timing attack on token comparison | Constant-time | Constant-time on the digest. |
| Brute force | n/a | 256-bit secrets. Rate limiting is a follow-up at the API layer. |
| A local process on a shared host uses open loopback | Possible in compatibility mode | Multi-user mode requires a credential on loopback (C1). |
| The control website over-reaches | n/a | `provision` only: cannot run, read, administer or reassign. Every call audited. |
| Conflicting or replayed provisioning | n/a | Idempotent assign; `409` on conflict; Idempotency-Key and signature window. |
| A Jev permission mistaken for authorization | n/a | Not consulted by `authorize` (tested). |
| Entropy failure at issue time | n/a | Issuing fails. No fallback secret. |

## Migration sequence (separate PRs)

1. **This PR: identity foundation.** Types, policy, credential hashing,
   audit records, the API seam, and unit tests. No behavior change.
2. **Identity store and CLI.** `identity.json` (owner-only), `lightagent
   identity enable | status`, `principal add`, `credential issue | revoke`,
   `owner assign | reassign`, `grant | revoke`, and a dry-run report. The API
   ignores the store unless multi-user mode is enabled; the store is created
   only by `enable`.
3. **API enforcement in multi-user mode** (after C1–C6). Resolve the caller
   from the store. Resolve the effective profile server-side, then call
   `authorize` before `RunManager::start`. `RunState` records profile and
   principal. Check run, event, cancel and approval access. Per-request
   session stores for the authorized profile. Filter `/profiles` and `/jev`.
   Settings are operator-only. Write the audit log. Integration tests cover
   the full matrix, including concurrency and revocation mid-session.
4. **Provisioning API**, behind its own enable flag, once the website
   contract is agreed.
5. **Web UI**: credential sign-in and a profile switcher limited to
   authorized profiles.
6. **Runtime toolset enforcement**, an independent track
   (`profile-toolset-enforcement-proposal.md`).

## Rollback

- Slice 1 adds library code that no request path calls. Reverting removes
  it with no behavior change.
- From slice 2 on, deleting or renaming `identity.json` returns the
  installation to compatibility mode. That restores single-operator access,
  so an operator rolling back a multi-user installation must also restore a
  loopback-only bind or a single `--key-env` key, or the rollback widens
  access. Each later PR's notes repeat this.
