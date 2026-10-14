# Handoff to Web: what `LA-WEB-PROVISIONING/1` must specify

Audience: the coding agent writing `docs/PROVISIONING_PROTOCOL.md` and its
fixtures in `lightagent-control`. Lightagent baseline: PR #26 at `340cb48`
(open, not activated). The full evidence is in
`provisioning-bridge-audit.md`, next to this file.

Lightagent will implement **only** against a frozen, versioned and reviewed
copy of the specification and fixtures. Anything this document leaves open
is Web's to decide in the specification. Lightagent will not invent it.

## 1. Approved decisions

- v1 commands: `profile.create`, `profile.status`, `profile.revoke`. Nothing
  else. `profile.disable` and `profile.enable` are out of v1. Runtime
  invocation is out of scope.
- **Polling only.** The harness never calls Web. There are no callbacks,
  no outbox and no callback URL anywhere in the protocol.
- Revoke is **irreversible** removal of the owner binding. The profile's
  data stays on the harness for its operator. Revoked `agt_` ids can never
  be provisioned again.
- Display names are limited to 1–64 characters for provisioning.
- A valid Web signature is **never** authority on the harness by itself
  (see §3).

## 2. The ten audited items: what the spec must say

| # | Item | Required in the spec |
|---|---|---|
| C1 | Callbacks | None. Status comes from `profile.status` polling only |
| C2 | Disable / enable | Not part of v1. A harness receiving them answers `unsupported_operation`. D1 may keep the values, but v1 never sends them |
| C3 | Revoke | Addressed by the **operation id** that `profile.create` returned. Terminal. Repeating it returns the same result |
| C4 | Display name | 1–64 **Unicode scalar values** (not UTF-16 units, so `string.length` is wrong), no control characters, no leading or trailing whitespace. Web validates before signing |
| C5 | Revocation version | Not in v1. Web follows W1–W4 (§6) instead |
| C6 | Subject | Name the field and its source, for example the immutable `usr_…` id. It must be 1–255 printable ASCII. The harness derives its own principal from `(issuer, subject)` and never uses `usr_…` as a principal id |
| C7 | Authority | Signature verification proves which Web key signed. Local admission is separate (§3) |
| C8 | Permissions | Commands carry only `template: "assistant"`. No permissions, tools, model, URL, path or credential field. `permissions_json` stays a Web-side ceiling and grants nothing on the harness |
| C9 | Encodings | One encoding per field: the public key (D1 stores standard base64 with padding), the signature (contract: base64url without padding) and the key id. Mixing them must fail |
| C10 | State mapping | Map the harness operation `state` to D1 job and profile states (§4). HTTP `200`/`201` alone is never `completed` |

## 3. Authentication boundaries Lightagent will enforce

In this order, all before any side effect:

1. Transport: HTTPS to the operator-registered origin. Body ≤ **8 KiB**,
   `application/json`.
2. Strict parse: duplicate JSON keys and unknown fields are refused before
   canonicalization.
3. Signature: Ed25519 over the canonical bytes defined in the spec. The key
   id must be one the harness operator configured. Purpose/domain
   separation, audience = **this harness id**, `issuedAt`/`expiresAt` with
   stated skew, and a nonce not seen within its validity window.
