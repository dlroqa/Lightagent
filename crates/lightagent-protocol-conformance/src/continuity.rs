//! r4 §10.6 C1–C3, in the abstract form `ledger-continuity.json` uses: sets
//! of operation ids in the ledger, in identity provisioning records, and in
//! profile markers.
//!
//! Lightagent's stores do not carry those sets as such. Identity
//! idempotency records are pruned at 10,000 and have no accessor, and profile
//! markers hold the operation's random `marker` token rather than its id.
//! L1 therefore implements C2 and C3 with a generation marker (§10.6
//! "Limitation", the review's §3). This module checks only that the
//! abstract rule gives the fixture verdicts.

use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct LedgerState {
    pub harness_id: Option<String>,
    pub retired: bool,
    /// `(operation id, state)`.
    pub operations: Vec<(String, String)>,
}

/// The activation verdict: serve, or the first failing reason.
pub fn check(
    configured_harness_id: &str,
    ledger: Option<&LedgerState>,
    identity_records: &[String],
    profile_markers: &[String],
) -> Result<(), &'static str> {
    // C1.
    let ledger = ledger.ok_or("NO_LEDGER")?;
    match ledger.harness_id.as_deref() {
        None => return Err("UNSTAMPED_LEDGER"),
        Some(id) if id != configured_harness_id => return Err("HARNESS_ID_MISMATCH"),
        Some(_) => {}
    }
    if ledger.retired {
        return Err("RETIRED");
    }
    let known: BTreeSet<&str> = ledger
        .operations
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    // C2: nothing outside the ledger names an operation it lacks.
    if identity_records
        .iter()
        .chain(profile_markers)
        .any(|op| !known.contains(op.as_str()))
    {
        return Err("LEDGER_BEHIND_STORES");
    }
    // C3: every bound or revoked operation has its identity record.
    let recorded: BTreeSet<&str> = identity_records.iter().map(String::as_str).collect();
    if ledger.operations.iter().any(|(id, state)| {
        matches!(state.as_str(), "completed" | "revoked") && !recorded.contains(id.as_str())
    }) {
        return Err("STORES_BEHIND_LEDGER");
    }
    Ok(())
}
