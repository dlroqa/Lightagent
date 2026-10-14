# L0 review: `LA-WEB-PROVISIONING/1`

| | |
|---|---|
| Current candidate | `la-web-provisioning-1/r2`, Web commit `d31e544968447c3f7901f004321c20983e879267` (metadata commit `fbc5c20`), branch `l0/la-web-provisioning-r2` |
| Compatibility digest reviewed | `8649b453d55013cc5d35ed81bc052d219c4afd56eec0d031798f680923248b9a` |
| Lightagent baseline | PR #26 `340cb48` (service unchanged), PR #27, PR #28 |
| Verdict | **REJECT r2.** RC2, RC3 and RC4 are resolved, and RC1 is correct for a harness clock that never steps. Two gaps let Web conclude `failed` while the user owns a profile: RC5 (harness clock steps) and RC6 (journal before send). Both corrections are narrow (§4). Accept r3 with a new digest once they are made |
| Lightagent acceptance row | **none recorded.** Acceptance waits for a digest that passes |

The mirror in `la-web-provisioning-1-r2/` was extracted with `git archive`
from Web commit `d31e544`, not from a working tree. It is a review copy of a
candidate and must never be edited. `ACCEPTANCE.md` is not mirrored: it is
Web's metadata, outside the manifest.

## 1. Provenance

| Check | Result |
|---|---|
| Candidate commit `d31e544…` exists, parent `775c7c7` | yes |
| Metadata commit `fbc5c20` changes only `ACCEPTANCE.md` and names `d31e544` and digest `8649b453…` | yes |
| Hashed files committed (not untracked); Web working tree clean | yes |
| `MANIFEST.json` at `d31e544` (and at `fbc5c20`) hashes to `8649b453…` | yes |
| Specification and 8 fixture files: `sha256sum -c` | 10/10 OK |
| `fixtureDigest` | `49c28a8b…1f55`, recomputed |
| Runtime or Cloudflare configuration changes in `d31e544` | none. `src/`, `wrangler*` and `migrations/` are untouched. Only docs, `.gitattributes`, a test-only `package.json` script, and test/tooling files |
| r1 command, redelivery and key vectors carried into r2 | byte-identical apart from `fixtureSet` |
| Reachability of the commit | **local only**: `ACCEPTANCE.md` states the Web repository has no remote. See §6 |

## 2. Verification results

Independent Rust in `crates/lightagent-protocol-conformance`, written from
the specification text. Web's reference codec was not read.

| Fixture set | Result |
|---|---|
| `commands-valid.json` | 15/15 accepted |
| `commands-invalid.json` | 132/132 with the exact code and registered HTTP status |
| `redelivery.json` | 6/6, plus §9.1 invariants and the counter-example |
| Canonical JSON, signing input, `payloadHash`, Ed25519 re-signature, wire body | 20/20 |
| `responses.json` (§12.4 classification and §13.1 ownership) | 45/45 |
| `create-resolution.json` (§13.2 R1–R3) | 21/21 |
| `execution-deadline.json` | 10/10: 5 single step-14 checks, and 5 races both with and without step 14 |
| Manifest, `fixtureIds`, test keys | all verified |

**Schedule model** (`model.rs`). One harness serving one job, with calls
serialized by the ledger lock (P2) and recorded before effect (P3):

| Schedules | Without step 14 | r2 step 14 | Step 14 + fence (RC5) |
|---|---|---|---|
| 20,000 random, clock never steps | 67 unsafe | 0 | 0 |
| Hand-built: harness 30 s behind Web, 1 s backward step | n/a | **unsafe** | safe |
| 50,000 random clock-step hazards within r2's limits | n/a | **1,068 unsafe** (backward steps from 13 s) | 0 |
| 20,000 random, clock steps of up to ±90 s | n/a | n/a | 0 |
| The 5 fixture races | as fixture | as fixture | identical to step 14: no liveness cost |

"Unsafe" means Web concludes `failed` while an operation exists.

**Service semantics** (`provisioning/tests.rs`, `web_*`, test-only, service
unchanged):
- **Ordering:** S1, S2, S3, S11, exact redelivery, `idempotency_conflict`,
  and merits refusals recording nothing.
- **Failed-id reuse.**
- **`ownerBound` combinations:**
  - `profile_created`/`true`, `completed`/`false`, `failed`/`true` and
    `revoked`/`true`;
  - revoke after an operator removed ownership answers `revoked`;
  - a reassigned profile is never re-bound by a create replay, and a new key
    is `profile_unavailable`.