4. **Local admission (PR #26, unchanged).** The operator maps the Web key
   id to one local provisioning credential: role `operator`, restriction
   exactly `{provision}`, an active principal, and an unrevoked provisioner
   registration listing the allowed issuers, templates and quota. All of
   this is re-read on every command. If the operator revokes that local
   credential, the next command is refused even with a valid signature.
5. Command checks (PR #26): the issuer is registered for this provisioner,
   the template is allowed, the quota holds, the profile id is unknown, and
   status and revoke reach only this provisioner's own operations (anything
   else is `unknown_operation`).

The signed key cannot reach any run, session, tool, settings, Jev or
administration route. That is already tested route by route in PR #26.

## 4. Exact harness semantics (PR #26)

### `profile.create`

| Field | Rule |
|---|---|
| `profileId` | `^[a-z0-9][a-z0-9_-]{0,63}$`, not `default`, unknown to the harness (`agt_` + 26 base-36 characters fits) |
| `issuer` | `https://` URL, ≤ 256 bytes, printable ASCII, no userinfo, query or fragment. Must match the operator registration **byte for byte** |
| `subject` | 1–255 printable ASCII |
| `displayName` | as in C4 |
| `template` | `"assistant"` |
| `idempotencyKey` | Web's 16–128 `[A-Za-z0-9_-]` (the harness accepts 8–128 `[A-Za-z0-9._:-]`) |

Idempotency is scoped to `(provisioner, key)`. The same key with the same
`(issuer, subject, profileId, displayName, template)` returns the same
operation, resuming it if interrupted, even after it was revoked. The same
key with any of those changed is `idempotency_conflict`, and nothing
changes.

Result: `operationId` (32 lowercase hex), `state`, `failure` (code or null),
`principalId` (opaque `ext-…`; store it, do not interpret it), `profileId`,
`template`, `ownerBound`, `createdUnix`, `updatedUnix` (seconds), and
`replayed`.

### `profile.status` and `profile.revoke`

Both take `operationId`. Status is read-only. Revoke on `completed` moves to
`revoked` with `ownerBound: false`. On `pending` or `profile_created` it is
`409 operation_in_progress` (retry later). On `failed` it is
`409 not_revocable`. On a profile the operator reassigned it is
`409 ownership_changed`.

### Operation states and the suggested D1 mapping

| Harness `state` | Meaning | D1 job | D1 profile |
|---|---|---|---|
| `pending`, `profile_created` | committed, not finished (no usable access) | `accepted` | `provisioning` |
| `completed` | user owns the profile | `completed` | `active` |
| `failed` + `failure` code | refused on the merits; nothing usable remains | `failed` (error code uppercased) | `failed` |
| `revoked` | binding removed (terminal) | revoke job `completed` | `revoked` |
| no answer / timeout | **unknown** | keep `sending` or add an unknown state; never `failed` | `provisioning` |

### Error codes

The harness uses lowercase snake case and Web D1 uses `[A-Z0-9_]`, so the
spec should define the uppercase mapping. The codes come from PR #26 §5:
`invalid_*`, `unauthenticated`, `not_a_provisioner`,
`credential_not_provisioning_only`, `provisioner_inactive`,
`provisioner_revoked`, `issuer_not_allowed`, `template_not_allowed`,
`quota_exceeded`, `profile_outside_credential`, `ledger_full`,
`unknown_operation`, `idempotency_conflict`, `profile_unavailable`,
`principal_inactive`, `external_identity_conflict`, `ownership_changed`,
`operation_in_progress`, `not_revocable`, `issuer_not_trusted`,
`payload_too_large`, `unsupported_media_type`, `storage_unavailable`
(retry with the same key), `storage_corrupt`, `identity_unavailable`.

The gateway will add codes such as `bad_signature`, `unknown_key`,
`wrong_audience`, `expired`, `replayed_nonce`, `duplicate_key` and
`unsupported_operation`. The spec should fix them. No error carries paths,
subjects, issuers, tokens or stack traces.

## 5. Envelope: what the spec must fix

- `protocol` (`LA-WEB-PROVISIONING/1`), a direction/purpose label, `commandId`,
  `issuer` (the Worker as signer, distinct from the user's identity issuer
  if they differ), `audience` (= `harnessId`), `jobId`, `requestId`,
  `operation`, `idempotencyKey`, `nonce`, `issuedAt`, `expiresAt`, `keyId`,
  `payload` (per operation, closed schema) and `signature`. State which
  fields are mandatory for each operation, for example no `idempotencyKey`
  on `status`.
- **Redelivery:** a resend after a timeout keeps `idempotencyKey` and
  `payload` and gets a fresh `nonce`, `issuedAt`, `expiresAt` and
  signature. State whether `commandId` is reused. The harness's side-effect
  fence is the idempotency ledger; the nonce ledger only blocks byte replay.
- **Canonical bytes:** say exactly what is signed (JCS or a defined
  alternative), with numbers, Unicode and timestamp formats pinned. Prefer
  integers and strings only.
- **Responses:** decide whether synchronous responses are signed with the
  harness key (D1 reserves `harnesses.public_key` for that). If they are,
  bind them to `commandId` and use a separate purpose label.
- **Version negotiation:** an unknown `protocol` is refused with a fixed
  code.

## 6. Rules Web must follow (instead of a revocation version)

- **W1.** A create stays unresolved until the harness returns an operation
  or a definitive pre-admission refusal.
- **W2.** Resolve it by resending the **same key and payload**, freshly
  signed, until an `operationId` returns. Then revoke it if needed. Open a
  new job only after a harness `failed` state.
- **W3.** On user or profile suspension or revocation, revoke every
  `completed` operation and apply W2 to unresolved ones.
- **W4.** Never reuse an `agt_` id after revocation.

Optional, raise only if wanted: read-only `profile.status` by idempotency
key, so W2 does not need to create a profile just to revoke it.

## 7. Fixtures Lightagent needs (language-neutral files)

1. Test-only Ed25519 key pairs (Worker command key, a rotated key, a
   revoked key, and a harness response key if responses are signed), with
   key ids.
2. For each operation: the input object, the exact canonical bytes (hex
   or base64), the signature, and the full wire body.
3. Negative vectors, each with its expected error code: duplicate key at
   top level and nested; unknown field; wrong field type; bad signature;
   signature by an unknown, revoked or rotated-away key; wrong audience;
   wrong purpose; expired; `issuedAt` in the future beyond skew; non-NFC
   and escaped Unicode; numbers as floats or exponents; a 65-character and
   an 81-character display name; an oversize body (> 8 KiB); `disable`;
   `enable`; permission, URL or path fields in `payload`.
4. A manifest listing every file with its SHA-256, plus the spec's SHA-256
   and version. Both repositories pin this manifest.

## 8. Recommended tests

| Side | Test |
|---|---|
| both | every vector in §7 passes or fails identically in Rust and in `workerd` |
| Web | display name > 64 is refused before signing; disable/enable are never sent; the unknown outcome never becomes `failed`; W2 resends keep key and payload; W3 revokes after suspension |
| harness | a valid signature with no mapped, or a revoked, local credential is refused; the same key after revoke returns `revoked` without binding again (S1); a new key for a revoked id is `profile_unavailable` (S2); revoke racing create is `operation_in_progress`; another provisioner's operation is `unknown_operation`; a signed key reaches no runtime route |
| staging (later, separately approved) | create, duplicate, concurrent conflict, timeout after commit, harness restart, reordering, key rotation, wrong harness, ownership refusal, suspended user, stale revocation |
