# L0 review: `LA-WEB-PROVISIONING/1`

| | |
|---|---|
| Current candidate | `la-web-provisioning-1/r4`: Web commit `f70789ec69c6d747ba5b7c84fe1b3f8db0cba727`, metadata commit `0cf96f3744e986cb506a767b3c350a0c6c9311a7`, branch `l0/la-web-provisioning-r4` |
| Compatibility digest | `454066f8fb75e886119c401e4a45ec15ebf2b83115165b2e1f85180abfd29a43` |
| Lightagent baseline | PR #26 `340cb48` (service unchanged), PR #27, PR #28 |
| Verdict | **ACCEPT r4.** Lightagent's acceptance is recorded in `ACCEPTANCE.md`, next to this file. It carries one binding implementation obligation: L1 implements the §10.6 check with a generation marker (§3) |
| Scope | This is protocol acceptance. It does not mean anything is ready to run: the gateway, fence, nonce store, continuity check, Web transport and D1 journal are not implemented (§6) |

The mirror in `la-web-provisioning-1-r4/` was taken with `git archive` of
`f70789e`, from an isolated clone of the bundle. Web's `ACCEPTANCE.md` is not
mirrored. The mirror is a pinned copy and must never be edited.

## 1. Provenance

| Check | Result |
|---|---|
| Bundle `la-web-provisioning-1-r4.bundle` against its `.sha256` sidecar | OK, `ea4524c4a5197dc765a6f055bad1fabde39537f5ad713314a74ecf7da941e889` (the expected value) |
| `git bundle verify` | "is okay", complete history, one ref `refs/heads/l0/la-web-provisioning-r4` → `0cf96f3` |
| History | `0cf96f3` → `f70789e` → `227447b` (r3 metadata) → `15f196c` → `fbc5c20` → `d31e544` → `775c7c7` |
| Metadata commit `0cf96f3` | Changes only `ACCEPTANCE.md`. Names `f70789e` and `454066f8…` |
| `MANIFEST.json` at `f70789e` and at `0cf96f3` | both `454066f8…` |
| `sha256sum -c` over the mirror | 12/12 OK, including the new `ledger-continuity.json` |
| Non-protocol changes `227447b..f70789e` (`src/`, `migrations/`, Wrangler configuration, `package*.json`) | none. Only the specification, fixtures, `ACCEPTANCE.md`, and test/tooling files |
| Wire format | Unchanged from r3. All command, redelivery, response and key vectors are byte-identical apart from `fixtureSet` (verified via the 15/132/6/45 results and the r3 → r4 diff) |

**Normative changes r3 → r4:**
- A9: ledger continuity.
- §10 step 13: the C1 stamp is re-checked under the lock.
- §10.2: the authentication-decision order. The `keyId` → credential mapping
  is resolved at step 15, after the nonce.
- P8.
- The corrected §10.5 restore row.
- §10.6: relationships, C1–C3, fail-closed behaviour, retirement.
- §11: new code `LEDGER_CONTINUITY_FAILED` (`503`, `protocol`,
  `transmission-only`).
- §13.2 R3a: `PROFILE_UNAVAILABLE` while a transmission is open, and
  `LEDGER_CONTINUITY_FAILED`, need attention.
- §13.10: primary-consistent D1 reads.
- Retired-harness plans.
- §17 item 3 corrected.

## 2. Verification results

