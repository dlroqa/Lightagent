# User identity and profile ownership

Status: **architecture and first slice.** The slice in this PR adds the shared
identity and authorization vocabulary (`lightagent_core::identity`) and one
API seam (`AuthConfig::caller`), with unit tests. Nothing calls them on a
request path yet, so runtime behavior is unchanged. Enforcement waits for
approval of the [compatibility contract](#compatibility-contract-needs-approval).

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

The target, once enforced in multi-user mode. "Grantee" means the owner
granted that capability.

| Request | Operator | Owner | Grantee | Other user | No credential |
| --- | --- | --- | --- | --- | --- |
| Start a run as profile P (`runs_write`) | ✓ | ✓ | if granted | 403 | 401 |
| Read or stream, or cancel, a run of P | ✓ | ✓ | if granted (`runs_read` / `runs_write`) | 403 | 401 |
| List or decide approvals of P's runs | ✓ | ✓ | if granted `approvals_write` | not listed / 403 | 401 |
| Read or write P's sessions | ✓ | ✓ | if granted | 403 | 401 |
| P's Jev status and diagnostics (`tools_read`) | ✓ | ✓ | if granted | 403 | 401 |
| List profiles | all | owned + granted only | owned + granted only | own only | 401 |
| Settings, `jev/verify`, credentials, ownership (`administer`) | ✓ | 403 | 403 | 403 | 401 |
| Provisioning (`provision`) | ✓ (or a provisioning credential) | 403 | 403 | 403 | 401 |

In the compatibility mode every caller is the operator, so every row is ✓,
exactly as today.

## Compatibility contract (needs approval)

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

Decisions for you before enforcement is implemented:

| # | Decision | Recommendation |
| --- | --- | --- |
| C1 | Loopback in multi-user mode: open or credential-required? | Credential-required. Loopback is shared by every local process and user. |
| C2 | The existing `--key-env` key in multi-user mode | It keeps working as an operator credential, with a startup notice recommending an issued one. Removal would be a later, announced change. |
| C3 | Existing profiles at enablement | They stay unowned (operator-only) until the operator assigns them. No guessing. |
| C4 | Terminal and ACP | They stay the local operator (`LocalProcess`). The OS account is their boundary, and they never listen on a network. |
| C5 | Runs started before enablement | Kept operator-only. They are in memory only and end at restart. |
| C6 | Default grants for a new profile's owner | All non-operator capabilities. Owners grant others explicitly. |

## Provisioning interface (contract proposal, not implemented)

For the separate Cloudflare Workers control website. It is narrow,
authenticated, idempotent and audited. It can never give the website the
harness's full privileges.

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
