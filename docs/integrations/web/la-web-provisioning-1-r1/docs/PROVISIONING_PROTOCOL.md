# LA-WEB-PROVISIONING/1: provisioning protocol

| | |
|---|---|
| Protocol identifier | `LA-WEB-PROVISIONING/1` |
| Revision | `la-web-provisioning-1/r1` |
| Status | **Candidate.** It is not frozen until Web and Lightagent both record acceptance of the same compatibility digest in `protocol/la-web-provisioning-1/ACCEPTANCE.md`. |
| Owner | Web (`lightagent-control`). Lightagent mirrors a byte-identical copy. |
| Fixtures | `protocol/la-web-provisioning-1/` (listed in `MANIFEST.json`) |
| Reference codec | `tests/contracts/provisioning/reference.ts` (test-only; not imported by the Worker) |

This document is normative. The key words MUST, MUST NOT, SHOULD and MAY are
used as in RFC 2119. Section numbers (§n) are stable within a revision.
Wherever this text and a fixture disagree, the disagreement is a defect in
the revision, and neither side may ship until it is fixed. Neither side may
silently prefer one of the two.

The status line above is deliberately not changed when the protocol is
accepted, because the file is hashed. Acceptance is recorded in
`ACCEPTANCE.md`, which is outside the manifest.

## 1. Scope and frozen decisions

`LA-WEB-PROVISIONING/1` carries Ed25519-signed control commands from the Web
Worker to one registered Lightagent harness. It does three things:

| Operation | Effect on the harness |
|---|---|
| `profile.create` | Create the profile `agt_…` and bind it to the local principal derived from `(issuer, subject)`, through Lightagent's existing `ProvisioningService::provision` |
| `profile.status` | Read one operation's state. Read-only |
| `profile.revoke` | Irreversibly remove the owner binding of a completed operation (`ProvisioningService::revoke`). The profile data stays with the harness operator |

Frozen for v1:

1. Web signs. Lightagent verifies the signature **and** independently applies
   its own registered `{provision}`-only authority (§10.2). A valid signature
   grants nothing by itself.
2. Polling only. The harness never sends a request to Web. There are no
   callbacks, no callback URL, and no outbox.
3. `profile.disable` and `profile.enable` are not v1 operations. A harness
   refuses them (`UNSUPPORTED_OPERATION`). Web never sends them.
4. Revocation is addressed by the **operation id** that `profile.create`
   returned, as in Lightagent today. It is terminal.
5. Display names are 1–64 Unicode scalar values (§8.1).
6. Web `agt_` ids are Lightagent profile ids unchanged. Lightagent derives its
   principal from `(issuer, subject)`. The body never carries a principal,
   owner, role or permission.
7. No revocation-version counter in v1. Ordering safety comes from Lightagent's
   operation ledger and Web rules R1–R6 (§13). Analysis: §13.8.
8. A profile id is never assigned to a second Web profile, and is never
   provisioned again after it was revoked. A network timeout is never
   evidence of failure.
9. The browser never talks to a harness. Only the Worker sends commands.
10. Runtime invocation, sessions, tools and permissions are out of scope.

## 2. Roles and trust

| Party | Owns | Trusted for |
|---|---|---|
| Web Worker | Users, sessions, account status, `agt_` placement, desired state, D1 jobs, the signing key | Asserting `(issuer, subject)` for users it authenticated, and its intent |
| Lightagent harness | Local principals, the operation ledger, profiles, ownership bindings, runtime isolation | Executing or refusing commands under its own authority, and reporting operation state |
| Harness operator | The trusted-key list, the key → local credential mapping, the provisioner registration (issuers, template, quota) | Deciding whether Web may provision at all |

Neither side reads or writes the other's database or filesystem. The
`(issuer, subject)` pair is Web's assertion, and Lightagent trusts it only
because an operator registered that issuer for the provisioner that the
signing key maps to (§10.2).

## 3. Transport

| Item | Rule |
|---|---|
| URL | `{endpoint}/la-web-provisioning/1/commands`, where `{endpoint}` is `harnesses.endpoint` without a trailing `/`. One URL for all three operations. The operation is inside the signed body, never in the path or query |
| Method | `POST` only. Anything else: `405 METHOD_NOT_ALLOWED` |
| Request headers | `Content-Type: application/json` (exact string, no parameters; otherwise `415 UNSUPPORTED_MEDIA_TYPE`), `Accept: application/json`. No `Authorization` header, no cookies, no custom authentication header. The signature is the request credential |
| Body | At most **8192 bytes** (`MAX_MESSAGE_BYTES`). A harness MUST stop reading after 8192 bytes and answer `413 PAYLOAD_TOO_LARGE`, whatever `Content-Length` says |
| TLS | `https://` with certificate validation by the Workers runtime. `http://` only for `environment = 'development'`, as the `harnesses` CHECK already enforces |
| Redirects | Never followed (`fetchWithTimeout` uses `redirect: "manual"`). A 3xx is an *uncertain* outcome (§12.4) |
| Timeout | Web SHOULD use 10 s per transmission. A timeout is an *uncertain* outcome |
| Response | `application/json`, at most 8192 bytes, schemas in §12. Web reads at most 8192 bytes |
| Who calls | Only the Worker, from server code. Endpoint and harness come from the administrator-controlled `harnesses` row, never from a request |

## 4. Message encoding

### 4.1 Text

A message is UTF-8 (RFC 3629) without a byte order mark. Invalid UTF-8,
overlong forms, encoded surrogates, and a leading `EF BB BF` are
`MALFORMED_JSON`.

### 4.2 Strict JSON

The body MUST be one JSON text (RFC 8259) with the I-JSON restrictions of
RFC 7493. A verifier MUST use a parser that:

- rejects any syntax error, trailing data, a raw control character inside a
  string, and a `\u` escape that is a lone surrogate (`MALFORMED_JSON`);
- rejects nesting deeper than **16** levels (`MALFORMED_JSON`);
- detects **duplicate member names at every level**, compared after
  unescaping (`"nonce"` and `"nonce"` are the same name). It reports
  `DUPLICATE_KEY` only after the whole text is known to be well formed, so
  that a malformed text is always `MALFORMED_JSON`;
- keeps each number's exact lexeme, because the value profile (§4.3) is a
  lexical rule.

`JSON.parse`, `serde_json::from_slice` with default settings and similar
general-purpose parsers keep the last duplicate silently and lose number
lexemes. They MUST NOT be the first stage. The reference uses a small
dedicated parser (`parseStrictJson`). A Rust implementation SHOULD do the
same, or use a streaming tokenizer that exposes raw number text and every
member name.

### 4.3 Command value profile

After parsing, every value in a command MUST be one of:

