# L0 review: `LA-WEB-PROVISIONING/1`

| | |
|---|---|
| Current candidate | `la-web-provisioning-1/r3`, Web commit `15f196ca804e7c999a8ace1792443af8ecee3d49`, metadata commit `227447b945218c54c6caa60de4af97995db59af4`, branch `l0/la-web-provisioning-r3` |
| Compatibility digest reviewed | `bd410d38aab5890eb8ba97cedbd41a3913046f8d1fd90ff4c6d9a171c9d94a8d` |
| Lightagent baseline | PR #26 `340cb48` (service unchanged), PR #27, PR #28 |
| Verdict | **REJECT r3, narrowly.** RC5 and RC6 are correct, and so are r3's own additions: the fence durable before unlock, durable nonces discarded only under the fence, and `transmission-only` refusals. One counterexample remains, and it is reproduced against the PR #26 service. r3 §10.5 sanctions "initializing a new ledger for an existing harness id" with only a fence raise, and then Web can conclude `failed` for a profile the user owns. r4 needs one text change and one fixture (RC7, §4). There is no wire change |
| Lightagent acceptance row | **none recorded** |

## 1. Provenance

| Check | Result |
|---|---|
| Bundle `la-web-provisioning-1-r3.bundle` (Web-L0-review delivery) against its `.sha256` sidecar | OK, `e450270f4eab77ed7035fa7bbff0c51fdc016d9e7a9be553b76055743406cee4` |
| `git bundle verify` | "is okay", complete history, one ref `refs/heads/l0/la-web-provisioning-r3` → `227447b…` |
| Clone of the bundle (isolated scratch directory, not the Web working tree) | `227447b` → `15f196c` → `fbc5c20` → `d31e544` → `775c7c7`. The r2 commits are the ones reviewed before |
| Candidate commit `15f196c…` exists; metadata commit `227447b` changes only `ACCEPTANCE.md` and names `15f196c` and digest `bd410d38…` | yes |
| `MANIFEST.json` at `15f196c` and at `227447b` | both `bd410d38…` |
| Mirror (`la-web-provisioning-1-r3/`, `git archive 15f196c`, `ACCEPTANCE.md` not mirrored): `sha256sum -c` | 11/11 OK |
| `fixtureDigest` | `0190187a…a857`, recomputed |
| Non-protocol changes `fbc5c20..15f196c` in `src/`, `migrations/`, Wrangler configuration, `package*.json`, secrets | none. Only the specification, fixtures, `ACCEPTANCE.md`, and test/tooling files (`tests/contracts/*`, `tools/provisioning-fixtures.ts`) |
| r2 compatibility | All command, redelivery, response and key vectors are byte-identical, as are the 5 races. r2's deadline cases only gain `fence: 0`. No r2 resolution case changes its expectation. `errors.json` only gains `createResolution` |

**Normative changes r2 → r3:**
- §9.2: nonces are durable (MUST) and retained until the fence excludes them.
- §9.3: clock steps no longer affect safety.
- §10 steps 14–15: fence check, and an advance durable before unlock.
- §10.4: P6 (fence) and P7 (nonces).
- §10.5: the fence, including the restore rule.
- §11: `createResolution`; `protocol` refusals become `transmission-only`.
- §13.2 R3: "open" transmissions.
- §13.2.1–§13.2.3: invariant S, A1–A8, the proof, the counterexamples.
- §13.10: the journal (RC6).
- §16.2 item 9.
- §17, §18.

## 2. Verification results

