# Provisioning-ledger continuity (L1)

Status: **implemented in `lightagent_core::provisioning`, not activated.**
Nothing serves `LA-WEB-PROVISIONING/1`. No gateway, route, key, harness
registration or startup path uses this code.

This is Lightagent's binding L1 condition for the frozen protocol
`LA-WEB-PROVISIONING/1` r4 (compatibility digest
`454066f8fb75e886119c401e4a45ec15ebf2b83115165b2e1f85180abfd29a43`,
mirrored in `docs/integrations/web/la-web-provisioning-1-r4/`):

- r4 §10.6 "Ledger continuity and harness retirement";
- property P8 (§10.4);
- Lightagent's `docs/integrations/web/ACCEPTANCE.md`, condition 1;
- the design in `docs/integrations/web/l0-protocol-review.md` §3.

It changes nothing on the wire. It adds no request field, response field,
error code or fixture.

## 1. The problem

A harness id has exactly one provisioning ledger (§13.2 A1). Three stores
take part in provisioning, and none of them is a transaction with the
others:

- the ledger (`<home>/provisioning/state.json`);
- the identity store (`<home>/identity/state.json`);
- the profile store (`<home>/profiles/`).

If the ledger is lost, replaced or restored older while the identity and
profile stores keep their bindings, the ledger forgets idempotency keys. A
resend of a create that already bound its user then gets a merits refusal,
and Web fails a profile the user owns (RC7). If the identity store goes back
instead, the ledger reports bindings that no longer exist.

r4 requires the harness to detect both cases and refuse to serve. Its C2 and
C3 are stated over operation ids. Lightagent's identity provisioning records
are idempotency entries, pruned beyond 10,000 (`MAX_IDEMPOTENCY`), so a
literal C3 cannot hold for the life of a harness. r4 §10.6 says that in this
case L1 MUST add a durable marker. This document describes that marker.

## 2. Schema

Both additions are optional at the top level. They are written only once an
operator initializes the ledger, and they are not serialized while absent.
A ledger or identity document nobody initialized keeps its exact bytes, and
older documents parse unchanged. Neither store's `schema` number changes.

Inside each addition, **every field is required**. A stamp or marker missing
any field does not parse, so the store fails closed. It is never read as
absent.

### Ledger: `continuity` (`LedgerContinuity`)

| Field | Meaning |
|---|---|
| `harness_id` | The `hns_…` id the ledger belongs to (`^hns_[a-z0-9]{8,60}$`) |
| `continuity_id` | 128 random bits, fixed at initialization. Initializing again, even for the same harness id, produces another value |
| `initialized_unix` | When it was initialized |
| `retired`, `retired_unix` | Permanent retirement, or `false` and `0` |
| `identity_sequence_seen` | The identity store's provisioning sequence after this ledger's last bind or revoke |
| `identity_generation_seen` | The identity document generation that bind or revoke committed as |

### Identity store: `provisioning_continuity` (`ProvisioningContinuity`)

| Field | Meaning |
|---|---|
| `continuity_id` | The ledger's continuity id |
| `sequence` | Provisioning bind and revoke transactions applied. **Nothing else advances it**: unlike `generation`, ordinary operator transactions leave it alone |
| `ledger_generation_seen` | The ledger generation when that transaction ran |
| `last_operation`, `last_action` | The operation, and `bind` or `revoke` |

The profile store is unchanged. Each provisioned profile already carries
`.provisioning-operation`, holding its operation's random marker token. That
file survives completion, revocation, operator reassignment and crashes.

## 3. Atomicity of the sequence

| Store | Written with | One durable write |
|---|---|---|
| Identity: `provisioning_continuity` | the bind transaction (principal, owner, binding) or the revoke transaction (ownership removal) | yes: `IdentityStore::transact` replaces one document atomically (temporary file, `fsync`, rename, directory `fsync`) |
| Ledger: `identity_sequence_seen`, `identity_generation_seen` | the operation's state change (`completed` or `revoked`) | yes: one `save` of the ledger |

The identity half is advanced inside the transaction closure. That happens
only after the closure checks that the stored marker follows from the
ledger's view:

- it is absent, before the ledger's first bind or revoke; or
- it carries this continuity id, at exactly the sequence the ledger saw; or
- it is one ahead **for this same operation and action**. That is an
  interrupted call's transaction, applied again because its idempotency
  record was pruned.

Otherwise the transaction is refused with `continuity_mismatch` and writes
nothing. The operation stays retryable.

- A rolled-back or foreign identity store is never advanced, so later
  provisioning cannot hide a rollback.
- While one operation's transaction is in flight, every other bind and
  revoke is refused until that operation settles. The identity store is
  therefore **never more than one transaction ahead**, and two interrupted
  calls cannot leave a state the check would reject.
- The cost: after a crash, other operations wait, retryably, until a resend
  or `reconcile` settles the interrupted one. `reconcile` may need two
  passes. An operator can always remove access directly through the identity
  store.

The two stores are not one transaction. The only window is a crash after
the identity commit and before the ledger save. That leaves the identity
store exactly **one** transaction ahead, on an operation the ledger still
shows in flight:

