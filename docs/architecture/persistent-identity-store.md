# Persistent identity store

Status: **implemented, not activated.** Identity, ownership, grants,
credentials and audit now survive restarts in a durable, transactional store,
and `IdentityAuthority::persistent` reads them. `lightagent serve` still never
builds a multi-user authority. There is no activation flag, no new route,
and no CLI. Single-user installations are untouched.

## 1. What existed (audit, master `b5b5a12`)

| Area | Source | State before this change |
| --- | --- | --- |
| Principals, roles, capabilities | `lightagent-core/src/identity/mod.rs` (`PrincipalId`, `Role`, `Capability`, `Restriction`, `Caller`, `authorize`, `authorize_capability`) | Pure policy over values. No principal registry: a principal existed only inside a credential record. |
| Credentials | `identity/credential.rs` `CredentialStore` | 256-bit `getrandom` secrets, `la_<16 hex>_<64 hex>`, plain SHA-256 of the secret, constant-time compare, revocation. No expiry, no rotation. **In memory only.** |
| Ownership | `identity/ownership.rs` `OwnershipRegistry` | Owner + grants, `assign` (idempotent / conflict), `reassign` (drops grants), `remove`. **In memory only.** No link to the profile on disk. |
| Audit | `identity/audit.rs` `AuditRecord`; `lightagent-api/src/auth.rs` `IdentityAuthority::record` | Fixed codes only. Last 4,096 records **in memory**. |
| Multi-user authorization | `lightagent-api/src/auth.rs` `IdentityAuthority`, `AuthConfig::multi_user` (#16) | `RwLock<IdentityState>`, read per request; `update()` mutates memory. `serve` never builds it. |
| Run ownership | `lightagent-api/src/manager.rs` `RunOwner`, `start_as` | In memory, per server lifetime |
| Profiles | `lightagent-core/src/profile.rs` `ProfileStore` | `profiles/<id>/profile.json`. Ids are immutable (no rename). `delete` removes the directory. No creation identity, so a re-created profile is indistinguishable from the old one. |
| Persistence | `lightagent-core/src/paths.rs` `write_private` (temp file + rename, `0600`, `0700` dirs); `lightagent-store` (sessions as JSON files) | Owner-private atomic JSON files. **No `fsync`.** No SQL database in the workspace. |
| Config and secrets | `config.rs` `SecretRef` (env / owner-only file) | Secrets by reference only |
| CLI | `lightagent/src/lib.rs` `config get/set`, `profile …` | Nothing for identity |
| Schema versioning | `config.rs` serde defaults | No identity schema |

## 2. Backend choice

**One versioned JSON document, `<home>/identity/state.json`, written
transactionally**, alongside a bounded `audit.jsonl`.

- It reuses the storage Lightagent already trusts: owner-private files in
  the home directory, replaced atomically. That avoids a second database and
  a new dependency, as the project's dependency rules require.
- `rusqlite` is permitted by the dependency policy but absent from the
  workspace. For a few thousand principals and profiles, one small document
  gives full-state transactions with nothing to join or migrate per table.
  The schema keeps a migration path if a database is ever justified.
- What a database would add, and how this store provides it:

| Property | Implementation |
| --- | --- |
| Atomic transactions | Every change works on a copy, then: temp file → `sync_all` → `rename` over `state.json` → directory `sync_all` (Unix). Readers see the old document or the new one, never a partial one. |
| Isolation across processes | Exclusive `File::lock` (std, Rust ≥ 1.89; `flock` on Unix, `LockFileEx` on Windows) on `identity/lock` for the whole read-modify-write. Tested with 8 concurrent writers on separate handles, on all three platforms. |
| Durability | `fsync` of the file and the directory (an improvement over `write_private`, which does not sync) |
| Recovery | The previous generation is kept as `state.prev.json` on every commit |
| Fail closed | Unparseable, schema-less or newer documents are errors. They are never replaced by an empty state, and never overwritten. |

**Limits:**
- Every authorization reads and parses the whole document. That is fine at
  the intended scale; a generation-stamped cache can come later without
  allowing stale reads (§8).
- Very large installations would want an indexed store.

## 3. Schema and migrations

`IdentityDocument` (schema `1`), `serde` JSON:

| Field | Content |
| --- | --- |
| `schema` | `1` |
| `generation` | Increments on every commit |
| `principals` | `PrincipalRecord { id, role, status: active \| suspended, created_unix, updated_unix, external: Option<{issuer, subject}> }` |
| `trusted_issuers` | `https://` issuers whose external identities may be mapped |
| `credentials` | `CredentialRecord { key_id, principal, role, verifier, restriction, created_unix, revoked_unix, expires_unix, replaces }` |
| `ownership` | `profile → { owner, grants: principal → capabilities }` |
| `bindings` | `profile → random 128-bit marker` (see §5) |
| `reassignments` | `{ profile, from, to, by, at_unix }`, the last 10,000 |
| `idempotency` | `key → { fingerprint, generation, at_unix }`, the last 10,000 |

**Loading:**
- Absent: an empty document, generation 0. Nothing is created.
- `schema == 1`: parsed.
- `schema > 1`: `IncompatibleSchema`.
- `schema < 1`: `migrate`. There is no older schema, so this is
  `MigrationFailed`.
- Anything unparseable or missing `schema`: `Corrupt`.

None of the failures writes anything.

**Future migrations:** one step per version in `migrate`. The first commit
after a migration keeps the original as `state.prev.json`. A downgrade sees
a newer schema and refuses.

## 4. Credential security

| Requirement | Implementation |
| --- | --- |
| 256-bit random tokens | Unchanged: `getrandom`; issuing fails without entropy |
| No plaintext at rest | Only `verifier` is stored. Test `nothing_persisted_holds_a_plaintext_credential` scans every file the store writes. |
| Verifier | `v1:` + SHA-256(`"lightagent api credential verifier v1\0"` ‖ key id ‖ `0x00` ‖ secret) |
| Safe comparison | Constant-time over the equal-length verifier |
| Revocation | `revoked_unix`; checked at every authentication and refresh |
| Expiry | `expires_unix`; `Denial::Expired` (401, same response as revoked); an expiry not in the future is refused at issue |
| Replacement before revocation | `rotate_credential` issues a new credential (`replaces` = old id) while the old one keeps working until revoked |
| Never exposed after issue | `IssuedToken` is not serializable and its `Debug` is redacted. Errors and audit carry key ids and codes only. |
| Legacy `--key-env` | Untouched and never stored. A user credential is always issued by the store, never derived from it. |
| Backups | The identity directory holds no secret; a backup of it cannot authenticate anyone |
| File permissions | `identity/` `0700`; every file `0600` (tested on Unix). Windows inherits the user profile's ACL, as other Lightagent files do. |

**Why SHA-256 and not a password hash.** The secret is uniformly random with
256 bits of entropy. A slow, salted password hash defends against guessing
low-entropy, human-chosen secrets. Here, guessing is a 2^256 search however
fast the hash is, so a slow hash would only cost every request. The domain
label and key id separate these verifiers from any other SHA-256 use, and
bind each verifier to its record.

**Why no server key (HMAC pepper).** A pepper protects verifiers from offline
guessing after a database leak, which 256-bit secrets already rule out. It
would add a key to store, rotate and back up separately, for no gain.

**Format change.** The verifier format changed from `secret_sha256` (plain
SHA-256) to `verifier` (`v1:`). No credential was ever persisted before this
change, so nothing needs migrating.

## 5. Ownership and authorization

- **Who may change anything.** A transaction's actor must be an operator
  holding `provision` or `administer`. Each operation then requires:

| Operation | Capability |
| --- | --- |
| create a user, issue / rotate / revoke a user credential, suspend a user, assign an owner, revoke a grant, release a profile | `provision` |
| create an operator, operator credentials, reassign, add a grant, trust an issuer | `administer` |

  A user can never transact. A provisioning service would hold an operator
  credential restricted to `{provision}`, never full operator authority.
- **Escalation.**
  - A credential's role is the principal's own role, never chosen by the
    caller.
  - A user credential may not list operator-only capabilities.
  - Grants never include operator-only capabilities
    (`OwnershipError::OperatorOnly`).
  - Owners must be active users.
- **Ownership semantics (unchanged from #16, C3 and C6).**
  - Unowned profiles are operator-only.
  - Ownership is never inferred from a name.
  - `assign` is idempotent for the same owner and a conflict for another.
  - `reassign` is `administer`-only and transactional; it drops the
    previous owner's grants and records history.
  - The owner gets only the C6 minimum bundle.
  - Ownership grants no tool, no credential grant, no destination and no
    Jev permission. Those stay with the tool policy (#17), the egress
    grants (#20) and `platform.jev`.
- **Checked against the profile registry.**
  - Assigning requires `profiles/<id>/profile.json`.
  - Assigning writes a random marker, `profiles/<id>/.identity-binding`, and
    records it.
  - `effective_ownership` counts an ownership record only while the
    manifest exists **and** the marker matches.

| Situation | Behavior |
| --- | --- |
| Profile deleted (`ProfileStore::delete` removes the directory and marker) | Ownership stops counting at the next check. The profile is operator-only. |
| Re-created with the same id | No marker, or a different one: unowned, operator-only, until the operator releases and re-assigns |
| Copied or forged marker | Must equal the recorded random value: refused |
| Renamed | Ids are immutable; a "rename" is delete + create, so ownership does not follow |
| Missing profile at assign | `ProfileMissing` |

- **Not revealing existence.** Unowned, missing and other-owner profiles all
  map to the same API response (`rejection` groups
  `ProfileUnowned`/`ProfileNotGranted`/`ProfileOutsideCredential`).

## 6. Revocation and concurrency

**Consistency.**
- `IdentityAuthority::persistent` keeps **no cache across checks**.
- `authenticate`, `refresh`, `authorize_profile` and `authorize_run` each
  re-read `state.json` and the binding markers first.
- A commit is visible to the next check in any process. A check racing a
  commit sees the whole old document or the whole new one.
- An unreadable store empties the in-memory state: everything is denied.
- `update()` is refused in persistent mode; changes are transactions.

| Event | Effect | Tested |
| --- | --- | --- |
| Server restart | Nothing lost: state, revocations, ownership | `identity_and_ownership_survive_a_restart`, API test |
| Revocation from another process | Next request `Revoked`, including `refresh` of an authenticated caller | API test |
| Rotation | Old and new both valid until the old is revoked | `expiry_rotation_and_revocation_are_durable` |
| Suspension | Revokes every credential; reactivation restores none | same |
| Reassignment | Previous owner and its grantees lose access at the next check | `reassignment_is_transactional_and_drops_old_grants` |
| Profile deletion | Ownership stops counting | `ownership_counts_only_for_the_same_existing_profile`, API test |
| Concurrent writers | Serialized; no lost write; exactly one of 8 racing owners wins | `concurrent_transactions_serialize_without_losing_writes` |
| Concurrent readers | See a whole document | atomic rename |

**Running tasks and open streams.**
- Event streams re-check the caller (`refresh`) as they deliver, from #16,
  so a revoked caller's stream stops.
- **A run already executing is not cancelled** when its starter's
  credential or ownership is revoked; it finishes, though its events can no
  longer be read. Cancellation on revocation is not wired, and is a
  **blocker** before multi-user activation (§11).

## 7. Audit records

`identity/audit.jsonl`. One JSON line per record, appended with `fsync`,
`0600`:

| Field | Meaning |
| --- | --- |
| `at_unix` | Time |
| `generation` | The commit a change record belongs to. `null` for refusals and request decisions. |
| `principal` | The actor (or caller) id |
| `key_id` | The credential involved (public id), never a secret |
| `action` | `credential_issued`, `credential_rotated`, `credential_revoked`, `principal_created`, `principal_suspended`, `principal_reactivated`, `issuer_trusted`, `owner_assigned`, `owner_reassigned`, `owner_removed`, `grant_added`, `grant_revoked`, `idempotent_replay`, `authenticate`, `request:<capability>` |
| `profile` | The profile id, when relevant |
| `subject` | The principal a change was about |
| `outcome` | `allowed` or `denied:<code>` |

- No free-text field exists, so prompts, tokens, secrets and tool outputs
  cannot be recorded.
- Change records are written **before** the commit, tagged with the
  generation. A crash after that leaves records for a generation that was
  never committed, which a reader can detect, rather than a commit with no
  record.
- Refused transactions and the persistent authority's authorization
  decisions are recorded too.
- **Retention.** `AuditRetention { max_file_bytes: 1 MiB, max_files: 8 }`
  by default. Rotation moves `audit.jsonl` to `audit.1.jsonl` and onwards,
  discarding the oldest, which bounds storage (tested with small limits).

## 8. Provisioning compatibility (data model only)

The future Cloudflare Workers account-control service is supported by:
- **Stable identity.** Immutable `PrincipalId`, plus an optional
  `ExternalIdentity { issuer, subject }`.
  - It is accepted only from an issuer the operator registered
    (`trust_issuer`, `https://` only).
  - Each external identity maps to at most one principal.
  - The store does **not** verify tokens. The service must verify the
    issuer's signatures and audience itself, and then call Lightagent.
- **Idempotency.** `transact(.., Some((key, fingerprint)), ..)`:
  - The same key and fingerprint is `Replayed`: no change, and audited.
  - The same key with a different request is `IdempotencyConflict`.
- **Least privilege.** An operator credential restricted to `{provision}`
  can provision a user, assign an owner, issue and revoke user credentials,
  and release a profile. It cannot create operators, reassign, grant, or
  trust issuers. Tested.
- **Status queries.** `load()` gives principals, ownership, credential
  metadata and generation.
- **Transactions.** Multi-step provisioning (create user → assign → issue)
  runs in one transaction, or not at all.

Not implemented here: the provisioning API, the website, OIDC verification,
or any route.

## 9. Security tests

`crates/lightagent-core/src/identity/store/tests.rs` (12 tests) and
`lightagent-api` `auth::tests::a_persistent_authority_follows_the_store_and_fails_closed`.
All use temporary homes and synthetic identities, and run on Linux, macOS
and Windows.

| # | Requirement | Test |
| --- | --- | --- |
| 1 | Persistence across restart | `identity_and_ownership_survive_a_restart`; API test |
| 2 | Issuance and verification (incl. a forged secret) | same |
| 3 | Expiry | `expiry_rotation_and_revocation_are_durable` |
| 4 | Rotation | same |
| 5 | Revocation (also suspension) | same; API test |
| 6 | Ownership persistence | `identity_and_ownership_survive_a_restart` |
| 7 | Transactional reassignment | `reassignment_is_transactional_and_drops_old_grants` |
| 8 | Concurrent assignments | `concurrent_transactions_serialize_without_losing_writes` |
| 9 | Duplicate provisioning identifiers | `duplicate_identities_and_idempotency_keys_are_handled` |
| 10 | Storage corruption | `corruption_fails_closed_and_interrupted_writes_leave_the_old_state`; API test (store unreadable → deny) |
| 11 | Interrupted writes | same (stray temp file, previous generation kept) |
| 12 | Migration failures | `newer_and_unmigratable_schemas_are_refused_without_writing` |
| 13 | Incompatible schema | same |
| 14 | Audit retention | `audit_is_bounded_and_carries_no_secret` |
| 15 | No plaintext credentials; owner-only files | `nothing_persisted_holds_a_plaintext_credential`; existing `credential` test updated for the `v1:` verifier |
| 16 | Unauthorized owner changes | `users_cannot_change_identity_and_credentials_cannot_escalate`; `reassignment_…` (provision-only actor) |
| 17 | Capability escalation | same |
| 18 | Cross-profile authorization; missing and re-created profiles | `ownership_counts_only_for_the_same_existing_profile`; API test |
| 19 | Single-user compatibility | `an_installation_without_identity_state_is_untouched`; every existing test unchanged |
| 20 | No change to Jev and Lightweight routing | No Jev, coordinator, provider or Lightweight file changed; Jev evaluation and paired E2E checks |

## 10. Compatibility, rollback and recovery

- **Existing installations.** No `identity/` directory: nothing is read or
  written, and single-user mode never consults it. The legacy `--key-env`
  key and loopback compatibility behave exactly as before. No profile is
  ever assigned automatically.
- **API types.**
  - `Denial::Expired` added; `Denial::code()` gains `expired`.
  - `CredentialRecord` gains `expires_unix` and `replaces`, and renames
    `secret_sha256` to `verifier`. Nothing was persisted in the old format.
  - `AuditAction` gains change codes.
  - `IdentityAuthority::persistent` added; `IdentityAuthority::new` is
    unchanged.
- **Rollback.** Revert the PR. Older builds have no identity store and
  never read `identity/`, so a directory left behind is inert. Nothing else
  was written, and no configuration key was added.
- **Recovery.**
  - From a corrupt or partial `state.json`: stop the server, copy
    `state.prev.json` (or a backup) over `state.json`, and restart.
  - Audit records whose `generation` is greater than the document's are
    from a commit that never landed.
  - Backups need no secret handling, but must be kept owner-private: they
    reveal who owns what.

## 11. Remaining blockers before multi-user activation

1. **Operator interface.** A local CLI, `lightagent identity …`, for
   principals, credentials, ownership, grants, issuers and audit. Today
   only the library API exists.
2. **Wiring.** `serve` building `IdentityAuthority::persistent`, behind an
   explicit, reviewed activation flag, with C1–C6.
3. **Cancellation on revocation.** In-flight runs of a revoked credential,
   suspended principal or reassigned profile must be cancelled (§6).
4. **Scoped endpoints.** Sessions, `/profiles`, `/tools` and `/jev`, for
   users (still operator-only).
5. **Verified execution isolation** (the sandbox is experimental), plus
   durable profile-scoped remote credentials (the credential store
   designed in `remote-tool-egress.md` §11).
6. **Ingress.** TLS and trusted ingress, and rate limiting of failed
   authentications.
7. **Provisioning.** The provisioning API, with OIDC issuer verification,
   on the model in §8.
8. **Profile deletion hook.** `ProfileStore::delete` does not call
   `remove_profile`. The binding check makes the stale record inert, but
   the CLI should release it.
9. **Scale.** A generation-stamped cache, if per-request reads become
   measurable, without ever serving a stale revocation.
