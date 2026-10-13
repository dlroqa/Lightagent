# Profile-scoped API endpoints

Status: implemented, **not activated**. `lightagent serve` still never builds
multi-user mode. Everything below is exercised through the real HTTP handlers
with synthetic identities (`crates/lightagent-api/tests/profile_scoped.rs`).
None of it is reachable in production yet.

Layering, as implemented:

- **Authentication** establishes who the caller is (`AuthConfig::caller`).
- **Profile authorization** establishes which profile's resources they may
  reach (`IdentityAuthority::authorize_profile`), from the current
  ownership state.
- **Endpoint permission** establishes what they may read or change: the
  capability each route needs (`Scope`), and per-resource ownership such as
  a session's owner.
- **Tool policy and execution isolation** still independently constrain
  what a run may do.

No layer substitutes for another.

## 1. Source audit (master `900a2c7`)

| Concern | Where |
|---|---|
| Authentication and scopes | `AuthConfig::{caller, authorize}`, `Scope` (`lightagent-api/src/auth.rs`). In multi-user mode `authorize` admits operators only. |
| Principal and profile authorization | `authorize`, `authorize_capability`, `OwnershipRegistry`, `OWNER_DEFAULT` (`lightagent-core/src/identity`); `IdentityAuthority::{authorize_profile, authorize_run, refresh}` |
| Durable identity | `IdentityStore` (re-read per check by a persistent authority) |
| Run ownership | `RunOwner`, `authorized_run` (PR #16), revocable runs (PR #22/#23) |
| Sessions | `lightagent_store::{Session, SessionStore, SessionSummary}`. One store per profile at `profiles/<id>/sessions`. `AppState.sessions` is the server's default profile's store. Before this PR, sessions had **no owner**. |
| Profile-specific routes | `list_profiles`, `list_tools`, `list_skills`, `get_jev_status`, `get_provider_capabilities`, the session routes, `create_run` (`lightagent-api/src/lib.rs`) |
| Tool catalog | `RunFactory::tools`. Before this PR the catalog was the *operator's* view of the default profile (`LightweightRunFactory::tools`). |
| Jev diagnostics | `coordinator::status`: endpoint, key, verification, model, thresholds, the routing **allow-list of every profile**, and the profile's latest record |
| Provider | `RunFactory::provider_capabilities`: provider, `base_url`, model catalogs and runtime models |

Before this PR, every route above except the run and approval routes was
operator-only in multi-user mode (`deny` → `AuthConfig::authorize`). That was
safe, but users could not reach their own data.

## 2. Authorization matrix

Columns: single-user (unchanged) | multi-user **operator** (unchanged) |
multi-user **user** (new unless noted). "On P" means authorized for that
capability on the profile `P` (`?profile=`, default the server's profile) by
ownership or grant. The default owner bundle is `runs_read`, `runs_write`,
`sessions_read` and `approvals_write` (C6). `tools_read` and
`sessions_write` need an explicit grant.

| Route | Single-user | MU operator | MU user |
|---|---|---|---|
| `GET /health` | open | open | open |
| `GET /profiles` | `sessions_read` | all profiles | `sessions_read`: only owned or granted profiles that exist. `active_profile` is shown only if accessible. |
| `GET /tools` | `tools_read` | installation catalog | `tools_read` on P: the effective catalog of an untrusted run of P (`RunFactory::tools_for`). 403 if the runtime cannot compute it. |
| `GET /skills` | `tools_read` | default profile's skills | `tools_read` on P: P's skills, only if P's runs receive `skill.read` |
| `GET /jev` | `tools_read` | full status | `tools_read` on P: P's own permissions and latest outcome (`jev_status_for`). 403 if unavailable. |
| `POST /jev/verify` | admin | admin | **403** (operator-only) |
| `GET /provider` | `tools_read` | full capabilities | `tools_read` on P: the same keys, with provider, endpoint, catalogs and runtime models withheld. Only P's configured model is shown. |
| `GET`/`PUT /settings` | admin | admin | **403** (operator-only) |
| `POST /runs` | `runs_write` | any profile | `runs_write` on P (PR #16). With `session_id`: also `sessions_write` on P, and the session must be the user's own on P. |
| `GET /runs/{id}`, `/events`, `POST /cancel` | scope | any run | own runs only (PR #16), re-checked (PR #22/#23) |
| `GET /approvals`, `POST /approvals/{run}` | `approvals_write` | any run | own runs only; stale approvals refused |
| `GET /sessions`, `/sessions/search` | `sessions_read` | default store | `sessions_read` on P: the user's own sessions of P only |
| `POST /sessions` | `sessions_write` | default store | `sessions_write` on P: created in P's store with owner = the user |
| `GET /sessions/{id}` | `sessions_read` | default store | `sessions_read` on P; own session only |
| `PATCH`, `DELETE /sessions/{id}`, `POST …/attachments` | `sessions_write` | default store | `sessions_write` on P; own session only |
| static Web UI fallback | open | open | open (assets only; never under `/api/lightagent`) |

Unknown routes return 404. No new route was added.

## 3. Request sequence

```text
request ─► AuthConfig::caller (credential; revoked/expired → 401)
        ├─ single-user, or operator ─► the route's existing check (unchanged)
        └─ user ─► authorize_capability(scope)                    (403 missing_capability)
                ─► ProfileId::new(?profile= | default)            (400 if malformed)
                ─► IdentityAuthority::authorize_profile           (403, same body for
                   (current ownership; credential re-read)          other's/unowned/missing)
                ─► per-resource check (session.owner == caller
                   && session.profile == P)                       (404 "no such session")
                ─► restricted representation
```

The profile name only *selects* a store. The ownership registry, the session
record's server-written `owner` and `profile`, and the server-computed
catalog decide. The UI enforces nothing.

## 4. Session ownership model

- `Session.owner` (and `SessionSummary.owner`) is a new optional,
  server-written field. Serde skips it when absent, so existing files and
  single-user JSON are unchanged.
- It is set only when a user creates a session in multi-user mode. **Every
  existing session has no owner and stays operator-only.** Sessions are never
  assigned or reassigned later, including when their profile is reassigned:
  the new owner does not inherit the previous owner's sessions.
- A user reaches a session only when it is in the store of a profile the user
  is authorized for, `owner` equals the user, and its recorded `profile`
  equals that profile.
- Another user's session, an unowned one and a missing one return the same
  `404 {"error":"no such session"}`. For update and delete, ownership is
  checked *before* the active-run (409) check, so a foreign id reveals
  nothing.
- A read grant on another user's profile does not expose that owner's
  sessions.
- Search filters to the user's own sessions before applying the limit.
- Continuing a session in a run requires `sessions_write` on the profile and
  ownership. The transcript is saved to that profile's store.

## 5. Tool and skill visibility

`RunFactory::tools_for(profile, trust)` builds the catalog exactly as the
run would: `configured_registry_for` with the profile's
`RemoteAccess::for_run`, the explicit Jev tool, then the profile's tool policy
for that trust (a `RemoteUser` is always strict, and gets no local files,
terminal, Open Terminal, delegation or stdio MCP). A globally configured tool
therefore appears only if that profile's untrusted run would receive it.
Skills are listed only when the run would receive `skill.read`. Jev
classification has no input into this computation: it never grants tools
(D6).

The default `tools_for` and `jev_status_for` refuse. An embedding that cannot
compute a restricted view keeps these endpoints operator-only (403).

## 6. Jev diagnostic isolation

`coordinator::user_status` returns:

- `enabled`;
- the profile's own routing permission;
- the profile's own classification permission and mode;
- whether the explicit tool is available to it;
- its latest outcome: category, confidence, complexity, fallback reason,
  workflow, influenced, time.

It excludes the endpoint and key status, the connection check, the
classifier model and version, thresholds, token counts, the routing
allow-list, and every other profile's record. No prompt is stored or
returned. Jev settings and verification stay operator-only. The routing and
classification permission gates are unchanged and remain independent of
ownership.

## 7. Revocation integration

Every request authenticates again and authorizes against the current state.
A persistent authority re-reads the store; nothing caches a positive
decision. A revoked or expired credential gets 401 at the next request; a
suspended principal gets 401; a reassigned profile or revoked grant gets 403.
Long-lived access uses the existing infrastructure: guarded SSE streams and
supervised runs re-checked by the PR #23 monitor. No new monitor or authority
was added.

## 8. API and SSE compatibility

- Single-user, terminal and ACP: unchanged. They use the same code paths;
  `owner` is never written, so no JSON changes.
- Multi-user operators: unchanged.
- Additive only: `?profile=` (ignored in single-user mode), `owner` on
  sessions created by users, `profile` and `scope` on users' tool, skill,
  Jev and provider views. The user provider view keeps every operator key,
  with withheld values set to null or empty.
- SSE events and run and approval contracts are unchanged.
- `RunFactory` gains two provided methods; existing implementors compile
  unchanged.
- No new dependencies; `Cargo.lock` unchanged. No identity-store schema
  change.

## 9. Negative security tests (`tests/profile_scoped.rs`, real handlers)

| # | Case | Test |
|---|---|---|
| 1 | A lists B's sessions | `users_see_only_their_own_sessions` |
| 2 | A reads B's session | same (identical 404s) |
| 3 | A attaches B's session to a run | `a_run_cannot_attach_another_users_session` |
| 4 | Unauthorized update/delete | `mutations_need_ownership_and_write_permission` |
| 5 | Profile enumeration | `users_enumerate_only_accessible_profiles` |
| 6, 7 | Cross-profile and restricted catalog entries | `the_tool_catalog_is_profile_scoped`; `skills_are_listed_only_where_skill_read_is_available` |
| 8, 9 | Cross-profile Jev diagnostics; Jev settings | `jev_status_is_profile_scoped_and_settings_operator_only`; `coordinator::tool::tests::the_user_status_holds_only_the_profiles_own_state` |
| 10 | Provider secrets and metadata | `the_provider_view_withholds_installation_details` |
| 11, 12, 13 | Reassignment, revocation, suspension | `ownership_and_credential_changes_apply_at_the_next_request`; SSE revocation: `ownership.rs`, `revocation*.rs` |
| 14 | Read-only credential mutating | `mutations_need_ownership_and_write_permission` |
| 15 | Forged profile ids | `forged_profile_identifiers_are_refused` |
| 16 | Another user's resource ids | tests 1–4 use bob's real ids |
| 17 | Concurrency | `concurrent_requests_never_cross_profiles` |
| 18 | Single-user compatibility | `single_user_mode_is_unchanged`; unchanged `api.rs` |
| 19 | Run ownership and approvals | unchanged `ownership.rs` and `revocation.rs` suites |
| 20 | Lightweight routing | no Lightweight crate or routing code changed; paired E2E check |
| 21 | Jev never changes tool eligibility | `the_tool_catalog_is_profile_scoped` (catalog identical before and after a Jev read); D6 |
| 22 | No new access via other routes | `other_routes_grant_users_nothing_new`; unchanged `users_never_reach_operator_or_unscoped_endpoints` |

## 10. Cross-profile disclosure threat model

| Channel | Mitigation |
|---|---|
| Guessing or knowing ids | Identical 404 for foreign, unowned and missing sessions; ownership checked before the busy-state 409; run ids were already covered (PR #16) |
| Naming another profile | Identical 403 for owned-by-other, unowned and missing profiles |
| Enumeration via `/profiles` | Only accessible profiles; `active_profile` withheld unless accessible |
| Catalog and metadata | Catalog computed per profile and trust; Jev and provider views reduced to the profile's own state |
| Search snippets | Filtered to the user's own sessions before limiting |
| Read grants | A grant opens the profile, not its owner's sessions |
| Stale authority | No cached decisions; revocation applies at the next request or monitor sweep |

## 11. Remaining uncovered endpoints and limits

- **Operator-only by design:** `GET`/`PUT /settings` (installation-wide
  configuration, including secret references) and `POST /jev/verify`
  (spends an external request).
- **Embedding-dependent:** `/tools`, `/skills` and `/jev` for users require
  `tools_for` and `jev_status_for`. They are implemented by the production
  factory; any other embedding refuses (403).
- **Session sharing** between principals is not offered. Grants do not share
  sessions.
- **Attachments** are stored in the user's own session directory. Their
  contents are not scanned.
- **Listing a catalog connects configured HTTP MCP servers**, as the
  operator catalog does. It never spawns stdio servers for users.
- **Session limits:** listing and search read every session file of one
  profile, as before.

## 12. Migration and rollback

Nothing to migrate: existing sessions stay ownerless and therefore
operator-only. A user who needs one must start a new session. Rolling back
means reverting the PR. Session files written with `owner` remain readable
by the previous code, because serde ignores the unknown field. Multi-user
mode remains unreachable either way.

## 13. Remaining multi-user activation blockers

1. Serve wiring: a persistent authority and an explicit, validated
   activation setting.
2. A verified execution sandbox before untrusted runs get local execution.
3. Provisioning service and external issuers (not implemented).
4. Operator-credential lifecycle; legacy-key replacement.
5. Credential-attempt rate limiting and lockout.
6. A UI that sends `?profile=` and handles the restricted views (the API is
   ready; the panel is single-user).
7. A dedicated revocation event type; guaranteed Open Terminal cleanup.