| JSON type | Allowed form |
|---|---|
| object | members as in §7–§8 |
| string | §4.5, §4.6 |
| number | non-negative integer, lexeme `0` or `[1-9][0-9]*`, value ≤ 2^53 − 1 |

`null`, `true`, `false`, arrays, fractions (`1.0`), exponents (`1e3`), signs
(`-0`, `-1`) and values above 2^53 − 1 are `INVALID_JSON_VALUE`. Leading
zeros and `+` are not JSON, so they are `MALFORMED_JSON`. There is no null
handling in commands, because a field is either present with a valid value or
the command is refused. Responses (§12) use `null` and booleans in named
places only.

### 4.4 Field encodings (C9)

Each field has exactly one encoding. A value in another encoding of the same
bytes is `INVALID_FIELD`.

| Field | Encoding |
|---|---|
| `signature` | base64url (RFC 4648 §5), **no padding**, 64 bytes → exactly 86 characters, last character in `[AQgw]` (zero padding bits) |
| `nonce` | base64url, no padding, 32 random bytes → exactly 43 characters, last character in `[AEIMQUYcgkosw048]` |
| Ed25519 public key (harness configuration, `keys.json`) | base64url, no padding, 32 raw bytes (RFC 8032 encoding) → 43 characters |
| `keyId` | `^[a-z0-9][a-z0-9._-]{2,63}$` (a subset of D1 `harnesses.key_id`) |
| timestamps | integer Unix seconds, 10 digits (1000000000–9999999999). Milliseconds are refused |
| fixture bytes | lowercase hex (fixture files only, never on the wire) |

D1 `harnesses.public_key` (standard base64 with padding, 44 characters) is
reserved for a future harness response key (§12.1). It is not used in v1, and
no v1 field shares its encoding.

### 4.5 Identifier formats