**Fixtures** (independent Rust, `crates/lightagent-protocol-conformance`;
Web's reference code was not read):

| Fixture set | Result |
|---|---|
| `commands-valid.json` / `commands-invalid.json` / `redelivery.json` | 15/15, 132/132 (exact code and HTTP status), 6/6 |
| Canonical JSON, signing input, `payloadHash`, Ed25519 re-signature, wire body | 20/20 |
| `responses.json` | 45/45 |
| `create-resolution.json` | 29/29, including all four plans (active and retired, desired present and absent) |
| `execution-deadline.json` | 13/13 |
| `ordering-model.json` | 17 scenarios × 14 rule sets = **238/238**: answers, operation, surviving binding, `failedClosed`, Web's conclusion, `failedAt`, `unsafe` |
| `ledger-continuity.json` | 13/13 (abstract C1–C3, `continuity.rs`) |
| `errors.json` | `createResolution` equals §11's class mapping for every code. `LEDGER_CONTINUITY_FAILED` is `503`/`protocol`/`transmission-only` |

**Adversarial search** (the review's own generators and seeds; schedules
where Web concludes `failed` while an operation or binding exists from a
transmission issued by then):

| Family | r4 | Others |
|---|---|---|
| 40,000 broad schedules per rule set (copies, clock offset and steps, restarts, housekeeping, late keys, lock waits, crashes, unjournaled sends) | **0** | r2 74, review proposal 22, protocol refusals final 19, no journal 10, non-durable nonces 3 |
| 40,000 recovery hazards (one create, then a random subset of {ledger, identity, nonces, fence} restored, the fence to 0 or older, then resends) | **0** | r3 144, r4 without the continuity check 45, r4 without the restore's fence raise 8 |
| 40,000 broad schedules plus a recovery | **0** | — |
| 50,000 clock-step pairs; 20,000 schedules on a clock that never steps | fence: 0 | step 14 alone: 1,068; no step 14: 62 |
| Deterministic RC7 variants: fresh ledger with kept identity, fresh ledger with a quota refusal, older ledger, older identity, lost ledger with a pending `u`, consistent restore | never `failed` while bound. The first four fail closed with every later answer `LEDGER_CONTINUITY_FAILED` and the binding untouched | r3 fails the first three |

The model matched the fixtures only after two corrections toward PR #26's
real behaviour:
- `begin` checks quota before the profile id, which is why a fresh ledger
  answers `QUOTA_EXCEEDED` and R3a alone cannot catch it;
- a replay of a terminal operation does not bind again.

**PR #26 service** (`web_*`, 9 tests, test-only; the service is unchanged):
- S1, S2, S3, S11;
- failed-id reuse;
- every `ownerBound` combination, and operator removal and reassignment;
- `invalid_template` as a stored failure;
- the RC7 fresh-ledger refusal;
- **new:** `web_continuity_inputs_are_observable_on_the_real_stores` (§3).

## 3. RC7 against Lightagent's real storage

| Requirement | Finding |
|---|---|
| C1: the ledger exists, is stamped with the harness id, and is not retired | **Not present today.** PR #26 creates its ledger lazily, with no harness id (r4 P8 says so). L1 adds the fields to the ledger file under the existing lock. Enforceable |
| C2: nothing outside the ledger names an operation it lacks | **Enforceable now**, through profile markers. Each profile's `.provisioning-operation` holds its operation's random `marker` token (not the id), which only the ledger maps. The test shows markers survive completion, revocation, operator reassignment and crashes after the profile was created, and all map to ledger operations. After a fresh ledger, none do. A gateway can refuse before serving, and looking alters nothing |
| C3: the identity store is not behind the ledger | **Not enforceable as worded** ("every completed or revoked operation has its identity provisioning record"). Identity idempotency records, which hold `provisioning:<op>`, are capped at `MAX_IDEMPOTENCY = 10_000`, evicted oldest first on every commit, and not exposed. A literal C3 would fail closed falsely after 10,000 provisioning transactions. **r4 §10.6 anticipates this ("if they could be pruned, L1 MUST add a durable generation marker").** The marker is buildable from what exists: the identity store's `generation()` is public, monotone, and drops when an older copy is restored (tested) |

**Distinguishing the four cases** requires that marker. The smallest
design, all additive, for L1:

1. **Ledger fields:**
   - `harness_id`;
   - `continuity_id`: 128 random bits, written by an explicit `init`;
   - `retired`;
   - `identity_generation_seen`: the identity generation each bind or revoke
     transaction returned (`Outcome::Applied { generation }`).
2. **Identity document field** (serde default):
   `provisioning_continuity = { continuity_id, ledger_generation_seen }`,
   written in the same bind/revoke transaction.
3. **Check:**
   - **C1:** the stamp matches, and the ledger is not retired.
   - **C2:** the identity `continuity_id` equals the ledger's; its
     `ledger_generation_seen` ≤ the ledger's `generation`; every on-disk
     marker token maps to a ledger operation.
   - **C3:** the ledger's `identity_generation_seen` ≤ the identity
     `generation()`.
4. **`init`:** permitted only when no ledger continuity exists, no profile
   carries a marker, and the identity store has no `provisioning_continuity`.
   Otherwise it refuses, and the operator retires the id.

| Case | Outcome |
|---|---|
| New harness | No traces: `init` stamps, and C1–C3 pass |
| Ledger lost or replaced | Traces without a matching `continuity_id`, or unknown markers: fail closed, `init` refused, retire |
| Consistent restore | Ids match and generations are consistent: serve (after the fence raise) |
| Retired id reactivating | C1 `RETIRED`. If the ledger was deleted, the traces fail C2. If every store was wiped, nothing is bound, so S holds, and Web's registry, which keeps the retired id, refuses it |

A9 is therefore enforceable. The protocol does not rely on state Lightagent
cannot verify, because it explicitly requires this marker when records can be
pruned. Lightagent records that requirement as binding (`ACCEPTANCE.md`).

**Historical ownership:**
- A revoked profile keeps its marker and its ledger operation (C2 passes).
- An operator-reassigned profile keeps both. C3 does not depend on current
  ownership, and `ownerBound` reports it (§13.1).
- A profile an operator deleted removes its marker. Its stale ownership record
  is inert.
- Quarantined failed profiles live outside `profiles/`, and their operations
  are `failed` in the ledger.

## 4. Earlier safeguards (no regression)

The following are unchanged, with their fixtures and tests passing:
- RC1 deadline;
- RC2 `ownerBound`;
- RC3 `INVALID_TEMPLATE`;
- RC5 fence and nonces;
- RC6 journal;
- strict Ed25519 and JCS;
- `{provision}`-only admission;
- immutable idempotency;
- operation-id revoke;
- issuer/subject mapping;
- polling-only status;
- T1–T7.

§10.2 now places the `keyId` → credential mapping at step 15, after the
durable nonce, as this review required. Steps 10–12 still check the trusted
key, its revocation, the signature, audience, freshness and replay. A
signature without a mapping grants nothing.

Primary-consistent D1 reads (§13.10) are a Web implementation prerequisite.

## 5. Residual risks (accepted, explicit in r4)

- **A1:** two live copies of one ledger (a cloned VM, or `cloudflared`
  replicas on different hosts) are excluded only by deployment. L1 documents
  it, and the continuity id makes copies distinguishable afterwards.
- **Restores:** need an operator-verified time `T` for the fence raise.
- **Unsigned v1 responses** are acceptable only under §12.5 T1–T7. A forged
  refusal without an operation id stays unverifiable until `profile.lookup` or
  response signing exist. Both are required before broader multi-user
  execution (§12.1 gates).
- **A retired harness's live profiles** need an operator migration decision.
  Web never moves them itself.

## 6. Implementation readiness (separate from acceptance; nothing below exists yet)

| Stage | Lightagent | Web |
|---|---|---|
| L1 | Fail-closed `control_webhooks` configuration (off by default; behaviour identical when off). Explicit `init` and `retire` operator commands. The §3 continuity marker and check at start, activation and after restores; C1 per command under the lock | Migration 0003+: transmission journal (J, triggers, lease), job columns, a decoupled `attempt_count`, the suspension path, and harness retirement records |
| L2 | A separate, feature-gated loopback router; the parser and verifier from this crate; a durable nonce store (P7, purge under the ledger lock after raising the fence); credential mapping at step 15; `LEDGER_CONTINUITY_FAILED` before step 1 | Production codec from the reference, tested on the fixtures; signing secret, `keyId`, `PROVISIONING_ISSUER`; `classifyResponse` |
| L3 | One additive `ProvisioningService` entry point. Inside `exclusive()`: C1, clock and fence (P1, P6), `admit`, then advance and `save()` the fence; then the existing path. It records `identity_generation_seen` and `provisioning_continuity` in the bind and revoke transactions. `provision` and its HTTP API stay unchanged | Primary-consistent, level-triggered reconciler (R4–R6, R3a), and transport via `fetchWithTimeout` |
| Tests | Lock-wait race with a backward clock step; a duplicate of a refused command across a restart; the restore fence raise; C1–C3 on real stores, including pruning beyond 10,000 identity records; fail-closed answers alter nothing | Fixtures and the D1 journal in workerd |
| CI | This crate's offline job stays; L1–L3 tests on all three platforms; a later staged cross-repository job, reported as skipped when its environment is absent | Existing gates |

Staging, ingress, real keys, merges and activation each need separate
approval.

## History

| Revision | Digest | Outcome |
|---|---|---|
| r1 | `030537b8…a1d2` (uncommitted) | Rejected: RC1–RC4 |
| r2 | `8649b453…8b9a` (`d31e544`) | Rejected: RC5, RC6 |
| r3 | `bd410d38…4a8d` (`15f196c`) | Rejected: RC7 |
| r4 | `454066f8…9a43` (`f70789e`) | **Accepted by Lightagent** (`ACCEPTANCE.md`) |