| `last_action` | Ledger state of `last_operation` | Identity store must show |
|---|---|---|
| `bind` | `profile_created` | the operation's principal exists and owns the profile |
| `revoke` | `completed` | that ownership is gone |

The check accepts this state, and only for that operation in that state,
holding that effect. Every other shape fails as `LEDGER_BEHIND_STORES`: the
wrong state, the wrong action, an unknown operation, missing effects, or two
or more ahead. Each of these is tested. The recovery is the existing
one:

- a resend of the same command, or `reconcile`, resumes the operation;
- the identity transaction answers `Replayed` (or, if its idempotency record
  was pruned, applies again);
- the ledger records the store's current marker with the state change.

The ledger's two values never decrease.

## 4. The check (`check_continuity`)

`ProvisioningService::check_continuity(harness_id)` evaluates C1, C2 and C3,
in that order, and returns the first failure. It runs under the ledger lock
and reads the three stores. **It changes nothing:** a missing ledger is
reported without creating one. The tests compare every file before and
after.

| # | Condition | Failure |
|---|---|---|
| C1 | A ledger file exists | `NO_LEDGER` |
| C1 | It is stamped | `UNSTAMPED_LEDGER` |
| C1 | With this harness id | `HARNESS_ID_MISMATCH` |
| C1 | The stamp's harness id and continuity id are well formed | `UNSTAMPED_LEDGER` |
| C1 | And is not retired (`retired`, or any `retired_unix`) | `RETIRED` |
| C2 | The identity marker, if present, has this continuity id and `ledger_generation_seen ≤` the ledger's generation | `LEDGER_BEHIND_STORES` |
| C2 | Its sequence is the ledger's `identity_sequence_seen`, and `last_operation` is in the ledger, `completed` (bind) or `revoked` (revoke). Or it is exactly one ahead, with the in-flight state of §3. Never further ahead | `LEDGER_BEHIND_STORES` |
| C2 | **Every** `.provisioning-operation` token under `profiles/` belongs to a ledger operation (checked independently of the counters) | `LEDGER_BEHIND_STORES` |
| C3 | Once the ledger has bound or revoked (`identity_sequence_seen > 0`, or any operation `completed` or `revoked`), the identity marker exists | `STORES_BEHIND_LEDGER` |
| C3 | Its sequence is `≥ identity_sequence_seen` | `STORES_BEHIND_LEDGER` |
| C3 | The identity generation is `≥ identity_generation_seen` | `STORES_BEHIND_LEDGER` |
| C3 | Every `completed` or `revoked` operation's principal exists. Principals are never deleted, so this survives pruning and reassignment | `STORES_BEHIND_LEDGER` |

`Ledger::check_stamp(harness_id)` is C1 on a loaded ledger. A gateway runs it
under the ledger lock for every command (step 13). A storage or parse error
is `ContinuityError::Storage`. Either way a gateway answers
`LEDGER_CONTINUITY_FAILED` and serves nothing.

**Why the sequence and not only the generation.** The identity generation
counts every identity transaction. Suppose the identity store is restored
older, then gets ordinary operator transactions (issuing a credential,
trusting an issuer). Its generation climbs back over
`identity_generation_seen`, and a generation-only C3 passes. The
provisioning sequence does not move on those transactions, so the rollback
stays visible. The generation check is kept as the condition r4 names. The
sequence makes it strict.

The ledger generation has the same weakness: registrations raise it. So C2
also requires the identity marker's `last_operation` to be in the ledger. The
markers are checked on their own as well. The counters are never the only
evidence.

## 5. Initialization and retirement

**`initialize(actor, harness_id)`** (operator `administer`) is the only way a
ledger gets a stamp. It is refused while any trace of earlier provisioning
remains:

| Trace | Refusal |
|---|---|
| a stamp, live or retired | `already_initialized` |
| a recorded operation or idempotency key, or anything in `staging/` or `quarantine/` | `provisioning_history_exists` |
| a `.provisioning-operation` file in any profile | `profile_markers_exist` |
| the identity store's `provisioning_continuity` | `identity_continuity_exists` |
| a retained `provisioning…` identity idempotency record, or any principal mapped to an external identity (what provisioning binds) | `provisioned_identities_exist` |

A missing ledger is never a clean state on its own: these traces decide.
Provisioner registrations made before initialization are kept. A harness
whose initialization is refused is retired, and a replacement gets a new
harness id (r4 §10.6, steps 1–4).

**`retire(actor, harness_id)`** (operator `administer`, idempotent) is
permanent. Nothing clears `retired`, and `initialize` refuses a retired
ledger.

- C1 then fails, so a gateway serves nothing.
- `begin` refuses every new operation on a retired ledger
  (`harness_retired`), so no route records a new operation.
- `resume` refuses to advance an operation recorded before retirement
  (`harness_retired`). No resend or `reconcile` creates a profile or a
  binding afterwards. Such an operation stays unresolved for the operator,
  as r4 §10.6 step 3 requires.