| Field | Pattern | Source |
|---|---|---|
| `commandId` | `^cmd_[a-z0-9]{8,60}$` | Web `randomId("cmd")`, new per transmission |
| `jobId` | `^prv_[a-z0-9]{8,60}$` | `provisioning_jobs.id` |
| `requestId` | `^req_[a-z0-9]{8,60}$` | `provisioning_jobs.request_id` |
| `audience` | `^hns_[a-z0-9]{8,60}$` | `harnesses.id`, the target harness |
| `profileId` | `^agt_[a-z0-9]{8,60}$` | `agent_profiles.id` (also satisfies Lightagent `^[a-z0-9][a-z0-9_-]{0,63}$`, and is never `default`) |
| `subject` | `^usr_[a-z0-9]{8,60}$` | `user_profiles.id`: immutable, never the Better Auth id, never an email |
| `issuer` | `https://` + lowercase DNS host + optional `:port` (1–65535), no path, no trailing `/`, no userinfo, query or fragment, ≤ 256 bytes | Web's configured issuer identifier. Compared byte for byte with the operator registration. A subset of Lightagent's issuer rule |
| `idempotencyKey` | `^[A-Za-z0-9_-]{16,128}$` | `provisioning_jobs.idempotency_key` (a subset of Lightagent's 8–128 `[A-Za-z0-9._:-]`) |
| `operationId` | `^[0-9a-f]{32}$` | Returned by the harness |
| `principalId` (responses) | `^[a-z0-9][a-z0-9_-]{0,63}$` | Lightagent `PrincipalId`. Opaque to Web: stored, never interpreted |

### 4.6 Unicode policy

- Every string except `displayName` is ASCII by its pattern.
- `displayName` MUST already be in Unicode Normalization Form C. A verifier
  checks `NFC(s) == s` and MUST NOT normalize on the receiver side, because
  normalizing after verification would change bytes that were signed.
- Lengths count **Unicode scalar values** (JavaScript `[...s].length`, Rust
  `s.chars().count()`), never UTF-16 code units (`s.length`) or bytes.
- Unpaired surrogates cannot occur. They are refused at parse time (§4.2).
- No other normalization, case folding or confusable mapping is applied to
  any field.

## 5. Canonical form

The canonical form of a JSON value is its **JSON Canonicalization Scheme**
serialization (RFC 8785, JCS). Inside the §4.3 profile, JCS reduces to:

- objects: members sorted by name in UTF-16 code-unit order. Every v1 member
  name is ASCII, so this is byte order. No whitespace anywhere;
- strings: `"` and `\` escaped as `\"` and `\\`; U+0008, U+0009, U+000A,
  U+000C and U+000D as `\b \t \n \f \r`; other characters below U+0020 as
  `\u00xx` with lowercase hex; **every other character as raw UTF-8**,
  including `/`, U+007F, U+2028 and non-ASCII. Since the valid v1 strings
  contain no control characters, a valid canonical body never contains a
  `\u` escape;
- numbers: the decimal digits of the integer.

A JCS library may be used if it produces identical bytes for every fixture.
Implementing the profile directly (about 40 lines) is also acceptable. Plain
`JSON.stringify` (insertion order) and `serde_json::to_vec` are not
canonicalizers. The fixture `signature-over-json-stringify` shows the
difference.

**The wire body MUST be the canonical form of the complete command, including
`signature`.** A verifier re-serializes the parsed command and compares the
bytes exactly. A difference is `NON_CANONICAL`, whether it is whitespace,
member order, an escape such as `é`, `\/` or `C`, a trailing
newline or a leading space. One accepted byte form per command removes
parser-differential ambiguity: what was signed is what was sent, minus the
signature member.

## 6. Signature

### 6.1 Signed bytes

```text
signingInput = SIGNING_CONTEXT ‖ UTF-8( JCS( command without the "signature" member ) )

SIGNING_CONTEXT = "LA-WEB-PROVISIONING/1" 0x00 "provisioning-command" 0x00
                = 4c412d5745422d50524f564953494f4e494e472f31 00
                  70726f766973696f6e696e672d636f6d6d616e64 00
```

The context binds the protocol version and the message purpose outside the
JSON, so a signature made for another purpose (for example the reserved
`provisioning-response`, or a future `agent-invocation`) can never verify as a
command, even under the same key. The same values also appear inside the JSON
(`protocol`, `purpose`) and are checked there (§10), so a confused purpose is
reported as `WRONG_PURPOSE` rather than as a bare signature failure.

`0x00` cannot occur in JCS output, so the concatenation is unambiguous.

### 6.2 Algorithm

- **Ed25519** (RFC 8032 §5.1, "pure" Ed25519: no prehash, no context
  parameter). Ed25519ph and Ed25519ctx are not used, because Web Crypto in
  workerd offers only pure Ed25519.
- Verification MUST reject a signature whose scalar `S` is not below the
  group order `L` (RFC 8032 §5.1.7). The reference checks this explicitly
  and does not rely on a library default. Fixture: `signature-noncanonical-s`.
- The signature is over `signingInput` exactly, never over a hash of it.
- Ed25519 is deterministic: the same key and bytes always give the same
  signature. The fixtures depend on this.

### 6.3 Worked example (`valid-create-basic`)

`JCS(command without signature)`, 647 bytes (the signing input adds the 43-byte context: 690 bytes):

```json
{"audience":"hns_fixtureharnessalpha0000001","commandId":"cmd_430fe33cce765249e22be19eb9","expiresAt":1791000060,"issuedAt":1791000000,"jobId":"prv_fixturejobcreate000000001","keyId":"web-test-active","nonce":"uJwbU6ZT_YxFgSeatXYQM0aiQ3ZZwhRq6c1hwVY5sog","operation":"profile.create","payload":{"displayName":"Research assistant","idempotencyKey":"idk_fixturecreate00000000001","issuer":"https://lightagent-control.example","profileId":"agt_fixtureprofile000000000001","subject":"usr_fixtureuser00000000000001","template":"assistant"},"protocol":"LA-WEB-PROVISIONING/1","purpose":"provisioning-command","requestId":"req_fixturerequest00000000001"}
```

Signed with the test-only key `web-test-active` (public key
`e_ezzMPOYjkP-vj8ROC_HIkvz6wHTM2zMLNFjNdTGwQ`), the signature is

```text
mTaD9VRZ89cf2gwes4YUoJWXfH15uOrbmYl6GMpQ85CbKXj7HWB7RtTNyR4gPe1nl3uTQEujsua9QMd1HEv0AA
```

and the wire body (748 bytes) is the JCS form of the command with
`"signature"` inserted in sorted position, between `"requestId"` and the
closing brace. The exact bytes are `bodyHex` in `commands-valid.json`.

### 6.4 Keys, rotation and revocation

- Web holds the Ed25519 private key as a Cloudflare secret. It is never in
  `wrangler.jsonc`, never in a `VITE_*` or `LIGHTAGENT_PUBLIC_*` variable,
  never in D1 and never in the client bundle. L0 creates no key.
- Each harness operator configures a **trusted-key list**:
  `{ keyId, publicKey, status: "active" | "revoked" }`. `keyId` is chosen by
  Web and is unique per Web environment. Staging and production never share
  a key.
- A `keyId` that is not in the list is `UNKNOWN_KEY`. One with status
  `revoked` is `KEY_REVOKED`, even when the signature is valid.
- **Rotation:** (1) the operator adds the new key as `active` (overlap);
  (2) Web starts signing with it; (3) once no command signed by the old key
  can still be accepted (wait ≥ 120 + 30 s), the operator removes the old key,
  which then gives `UNKNOWN_KEY`, or marks it `revoked`. Redeliveries may
  change `keyId` across a rotation (§9.1). Fixtures:
  `valid-create-rotated-key`, `valid-status-after-rotation`,
  `key-rotated-away`.
- **Compromise:** mark the key `revoked` at once on every harness. Lightagent
  can additionally revoke the mapped local credential (§10.2), which stops
  every key mapped to it.
- **Every trusted `keyId` for one Web environment MUST map to the same local
  provisioner principal** (§10.2). Idempotency is scoped to the provisioner,
  so a redelivery under a rotated key must reach the same scope.

## 7. Command envelope

Schema identifiers: `LA-WEB-PROVISIONING/1#command/profile.create`,
`…#command/profile.status`, `…#command/profile.revoke`.

A command is a JSON object with **exactly** these 13 members. All are
required. Any other member is `UNKNOWN_FIELD`, and a missing one is
`MISSING_FIELD`.

| Member | Type | Rule | Identity role (§9.1) |
|---|---|---|---|
| `protocol` | string | exactly `LA-WEB-PROVISIONING/1` | logical |
| `purpose` | string | exactly `provisioning-command` | logical |
| `operation` | string | `profile.create`, `profile.status` or `profile.revoke` | logical |
| `commandId` | string | §4.5. Unique per transmission. Echoed in the response | per transmission |
| `jobId` | string | §4.5. The D1 job this transmission serves (a status poll uses the job it reconciles) | logical |
| `requestId` | string | §4.5. The originating API request of that job, for log correlation | logical |
| `audience` | string | §4.5. The target harness id. MUST equal the verifier's own configured harness id | logical |
| `keyId` | string | §4.4 | may change on rotation |
| `issuedAt` | integer | §4.4. Web's clock at signing | per transmission |
| `expiresAt` | integer | §4.4. `1 ≤ expiresAt − issuedAt ≤ 120` (otherwise `INVALID_LIFETIME`). Web SHOULD use 60 | per transmission |
| `nonce` | string | §4.4. 32 bytes from a CSPRNG, new per transmission | per transmission |
| `payload` | object | §8, closed per operation | logical |
| `signature` | string | §4.4, §6 | per transmission |

The external identity is carried in the `profile.create` payload as `issuer`
and `subject`. The envelope has no separate signer-issuer field: the signer
is identified by `keyId`, and the harness operator binds `keyId` to a local
credential (§10.2).

## 8. Payloads

### 8.1 `profile.create`

Exactly these members:

| Member | Rule | Error |
|---|---|---|
| `profileId` | §4.5 | `INVALID_PROFILE_ID` |
| `issuer` | §4.5 | `INVALID_ISSUER` |
| `subject` | §4.5 | `INVALID_SUBJECT` |
| `displayName` | NFC; 1–64 Unicode scalar values; no character of general category Cc (U+0000–U+001F, U+007F–U+009F); first and last character not Unicode `White_Space` (U+0009–U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000–U+200A, U+2028, U+2029, U+202F, U+205F, U+3000) | `INVALID_DISPLAY_NAME` |
| `template` | exactly `assistant` | `INVALID_TEMPLATE` |
| `idempotencyKey` | §4.5 | `INVALID_IDEMPOTENCY_KEY` |

The display-name rule is Lightagent's (`chars().count()`, `char::is_control`,
`str::trim`) plus NFC. JavaScript's `String.prototype.trim` uses a different
set: it strips U+FEFF and does not strip U+0085. It MUST NOT be used.
Fixtures pin the difference (`valid-create-leading-bom-char`,
`display-name-control-c1`). Web MAY apply a stricter name policy in its UI.
It MUST NOT sign a name that fails this rule.

### 8.2 `profile.status` and 8.3 `profile.revoke`

Exactly `{ "operationId", "profileId" }`. `operationId` is the value a
`profile.create` returned. `profileId` MUST be that operation's profile. A
harness answers a mismatch exactly like an unknown operation
(`UNKNOWN_OPERATION`), so the field is a consistency check and not an
oracle. Neither operation takes an idempotency key: status is read-only, and
revoke is idempotent by operation.

### 8.4 Forbidden content