- **`invalid_template`** as a stored failure: an unknown recorded template is
  resumed by `reconcile`, and `status` reports `failed`/`invalid_template`.

## 3. Findings on r2

**RC1 (execution deadline): correct, but conditional.** PR #26 matches the
current code exactly: `exclusive` takes an in-process `Mutex` and then a
blocking `File::lock` with no deadline, and `provision` reads its clock before
`exclusive`. So step 14 needs the new entry point P1 describes.

With step 14, R3 is sound if the harness clock never steps backward. The r2
argument (§13.2) is right, and the clock-free part is right too:
- `t`'s admission check gives `h(t) ≥ t.issuedAt − 30 ≥ u.expiresAt + 30`;
- `u` can start only while `h < u.expiresAt + 30`.

These are the answers to Phase 3:
- the clock is checked at admission (step 12);
- the clock is checked again after the lock, before the lookup (step 14);
- at or after `expiresAt + 30` nothing begins or resumes;
- a command that started in time may finish late (P2, fixture
  `begun-before-deadline-completes-late`);
- expiry never proves failure (`elapsed-time-alone`, `timeouts-exhausted`).

**Smallest safe implementation point:** a new
`ProvisioningService` entry point that takes the deadline, reads the clock
inside `exclusive()` before `admit`, and is otherwise `provision`. `provision`
and the PR #26 HTTP API stay unchanged. This is §16.2 item 9 plus RC5.

**RC5 (new, required): harness clock steps void R3.** r2 says a backward
step "larger than 30 s" voids R3 (§9.3, §17 item 3) and asks only for SHOULD
slewing. Both are insufficient.

The step that suffices is any step larger than `h(t) − (u.expiresAt + 30)`.
With the harness 30 s behind Web, which r2 accepts, that can be 1 s:
1. `u` is admitted and waits for the lock.
2. `t` is issued at `u`'s horizon and is refused on the merits at harness
   time `u.expiresAt + 30`.
3. The clock steps back 1 s.
4. `u` starts at `u.expiresAt + 29`, passes step 14, and binds the user.
5. Web's R3 has already concluded `failed`.

The test `a_one_second_backward_step_defeats_step_14_but_not_the_fence`
reproduces this. Backward steps happen in practice: NTP and chrony step
corrections, VM snapshot restore and migration, and manual changes. The R3
inference therefore cannot be approved as specified.

**RC6 (new, required): journal before send.** R3 quantifies over "every
transmission of the job". §16.1.4 lists the journal's columns but never
requires a transmission to be durably journaled **before** it is sent. A Worker
can fail after `fetch` and before its D1 write. The unjournaled transmission
`u` can then commit while a later merits refusal makes R3 conclude `failed`.

**RC2 (`ownerBound`): resolved.** Every combination is valid, and §13.1
assesses ownership without moving state. Tested against the service:
- `completed`/`false` after reassignment;
- revoke → `ownership_changed` when another owner exists, and `revoked` when
  none remains;
- `revoked`/`true` after an operator gives the profile back;
- a replay never re-binds, and a new key is refused.

Web's classifier (`responses.json`, 45/45) never infers ownership from
`state`. Web never re-creates or re-binds a reassigned profile (§13.1, §13.6).

**RC3 (`INVALID_TEMPLATE`): resolved.** It is a reachable stored failure
(tested). Its registry entry, its two meanings, its R1 handling, and the
no-auto-retry rule all match the service. A retry with a new key under the
same build would be refused `INVALID_TEMPLATE` at validation, so "needs
operator attention" is right.

**RC4 (provenance): resolved**, with the reachability caveat in §6.

**CL1–CL4: accepted** as written. Non-terminal create answers are
`RESPONSE_INVALID`, and status never completes a create. The forged-failure
threat case and the Unicode-version note are in. T1–T7 are reviewed in §5.

## 4. Required corrections for r3 (new digest)

**RC5. Fail-closed execution fence.** Replace §9.3's clock-discipline
paragraph and §17 item 3. Add to §10 step 14 and §10.4 P1:

> The harness keeps a durable, non-decreasing **execution fence**, which
> starts at 0, under the ledger lock. Step 14 refuses `EXPIRED` with no effect
> if `now ≥ expiresAt + 30` **or** `expiresAt + 30 ≤ fence`. When a create
> passes step 15 (local admission), the harness sets
> `fence := max(fence, issuedAt − 30)` and makes it durable before the call's
> answer is released. The fence is not an operation and is not keyed:
> "a `begin` refusal records nothing" still holds for the operation ledger.

