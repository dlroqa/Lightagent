//! Provisioning-ledger continuity (`LA-WEB-PROVISIONING/1` r4 §10.6, P8).
//!
//! A harness id has exactly one provisioning ledger. An operator stamps it
//! with the harness id by an explicit [`ProvisioningService::initialize`];
//! nothing creates a stamped ledger implicitly. The check below decides
//! whether the ledger, the identity store and the profile store still belong
//! together. A gateway serves only while it passes, and answers
//! `LEDGER_CONTINUITY_FAILED` otherwise.
//!
//! r4 states C2 and C3 over operation ids: every identity provisioning
//! record and profile marker names a ledger operation, and every bound or
//! revoked operation has its identity record. Lightagent's identity
//! provisioning records are idempotency entries, pruned beyond 10,000, so
//! the literal C3 cannot hold for the life of a harness. r4 requires a
//! durable marker in that case. It is:
//!
//! - in the ledger, the stamp ([`LedgerContinuity`]): harness id, a random
//!   continuity id, retirement, and the identity sequence and generation the
//!   ledger last saw;
//! - in the identity store,
//!   [`ProvisioningContinuity`](crate::identity::store::ProvisioningContinuity):
//!   the continuity id,
//!   a provisioning-only sequence, the ledger generation it saw, and the
//!   last operation and action.
//!
//! The identity half commits in the same identity transaction as the
//! ownership change. The ledger half commits in the same ledger save as the
//! operation's state change. A crash between the two leaves the identity
//! store exactly one transaction ahead, on the operation the ledger still
//! shows in flight. That state is recognized here, and a resend or
//! `reconcile` finishes it. Every other disagreement fails closed.
//!
//! The profile markers are checked on their own: each `.provisioning-operation`
//! token in `profiles/` must belong to a ledger operation. The sequence is
//! one piece of evidence, never the only one.
//!
//! [`ProvisioningService::initialize`]: super::ProvisioningService::initialize

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Ledger, Operation, OperationState, valid_operation_id};
use crate::identity::store::{ContinuityAction, IdentityDocument};

/// The ledger's harness stamp. Every field is required: a ledger whose stamp
/// lost a field does not parse, and the service treats it as corrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerContinuity {
    /// The harness id (`hns_…`) this ledger belongs to.
    pub harness_id: String,
    /// 128 random bits, fixed at initialization. A ledger initialized again,
    /// even for the same harness id, has another.
    pub continuity_id: String,
    pub initialized_unix: u64,
    /// Retirement is permanent: nothing clears it.
    pub retired: bool,
    /// When it was retired, or `0`.
    pub retired_unix: u64,
    /// The identity store's provisioning sequence as of this ledger's last
    /// bind or revoke.
    pub identity_sequence_seen: u64,
    /// The identity document generation that bind or revoke committed as.
    pub identity_generation_seen: u64,
}

/// Why the ledger, the identity store and the profile store do not belong
/// together. The codes are r4's (`ledger-continuity.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContinuityFailure {
    /// C1: there is no ledger file.
    #[error("no provisioning ledger")]
    NoLedger,
    /// C1: the ledger was never initialized for a harness.
    #[error("the provisioning ledger is not stamped with a harness id")]
    UnstampedLedger,
    /// C1: the ledger belongs to another harness id.
    #[error("the provisioning ledger belongs to another harness id")]
    HarnessIdMismatch,
    /// C1: the operator retired this harness id.
    #[error("this harness id is retired")]
    Retired,
    /// C2: the identity or profile store records provisioning the ledger
    /// lacks (a fresh, replaced or older ledger).
    #[error("the provisioning ledger is behind the identity or profile store")]
    LedgerBehindStores,
    /// C3: the ledger records provisioning the identity store lacks (an
    /// older, replaced or new identity store).
    #[error("the identity store is behind the provisioning ledger")]
    StoresBehindLedger,
}

impl ContinuityFailure {
    /// The r4 reason code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoLedger => "NO_LEDGER",
            Self::UnstampedLedger => "UNSTAMPED_LEDGER",
            Self::HarnessIdMismatch => "HARNESS_ID_MISMATCH",
            Self::Retired => "RETIRED",
            Self::LedgerBehindStores => "LEDGER_BEHIND_STORES",
            Self::StoresBehindLedger => "STORES_BEHIND_LEDGER",
        }
    }
}

/// `^hns_[a-z0-9]{8,60}$` (r4 §3, `audience`).
pub fn valid_harness_id(id: &str) -> bool {
    id.strip_prefix("hns_").is_some_and(|rest| {
        (8..=60).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    })
}

impl Ledger {
    /// The harness stamp, if an operator initialized this ledger.
    pub fn continuity(&self) -> Option<&LedgerContinuity> {
        self.continuity.as_ref()
    }