No v1 payload may carry permissions, tool grants, toolsets, approval policy,
model, provider, base URL, endpoint, callback URL, filesystem path, `cwd`,
workspace, principal id, owner, role, capability, credential, token,
revocation version, or any configuration. Because the schemas are closed,
each of these is `UNKNOWN_FIELD`. Web's `permissions_json` is a Web-side
ceiling and grants nothing on the harness (C8). The `template` name is the
only lever, and the harness defines what it means.

`profile.disable`, `profile.enable`, `agent.*`, `profile.lookup` and any other
operation name are `UNSUPPORTED_OPERATION`.

## 9. Identity, retries, replay and time

### 9.1 Logical command and transmission

A **logical command** is one D1 job's intent. A **transmission** is one signed
HTTP request for it. Every transmission of one logical command:

- repeats `protocol`, `purpose`, `operation`, `jobId`, `requestId`, `audience`
  and the **byte-identical canonical `payload`**, and therefore the same
  `idempotencyKey`;
- renews `commandId`, `nonce`, `issuedAt`, `expiresAt` and `signature`;
- may change `keyId` only across a key rotation (§6.4).

`commandId` is therefore a per-transmission correlation id (logs, response
binding). The retry identity is `(jobId, operation, payloadHash)`, and the
harness's side-effect fence is its idempotency ledger `(provisioner,
idempotencyKey)`.

```text
payloadHash = lowercase hex SHA-256( "LA-WEB-PROVISIONING/1" 0x00 "payload" 0x00 operation 0x00 ‖ JCS(payload) )
```

`payloadHash` is the value Web stores as `provisioning_jobs.request_hash`. A
transmission whose `payloadHash` differs from its job's is a Web defect and
MUST NOT be sent. If one is sent anyway, Lightagent answers
`IDEMPOTENCY_CONFLICT` and nothing changes. Fixtures: `redelivery.json`.

### 9.2 Nonces

- The harness records each accepted nonce **before** local admission and
  before any side effect, atomically (insert-if-absent), and keeps it at least
  until `expiresAt + 30` s. A nonce already present is `REPLAY_DETECTED`.
- Nonces are scoped to the harness: one ledger across all keys.
- If the nonce store cannot be read or written, the harness refuses with
  `NONCE_STORE_UNAVAILABLE`. It never skips the check.
- Commands refused before the nonce step (§10 steps 1–12) are not recorded,
  so unauthenticated traffic cannot fill the ledger.
- The nonce ledger blocks byte replay only. Exactly-once effects come from the
  idempotency ledger (create) and terminal operation states (revoke).
  Persisting nonces across a harness restart is SHOULD, not MUST: a replay
  after a restart can only repeat an idempotent outcome.

### 9.3 Time

| Constant | Value |
|---|---|
| Maximum lifetime `expiresAt − issuedAt` | 120 s (Web uses 60 s) |
| Maximum clock skew `MAX_CLOCK_SKEW_SECONDS` | 30 s |
| Harness accepts when | `issuedAt − 30 ≤ now < expiresAt + 30` (harness clock) |
| `NOT_YET_VALID` | `issuedAt > now + 30` |
| `EXPIRED` | `now ≥ expiresAt + 30` |
| **Acceptance horizon** of a transmission (Web clock) | `expiresAt + 60` (2 × skew). After it, no copy of that transmission can be accepted by a harness whose clock is within 30 s of Web's |

Command expiry bounds how long a delayed copy can arrive. It **never** tells
Web what happened to that copy (§13.2).

## 10. Verification and execution order

A harness MUST apply these steps in order and answer with the first failing
step's code. Steps 1–12 are the protocol layer, which every fixture in
`commands-*.json` exercises. Steps 13–14 are Lightagent's existing service,
reached only after the protocol layer passes. Nothing may have a side effect
before step 12.

| # | Step | Code on failure |
|---|---|---|
| 1 | `POST`, exact `Content-Type`, body ≤ 8192 bytes | `METHOD_NOT_ALLOWED`, `UNSUPPORTED_MEDIA_TYPE`, `PAYLOAD_TOO_LARGE` |
| 2 | UTF-8 and strict JSON (§4.1, §4.2) | `MALFORMED_JSON` |
| 3 | No duplicate member names at any depth | `DUPLICATE_KEY` |
| 4 | Value profile (§4.3), over the whole document | `INVALID_JSON_VALUE` |
| 5 | Top level is an object whose `protocol` member is a string; then the string equals `LA-WEB-PROVISIONING/1` | `INVALID_ENVELOPE`; `UNSUPPORTED_PROTOCOL` |
| 6 | Closed envelope: no unknown member, then no missing member | `UNKNOWN_FIELD`, `MISSING_FIELD` |
| 7 | `purpose` (string, then value); `operation` (string, then v1 set); then the other envelope members in JCS name order (`audience`, `commandId`, `expiresAt`, `issuedAt`, `jobId`, `keyId`, `nonce`, `payload`, `requestId`, `signature`) | `INVALID_FIELD`, `WRONG_PURPOSE`, `UNSUPPORTED_OPERATION`, `INVALID_FIELD` |
| 8 | Closed payload for the operation (unknown, then missing), then payload members in JCS name order; then lifetime | `UNKNOWN_FIELD`, `MISSING_FIELD`, the field's code (§8), `INVALID_LIFETIME` |
| 9 | Body bytes equal `JCS(parsed command)` (§5) | `NON_CANONICAL` |
| 10 | `keyId` is trusted and not revoked (§6.4) | `UNKNOWN_KEY`, `KEY_REVOKED` |
| 11 | `S < L`, and Ed25519 verifies `signature` over `signingInput` (§6) | `BAD_SIGNATURE` |
| 12 | `audience` equals this harness id; time window (§9.3); nonce recorded once (§9.2) | `WRONG_AUDIENCE`; `NOT_YET_VALID`, `EXPIRED`; `REPLAY_DETECTED`, `NONCE_STORE_UNAVAILABLE` |
| 13 | **Local admission** (§10.2) | §11 admission codes |
| 14 | Execute through `ProvisioningService` (§10.3) | §11 payload, merits, operation, conflict and indeterminate codes, or an operation response |

The reference implements steps 1 (size only) to 12 in `verifyCommand`. A
negative fixture is single-fault: apart from its one defect, it is canonical
and correctly signed, so skipping any check makes that vector fail.

### 10.2 Local admission (Lightagent PR #26, unchanged)

The harness operator maps each trusted `keyId` to **one local provisioning
credential key id**. This is configuration, never command content. For every
command the gateway builds a `Caller` from that credential through
`CredentialStore::caller_for_at`, and `ProvisioningService::admit` then
re-checks, under the ledger lock, that: the credential is unrevoked,
unexpired and not rotated away; its role is `operator`; its restriction is
**exactly** `{provision}`; the principal is active; and an unrevoked
provisioner registration exists. If the operator revokes that credential, the
next command is refused even with a valid signature. A `keyId` with no
mapping is `UNAUTHENTICATED`.

The signing key reaches no run, session, tool, settings, Jev or
administration route. PR #26 tests this route by route
(`provisioning_boundary.rs`). The gateway is a separate router.

### 10.3 Execution

| Operation | Service call | Extra gateway rule |
|---|---|---|
| `profile.create` | `provision(caller, idempotencyKey, {issuer, subject, profile_id: profileId, display_name: displayName, template})` | none |
| `profile.status` | `status(caller, operationId)` | `operation.profile == payload.profileId`, otherwise `UNKNOWN_OPERATION` |
| `profile.revoke` | `revoke(caller, operationId)` | same |

Lightagent's order inside `provision` is admit → idempotency-key format →
payload validation → credential profile limit → **idempotency lookup** →
`begin` (issuer allowed, template allowed, quota, ledger capacity, profile id
unknown, principal resolution). A `begin` refusal records nothing. §13.2
depends on this order. Any change to it on the Lightagent side is a protocol
change.

## 11. Errors

Codes are uppercase `[A-Z0-9_]{1,64}`, so they fit
`provisioning_jobs.error_code` unchanged. The registry, with HTTP status,
class and applicable operations, is `errors.json`. It is normative.

| Class | Meaning for Web | Codes (HTTP) |
|---|---|---|
| `protocol` | Refused before any side effect. Says nothing about *other* transmissions | `METHOD_NOT_ALLOWED` 405, `UNSUPPORTED_MEDIA_TYPE` 415, `PAYLOAD_TOO_LARGE` 413, `MALFORMED_JSON` `DUPLICATE_KEY` `INVALID_JSON_VALUE` `INVALID_ENVELOPE` `UNSUPPORTED_PROTOCOL` `UNKNOWN_FIELD` `MISSING_FIELD` `WRONG_PURPOSE` `UNSUPPORTED_OPERATION` `INVALID_FIELD` `INVALID_LIFETIME` `NON_CANONICAL` 400, `UNKNOWN_KEY` `KEY_REVOKED` `BAD_SIGNATURE` `NOT_YET_VALID` `EXPIRED` 401, `WRONG_AUDIENCE` 403, `NONCE_STORE_UNAVAILABLE` 503 |
| `payload` | Deterministic in the payload: the same payload can never succeed | `INVALID_PROFILE_ID` `RESERVED_PROFILE_ID` `INVALID_ISSUER` `INVALID_SUBJECT` `INVALID_DISPLAY_NAME` `INVALID_TEMPLATE` `INVALID_IDEMPOTENCY_KEY` `INVALID_OPERATION_ID` 400 |
| `admission` | Local authority refused **before** the idempotency lookup. State-dependent | `UNAUTHENTICATED` 401, `NOT_A_PROVISIONER` `CREDENTIAL_NOT_PROVISIONING_ONLY` `PROVISIONER_INACTIVE` `PROVISIONER_REVOKED` `PROFILE_OUTSIDE_CREDENTIAL` 403 |
| `merits` | create only. Refused **after** the idempotency lookup, so at that moment no operation existed for this key | `ISSUER_NOT_ALLOWED` `TEMPLATE_NOT_ALLOWED` `QUOTA_EXCEEDED` `LEDGER_FULL` 403, `PROFILE_UNAVAILABLE` `PRINCIPAL_INACTIVE` `EXTERNAL_IDENTITY_CONFLICT` 409 |
| `operation` | status/revoke on a known operation id | `UNKNOWN_OPERATION` 404, `OPERATION_IN_PROGRESS` `NOT_REVOCABLE` `OWNERSHIP_CHANGED` 409 |
| `conflict` | Web defect: the same key with a different payload. Never a failure | `IDEMPOTENCY_CONFLICT` 409 |
| `indeterminate` | The command **may have taken effect**. Treated exactly like a timeout | `STORAGE_UNAVAILABLE` `STORAGE_CORRUPT` `IDENTITY_UNAVAILABLE` 503, `INTERNAL_ERROR` 500, `REPLAY_DETECTED` 409 |

`REPLAY_DETECTED` is indeterminate because a network-duplicated copy of the
same transmission may have been the one that executed. `STORAGE_UNAVAILABLE`
can leave a `pending` operation in the ledger (PR #26 §6).

Operation failure codes, which appear only in `result.failure` of a `failed`
operation: `ISSUER_NOT_TRUSTED`, `PRINCIPAL_INACTIVE`,
`EXTERNAL_IDENTITY_CONFLICT`, `PROFILE_UNAVAILABLE`, `OWNERSHIP_CHANGED`,
`PROVISIONER_REVOKED`.

Lightagent's mapping is mechanical. Each lowercase PR #26 code becomes the
same name in uppercase (`invalid_request` → `MALFORMED_JSON`, `UNKNOWN_FIELD`,
`MISSING_FIELD` or `INVALID_FIELD`, depending on the gateway step that
catches it; `missing_idempotency_key` → `INVALID_IDEMPOTENCY_KEY`). The codes
the gateway adds are listed as `"gateway"` in `errors.json`. No error body
carries a path, subject, issuer, display name, token, key material or stack
trace.

## 12. Responses

### 12.1 Response authentication decision (v1)

**v1 responses are not application-signed.** Their integrity rests on:

1. HTTPS to the administrator-registered endpoint of the specific harness,
   with certificate validation by the Workers runtime. No redirects.
2. **Binding:** every response echoes `protocol`, `commandId`, `audience` and
   `operation`. Results echo `profileId`, and for status and revoke the
   `operationId`. Web rejects any mismatch (§12.4). An answer meant for
   another command, another harness or another profile is never applied.
3. Strict parsing (duplicates, closed schemas, 8 KiB) and internal
   consistency (`ownerBound` ⇔ `completed`; `failure` ⇔ `failed`).
4. Monotonic observation: Web never moves an operation's observed state
   backwards (§13.1), so a stale or replayed answer cannot undo a revocation
   that Web has observed.
5. Polling: the observed state is re-read with `profile.status`. No
   unsolicited message is ever trusted.

Threat model:

| Threat | Without response signatures | Residual risk |
|---|---|---|
| Network attacker without a valid certificate | Stopped by TLS | none |
| Answer from another harness, or a cached or misrouted answer | Per-harness endpoint, audience and commandId echo | none |
| Attacker who controls TLS termination in front of the harness (an ingress or tunnel operated by someone other than the harness operator) | Could forge `completed`, `failed` or `revoked`. It could also drop commands, but signed commands cannot be forged | A forged `revoked` would make Web believe a binding was removed when it was not. This is **silent** where dropping is visible |
| Compromised harness | Signing would not help: the harness holds its own key and enforces the binding | Trust in the harness is inherent |

Rationale: in v1 a provisioned user holds no Lightagent credential.
Provisioning issues none (PR #26 §13.3), and every future user path is
Worker-mediated (AGENT_ACCESS_CONTROL §4.1), so it checks Web state first. A
binding that silently survived a forged `revoked` therefore grants nothing a
user can exercise without separate operator action. Response signatures
would add a harness key, its distribution into `harnesses.public_key`, and
Rust signing code, and would protect only against an intermediary between
TLS termination and the harness.

**Gate:** response signing becomes **required** (a new protocol revision,
context `"LA-WEB-PROVISIONING/1" 0x00 "provisioning-response" 0x00`, which is
reserved now, bound to `commandId`, signed by a harness key held in
`harnesses.public_key`) before either of the following happens: (a) a user
can reach a harness by any path that does not pass the Worker guard (for
example AGENT_ACCESS_CONTROL §4.2 scoped tokens), or (b) a harness is reached
through TLS termination that the harness operator does not control.

### 12.2 Operation response (`200`, `201`)

Schema `LA-WEB-PROVISIONING/1#response/operation`. Exactly:

