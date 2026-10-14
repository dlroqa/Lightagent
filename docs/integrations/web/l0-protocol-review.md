# L0 review: `LA-WEB-PROVISIONING/1` candidate r1

| | |
|---|---|
| Candidate | `la-web-provisioning-1/r1` |
| Compatibility digest reviewed | `030537b8be8cc003cff31e503343d5082629eab3f3f10c82fb17d7776b24a1d2` |
| Lightagent baseline | PR #26 `340cb48` (service), PR #27 (audit and handoff) |
| Verdict | **REJECT r1.** It is secure and implementable in principle, but four corrections are required (§4). Accept a revision r2 with a new digest once they are made |

The review used Web's files directly. A byte-identical mirror is in
`la-web-provisioning-1-r1/`, next to this file. The mirror is a review copy
of a **candidate**, not a frozen protocol, and must never be edited.

## 1. Provenance

- The candidate files are **not committed** in the Web repository. They are
  untracked in the working tree on top of `775c7c7` (`phase-5-account-ui`),
  next to uncommitted edits to other Web docs. The mirror was copied from that
  working tree, and its bytes still match the manifest and Web's current files.
- `ACCEPTANCE.md` defines the digest as the hash of `MANIFEST.json` "exactly
  as committed". No such commit exists yet, so neither side can record
  acceptance against a commit (RC4).

## 2. Verification results

All results come from the independent Rust implementation in
`crates/lightagent-protocol-conformance`. It is written from the
specification text, Web's reference codec was not read, and it uses no Web
tooling.

| Check | Result |
|---|---|
| `MANIFEST.json` SHA-256 equals the reported digest | pass |
| Specification and 7 fixture files: size and SHA-256 per `MANIFEST.json`; `MANIFEST.sha256` agrees; `fixtureDigest` recomputed | pass (`sha256sum -c`: 9/9 OK) |
| `fixtureIds` list exactly the vector ids in each file | pass |
| Test keys: `seed = SHA-256(label)`, public keys derived with `ring` | 4/4 |
| `commands-valid.json` | 15/15 accepted |
| `commands-invalid.json` | 132/132 refused with exactly the expected code (and the registered HTTP status) |
| `redelivery.json` | 6/6 as expected |
| Canonical JSON, signing input, `payloadHash`, deterministic Ed25519 signature (re-signed from the test seed), and exact wire body reproduced | 20/20 (15 valid commands and 5 detailed redeliveries); worked example §6.3: 647, 690 and 748 bytes |
| Redelivery invariants (§9.1) and the "not a redelivery" counter-example | pass |
| `responses.json` (35), `create-resolution.json` (16) | Web-side classification vectors (§15). Reviewed by hand against PR #26 semantics; see RC2 and §3.F |

Each check is load-bearing. Removing the duplicate-key, NFC, canonical-bytes
or replay check makes 4, 1, 7 and 3 vectors fail respectively.

One exception: removing the explicit `S < L` check changes nothing, because
`ring` 0.17.14 also refuses a non-canonical `S`. The fixture
`signature-noncanonical-s` therefore cannot show whether the check exists
when `ring` is the backend. The explicit check stays, as §6.2 requires, and a
unit test covers it. This needs no protocol change.

## 3. Semantic compatibility with PR #26

These were tested against the service itself. The new tests are in
`provisioning/tests.rs`, each starting `web_`. They are test-only, and the
service is unchanged.

**A. Unicode normalization.** Accept NFC (§17.1). No new crate is needed:
`icu_normalizer` 2.3.0 (ICU4X), with `compiled_data`, is already compiled
into every Lightagent binary through `reqwest → url → idna → idna_adapter`.
The verifier declares it directly with `default-features = false`, so
`Cargo.lock` gains only the workspace package entry. It gains no crate and
no feature. Do not add the `unicode-normalization` crate: it would be a new
crate. Rust's `char::is_control` (Cc) and `char::is_whitespace`
(`White_Space`) are exactly the §8.1 sets. Different Unicode versions on the
two sides can only disagree about characters assigned between them. That
produces `INVALID_DISPLAY_NAME`, a final refusal with no effect (CL4).

**B. Retrying a failed create on the same id (§17.3): confirmed.** After an
operation for `agt_…` ends `failed`, a new idempotency key provisions the same
id. `begin` reserves an id only through non-failed operations, and the failed
profile is in quarantine (test
`web_a_failed_create_leaves_its_profile_id_available_to_a_new_key`). It
succeeds only once the cause of the failure is gone. The failed key keeps
answering its failure.

**C. Ordering.** All of these are tested:
- Exact redelivery is one operation.
- A changed payload under the same key is `idempotency_conflict`, and nothing
  changes.
- A delayed create after a revoke returns the recorded `revoked` operation
  and binds nothing (S1/S11).
- A new key or another subject on a revoked id is `profile_unavailable` (S2).
- A revoke of an unfinished operation is `operation_in_progress` without
  effect (S3).
- A refusal on the merits records nothing, so the same key can succeed later.

**D. Error mappings.** Every registered code traces to the same-named PR #26
code, with the same meaning, or is a gateway code. The order PR #26 enforces
matches the class split:
- `admission` codes, including `profile_outside_credential`, come before the
  idempotency lookup.
- `merits` codes come only from `begin`, which records nothing.

PR #26 codes the gateway can never return are the operator-only codes:
`operator_required`, `invalid_quota`, `unknown_principal`,
`unknown_provisioner`, and `issuer_not_trusted` as a request error.