**Fixtures** (independent Rust, `crates/lightagent-protocol-conformance`;
Web's reference code was not read):

| Fixture set | Result |
|---|---|
| `commands-valid.json` | 15/15 |
| `commands-invalid.json` | 132/132, exact code and HTTP status |
| `redelivery.json` | 6/6 |
| Canonical JSON, signing input, `payloadHash`, Ed25519 re-signature, wire body | 20/20 |
| `responses.json` | 45/45 |
| `create-resolution.json` | 26/26, including every `plan` (desired present and absent) |
| `execution-deadline.json` | 13/13: 8 single checks (wall clock and fence), plus 5 races with and without step 14 |
| `ordering-model.json` | 10 scenarios × 10 rule sets = 100/100: answers, operation, Web's conclusion, `failedAt` and `unsafe`. Replayed by an independent event-driven model (`ordering.rs`). Only its answer-timing convention is taken from the fixtures |
| `errors.json` `createResolution` | equals §11's class mapping for every code |

**Adversarial search** (the review's own generator and seeds; each count is
schedules where Web concludes `failed` while an operation or binding exists
from a transmission issued by then):

| Search | r3 | Ablations |
|---|---|---|
| 40,000 broad schedules per rule set: 1–3 copies, clock offset ±30 s with up to two ±90 s steps, restarts, housekeeping, late key, lock waits, long holds, lost answers, crashes after record, unjournaled sends | **0** (also 0 without the wall-clock check) | r2 74, review proposal 22, protocol refusals final 19, no journal 10, non-durable nonces 3. The fence, durability before unlock and clock-purge ablations need narrow hazards: covered by the fixtures and the focused search below, matching Web's §18 split |
| 50,000 focused clock-step pairs (from r2's review) | fence: 0 | step 14 alone: 1,068 |
| 20,000 schedules on a clock that never steps | 0 | no step 14: 62 |
| 20,000 broad schedules plus one **ledger replacement** under r3's §10.5 rule | **6 unsafe** | see RC7 |

**PR #26 service** (`web_*` in `provisioning/tests.rs`, 8 tests, test-only,
service unchanged):
- S1, S2, S3, S11;
- failed-id reuse;
- every `ownerBound` combination, and operator removal and reassignment;
- `invalid_template` as a stored failure;
- **new:** `web_a_fresh_ledger_answers_a_merits_refusal_for_a_bound_profile`
  (RC7).

## 3. Assessment

### RC5: the execution fence

**Correct.** I checked §13.2.1 line by line against PR #26:

- **Lemma 1 (final refusals).**
  - `admission` refusals come from `admit` under the lock, at step 15.
  - `merits` refusals come from `begin`, which records nothing before
    returning (verified in code).
  - Both happen after the nonce was durably recorded at step 12.
  - `payload` refusals are pre-nonce but deterministic in a byte-identical
    payload. A8 freezes the step-8 rules. Every r3-valid payload passes PR #26
    `validate`.
- **Lemma 2 (exclusion by a later merits refusal).**
  - `t` passed step 15, so `F ≥ t.issuedAt − 30 ≥ u.expiresAt + 30` was durable
    before unlock.
  - A copy of `u` that locks later is refused by the fence, whatever the
    clock says.
  - A copy that locked earlier finished durably first (P2: `exclusive` holds
    the mutex and the file lock for all of `work()`). `t`'s lookup would then
    have found it (P3: `begin` records `pending` before `materialize`).
- **No clock in the proof.** The harness clock appears nowhere in the proof,
  and none of my searches found a schedule against it except RC7.

The three named cases:

| Case | Finding |
|---|---|
| A. Fence persistence after unlock | Excluded by A4, and enforceable in Lightagent: `exclusive` releases the file lock only after `work()` returns, and `load()` re-reads the ledger file under the lock on every call (no cache). The fence is one field in that file, written by `save()` (`write_durable`) inside `work()`. A second gateway process therefore always reads the advanced value. Fixture `fence-visible-only-after-unlock` reproduced |
| B. Replay of a refused command | Excluded by A5: nonces are durable across restarts, and discarded only after the fence is raised past the command. After a restart a copy is `REPLAY_DETECTED` (indeterminate). After a purge and a clock rollback, the fence refuses it. Fixtures `duplicate-of-refused-command-after-restart` and `duplicate-after-clock-purge-and-rollback` reproduced. A nonce-state restore is covered by the restore rule (but see RC7) |
| C. Early refusal | Correct: `UNKNOWN_KEY`, `KEY_REVOKED`, `BAD_SIGNATURE`, `WRONG_AUDIENCE`, `NOT_YET_VALID`, `EXPIRED` (step 12 or 14) and `NONCE_STORE_UNAVAILABLE` are `transmission-only` and never evidence. Only `payload` (deterministic), `admission` (step 15) and `merits` (step 16) are `final`. Fixtures `pre-nonce-refusal-then-duplicate`, `single-protocol-refusal`, `single-expired-refusal` and `protocol-refusal-ruled-out-by-merits` reproduced |

**Additional adversarial review:**

| Item | Finding |
|---|---|
| Forward-issued stamps, skew | Step 12 bounds `issuedAt − 30 ≤ now`, so every advance has `F ≤ now`. A fast harness clock raises `F` only to its own `now`. The cost is a fail-closed outage after the clock is corrected, never a safety loss |
| Overlapping windows, out-of-order delivery | Safe (`overlapping-windows-out-of-order`) |
| Several provisioners or Web environments sharing one fence | Safe. No liveness cost on a clock that never steps back: a fence refusal implies `expiresAt + 30 ≤ F ≤ now`, which the wall-clock check refuses anyway |
| Crash between nonce and fence persistence | No answer was released (uncertain), and later copies are `REPLAY_DETECTED`. Safe |
| Crash between profile commit and response | Uncertain, then the R4 resend returns the operation (R1). Safe |
| Restoring an older ledger or fence snapshot | **Unsafe unless restored consistently with the identity store: RC7** |
| Two processes or ledgers sharing one harness id | A1 forbids it. On one host, processes share the file lock and the ledger (safe). Separate ledgers (two hosts, `cloudflared` replicas on different hosts, a cloned VM) break idempotency itself, and no protocol can detect a clone. This must be an explicit operator assumption, and is (A1) |
| Overflow, invalid stamps | Stamps are 10-digit integers, and `F` is a `u64` of seconds. No overflow |
| Legitimate retries | Refused only if issued ≥ 120 s before the newest admitted create. Web's fresh retries pass. A Web clock stepping back by more than about 120 s fails closed |
| Are A1–A8 enforceable by Lightagent? | A1: by deployment, plus a harness-id check in the ledger (L1). A2: by configuration (one provisioner per key map). A3: yes, PR #26 today. A4: yes, one field under `exclusive`. A5: yes, a new nonce store whose purge takes the ledger lock first. A6, A7: Web. A8: by the protocol. **An additional assumption is missing: RC7** |

### RC6: the write-before-send journal

**Correct.** §13.10 requires all ten properties the brief lists:
- durable job and desired state (step 2);
- an immutable logical command (step 3, and the payload-hash trigger);
- durable intent before any byte is sent (steps 4–5, invariant J);
- an atomic claim and insert (one D1 batch, which runs as a SQL
  transaction);
- conservative treatment, with recoverable lost answers and identity reuse,
  crash-safe reconciliation and suspension handling (§13.5);
- no `failed` while an open transmission remains (R3).

The six cases come out as follows:

| Case | Treatment |
|---|---|
| Prepared, not journaled | Cannot be sent (step 5) |
| Journaled, not sent | Uncertain, never "not sent" (`journaled-never-sent`) |
| Sent, no answer | Uncertain |
| Answered indeterminate | Uncertain |
| Completed, answer lost | R4 resend, then R1 |
| Revoked, profile still on disk | S1 returns `revoked`. A new key is `PROFILE_UNAVAILABLE` |

The append-only and fixed-payload triggers, `CHECK` constraints and the
conditional claim (`INSERT … SELECT … WHERE lease_token = ?` in the same
batch) use SQLite features that D1 supports and that Web's migration 0001
already uses. Safety rests on J, not on the lease.

Implementation note (not a protocol defect): the reconciler must read the
journal from D1's primary, or with a Sessions bookmark, not from a lagging
replica.

### Authority and existing Lightagent functions

No r3 rule bypasses PR #26: `{provision}`-only admission is re-read under
the lock, and a signature without a mapping is `UNAUTHENTICATED`. Also
unchanged: issuer/subject mapping, ownership, operation-id revoke,
idempotency, profile-id rules, the credential vault and profile isolation,
and the standalone operator API.

One implementation obligation for L2: the `keyId` → credential mapping must
be resolved at step 15, after the nonce is recorded, because
`UNAUTHENTICATED` is a `final` refusal. Resolving it at step 10 would make it
a pre-nonce refusal wrongly labelled final.

### Response authentication (v1)

Within §12.5 (T1–T7: operator-controlled Tunnel, loopback origin, single-path
ingress, service-token Access, no browser path, no public listener):

| Threat | v1 status |
|---|---|
| Forged success | Needs a position inside TLS. Effect: Web shows `active`. Grants nothing without the Worker |
| Forged `failed`, or a refusal without an operation id | Residual, and cannot be re-checked without `profile.lookup` |
| Wrong observed-state transitions | Prevented by forward-only `state` and the correlation echoes |
| Key rotation | Covered by §6.4 |
| Harness unreachable | Uncertain, and R4 keeps reconciling, never `failed` |

These risks are acceptable for the narrowly defined v1 deployment. Before
broader multi-user execution (§12.1 gates a–c), response signing and
`profile.lookup` must be added, along with the separately proven tenant
gateway.

## 4. Required correction for r4 (new digest)

**RC7. Ledger continuity.** The smallest counterexample (reproduced against
the service, the model, and 6 of 20,000 random schedules):

1. `u` creates the profile and binds the user. Its answer is lost.
2. The harness's ledger is replaced. §10.5 allows "initializing a new ledger
   for an existing harness id" and sets `F := max(F, T + 150)`. The identity
   store and the profile directory are kept.
3. Web's R4 resend `t` is issued after `T` and passes the fence. The new ledger
   has no operation for the key. PR #26 `begin` refuses `PROFILE_UNAVAILABLE`,
   because the identity store still records the ownership.
4. That is a `merits` refusal issued after `u`'s horizon, so R3 concludes
   `failed` while the user owns the profile. Lemma 2 step 4 fails because P3
   ("every later lookup sees every operation an earlier call started") no
   longer holds across the replacement.

§17 item 3 says restores are "covered by PR #26 §12". They are not. PR #26 §12
only restores the ledger and the identity store **together** from one point.
It never sanctions a new ledger beside a kept identity store.

Normative change:
- **§13.2.1:** add A9: "Ledger continuity: the operations recorded for a
  harness id are never lost while their effects (profiles, ownership) persist.
  A ledger is restored only together with the identity and profile stores,
  from one consistent point."
- **§10.5, "Restore or new ledger" row:** replace with: "A restore restores the
  provisioning ledger (operations, fence, nonces), the identity store and the
  profile store together from one consistent point (PR #26 §12), then sets
  `F := max(F_restored, T + 150)`. A harness id's ledger is never initialized
  anew, or restored alone. If it is lost, the harness id is **retired**: the
  harness is registered under a new id, and every unresolved create on the
  retired id stays unresolved (needs attention), never `failed`. Commands to
  the retired id are `WRONG_AUDIENCE`, which is `transmission-only`."
- **§17 item 3:** correct accordingly.
- **Fixture:** add `fresh-ledger-retained-identity` to `ordering-model.json`
  (the schedule above). It is unsafe under r3's rule, and safe under r4 (the
  resend goes to the new id, or is never answered on the merits).

Lightagent can enforce RC7 (L1). The gateway fails closed at startup if its
ledger is new or lacks the configured harness id while the identity store
holds provisioning bindings (`provisioning:` transaction keys, or
`.provisioning-operation` markers). It also records the harness id in the
ledger and refuses a mismatch.

## 5. Recommended clarifications (same revision, non-blocking)

1. §10.2: state that the `keyId` → credential mapping is resolved at step 15,
   after the nonce.
2. §13.10: the reconciler reads the journal with primary consistency.

## 6. Implementation readiness after acceptance (not started)

| Stage | Lightagent | Web |
|---|---|---|
| L1 | Fail-closed `control_webhooks` configuration: harness id (recorded in the ledger, RC7 startup check), trusted keys, `keyId` → credential mapping onto one provisioner. Off by default; a disabled gateway leaves behaviour identical to today | Migration 0003+: journal (§13.10), job columns, a decoupled `attempt_count`, the suspension path; display-name check; v1 operation set only |
| L2 | A separate, feature-gated router on loopback; this crate's parser and verifier moved into it; durable nonce store (P7; its purge takes the ledger lock and raises the fence first); credential mapping at step 15 | Production codec from the reference, tested on the fixtures; signing secret, `keyId`, `PROVISIONING_ISSUER`; `classifyResponse` |
| L3 | One additive `ProvisioningService` entry point, `provision_before(caller, key, request, issued_at, expires_at)`: inside `exclusive()` read the clock and `execution_fence`, refuse `EXPIRED`, `admit`, advance and `save()` the fence, then the existing `provision` path. `provision` and its HTTP API stay unchanged. Tests: the lock-wait race with a backward clock step, a duplicate of a refused command across a restart, and the restore rule | Level-triggered reconciler (R4–R6) and lease recovery; transport through `fetchWithTimeout` |
| CI | This crate's offline job, plus L3 service tests on all platforms; a staged cross-repository job later, reported as skipped when its environment is absent | Fixture and D1 journal tests in workerd |

Staging, ingress, real keys, merges and activation each need separate
approval.

## History

| Revision | Digest | Outcome |
|---|---|---|
| r1 | `030537b8…a1d2` (uncommitted) | Rejected: RC1 execution deadline, RC2 `ownerBound`, RC3 `INVALID_TEMPLATE`, RC4 provenance |
| r2 | `8649b453…8b9a` (`d31e544`) | Rejected: RC5 clock steps void R3 (fence), RC6 journal before send |
| r3 | `bd410d38…4a8d` (`15f196c`) | Rejected: RC7 ledger continuity |
