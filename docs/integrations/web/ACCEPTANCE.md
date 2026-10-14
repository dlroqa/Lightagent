# LA-WEB-PROVISIONING/1: Lightagent acceptance record

Lightagent's side of the joint acceptance. Web keeps the canonical
`protocol/la-web-provisioning-1/ACCEPTANCE.md`. The protocol is **frozen**
only when Web's row and the row below name the same revision, digest and
candidate commit.

| Field | Value |
|---|---|
| Revision | `la-web-provisioning-1/r4` |
| Compatibility digest (SHA-256 of `MANIFEST.json`) | `454066f8fb75e886119c401e4a45ec15ebf2b83115165b2e1f85180abfd29a43` |
| Candidate commit (Web) | `f70789ec69c6d747ba5b7c84fe1b3f8db0cba727` |
| Metadata commit (Web) | `0cf96f3744e986cb506a767b3c350a0c6c9311a7` |
| Delivery | `git bundle` `la-web-provisioning-1-r4.bundle`, SHA-256 `ea4524c4a5197dc765a6f055bad1fabde39537f5ad713314a74ecf7da941e889`, `git bundle verify` OK |
| Pinned mirror | `docs/integrations/web/la-web-provisioning-1-r4/`, and `COMPATIBILITY_DIGEST` in `crates/lightagent-protocol-conformance/tests/fixtures.rs` |
| Evidence | `l0-protocol-review.md`, and draft PR #28 with its GitHub Actions runs |
| Date | 2026-10-14 |
| Verdict | **Accepted** |

## Binding conditions of this acceptance

1. **Ledger continuity (§10.6, P8).** Lightagent's identity idempotency
   records are pruned (`MAX_IDEMPOTENCY = 10_000`). C2 and C3 are therefore
   implemented with the durable generation marker that r4 §10.6 requires in
   that case, as specified in `l0-protocol-review.md` §3, before any
   activation.
2. **Order of steps.** The `keyId` → credential mapping is resolved at step
   15, after the nonce is durably recorded (§10.2).
3. **No change to PR #26's public behaviour.** The fence and deadline live in
   one additive `ProvisioningService` entry point. `provision` and its HTTP
   API stay unchanged.
4. **Acceptance is not activation.** No gateway is mounted, no key is
   trusted, and no harness is registered by this record. Each of those, and
   every merge and deployment, needs separate approval.

## Row for Web's `ACCEPTANCE.md`

| Side | Revision | Compatibility digest | Candidate commit | Reviewer | Date | Verdict |
|---|---|---|---|---|---|---|
| Lightagent | r4 | `454066f8fb75e886119c401e4a45ec15ebf2b83115165b2e1f85180abfd29a43` | `f70789ec69c6d747ba5b7c84fe1b3f8db0cba727` | Lightagent L0 review (PR #28) | 2026-10-14 | accepted, with the conditions in Lightagent `docs/integrations/web/ACCEPTANCE.md` |