- Nothing is deleted or reassigned. Profiles, markers, ownership, principals
  and credentials stay for the operator, as the tests verify.
- Status and revocation keep working: removing access is never blocked.

## 6. What no local check can detect

These are excluded only by procedure and by Web, never by this check:

1. **Every store rolled back together**: a VM snapshot, or a restore of the
   ledger, identity and profile stores from one recovery point. The stores
   are consistent with each other, so C1–C3 pass (tested). Any provisioning
   after that point is forgotten by all three: S holds, and Web sees
   `UNKNOWN_OPERATION` for what it knew (§13.4). The danger is an old
   **command** being accepted again. The defences are the nonce store (P7,
   L2) and the execution fence (P6, L3). The restore procedure below raises
   the fence before serving.
2. **Every store wiped**: nothing is bound, so S holds, and `initialize`
   succeeds again under the same harness id (tested). Web's registry keeps
   the retired or known id and must refuse to reactivate it (§10.6).
3. **Two live copies of one installation** (a cloned VM): excluded only by
   A1.
4. **Deliberate forgery by someone who can write the stores.** The markers
   are integrity evidence against loss and partial restore, not
   authentication. A writer who edits the identity store's marker after
   rolling it back is caught when the lost transaction created a principal
   (tested), but not when it only bound an existing one. Defending against
   a filesystem-level adversary needs a key held outside these stores (a MAC
   over the markers). That is out of scope for L1, and the stores are
   owner-only (`0600`).

### Required operator restore procedure (before any activation)

Activation (L2/L3, separately approved) MUST require this procedure, and
MUST NOT serve without it:

1. Stop the gateway. It answers `LEDGER_CONTINUITY_FAILED` until step 5.
2. Restore the ledger, nonce store, identity store and profile store
   **together**, from one mutually consistent recovery point (A9). Never
   restore one alone.
3. Run `check_continuity`. If it fails, do not repair by hand: retire the
   harness id.
4. Raise the execution fence to `max(F_restored, T + 150)`, where `T` is an
   operator-verified current time (§10.5). The fence is L3 and does not
   exist yet. Activation is blocked until it does, and until this step is
   verified.
5. Only then serve.

## 7. Compatibility and migration

- **Existing installations.** No ledger or identity document gains a field
  until an operator calls `initialize`. `provision`, `status`, `revoke`,
  `reconcile`, the HTTP contract and their codes are unchanged for every
  ledger that exists today (tested byte for byte).
- **Pre-L1 ledgers with operations** cannot be initialized
  (`provisioning_history_exists`). There is no adoption path. Nothing was
  ever activated or deployed, so no such ledger serves Web.
- **Downgrade.** An older build ignores the new fields and drops them on its
  next write, because the documents do not refuse unknown fields. Such a
  ledger fails C1 (`UNSTAMPED_LEDGER`). Such an identity store fails C3
  (`STORES_BEHIND_LEDGER`). Both fail closed, and neither is unsafe.
  Re-upgrading does not heal them: the operator retires the harness id.
- **Operator API on an initialized ledger.** It works, and it keeps the
  marker in step through the same bind and revoke transactions. On stores
  whose continuity is broken, its bind and revoke refuse in the identity
  transaction (`storage_unavailable`, retryable). Its creates on a retired
  ledger are refused with `harness_retired`.
- **Behaviour changes, limited to stamped ledgers** (approved in review).
  `provision` and `revoke` keep their signatures and codes. Inside them:
  - the bind and revoke identity transactions gain the gated continuity
    write;
  - `begin` and `resume` gain the retirement refusal;
  - `revoke` gains a test-only crash point, inert outside tests.

  All of these are inert unless `initialize` was called.

## 8. Tests

They are in `crates/lightagent-core/src/provisioning/tests/continuity.rs`.
The CI job "LA-WEB-PROVISIONING/1 fixtures (offline)" runs them with
`--nocapture`, and `check.sh` runs them on Linux, macOS and Windows.

- **The 13 frozen `ledger-continuity.json` cases**, each reproduced on real
  stores through `provision` and `revoke`, with real file losses and
  restores. The fixture file is pinned to its `MANIFEST.sha256` entry.
- **Sequence:** it advances with every bind and revoke, and not with
  operator transactions. An interrupted bind and an interrupted revoke are
  recognized and finished.
- **Adversarial cases:**
  - an identity rollback whose generation is lifted again by operator
    transactions is still detected, and a later bind refuses and cannot
    heal it;
  - a ledger rollback whose generation is lifted again by registrations is
    still detected;
  - counters forged into agreement, and a planted marker, are caught by the
    markers;
  - pruned idempotency records change nothing.
- **Missing metadata:** after a downgrade (a stamp or marker removed) the
  check fails closed. Each required field removed in turn does not parse.
- **Initialization:** refused for each trace in turn, for history, for a bad
  id and for a non-operator. Registrations are kept.
- **Retirement:** permanent, no new operation, nothing deleted, status and
  revoke still work.
- **Limits:** the consistent rollback of every store, and re-initialization
  after a complete wipe, are documented and asserted.