    /// C1 on a loaded ledger: stamped with `harness_id` and not retired. A
    /// gateway re-checks this under the ledger lock for every command. A
    /// missing ledger loads as an empty one, so it is `UnstampedLedger` here,
    /// and so is a stamp whose harness id or continuity id is malformed. A
    /// recorded retirement time counts as retired, whatever `retired` says.
    pub fn check_stamp(&self, harness_id: &str) -> Result<&LedgerContinuity, ContinuityFailure> {
        let stamp = self
            .continuity
            .as_ref()
            .filter(|stamp| {
                valid_harness_id(&stamp.harness_id) && valid_operation_id(&stamp.continuity_id)
            })
            .ok_or(ContinuityFailure::UnstampedLedger)?;
        if stamp.harness_id != harness_id {
            return Err(ContinuityFailure::HarnessIdMismatch);
        }
        if stamp.retired || stamp.retired_unix != 0 {
            return Err(ContinuityFailure::Retired);
        }
        Ok(stamp)
    }
}

/// C1–C3 over what the stores hold now. `markers` are the tokens of every
/// `.provisioning-operation` file under `profiles/`. Reads nothing itself.
pub(crate) fn evaluate(
    harness_id: &str,
    ledger: &Ledger,
    identity: &IdentityDocument,
    markers: &BTreeSet<String>,
) -> Result<(), ContinuityFailure> {
    use ContinuityFailure::{LedgerBehindStores, StoresBehindLedger};

    // C1.
    let stamp = ledger.check_stamp(harness_id)?;
    let seen = stamp.identity_sequence_seen;
    let marker = identity.provisioning_continuity();

    // C2: nothing outside the ledger records provisioning it lacks.
    if let Some(marker) = marker {
        if marker.continuity_id != stamp.continuity_id
            || marker.ledger_generation_seen > ledger.generation
        {
            return Err(LedgerBehindStores);
        }
        let last = ledger.operations.get(&marker.last_operation);
        let consistent = match marker.sequence.checked_sub(seen) {
            // Settled: the ledger saved the state this transaction produced.
            Some(0) => {
                last.map(|op| op.state)
                    == Some(match marker.last_action {
                        ContinuityAction::Bind => OperationState::Completed,
                        ContinuityAction::Revoke => OperationState::Revoked,
                    })
            }
            // In flight: the identity transaction committed and the process
            // stopped before the ledger saved. The ledger still records this
            // same operation in the state before that transaction, and the
            // identity store holds that transaction's effect. A resend or
            // reconcile ends it.
            Some(1) => last.is_some_and(|op| in_flight(op, marker.last_action, identity)),
            // More than one transaction the ledger never saw.
            Some(_) => false,
            // The identity store is behind: C3 below.
            None => true,
        };
        if !consistent {
            return Err(LedgerBehindStores);
        }
    }
    let tokens: BTreeSet<&str> = ledger
        .operations
        .values()
        .map(|op| op.marker.as_str())
        .collect();
    if markers.iter().any(|token| !tokens.contains(token.as_str())) {
        return Err(LedgerBehindStores);
    }

    // C3: the identity store holds every bind and revoke the ledger saw.
    // An absent marker is never a clean state once the ledger has bound.
    let bound_or_revoked = ledger.operations.values().any(|op| {
        matches!(
            op.state,
            OperationState::Completed | OperationState::Revoked
        )
    });
    // Principals are never deleted, so every principal a bind created or
    // resolved is still there, whatever was pruned or reassigned since.
    if ledger
        .operations
        .values()
        .filter(|op| {
            matches!(
                op.state,
                OperationState::Completed | OperationState::Revoked
            )
        })
        .any(|op| identity.principal(&op.principal).is_none())
    {
        return Err(StoresBehindLedger);
    }
    match marker {
        None if seen > 0 || bound_or_revoked => return Err(StoresBehindLedger),
        Some(marker) if marker.sequence < seen => return Err(StoresBehindLedger),
        _ => {}
    }
    if identity.generation() < stamp.identity_generation_seen {
        return Err(StoresBehindLedger);
    }
    if bound_or_revoked && seen == 0 {
        return Err(StoresBehindLedger);
    }
    Ok(())
}

/// Whether `op` is exactly where an interrupted `action` leaves it: the
/// ledger shows the state before the transaction, and the identity store
/// shows its effect. A bind created (or resolved) the principal and made it
/// the owner. A revoke removed that ownership.
fn in_flight(op: &Operation, action: ContinuityAction, identity: &IdentityDocument) -> bool {
    let owned = identity
        .recorded_ownership()
        .get(&op.profile)
        .is_some_and(|record| record.owner == op.principal);
    match action {
        ContinuityAction::Bind => {
            op.state == OperationState::ProfileCreated
                && identity.principal(&op.principal).is_some()
                && owned
        }
        ContinuityAction::Revoke => op.state == OperationState::Completed && !owned,
    }
}
