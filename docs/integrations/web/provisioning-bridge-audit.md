# Web provisioning bridge: audit and delta (stage L0)

Status: **audit only. Nothing here is implemented, mounted or activated.**
This is not the protocol specification. The canonical specification is to be
`docs/PROVISIONING_PROTOCOL.md` in the Web repository (`lightagent-control`)
after joint review. This repository will mirror a versioned copy and its
fixtures here once that file exists and is frozen.

## 1. Evidence baseline (2026-10-13)

| Item | Value | Kind |
|---|---|---|
| Lightagent `master` | `84fa762` (PR #25 merge: profile remote credentials) | built, merged |
| PR #26 `security/profile-provisioning-api` | `340cb48`, open, mergeable, not activated | open-PR code |
| PR #26 CI at `340cb48` | all green: check linux-x64 / macos-arm64 / windows-x64, package smoke, standalone Web UI render, paired Lightweight E2E, Open Terminal, Qdrant + Infinity, Jev evaluation, bubblewrap, revocation latency ([run 38394054537](https://github.com/dlroqa/Lightagent/actions/runs/38394054537)) | baseline |
| Web `lightagent-control` | `775c7c7` on `phase-5-account-ui`, clean | read only |
| Web `docs/PROVISIONING_PROTOCOL.md` | **does not exist** | planned |
| Web signing, canonicalization, Ed25519 code | **none** (only D1 tables `provisioning_jobs`, `provisioning_nonces`, `harnesses.key_id/public_key`) | planned |

This branch is stacked on PR #26 because every later stage reuses its
`ProvisioningService`. It does not merge or change PR #26.

## 2. L0 gate result: blocked

The stage-L0 gate requires a frozen `LA-WEB-PROVISIONING/1` specification and
shared Ed25519 fixtures before any transport is written. Neither exists on
either side. Therefore this branch adds **no** verifier, canonicalizer,
signing format, gateway route, callback sender, configuration section,
dependency or compatibility manifest. A manifest with no pinned spec hash
would claim compatibility that cannot be checked.

## 3. Conflicts with existing code (decisions needed before the spec freezes)

| # | Contract or Web expectation | Lightagent today (PR #26) | Needed |
|---|---|---|---|
| C1 | Harness sends signed callbacks to Web (L4) | `profile-provisioning.md` §10.6: "Status, not callbacks. Lightagent does not call the website." No outbound HTTP exists in this path | An explicit decision to add an outbound channel. Polling `status` alone already satisfies "authenticated status/reconciliation" and adds no egress, SSRF or outbox surface. Recommendation: ship polling first, callbacks as a later stage |
| C2 | Web operations `profile.disable` and `profile.enable` (D1 CHECK) | No reversible disable exists. Suspending a principal is operator-only and affects every profile that principal owns | Exclude `disable`/`enable` from v1. Do not alias them to `revoke` |
| C3 | Web `profile.revoke` (data retained, runs cancelled) | `revoke` removes the owner binding in one identity transaction. The profile is kept for the operator, and runs stop at their next revalidation. It is addressed by **operation id**, not profile id, and is terminal (no re-grant) | Compatible in meaning. The spec must say revoke is terminal, and the adapter must resolve `(provisioner, profile)` to its operation |
| C4 | `agent_profiles.agent_name` 1–80 (Zod, UTF-16 units) | `display_name` 1–64 Unicode scalar values, no control characters, no surrounding whitespace | Web names of 65–80 characters fail with `invalid_display_name`. Either Web limits names to 64 for provisioning or the spec defines truncation. The counting unit must be stated |
| C5 | `revocation_version` generation carried in commands; harness rejects older generations | No per-profile generation. Ordering comes from the ledger state machine (`completed` → `revoked`, terminal) | The adapter must store the highest generation it has seen for each profile and reject lower ones. Because revoke is terminal, a stale `create` cannot reopen a revoked profile today |
| C6 | Subject sent for the user | Principal derived from `(issuer, subject)`, never from a body id | The spec must name the subject. An immutable `usr_…` is acceptable as an opaque subject of the Web issuer, because Lightagent never uses it as a principal id |
| C7 | Signing authority | Admission is credential-based: role `operator`, restriction exactly `{provision}`, active principal, unrevoked registration, re-read on every call | See §4. The signature can replace possession of the bearer secret. It cannot replace the registered credential |
| C8 | `permissions_json` ceiling `{chat, memory, rag, web, mcp, shell}` | Only template `assistant` (no tools, strict policy). Requests carrying permissions are refused | The v1 spec carries a template name only. Web's ceiling is advisory to Web and grants nothing on the harness |
| C9 | Web `public_key` stored as standard base64 with padding; the contract asks for base64url signatures | No format exists | The spec must fix one encoding per field |
| C10 | Job states `pending/sending/accepted/completed/failed/cancelled` | Operation states `pending/profile_created/completed/failed/revoked` | The spec must map them. `accepted` must not mean `completed`, and `profile_created` reads as "in progress" |

Compatible as is (verified against source):

- **Profile id.** `agt_` + 26 of `[a-z0-9]` (Web `randomId`) satisfies
  `ProfileId` `^[a-z0-9][a-z0-9_-]{0,63}$`. No external mapping table is
  needed. Web's D1 CHECK `agt_[a-z0-9]*`, 12–64, also fits.
- **Idempotency key.** Web's 16–128 `[A-Za-z0-9_-]` is a subset of
  Lightagent's 8–128 `[A-Za-z0-9._:-]`.
- **Body bound.** The PR #26 limit of 8 KiB (`MAX_BODY_BYTES`) stays the
  ceiling for signed commands.

## 4. Adapter seam (no change to PR #26 needed)

`ProvisioningService::admit` (`crates/lightagent-core/src/provisioning.rs`)
takes a `Caller` whose source is `PrincipalSource::Credential { key_id }`. It
re-resolves that key id with `CredentialStore::caller_for_at` under the
ledger lock and does not re-check the secret, because the HTTP layer did
that. A gateway can therefore work without any change to the service:

1. Verify the signed command (stage L2, against the frozen spec).
2. Map the verified Web signing key id to one **operator-registered** local
   provisioning credential key id. This mapping is configuration owned by
   the operator, not command content.
3. Build the `Caller` through `caller_for_at`. `admit` then applies every
   existing check: revocation, rotation, expiry, `{provision}`-only, active
   principal, registration, issuer, template, quota and the credential's
   profile limit.
4. Call `provision`, `status` or `revoke`. For `status` and `revoke` by
   profile, find the caller's own operation for that profile in the ledger.
   `owned_operation` re-checks ownership under the lock.

Result: there is one ledger and one authority. A valid signature with no
registered credential admits nothing. Revoking the local credential stops
the gateway even while Web's key is still trusted.

## 5. Planned changed files (not started)

| Stage | Files | Blocked on |
|---|---|---|
| Spec mirror | `docs/integrations/web/LA-WEB-PROVISIONING-1/` (spec copy, fixtures, `MANIFEST.sha256`), `fixtures/` loader test | Web freezes the spec and fixtures |
| L1 config | `crates/lightagent-core/src/control_webhooks.rs` (fail-closed parse; off by default), `config.rs` | Spec key and id formats (C9) |
| L2 gateway | `crates/lightagent-api/src/control_gateway.rs` behind a new cargo feature, a separate `Router` that is never merged into `router()` without the opt-in. Ed25519 through `ring` (already in `Cargo.lock` via rustls), so no new crypto crate. Canonicalization per the spec | Spec, C1–C3 |
| L3 adapter | the §4 seam, plus a per-profile generation store (C5) | C5, C6 |
| L4 callbacks | outbox under `<home>/control/` | C1 decision |
| L5 | tests, `check.yml` feature jobs, offline fixture job, staging runbook | all of the above |

## 6. What needs approval

- Web authors and freezes `PROVISIONING_PROTOCOL.md` and the fixtures,
  resolving C1–C10.
- Decision on C1: polling only for v1, or callbacks too.
- PR #26 review and merge, with explicit approval. Until then this work stays
  stacked on it.