```json
{
  "protocol": "LA-WEB-PROVISIONING/1",
  "commandId": "<echo>",
  "audience": "<echo>",
  "operation": "<echo>",
  "replayed": false,
  "result": {
    "operationId": "<32 lowercase hex>",
    "state": "pending | profile_created | completed | failed | revoked",
    "failure": null,
    "principalId": "<Lightagent PrincipalId>",
    "profileId": "<echo of payload.profileId>",
    "template": "assistant",
    "ownerBound": true,
    "createdUnix": 1791000001,
    "updatedUnix": 1791000002
  }
}
```

- `replayed` is present **only** for `profile.create`: `201` with
  `replayed: false` when this transmission created the operation, and `200`
  with `replayed: true` when the key already had one. Status and revoke
  answer `200` without `replayed`.
- `failure` is `null` unless `state` is `failed`, in which case it is an
  operation failure code (§11). A failed operation is an **operation
  response** (`200`/`201`), not an error. This differs from PR #26's HTTP API,
  which answers `409` with the operation, and the gateway maps between them.
- `ownerBound` is `true` if and only if `state` is `completed`.
- A revoke response's `state` is always `revoked`. Repeating a revoke returns
  the same answer.
- Integers follow §4.3. `updatedUnix ≥ createdUnix`.