Add to §13.2: "R3's horizon clause rests on P1–P3 and the fence. It holds for
any harness clock behaviour, because the fence uses only signed Web stamps:
`t` passing step 15 sets `fence ≥ t.issuedAt − 30 ≥ u.expiresAt + 30`."

A harness clock that is wrong forward only causes extra `EXPIRED` answers,
which are fail-closed. The fence cannot be pushed into the future by a bad
command, because step 12 refuses `issuedAt > now + 30`.

New fixtures in `execution-deadline.json`:
- a race with a backward step after `t` (unsafe with step 14 alone, safe with
  the fence);
- single fence checks at `expiresAt + 30 = fence` (refuse) and
  `expiresAt + 30 = fence + 1` (proceed).

The Lightagent L3 cost is a few lines in the same new entry point, and one
field in the ledger file.

**RC6. Journal before send.** Add to §13.2 (inputs) and §16.1.4:

> Web MUST commit a transmission's journal record (`command_id`, `issued_at`,
> `expires_at`) durably before the request leaves the Worker. A transmission
> without a committed record is never sent. Its later outcome is recorded
> against that record.

Add a `create-resolution.json` note or case showing that a transmission
missing from the journal is not allowed to exist.

## 5. Remaining risks (reviewed)

| # | Item | Assessment |
|---|---|---|
| 1 | Unsigned responses through §12.5 T1–T7 | Acceptable for v1 under gates (a)–(c). Lightagent should keep the gateway on loopback (T7) and send no CORS headers (T6). Validating the Access JWT is optional defence in depth, never authority |
| 2 | Forged `failed`, including refusals without an operation id | Residual, as r2 states. It needs a position inside TLS. It is closed only by response signing or `profile.lookup`. It grants nothing in v1 |
| 3 | Issuer/subject stability and `{provision}`-only admission | Unchanged and sound: `usr_…` is immutable, the issuer is compared byte for byte, every `keyId` maps to one provisioner, and admission is re-read per command (§10.2) |
| 4 | Create/revoke ordering and idempotency | Sound, and tested against the service |
| 5 | Transient refusals | Safe: with no uncertain transmission nothing exists, so `failed` is never wrong about a binding. But a transient `EXPIRED`, `NOT_YET_VALID`, `UNKNOWN_KEY` or `UNAUTHENTICATED` (skew, rotation, operator configuration) fails the job at once. Recommendation (non-blocking): Web retries protocol- and admission-class refusals for a bounded period before failing |
| 6 | Unicode versions | Safe (final `INVALID_DISPLAY_NAME`, no effect). Harness NFC data: `icu_normalizer_data` 2.3.0 |
| 7 | D1 journal and reconciliation | Sufficient with RC6 |
| 8 | `attempt_count` CHECK (0–10) and suspension transitions | Implementation readiness, not protocol. The migration in §16.1.4 must decouple reconciliation from the CHECK. A profile whose harness binding was revoked must not return to `active`, because D1 still allows `suspended → active` |

## 6. Before freezing (non-protocol)

The candidate commit exists only in a local Web repository with no remote.
Before both sides record acceptance of r3, make the candidate commit
retrievable by both reviewers: push it to a remote, or attach a `git bundle`
to the review. Otherwise the acceptance rows name a commit nobody else can
fetch.

## 7. Implementation readiness, after acceptance (not started)

| Lightagent | Web |
|---|---|
| L1: fail-closed `control_webhooks` configuration (harness id, trusted keys, `keyId` → credential mapping) | Production codec ported from the reference, tested against the fixtures |
| L2: separate, feature-gated router on loopback; strict parser and verifier (this crate's code moved into a gateway crate); nonce ledger | Migration: transmission journal (RC6), observed state, decoupled `attempt_count`, suspension path |
| L3: the P1 + fence entry point in `ProvisioningService` (RC1, RC5), the §10.3 adapter, and the lock-wait race test against the real service | Signing secret and `keyId`, `PROVISIONING_ISSUER`, `classifyResponse`, a level-triggered reconciler (R4–R6) |
| CI: this crate's offline job stays; a staged cross-repository job later | Display-name check before signing; v1 operation set only |

Staging, ingress, real keys, merges and activation each need separate
approval.

## r1 history

r1 (`030537b8…a1d2`) was rejected with RC1 (execution deadline), RC2
(`ownerBound` ⇔ `completed`), RC3 (missing `INVALID_TEMPLATE`) and RC4
(uncommitted candidate). It also had CL1–CL4. r2 resolves all of them except
the RC1 clock caveat that RC5 addresses.
