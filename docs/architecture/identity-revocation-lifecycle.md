# Identity administration and revocation-aware runs

Status: implemented, **not activated**. `lightagent serve` still never builds
multi-user mode, so no production request reaches the revocation path. The
operator CLI manages the durable store; nothing reads that store in a running
server yet.

Principle: authorization is re-checked throughout an agent run, not only when
the request that started it was authenticated.

## 1. Source audit (master `c797cef`)

| Concern | Type / function | Where |
|---|---|---|
| Request identity | `AuthConfig::{open, keyed, multi_user, caller, authorize}` | `lightagent-api/src/auth.rs` |
| Identity authority | `IdentityAuthority::{new, persistent, refresh, authorize_profile, authorize_run, record, audit_log}` (`sync` re-reads the store) | `lightagent-api/src/auth.rs` |
| Durable store | `IdentityStore::{load, transact, effective_ownership, append_audit, audit}`, `Transaction::{create_principal, set_status, issue_credential, rotate_credential, revoke_credential, assign_owner, reassign_owner, grant, revoke_grant, remove_profile}` | `lightagent-core/src/identity/store.rs` |
| Ownership | `OwnershipRegistry`, `ProfileOwnership::permits`, `authorize`, `authorize_capability` | `lightagent-core/src/identity/{ownership,mod}.rs` |
| Credentials | `CredentialStore::{issue_expiring, rotate, revoke, revoke_principal, authenticate_at, caller_for}`; `IssuedToken` (not serializable, redacted `Debug`) | `lightagent-core/src/identity/credential.rs` |
| Audit | `AuditRecord::decision`, `AuditAction`, `StoredAudit` | `lightagent-core/src/identity/audit.rs`, `store.rs` |
| Run manager | `RunManager::{start, start_as, get, awaiting_approval}`, `RunState` (events, status, pending approval, `CancellationToken`, decision channel), `RunFactory::{run, run_as}`, `drive` | `lightagent-api/src/manager.rs` |
| Run ownership | `RunOwner { principal, profile, trust }` (immutable) | `lightagent-api/src/manager.rs` |
| API run handlers | `create_run`, `get_run`, `run_events`, `cancel_run`, `list_approvals`, `respond_approval`, `authorized_run` (+ `StreamGuard`), `event_stream` | `lightagent-api/src/lib.rs` |
| Production factory | `LightweightRunFactory::run_as` (config reload, Jev preflight, tool policy, `BoundedExecutor`, `manager::drive`) | `lightagent/src/serve.rs` |
| Tool dispatch | `BoundedExecutor::{approval_for, invoke, remember}`, `CallGuard`, per-call `tokio::select!` on cancellation and timeout | `lightagent-tools/src/executor.rs` |
| Delegated workers | `AgentDelegate::call` builds a fresh `BoundedExecutor` and `AgentLoop`, shares `ctx.cancel` | `lightagent-tools/src/builtins/delegate.rs` |
| Remote tools | `web.fetch` (`fetch_text`, per-hop vetting), `web.search`, `rag.realtime`, `McpTool::call` (`tools/call`), `OpenTerminalRun` | `builtins/web.rs`, `lightagent-rag/src/realtime.rs`, `lightagent-mcp/src/tool.rs`, `builtins/open_terminal.rs` |
| Sandbox gate | `SandboxGate::route` takes an `AuthorizationCheck` closure | `lightagent-tools/src/sandbox.rs` |
| CLI | clap `Command` enum, `dispatch`, `--json` global flag | `lightagent/src/lib.rs` |

Gaps found: the SSE `StreamGuard` re-checked a *subscriber*, but nothing
re-checked the *run*. A run authorized at `create_run` kept calling tools,
models, remote services and workers after its credential was revoked.
`drive` resumed after an approval without re-authorizing. Delegated workers
had no check of their own. A retained `Caller` passed to `authorize_run`
was trusted as-is.

No second authority and no parallel run manager were added. The changes
extend `IdentityAuthority`, `RunManager`/`RunState`/`RunOwner`,
`RunFactory`, `BoundedExecutor` and `ToolCtx`.

## 2. Operator CLI: `lightagent identity`