### 12.3 Error response (all other statuses)

Schema `LA-WEB-PROVISIONING/1#response/error`. Exactly
`{ "protocol": "LA-WEB-PROVISIONING/1", "commandId": <echo or null>, "code": "<CODE>" }`.
`commandId` is echoed once the envelope passed §10 step 8, and is `null`
before that. The HTTP status MUST be the one in `errors.json` for the code.

### 12.4 Web's classification

Web classifies each transmission's outcome as exactly one of the following
(reference: `classifyResponse`; fixtures: `responses.json`):

| Outcome | When |
|---|---|
| `operation` | `200`/`201` and every §12.2 rule holds, including all echoes |
| `refused` | An error status, a well-formed §12.3 body, `commandId` equal or `null`, a registered code whose HTTP status matches and which applies to this operation, and a class other than `indeterminate` |
| `uncertain` | Everything else: a timeout, a network error, any other status (for example `202` or `3xx`), an HTML error page, an oversized body, any schema or echo mismatch (`RESPONSE_INVALID`), or an indeterminate code |

An `uncertain` outcome never changes Web's view of what exists on the
harness.

## 13. Lifecycle and reconciliation (Web state machine)

### 13.1 Desired and observed state

Web keeps two separate facts:

- **Desired state** (Web-authoritative): whether profile `agt_…` should be
  bound for its owner. It comes from `agent_profiles.status` and
  `user_profiles.status`. The Worker guard enforces it on every Web request
  immediately, whatever the harness says.
- **Observed state** (harness-reported): per create operation, `unknown`
  (no operation id yet), or the last `result.state` seen, plus
  `operationId`, `principalId` and the observation time. It changes only from
  an `operation` outcome, and only forward:
  `unknown → pending → profile_created → completed → revoked`, with
  `unknown | pending | profile_created → failed`. A response that would move
  it backwards is an anomaly (`needs attention`). It is never applied.

A Web profile shown as `revoked` means access is withdrawn by Web. It means
"unbound on the harness" only once its revoke job is completed.

### 13.2 Resolving a create (rules R1–R3)

Inputs: every transmission of the job, with its `issuedAt`, `expiresAt` and
§12.4 outcome. Reference: `resolveCreateJob`. Fixtures:
`create-resolution.json`.

- **R1. An operation resolves the job.** If any transmission returned an
  operation, the job is resolved with that `operationId`, in its state,
  including `failed` and `revoked`. Two different operation ids for one job
  are an anomaly.
- **R2. Payload refusals are final.** If any transmission was refused with a
  `payload`-class code, the job is `failed` with that code, because the same
  payload can never succeed.
- **R3. Otherwise, refusals count only when they rule out every uncertain
  transmission.** The job is `failed` only if, for **every** `uncertain`
  transmission `u`, some transmission `t` was refused with a `merits`-class
  code and `t.issuedAt ≥ u.expiresAt + 60` (u's acceptance horizon). If there
  is no uncertain transmission, every transmission was refused before any side
  effect, so nothing exists and the job is `failed` with the latest code.
- Otherwise the job is **unresolved**. `IDEMPOTENCY_CONFLICT` and unknown
  codes are anomalies (`needs attention`), never failures.

Why R3 is stated this way: `admission` and `protocol` refusals happen before
the idempotency lookup, so they say nothing about whether an earlier copy
committed. A `merits` refusal happens after the lookup, so it proves no
operation existed *at that moment*. Only once every earlier copy has passed
its acceptance horizon is that also true for the future.

**Consequences:** exhausted retries never produce `failed`
(`timeouts-exhausted`). A timeout followed by a quick `QUOTA_EXCEEDED` stays
unresolved (`timeout-then-merits-too-early`).

### 13.3 Driving an unresolved create (rule R4)

- **R4.** While a create is unresolved, Web resends the **same logical
  command** (§9.1) with backoff, for example 10 s, 30 s, 2 min, then every
  15 min, until R1–R3 resolve it. If the user was suspended in the meantime,
  Web still resends, learns the operation id, and then revokes it (§13.5).
  Resending is safe: under the same key, Lightagent returns the existing
  operation and never creates a second one (S1, idempotency).
- After about 10 transmissions without resolution, Web marks the job
  `needs attention` for an operator, but **keeps** it unresolved and keeps
  reconciling at the slow cadence. It never marks it failed.
- After `pending` or `profile_created`, Web polls `profile.status` (5 s,
  15 s, 60 s, then every 15 min) until the state is `completed` or `failed`.
- A new create job for the same `agt_` id, with a **new** idempotency key, is
  allowed only after that id's previous create resolved as `failed` (by R1
  with state `failed`, R2 or R3). It is never allowed after `revoked`, and
  never while a job is unresolved (S6).