Two mismatches remain: RC2 and RC3.

**E. Response authentication.** Unsigned v1 responses are acceptable under
the §12.1 gate: no user credential is issued, and every user path goes
through the Worker. The threat table and the gate need two clarifications
(CL2, CL3). No browser-to-harness path exists in the candidate.

**F. Idempotency and reconciliation.** The transmission journal and R1–R4/R6
recover ambiguous outcomes without a second profile or binding, **except** the
R3 gap that RC1 closes. Status polling cannot recover an interrupted create
(CL1).

## 4. Required corrections (new revision and digest)

**RC1. Execution deadline under the ledger lock (§9.3, §10, §13.2).**
R3 assumes that a transmission `u` can have no effect after its acceptance
horizon. But the time window is checked at step 12, and `provision` then
waits without limit for the ledger lock: an in-process `Mutex`, then a
blocking `File::lock`. `provision` also reads its clock before taking the
lock. So `u` can pass step 12 in time and commit after a later transmission
`t` of the same job was refused on the merits. Web would then mark the job
`failed` (R3) while the user is bound, and R6 leaves `failed` alone.

Add to §10, step 14:

> Before the idempotency lookup, while holding the ledger lock, the harness
> re-reads its clock. If `now ≥ expiresAt + MAX_CLOCK_SKEW_SECONDS`, it
> refuses with `EXPIRED` and has no effect.

Add to §13.2: "This bound holds because of §10 step 14." Update the
`errors.json` entry for `EXPIRED` to say it may also come from step 14.

The Lightagent side is a narrow, additive service entry point that checks a
deadline under the lock. It is planned for the L3 stage. It is not built in
this review, and `provision` is unchanged.

**RC2. `ownerBound` is current ownership, not a function of state (§12.1
item 3, §12.2, fixture `invalid-owner-bound-pending`).** PR #26 computes
`owner_bound` as the effective ownership of (operation principal, profile)
when it answers. Tests show three reachable states that break
"`ownerBound` ⇔ `completed`":
- `profile_created` and bound: a crash after the identity commit;
- `completed` and not bound: an operator reassigned the profile;
- `failed` and bound: the same user was later provisioned on that id.

Web can observe the second, for example on a create re-send. It would
classify it `RESPONSE_INVALID` and keep re-sending without ever resolving.

Replace the rule with:

> `ownerBound` reports whether the operation's principal effectively owns
> `profileId` on the harness when it answers. It is independent of `state`.
> `completed`/`false` means the harness operator changed ownership, which
> needs attention. Any non-`completed` state with `true` is informational.
> Neither is `RESPONSE_INVALID`.

Change `invalid-owner-bound-pending` accordingly, and add a valid
`completed`/`false` response vector.

**RC3. Missing operation failure code (§11, `errors.json`
`operationFailureCodes`).** PR #26 can record `failed` with
`invalid_template` when a recorded operation's template is unknown at
materialization. That happens only across a harness build change, but it is
a real stored state. Add `INVALID_TEMPLATE` to `operationFailureCodes`.

**RC4. Commit the candidate.** Commit the specification, fixtures, manifest
and `.gitattributes` on a Web branch, and name that commit in
`ACCEPTANCE.md`. The digest is accepted only for committed bytes.

## 5. Recommended clarifications (same revision)

- **CL1 (§13.3).** In Lightagent v1, a `profile.create` operation response is
  always terminal (`completed`, `failed` or `revoked`). An interrupted create
  is answered with an indeterminate code. `profile.status` is read-only and
  never advances an operation. Recovery is R4, re-sending the same create,
  or an operator `reconcile`. Replace "after `pending` or `profile_created`,
  Web polls `profile.status`" with "re-send the create (R4); use status only
  to confirm".
- **CL2 (§12.1 threat table).** Add a forged `failed`. It is silent like a
  forged `revoked`: R6 leaves `failed` operations alone, so a real binding
  would remain. It is covered by the same gate.
- **CL3 (§12.1 gate b).** Say whether Cloudflare Tunnel or Access TLS
  termination counts as operator-controlled. Recommendation: only when the
  harness operator controls that Cloudflare account. Otherwise response
  signing is required first.
- **CL4 (§4.6).** Note that NFC tables follow each side's Unicode version.
  A disagreement yields `INVALID_DISPLAY_NAME` (final, no effect). Web MAY
  restrict names to characters assigned in a stated Unicode version.

## 6. Acceptance criteria status

| # | Criterion | Status |
|---|---|---|
| 1 | Manifest hashes verify | met |
| 2 | Rust reproduces canonical bytes and signatures | met |
| 3 | Every applicable fixture passes | met (153/153 command and redelivery vectors) |
| 4 | Specification matches PR #26 semantics | **not met**: RC1, RC2, RC3 |
| 5 | Unicode and retry questions resolved | met (A, B) |
| 6 | No rule bypasses `{provision}`-only credential checks | met: §10.2 keeps PR #26 admission, re-read on every command, and a signature alone is `UNAUTHENTICATED` without a mapping |
| 7 | No independent Lightagent function or boundary weakened | met: no route, gateway, key or production code added; the service is unchanged |

## 7. Re-review of r2

Replace the mirror with r2's committed bytes, then update
`COMPATIBILITY_DIGEST` in `crates/lightagent-protocol-conformance/tests/fixtures.rs`.
Re-run the `LA-WEB-PROVISIONING/1 fixtures (offline)` job, and check the
RC1–RC3 text against the service tests.