Every command runs as the local operator (`PrincipalSource::LocalProcess`):
the OS account that owns the Lightagent home (decision C4). There is no
network endpoint, no activation command, and no remote administration.

| Command | Effect | Writes |
|---|---|---|
| `identity status` | Schema, generation, counts of principals, credentials and owned profiles | never |
| `identity user create <id>` | Create an ordinary **user** principal | yes |
| `identity user list` | Principals, role, status, unrevoked credential count | never |
| `identity user suspend <id>` | Suspend a user and revoke every credential it holds | yes |
| `identity credential issue <user> [--expires-in S] [--capability C]… [--profile P]… [--secret-file F]` | Issue a scoped user credential (secret shown once) | yes |
| `identity credential rotate <key_id> [--expires-in S] [--secret-file F]` | Issue a replacement with the same restriction; the old one stays valid until revoked | yes |
| `identity credential revoke <key_id>` | Revoke one credential | yes |
| `identity credential list [--principal U]` | Key ids, state, expiry, restriction; never a secret or verifier | never |
| `identity owner assign <profile> <user>` | Assign an unowned, existing profile | yes |
| `identity owner reassign <profile> <user>` | Move a profile (drops the previous owner's grants) | yes |
| `identity owner remove <profile>` | Remove ownership and the binding marker | yes |
| `identity grants [list] [--profile P]` | Owners, grants, and whether ownership is effective | never |
| `identity grants revoke <profile> <principal> <capability>` | Revoke one grant | yes |
| `identity audit [--limit N]` | Recent audit records: codes and identifiers only | never |

Read-only commands never create the store. The first change creates it and
reports `store_created: true`. Ordinary startup (`chat`, `serve`, `acp`)
neither reads nor writes it. Issuing operator credentials, granting
capabilities and trusting issuers are deliberately not offered.

**Secret safety.** A credential's plaintext exists only in the `IssuedToken`
returned by the transaction. Before anything is issued, the delivery target
is fixed:

- `--secret-file F` creates a **new** file. It never overwrites one, and on
  Unix the file is `0600`. The token is written and `fsync`ed.
- Without it, the token is printed only when stdout is a terminal, the
  `CI`/`GITHUB_ACTIONS` variables are unset, and `--json` is not used. The
  notice goes to stderr and the token goes on its own line on stdout.
- `--json` output never contains a token, so a JSON log or a CI artifact
  cannot capture one.
- If delivery fails after issuance, the new credential is revoked in a
  follow-up transaction, and the error says so.

## 3. Run lifecycle (as implemented)

```text
POST /runs ──► AuthConfig::caller ──► IdentityAuthority::authorize_profile   (multi-user only)
                                              │
                    RunOwner::new(principal, profile, trust).with_credential(key_id)
                    IdentityAuthority::run_check(caller, owner) ─► RevocationCheck
                                              │
RunManager::start_supervised ── check() ──✗──► 401/403, no run created
                                              │ ✓
              RunState { owner, cancel, RunAuthorization::revocable(check, cancel, generation) }
                                              │
RunFactory::run_supervised ─► LightweightRunFactory::run_with(Some(authorization))
   │ verify ──✗──► Error + RunCompleted(Cancelled)
   │ Jev preflight (shares cancel)
   │ BoundedExecutor.with_authorization ─► ToolCtx.authorization
   │ AuthorizedProvider(LightweightProvider)
   ▼
drive_supervised ─ verify ─► AgentLoop
   ├─ each model turn ─────── AuthorizedProvider::stream ── verify ──✗──► no request; cancel
   ├─ each tool call ──────── BoundedExecutor::approval_for ── verify ──✗──► Deny (no prompt)
   │                          BoundedExecutor::invoke ──────── verify ──✗──► error, not launched
   │   ├─ web.fetch ───────── fetch_text_authorized: verify before every hop
   │   ├─ web.search / rag.realtime / MCP tools/call / Open Terminal: verify before the request
   │   └─ agent.delegate ──── verify before the worker; worker executor + provider carry the
   │                          same authorization and cancel token; verify again after it ends
   ├─ approval pause ──────── select { cancel ⇒ revoked/cancelled, decision ⇒ verify, then resume }
   └─ tool in flight ──────── executor select! on cancel drops the call future
SSE /runs/{id}/events ─ StreamGuard(refresh + authorize_run) at each event, every 2 s,
                        and at once on RunManager::revalidate/revoke (watch channel)
POST /approvals/{run} ─ authorization().verify() ──✗──► 409 (stale approval)
GET  /runs/{id} ─────── "revocation": "<code>" when revoked (multi-user only)

Supervisor (in-process, RunManager):
  revoke(actor: operator + administer, RevocationScope, Denial) ─► affected runs: revoke + cancel
  revalidate() ─► verify every active revocable run; wake every open stream
  affected_by(scope) ─► Credential(key_id) | Principal | Profile (user runs) | Grant (user runs)
```

## 4. Revocation consistency model

- **Source of truth.** The identity state: the in-memory `IdentityState`
  of an `IdentityAuthority::new`, or `identity/state.json` for an
  `IdentityAuthority::persistent`, which re-reads it at every check.
- **What a run holds.** A `RunAuthorization`: a `RevocationCheck` holding
  only identifiers (the `Caller` it re-reads and the immutable `RunOwner`),
  the run's cancellation token, and the store generation it was authorized
  at. It never holds an earlier decision. `authorize_run` now re-reads a
  credential-backed `Caller` before deciding, so a `Caller` kept from before
  a revocation, expiry or narrowing authorizes nothing.
- **What a check decides.** The credential is re-read by key id (revoked →
  `revoked`, expired at the authority's clock → `expired`). Then the run's
  owner and profile are authorized for `runs_write` against current
  ownership: removed or reassigned ownership, a revoked grant, or a
  narrowed credential → `profile_not_granted` / `profile_unowned` /
  `profile_outside_credential` / `missing_capability`.
- **Pull.** Every enforcement point in §3 calls `verify`. A change committed
  before a check is seen by that check: the store is replaced by atomic
  rename, so a reader sees the old or the new document, never a mix.
- **Push.** `RunManager::revoke` cancels the affected runs at once, and
  `RunManager::revalidate` re-checks every active revocable run. Both wake
  open event streams.
- **Final.** The first refusal is recorded and cancels the run. Every later
  check returns the same denial, even if the state would admit the caller
  again. Its audit record is written once (`request` / `runs_write` /
  denied, with codes only).
- **Fail closed.** An unreadable or newer-schema store makes `refresh` fail,
  so the run is revoked at its next check.
- **Not revocable.** Runs started by `RunManager::start`/`start_as`: the
  terminal, ACP and the single-user API. Operator runs through the legacy
  `--key-env` key are also never revoked by store state, because the key
  cannot change while the server runs (C2). Operators keep explicit
  administrative cancellation (`cancel_run`, `RunManager::revoke`).
- **Window.** An action that passed its check before the commit may finish.
  Between enforcement points, for example in one long model stream, a pull
  change is not observed until the next point. Only a push or `revalidate`
  interrupts it.

## 5. Cancellation of tools and workers, and the limits of revocation

Revocation prevents new actions once enforcement observes it. It cannot
undo a side effect already committed.

| Work in flight | On revocation | Limit |
|---|---|---|
| Model stream | Stream dropped (the loop's `select!` on cancel); the HTTP connection closes | The provider may finish generating server-side |
| Local tool (`fs.*`, `skill.read`, …) | The call future is dropped by the executor's `select!` | A write already issued to the OS may have completed |
| `terminal.run` | Future dropped; `kill_on_drop(true)` kills the child | Effects the process already caused remain |
| `web.fetch` / `web.search` / `rag.realtime` | Request future dropped; connection closed; no further hop | The remote server may already have acted on the request |
| HTTP MCP `tools/call` | Request future dropped | The server may complete the call; the response is discarded |
| stdio MCP | Request future dropped; the server process keeps running for other runs | The server may complete the call (untrusted runs never get stdio MCP) |
| Open Terminal | No new command is sent | A started remote command may continue; its cleanup on cancellation is best-effort (unchanged behaviour) |
| Delegated worker | Shares the run's token: its model stream and tools are dropped; its own checks refuse further tools and turns; its result is replaced by the refusal | As above, per tool |
| Approval wait | The wait ends; a queued or later decision is refused | — |
| Jev preflight | Not started for a revoked run; one in flight shares the run's token | The classifier may have received the minimized description |

Work that cannot be reliably cancelled (remote services, stdio servers,
Open Terminal) gets no new dispatch once a revocation is observed. The
tests above prove that no further request is sent.

## 6. Cross-process limitations

- The CLI runs in its own process and commits to `identity/state.json`. A
  server with a persistent authority (not wired yet) observes such a change
  only at a run's next enforcement point (pull). A run blocked in one long
  model stream or tool sees it only after a push or a `revalidate`.
- No notification exists across processes. Activation must add **bounded
  revalidation**: one check per active revocable run on a fixed interval,
  or when the store's generation changes. No unbounded filesystem polling
  is added here: the only periodic work is the existing 2-second
  stream-guard check.
- A check of a persistent authority reads and parses `state.json` (bounded
  by the store's own limits). At scale, a constant-time generation probe
  (for example a generation file written in the same transaction) would
  avoid re-parsing when nothing changed. This is a follow-up.
- Expiry is judged at check time. Nothing schedules a check at the moment a
  credential expires, so the bounded revalidation above also covers
  expiry.
- Several server processes each supervise only their own runs. A
  cross-process push needs a shared signal; pull and revalidation apply in
  every process.

## 7. Integration boundary

- `create_run` in multi-user mode starts runs with `start_supervised`.
  Multi-user mode is still constructed only by tests, so this path is
  unreachable in production.
- `LightweightRunFactory::run_supervised` wires the authorization into the
  executor, provider, delegation and approvals. It is reached only through
  `start_supervised`, so it is unreachable too.
- The tests use a test-injected authority (`IdentityAuthority::new` with an
  injected clock, or `persistent` over a scratch store) and a test factory
  that drives the real `AgentLoop` and `BoundedExecutor` exactly as the
  production factory wires them.
- No end-to-end production revocation is claimed.

## 8. Security tests

| # | Requirement | Test |
|---|---|---|
| 1 | Issuance without plaintext persistence | `identity_admin::tests::issuing_never_persists_or_reports_the_secret` |
| 2 | Rotation and expiry | `…::rotation_and_expiry`; `revocation::expiry_ends_streams_and_runs_and_stales_approvals` |
| 3 | Credential revocation | `…::revocation_and_suspension`; `revocation::revocation_during_inference_stops_the_model` |
| 4 | Principal suspension | `…::revocation_and_suspension`; `revocation::suspension_while_a_tool_executes_tears_it_down` |
| 5 | Profile reassignment | `…::ownership_assignment_reassignment_and_removal`; `revocation::reassignment_stops_only_the_previous_owners_run` |
| 6 | Ownership removal | `revocation::revocation_before_a_run_starts_refuses_it`; `revocation::revocation_during_an_approval_wait_ends_it` |
| 7 | Run-specific cancellation | `revocation::concurrent_runs_are_revoked_precisely`; `revocation::only_an_operator_may_revoke_runs` |
| 8 | SSE stream termination | `revocation::expiry_ends_streams_and_runs_and_stales_approvals` |
| 9 | Tool dispatch blocked | `lightagent-tools` `revocation::a_revoked_run_launches_no_tool`; `revocation::revocation_before_a_tool_blocks_it_and_any_further_turn` |
| 10 | Remote HTTP blocked | `lightagent-tools` `revocation::remote_http_tools_respect_revocation`, `a_revoked_fetch_opens_no_connection`; `lightagent-mcp` `revocation::*` |
| 11 | Delegated-worker cancellation | `revocation::delegated_workers_follow_the_runs_authorization` |
| 12 | Stale approval rejection | `revocation::a_stale_approval_never_resumes_the_run`; tools `an_approval_granted_before_revocation_is_stale`; HTTP 409 in `expiry_ends_streams_…` |
| 13 | Cross-profile isolation | `revocation::reassignment_stops_only_the_previous_owners_run`, `concurrent_runs_are_revoked_precisely` |
| 14 | Concurrent run revocation | `revocation::concurrent_runs_are_revoked_precisely` |
| 15 | Identity-store corruption | `identity_admin::tests::a_corrupt_or_newer_store_fails_closed`; `revocation::an_unreadable_identity_store_stops_revocable_runs` |
| 16 | Invalid operator commands | `identity_admin::tests::invalid_operator_commands_are_refused` |
| 17 | Audit without secrets | `…::issuing_never_persists_or_reports_the_secret`; one coded record in `revocation_before_a_tool_blocks_it…` |
| 18 | Single-user compatibility | `revocation::single_user_runs_are_unchanged`; tools `an_unrevocable_run_is_unchanged`; core `an_unconditional_run_is_never_consulted…`; the unchanged `ownership.rs` and `api.rs` suites |
| 19 | Jev permissions independent | `identity_admin::tests::identity_changes_leave_settings_and_jev_permissions_alone`; the check consults identity state only |
| 20 | Experimental sandbox inaccessible | tools `revocation::the_experimental_sandbox_stays_inaccessible_to_untrusted_runs` |

Waits use explicit signals (`Notify`, terminal status, watch channel). A
timeout appears only as a guard against a hang. No real credentials,
providers or paid services are used.

## 9. Credential and audit privacy review

- Tokens are never stored, logged, serialized or placed in a report.
  `IssuedToken` has a redacted `Debug` and no `Serialize`. CLI JSON carries
  key ids only.
- Listings show key ids, state, timestamps and restrictions. They never show
  the `verifier` field.
- Audit records hold fixed codes, principal ids, key ids and profile ids.
  Revocation messages carry only the denial code. No message, path or
  argument is included.
- The secret file is created new and owner-only. A failed delivery revokes
  the credential.
- `revoked_message` is the only text a revoked run emits. It is the same
  for every caller and reveals nothing about other principals.

## 10. Backward compatibility

- Single-user runs (`start`/`start_as`, ACP, terminal) take the unchanged
  `run_as` path with an unconditional authorization. The executor carries no
  authorization. `AuthorizedProvider` over an unconditional authorization
  passes through without consulting anything. `drive` produces the same
  events as before.
- API: `get_run` gains `revocation` only for a revoked run, and
  `respond_approval` returns 409 only for one. Neither can happen in
  single-user mode.
- `authorize_run` re-reads the caller first. Responses are unchanged (still
  404 / omitted), and the audit code for a stale caller is now the precise
  `revoked`/`expired`.
- `RunFactory::run_supervised` is a provided method with a default that
  checks at the start and then runs `run_as`, so existing implementors
  compile unchanged. `RunOwner` gains a private field behind the same
  constructors.
- `ToolCtx` gains a public `authorization` field. `ToolCtx::new` sets it, and
  no workspace code builds `ToolCtx` literally.
- No new dependencies; `Cargo.lock` is unchanged. Identity store schema 1 is
  unchanged, and audit uses existing actions.

## 11. Migration and rollback

There is nothing to migrate. Rolling back means reverting the PR: a store the
CLI created stays readable by the previous code (same schema) and is read by
nothing in a running server. To remove it, an operator deletes
`<home>/identity/` and the profiles' `.identity-binding` markers. Secret
files written with `--secret-file` belong to the operator.

## 12. Remaining multi-user blockers

1. Serve wiring: a persistent authority plus an explicit activation
   setting with startup validation. This PR adds neither.
2. Bounded revalidation (interval or generation change) for push
   cancellation across processes, including expiry (§6).
3. Profile-scoped session, tool-catalog and settings endpoints. These are
   operator-only in multi-user mode today.
4. A verified execution sandbox before untrusted runs get local execution.
   The experimental backend stays refused.
5. A provisioning service and external-issuer flows. Not implemented, by
   design.
6. Operator-credential lifecycle (issue and rotate operator credentials,
   replace the legacy key). Deliberately not in this CLI.
7. Credential-attempt rate limiting and lockout.
8. A dedicated revocation event type for clients. Today a revocation is an
   `error` event plus `revocation` on the run.
9. Guaranteed remote cleanup for Open Terminal on cancellation.
10. A constant-time generation probe for checks at scale.