### 13.4 Revoke (rule R5)

Revoke jobs need the create's `operationId`. If the create is unresolved, R4
runs first.

| Outcome | Web revoke job | Observed |
|---|---|---|
| operation `revoked` | `completed` | `revoked` |
| `NOT_REVOCABLE` | `completed`. Confirm with status that the operation is `failed` (nothing was bound) | `failed` |
| `OPERATION_IN_PROGRESS` | stays open. Poll status, then revoke once `completed` | unchanged |
| `OWNERSHIP_CHANGED` | `failed` with `OWNERSHIP_CHANGED`, plus needs attention (the operator reassigned the profile; Web cannot revoke it) | unchanged |
| `UNKNOWN_OPERATION` | needs attention. Never treated as revoked | unchanged |
| `uncertain` | resend the same logical command (revoke is idempotent) | unchanged |

### 13.5 Suspension, cancellation and revocation (rule R6)

- **R6.** When a profile's desired state becomes "absent" (the user deletes or
  revokes it, the account is suspended or disabled, or a create is cancelled),
  Web MUST durably record that intent in the same D1 transaction as the status
  change. A reconciler then drives it to completion: it revokes every
  `completed` operation, applies R4 to every unresolved create and then
  revokes the result, and leaves `failed` operations alone.
- The reconciler is **level-triggered**: it periodically selects every profile
  whose desired state is absent and whose create is not yet settled: either
  observed `completed` without a completed revoke, or still unresolved
  (§13.2). A lost event therefore cannot leave a binding behind.
- Because harness revocation is irreversible, suspending a user ends their
  existing harness bindings permanently. Reactivation creates **new** `agt_`
  profiles. The old data stays with the harness operator (Lightagent §8).

### 13.6 Profile identifiers

Web never assigns an `agt_` id to a second Web profile, never re-creates a
revoked one, and never re-creates one while a create for it is unresolved.
Lightagent refuses a revoked or known id with a new key
(`PROFILE_UNAVAILABLE`, S2), and with the same key returns the recorded
(revoked) operation (S1). Both sides therefore refuse reuse independently.

### 13.7 Mapping to D1 (existing schema)

| Observation | `provisioning_jobs.status` | `agent_profiles.status` |
|---|---|---|
| no transmission yet | `pending` | `provisioning` |
| transmission in flight, or last outcome uncertain or refused while unresolved | `sending`, or back to `pending` awaiting the next transmission (each new transmission enters `sending` again) | `provisioning` |
| operation `pending` or `profile_created` | `accepted` | `provisioning` |
| operation `completed` | `completed` | `active` |
| operation `failed` (R1), or R2/R3 failure | `failed`, `error_code` = the failure or refusal code | `failed` |
| create resolves as `revoked` (only after lost Web records) | `completed` + needs attention | → `revoked` (needs the transition noted in §16.1) |
| revoke job outcomes | §13.4 | `revoked` (set when the intent was recorded, §13.5) |
| anomaly | unchanged + needs attention | unchanged |

An HTTP `200` or `201` alone never means `completed`. Only `result.state`
does (C10).

### 13.8 Stale, delayed and reordered commands

| # | Scenario | Outcome |
|---|---|---|
| S1 | A create redelivered with the same key after its operation was revoked | Returns `revoked`, does not bind again |
| S2 | A create for a revoked or known id with a new key | `PROFILE_UNAVAILABLE` |
| S3 | A revoke racing a create in progress | `OPERATION_IN_PROGRESS`. Calls are serialized by the ledger lock |
| S4 | A revoke before its create | Impossible: a revoke names an operation id, which only a create returns |
| S5 | A create's outcome is unknown and Web gives up | Not allowed: R3 and R4 |
| S6 | A new key for the same profile after an unknown outcome | Not allowed: §13.3 |
| S7 | The user is suspended while a create is in flight | R6 revokes the eventual operation |
| S8 | The operator reassigns or suspends locally | `OWNERSHIP_CHANGED` or `PRINCIPAL_INACTIVE`. Web does not override the operator |
| S9 | A command captured for another harness | `WRONG_AUDIENCE` (fixtures `cross-harness-*`) |
| S10 | A byte-identical replay within the window | `REPLAY_DETECTED`. Outside the window: `EXPIRED` |
| S11 | A delayed create arrives after its revoke | Same key, so S1 applies. A revoke is terminal, so no command can restore access |

A delayed or reordered command therefore cannot restore access after
revocation, and duplicate commands cannot create duplicate bindings
(idempotency, profile-id uniqueness and one in-flight job per profile and
operation in D1). This holds without a revocation-version counter. A
counter would only be needed for reversible operations such as
`disable`/`enable`, which v1 excludes. If they are added later, a counter
must be reconsidered.

### 13.9 Optional: read-only lookup by idempotency key (evaluated, deferred)

A `profile.lookup { idempotencyKey, profileId }` operation would return the
operation recorded for `(provisioner, idempotencyKey)`, or
`UNKNOWN_OPERATION`. It would pass the same §10 and §10.2 admission, would be
scoped to the caller's provisioner exactly like `status`, would have no side
effect and no ledger write, and would need one read-only method in
`ProvisioningService`, not a second ledger or authority. Its only benefit is
for R6 after a suspension: Web could learn that nothing was created without
resending a create that then has to be revoked. A "not found" answer would
still be final only under the R3 horizon rule.

Decision for r1: **not included.** R4 is sufficient for safety. In v1 a
binding created and then revoked within minutes grants nothing usable
(§12.1). In v1, `profile.lookup` is `UNSUPPORTED_OPERATION` (fixture
`unsupported-profile-lookup`). Either side may propose it for a later
revision with these semantics.

## 14. Lightagent audit items C1–C10

| # | Audit item | Resolution in this revision |
|---|---|---|
| C1 | Callbacks | None. Polling with `profile.status` only (§1, §13.3). No callback field exists. `callbackUrl` is `UNKNOWN_FIELD` |
| C2 | Disable / enable | Not v1. `UNSUPPORTED_OPERATION` (fixtures `forbidden-profile-disable`, `forbidden-profile-enable`). The D1 CHECK keeps the values, and Web never sends them |
| C3 | Revoke | By `operationId` with a `profileId` consistency check (§8.2). Terminal and idempotent (§12.2) |
| C4 | Display name | 1–64 Unicode scalar values, NFC, no Cc, no leading or trailing White_Space (§8.1). Web validates before signing. Fixtures cover 65, 81, 65 astral and 64 astral |
| C5 | Revocation version | Not in v1. Rules R1–R6 and §13.8. `revocationVersion` is `UNKNOWN_FIELD` |
| C6 | Subject | `payload.subject` = `user_profiles.id` (`usr_…`), immutable. The issuer is Web's configured https origin identifier. Lightagent derives its own principal |
| C7 | Authority | Signature (§6) then local admission (§10.2). Every `keyId` maps to one `{provision}`-only credential |
| C8 | Permissions | Closed payloads. Only `template: "assistant"`. Forbidden fields are listed in §8.4 and covered by fixtures |
| C9 | Encodings | One encoding per field (§4.4). Standard base64 or hex for signatures and nonces is `INVALID_FIELD` |
| C10 | State mapping | §12.4, §13.7. HTTP status alone never means completion. A failed operation is an operation response with `state: failed` |

## 15. Fixtures, manifest and acceptance

| File | Content |
|---|---|
| `keys.json` | Four TEST-ONLY Ed25519 keys (`active`, `rotated`, `revoked`, `untrusted`) derived as `seed = SHA-256(label)`; three named verifier states (`overlap`, `afterRotation`, `harnessB`); constants |
| `commands-valid.json` | Valid commands, each with the command object, canonical JSON, `signingInputHex`, signature, `payloadHash` and the exact body |
| `commands-invalid.json` | Single-fault negative vectors with the expected code and HTTP status |
| `redelivery.json` | Create and revoke transmission series, including one across a key rotation, a replay, and a "not a redelivery" counter-example |
| `responses.json` | Response bodies and Web's expected classification |
| `create-resolution.json` | R1–R3 decision cases |
| `errors.json` | The normative error registry |
| `MANIFEST.json` | Protocol, revision, schema ids, fixture ids, specification and file SHA-256 hashes, `fixtureDigest` |
| `MANIFEST.sha256` | The same hashes in `sha256sum -c` format (run it from the Web repository root) |

- `fixtureDigest` = SHA-256 of the lines `"<sha256>  <path>\n"` for every
  fixture file, sorted by path.
- **Compatibility digest** = SHA-256 of the exact bytes of `MANIFEST.json`.
  It covers this document, every fixture and every schema id. Both
  repositories pin this value.
- The generator is deterministic (`npm run protocol:fixtures -- write|check`).
  `tests/static/provisioning-fixtures.test.ts` fails if any committed byte
  differs from a regeneration or from the manifest.
  `tests/contracts/provisioning-protocol.test.ts` runs every vector inside
  workerd.
- `.gitattributes` marks these files `-text`, so checkouts on any platform
  keep the hashed bytes.
- A verifier passes the fixtures when every `commands-*` and `redelivery`
  vector gives exactly its `expect`. A Web implementation additionally passes
  `responses.json` and `create-resolution.json`.

## 16. Required follow-up work (not part of L0)

### 16.1 Web

1. Production codec under `src/worker/provisioning/`, ported from the
   reference and tested against these fixtures (L0 adds no production code).
2. Signing key: a per-environment Cloudflare secret (for example
   `PROVISIONING_SIGNING_KEY`, a 32-byte seed in base64url) read through
   `readSecret`, plus a non-secret `keyId` variable. Extend the bundle secret
   scan to the new names. Production stays empty until approved.
3. `PROVISIONING_ISSUER`, a non-secret https origin identifier, explicit per
   environment. Development needs an `https://` identifier even though
   `PUBLIC_ORIGIN` is `http://localhost:5173`.
4. A new migration (0003 or later). Never edit 0001 or 0002. It adds a
   transmissions record per job (`command_id`, nonce hash, `key_id`,
   `issued_at`, `expires_at`, outcome, code), which R3 needs, and job columns
   `operation_id`, `principal_id`, `observed_state`, `observed_at`,
   `needs_attention` and `next_attempt_at`. It decouples reconciliation from
   the `attempt_count ≤ 10` CHECK, which must stop counting reconciliation
   attempts or stop gating `failed`. It also adds a profile path for
   suspension, so that once a harness binding is revoked a profile can no
   longer return to `active`, either through `suspended → revoked` or by
   moving profiles to `revoked` on suspension.
5. Display-name check at profile creation and rename (or at least before
   signing): `agent_name` allows 1–80 UTF-16 units, but provisioning needs
   §8.1.
6. `PROVISIONING_OPERATIONS` and `HarnessCapabilitiesSchema` still list
   `profile.disable` and `profile.enable` and cannot express
   `profile.status`. The sender must be restricted to the v1 set.
7. A level-triggered reconciler (Cron Trigger plus `scheduled` handler) for
   R4–R6 and nonce purging.
8. Transport through `fetchWithTimeout` (10 s, `redirect: "manual"`, 8 KiB
   read limit), and response classification exactly as `classifyResponse`.

### 16.2 Lightagent (adapter only, PR #26 service unchanged)

1. A gateway route `POST {mount}/la-web-provisioning/1/commands` in a separate
   `Router` behind a cargo feature, off by default, never merged into
   `router()` without opt-in.
2. A strict parser (§4.2: duplicates, lexemes, depth), the value profile,
   closed schemas, NFC checking (needs a normalization table, for example the
   `unicode-normalization` crate, which is a new dependency to review), JCS of
   the §4.3 subset, and the canonical byte comparison.
3. Ed25519 through `ring`, with an explicit `S < L` check.
4. Fail-closed configuration: own `harnessId`, the trusted-key list,
   `keyId` → local credential key id (all mapping to one provisioner).
5. A nonce ledger (§9.2) checked and recorded before admission.
6. The §10.3 adapter: `caller_for_at` with the mapped credential, the
   `profileId` consistency check for status and revoke, uppercase code
   mapping, failed operations as `200`/`201` operation responses, and echoes.
7. Regression tests for S1 and S2 (the audit records that they are
   established by reading the code but not yet asserted).
8. A mirror of the specification and fixtures, with a CI job that runs every
   vector and checks `MANIFEST.sha256`.

## 17. Open points for joint review

1. **Normalization dependency.** The NFC requirement adds a Unicode table to
   the Rust gateway. The alternative (no NFC rule, with Web normalizing before
   signing) is weaker but simpler. This revision chooses NFC.
2. **`profile.lookup`** (§13.9) is deferred, not rejected.
3. **Retrying a failed create on the same `agt_` id** (§13.3) relies on
   Lightagent accepting an id whose only operations are `failed` (PR #26 §6
   "No takeover"). Lightagent should confirm this, or Web will mint a new id
   instead.
4. **Response signing** is gated, not dropped (§12.1).
5. **Harness reachability** from Workers (public hostname, Tunnel, or Access
   service token as defence in depth) is a deployment decision. It is not part
   of the protocol.
