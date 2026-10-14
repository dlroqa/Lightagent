//! L1: provisioning-ledger continuity (`LA-WEB-PROVISIONING/1` r4 §10.6)
//! against real stores. Every case drives the real ledger, identity store
//! and profile store: operations go through `provision` and `revoke`, and
//! losses and restores replace or delete the files an operator would.

use std::collections::BTreeMap;

use sha2::Sha256;

use super::*;
use crate::ids::hex;

const HARNESS: &str = "hns_fixtureharnessalpha0000001";
const OTHER_HARNESS: &str = "hns_fixtureharnessbravo0000002";
const P1: &str = "agt_fixtureprofile000000000001";
const P2: &str = "agt_fixtureprofile000000000002";
const P3: &str = "agt_fixtureprofile000000000003";

/// `ledger-continuity.json`'s entry in the frozen r4 `MANIFEST.sha256`.
const FIXTURE_SHA256: &str = "21c04efbd8f7d2a05dc3c1391ab67102a88ee6dcc9122db7ddeec4755f6d9d65";

fn fixtures() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../docs/integrations/web/la-web-provisioning-1-r4/protocol/la-web-provisioning-1/ledger-continuity.json",
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        hex(&Sha256::digest(&bytes)),
        FIXTURE_SHA256,
        "frozen r4 bytes"
    );
    serde_json::from_slice(&bytes).unwrap()
}

fn initialized() -> World {
    let world = world();
    world.service.initialize(&operator(), HARNESS).unwrap();
    world
}

impl World {
    fn ledger_file(&self) -> PathBuf {
        self.service.root().join(STATE)
    }

    fn identity_file(&self) -> PathBuf {
        self.root().join("identity").join("state.json")
    }

    fn verdict(&self) -> Result<(), &'static str> {
        verdict_for(self, HARNESS)
    }

    fn create(&self, key: &str, profile_id: &str) -> OperationStatus {
        let status = self
            .provision(key, &web_request(profile_id))
            .unwrap()
            .status;
        assert_eq!(status.state, OperationState::Completed);
        status
    }

    fn stamp(&self) -> LedgerContinuity {
        self.service.load().unwrap().continuity().unwrap().clone()
    }

    fn marker(&self) -> Option<ProvisioningContinuity> {
        self.identity().provisioning_continuity().cloned()
    }

    /// Every file under the home, by path, for "changes nothing" checks.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![self.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        files
    }
}

fn verdict_for(world: &World, harness: &str) -> Result<(), &'static str> {
    match world.service.check_continuity(harness) {
        Ok(()) => Ok(()),
        Err(ContinuityError::Failed(failure)) => Err(failure.code()),
        Err(ContinuityError::Storage(error)) => Err(error.code()),
    }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn restore(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn edit_json(path: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value = serde_json::from_slice(&read(path)).unwrap();
    change(&mut value);
    restore(path, &serde_json::to_vec_pretty(&value).unwrap());
}

/// A completed, a revoked and a failed operation (whose profile is
/// quarantined).
fn with_three_outcomes(world: &World) {
    world.create("idk_fixturecompleted000000001", P1);
    let revoked = world.create("idk_fixturerevoked0000000001", P2);
    world
        .service
        .revoke(&world.site(), &revoked.operation_id)
        .unwrap();
    world.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        world
            .provision("idk_fixturefailed00000000001", &web_request(P3))
            .is_err()
    );
    let user = revoked.principal_id;
    operator_change(world.root(), |tx| {
        tx.set_status(&user, PrincipalStatus::Suspended).unwrap();
    });
    let failed = world
        .provision("idk_fixturefailed00000000001", &web_request(P3))
        .unwrap()
        .status;
    assert_eq!(failed.state, OperationState::Failed);
    operator_change(world.root(), |tx| {
        tx.set_status(&user, PrincipalStatus::Active).unwrap();
    });
}

/// The real-store scenario for each `ledger-continuity.json` case.
fn scenario(id: &str) -> World {
    match id {
        "initialized-empty" => initialized(),
        "consistent" => {
            let world = initialized();
            with_three_outcomes(&world);
            world
        }
        "no-ledger-no-records" => {
            let world = world();
            std::fs::remove_file(world.ledger_file()).unwrap();
            world
        }
        "fresh-ledger-retained-identity" => {
            // A ledger initialized elsewhere for this harness id, copied in
            // beside the stores of the original one.
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            let fresh = initialized();
            restore(&world.ledger_file(), &read(&fresh.ledger_file()));
            world
        }
        // PR #26 creates its ledger on the first registration.
        "auto-created-unstamped-ledger" => world(),
        "missing-ledger-retained-markers" => {
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            std::fs::remove_file(world.ledger_file()).unwrap();
            world
        }
        "harness-id-mismatch" => {
            let world = world();
            world
                .service
                .initialize(&operator(), OTHER_HARNESS)
                .unwrap();
            world
        }
        "retired" => {
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            world.service.retire(&operator(), HARNESS).unwrap();
            world
        }
        "ledger-older-than-identity" => {
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            let older = read(&world.ledger_file());
            world.create("idk_fixturecreate00000000002", P2);
            restore(&world.ledger_file(), &older);
            world
        }
        "identity-older-than-ledger" => {
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            let older = read(&world.identity_file());
            world.create("idk_fixturecreate00000000002", P2);
            restore(&world.identity_file(), &older);
            world
        }
        "empty-identity-store-with-ledger" => {
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            std::fs::remove_dir_all(world.root().join("identity")).unwrap();
            std::fs::remove_dir_all(world.profile_dir(P1)).unwrap();
            world
        }
        "profile-marker-newer-than-ledger" => {
            // Ledger and identity restored together; the profile store kept
            // the newer profile.
            let world = initialized();
            world.create("idk_fixturecreate00000000001", P1);
            let ledger = read(&world.ledger_file());
            let identity = read(&world.identity_file());
            world.create("idk_fixturecreate00000000002", P2);
            restore(&world.ledger_file(), &ledger);
            restore(&world.identity_file(), &identity);
            world
        }
        "pending-operation-without-records" => {
            let world = initialized();
            world.service.inject(Fault::PendingRecorded);
            assert!(
                world
                    .provision("idk_fixturecreate00000000001", &web_request(P1))
                    .is_err()
            );
            world.service.inject(Fault::ProfileCreatedRecorded);
            assert!(
                world
                    .provision("idk_fixturecreate00000000002", &web_request(P2))
                    .is_err()
            );
            assert_eq!(operation_for(&world, P1).state, OperationState::Pending);
            assert_eq!(
                operation_for(&world, P2).state,
                OperationState::ProfileCreated
            );
            world
        }
        other => panic!("no real-store scenario for fixture {other}"),
    }
}

// All 13 frozen r4 cases, reproduced on real stores: the real check gives
// each fixture's verdict, and looking changes no file.
#[test]
fn continuity_reproduces_every_frozen_fixture_on_real_stores() {
    let fixtures = fixtures();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 13);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        assert_eq!(case["input"]["configuredHarnessId"], HARNESS, "{id}");
        let expected = if case["expect"]["serve"].as_bool().unwrap() {
            Ok(())
        } else {
            Err(case["expect"]["reason"].as_str().unwrap())
        };
        let world = scenario(id);
        let before = world.files();
        let verdict = world.verdict();
        assert_eq!(world.files(), before, "{id}: the check changed a file");
        println!("{id}: real stores {verdict:?}, fixture {expected:?}");
        assert_eq!(verdict, expected, "{id}");
    }
}

// --- the sequence -------------------------------------------------------------

// The marker commits with the ownership change, and the ledger records it
// with the state change: after every settled call they agree.
#[test]
fn continuity_sequence_advances_with_each_bind_and_revoke() {
    let world = initialized();
    assert_eq!(world.marker(), None);
    let first = world.create("idk_fixturecreate00000000001", P1);
    let marker = world.marker().unwrap();
    assert_eq!(marker.sequence, 1);
    assert_eq!(marker.last_operation, first.operation_id);
    assert_eq!(marker.last_action, ContinuityAction::Bind);
    assert_eq!(marker.continuity_id, world.stamp().continuity_id);
    assert_eq!(world.stamp().identity_sequence_seen, 1);
    assert_eq!(
        world.stamp().identity_generation_seen,
        world.identity().generation()
    );

    world
        .service
        .revoke(&world.site(), &first.operation_id)
        .unwrap();
    let marker = world.marker().unwrap();
    assert_eq!(
        (marker.sequence, marker.last_action),
        (2, ContinuityAction::Revoke)
    );
    assert_eq!(world.stamp().identity_sequence_seen, 2);
    // A replayed revoke changes nothing.
    world
        .service
        .revoke(&world.site(), &first.operation_id)
        .unwrap();
    assert_eq!(world.marker().unwrap().sequence, 2);

    // Ordinary operator transactions raise the generation, never the
    // sequence.
    let generation = world.identity().generation();
    operator_change(world.root(), |tx| {
        tx.trust_issuer("https://unrelated.control.example")
            .unwrap();
    });
    assert_eq!(world.identity().generation(), generation + 1);
    assert_eq!(world.marker().unwrap().sequence, 2);
    assert_eq!(world.verdict(), Ok(()));
}

// A crash between the identity commit and the ledger save leaves the
// identity store one transaction ahead, on the operation the ledger shows
// in flight. That state is served, and the resend settles it.
#[test]
fn continuity_recognizes_and_finishes_an_interrupted_bind_or_revoke() {
    let world = initialized();
    world.service.inject(Fault::IdentityCommitted);
    assert!(
        world
            .provision("idk_fixturecreate00000000001", &web_request(P1))
            .is_err()
    );
    assert_eq!(world.marker().unwrap().sequence, 1);
    assert_eq!(world.stamp().identity_sequence_seen, 0);
    assert_eq!(world.verdict(), Ok(()));
    let done = world.create("idk_fixturecreate00000000001", P1);
    assert_eq!(world.stamp().identity_sequence_seen, 1);
    assert_eq!(world.verdict(), Ok(()));

    // The same for a revoke: the ledger as it was before the revoke's save.
    let before = read(&world.ledger_file());
    world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    restore(&world.ledger_file(), &before);
    assert_eq!(world.marker().unwrap().sequence, 2);
    assert_eq!(world.verdict(), Ok(()));
    let revoked = world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(revoked.state, OperationState::Revoked);
    assert_eq!(world.stamp().identity_sequence_seen, 2);
    assert_eq!(world.verdict(), Ok(()));
}

// Requirement 4: an older identity store, then ordinary operator
// transactions that lift its generation back over what the ledger saw. The
// generation alone would pass. The provisioning sequence still shows the
// rollback, and a later bind cannot hide it.
#[test]
fn continuity_detects_an_identity_rollback_after_operator_transactions() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let older = read(&world.identity_file());
    world.create("idk_fixturecreate00000000002", P2);
    restore(&world.identity_file(), &older);
    let seen = world.stamp().identity_generation_seen;
    let mut issuer = 0;
    while world.identity().generation() <= seen {
        issuer += 1;
        operator_change(world.root(), |tx| {
            tx.trust_issuer(&format!("https://issuer{issuer}.control.example"))
                .unwrap();
        });
    }
    assert!(issuer > 0);
    assert!(
        world.identity().generation() > seen,
        "generation alone passes"
    );
    assert_eq!(world.verdict(), Err("STORES_BEHIND_LEDGER"));

    // Provisioning onto the rolled-back store refuses in the identity
    // transaction and writes nothing there: the marker never catches up.
    let marker = world.marker().unwrap();
    assert_eq!(
        world
            .provision("idk_fixturecreate00000000003", &web_request(P3))
            .unwrap_err(),
        ProvisioningError::Unavailable
    );
    assert_eq!(world.marker().unwrap(), marker);
    assert!(world.effective().get(&profile(P3)).is_none());
    assert_eq!(world.verdict(), Err("STORES_BEHIND_LEDGER"));
}

// The mirror image: an older ledger whose generation is lifted again by
// registrations, with the newer profile deleted so no marker is left. The
// identity marker names an operation the ledger lacks.
#[test]
fn continuity_detects_a_ledger_rollback_after_registrations() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let older = read(&world.ledger_file());
    world.create("idk_fixturecreate00000000002", P2);
    restore(&world.ledger_file(), &older);
    std::fs::remove_dir_all(world.profile_dir(P2)).unwrap();
    let seen = world.marker().unwrap().ledger_generation_seen;
    while world.service.load().unwrap().generation() <= seen {
        world
            .service
            .register_provisioner(
                &operator(),
                &principal("control-site"),
                [ISSUER.to_owned()].into(),
                ["assistant".to_owned()].into(),
                10,
            )
            .unwrap();
    }
    assert_eq!(world.verdict(), Err("LEDGER_BEHIND_STORES"));
}

// Requirement 3: the markers are checked independently. Counters forged
// into agreement do not hide a profile the ledger never created.
#[test]
fn continuity_profile_markers_are_checked_on_their_own() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let ledger = read(&world.ledger_file());
    let identity = read(&world.identity_file());
    world.create("idk_fixturecreate00000000002", P2);
    restore(&world.ledger_file(), &ledger);
    restore(&world.identity_file(), &identity);
    assert_eq!(
        world.stamp().identity_sequence_seen,
        world.marker().unwrap().sequence,
        "the counters agree"
    );
    assert_eq!(world.verdict(), Err("LEDGER_BEHIND_STORES"));

    // A marker file planted in a profile that was never provisioned.
    let other = initialized();
    other.create("idk_fixturecreate00000000001", P1);
    ProfileStore::new(other.root())
        .create(&AgentProfile::new(
            profile(P2),
            "Planted",
            "",
            String::new(),
        ))
        .unwrap();
    std::fs::write(
        other.profile_dir(P2).join(MARKER_FILE),
        "00000000000000000000000000000000",
    )
    .unwrap();
    assert_eq!(other.verdict(), Err("LEDGER_BEHIND_STORES"));
}

// Identity idempotency records are pruned beyond 10,000. The check never
// relies on them, and an interrupted bind still completes after pruning.
#[test]
fn continuity_survives_pruned_identity_records() {
    let world = initialized();
    let first = world.create("idk_fixturecreate00000000001", P1);
    world
        .service
        .revoke(&world.site(), &first.operation_id)
        .unwrap();
    world.service.inject(Fault::IdentityCommitted);
    assert!(
        world
            .provision("idk_fixturecreate00000000002", &web_request(P2))
            .is_err()
    );
    edit_json(&world.identity_file(), |doc| {
        doc["idempotency"] = serde_json::json!({});
    });
    assert!(
        !world
            .identity()
            .has_idempotency_key_with_prefix("provisioning")
    );
    assert_eq!(world.verdict(), Ok(()));
    // Pruned, the bind runs again and advances the marker once more.
    world.create("idk_fixturecreate00000000002", P2);
    assert_eq!(world.marker().unwrap().sequence, 4);
    assert_eq!(world.stamp().identity_sequence_seen, 4);
    assert_eq!(world.verdict(), Ok(()));
}

// Requirements 1 and 5: missing continuity metadata after a downgrade, or a
// partial edit, fails closed. It never reads as a clean installation.
#[test]
fn continuity_missing_metadata_fails_closed() {
    // An older build rewrote the ledger without the stamp.
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    edit_json(&world.ledger_file(), |doc| {
        doc.as_object_mut().unwrap().remove("continuity");
    });
    assert_eq!(world.verdict(), Err("UNSTAMPED_LEDGER"));

    // An older build rewrote the identity store without the marker.
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    edit_json(&world.identity_file(), |doc| {
        doc.as_object_mut()
            .unwrap()
            .remove("provisioning_continuity");
    });
    assert_eq!(world.verdict(), Err("STORES_BEHIND_LEDGER"));

    // A stamp or marker missing one field does not parse at all.
    for field in [
        "harness_id",
        "continuity_id",
        "retired",
        "retired_unix",
        "identity_sequence_seen",
        "identity_generation_seen",
    ] {
        let world = initialized();
        world.create("idk_fixturecreate00000000001", P1);
        edit_json(&world.ledger_file(), |doc| {
            doc["continuity"].as_object_mut().unwrap().remove(field);
        });
        assert_eq!(world.verdict(), Err("storage_corrupt"), "{field}");
    }
    for field in [
        "continuity_id",
        "sequence",
        "ledger_generation_seen",
        "last_operation",
        "last_action",
    ] {
        let world = initialized();
        world.create("idk_fixturecreate00000000001", P1);
        edit_json(&world.identity_file(), |doc| {
            doc["provisioning_continuity"]
                .as_object_mut()
                .unwrap()
                .remove(field);
        });
        assert_eq!(world.verdict(), Err("storage_unavailable"), "{field}");
    }

    // A revoked-only history with the ledger's counters reset.
    let world = initialized();
    let done = world.create("idk_fixturecreate00000000001", P1);
    world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    edit_json(&world.ledger_file(), |doc| {
        doc["continuity"]["identity_sequence_seen"] = 0.into();
    });
    assert_eq!(world.verdict(), Err("LEDGER_BEHIND_STORES"));
    edit_json(&world.identity_file(), |doc| {
        doc.as_object_mut()
            .unwrap()
            .remove("provisioning_continuity");
    });
    assert_eq!(world.verdict(), Err("STORES_BEHIND_LEDGER"));
}

// Upgrade: a ledger and identity store nobody initialized keep their exact
// shape, and this build's provisioning on them writes no continuity field.
#[test]
fn continuity_leaves_an_uninitialized_installation_unchanged() {
    let world = world();
    let done = world.create("idk_fixturecreate00000000001", P1);
    world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    let ledger = String::from_utf8(read(&world.ledger_file())).unwrap();
    let identity = String::from_utf8(read(&world.identity_file())).unwrap();
    assert!(!ledger.contains("continuity"), "{ledger}");
    assert!(!identity.contains("continuity"), "{identity}");
    assert_eq!(world.verdict(), Err("UNSTAMPED_LEDGER"));
    // A stray identity marker does not affect an uninitialized ledger.
    edit_json(&world.identity_file(), |doc| {
        doc["provisioning_continuity"] = serde_json::json!({
            "continuity_id": "0".repeat(32),
            "sequence": 7,
            "ledger_generation_seen": 99,
            "last_operation": "0".repeat(32),
            "last_action": "bind",
        });
    });
    world.create("idk_fixturecreate00000000002", P2);
    assert_eq!(world.marker().unwrap().sequence, 7);
}

// --- initialize ------------------------------------------------------------------

// Requirement 6: no new trusted ledger while any trace of earlier
// provisioning remains, after the loss of the ledger, the markers or the
// identity marker, one at a time.
#[test]
fn continuity_initialize_refuses_while_any_trace_remains() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let init = || world.service.initialize(&operator(), HARNESS).unwrap_err();
    assert_eq!(init(), ProvisioningError::Conflict("already_initialized"));

    std::fs::remove_file(world.ledger_file()).unwrap();
    assert_eq!(init(), ProvisioningError::Conflict("profile_markers_exist"));
    std::fs::remove_file(world.profile_dir(P1).join(MARKER_FILE)).unwrap();
    assert_eq!(
        init(),
        ProvisioningError::Conflict("identity_continuity_exists")
    );
    edit_json(&world.identity_file(), |doc| {
        doc.as_object_mut()
            .unwrap()
            .remove("provisioning_continuity");
    });
    assert_eq!(
        init(),
        ProvisioningError::Conflict("provisioned_identities_exist")
    );
    // Even with the idempotency records pruned, the provisioned principal
    // (mapped to its external identity) remains a trace.
    edit_json(&world.identity_file(), |doc| {
        doc["idempotency"] = serde_json::json!({});
    });
    assert_eq!(
        init(),
        ProvisioningError::Conflict("provisioned_identities_exist")
    );
    assert!(
        !world.ledger_file().exists(),
        "a refused init creates nothing"
    );
}

#[test]
fn continuity_initialize_refuses_history_ids_and_non_operators() {
    // An uninitialized ledger that already recorded operations.
    let legacy = world();
    legacy.create("idk_fixturecreate00000000001", P1);
    assert_eq!(
        legacy.service.initialize(&operator(), HARNESS).unwrap_err(),
        ProvisioningError::Conflict("provisioning_history_exists")
    );

    // Quarantined profiles of failed operations are history too.
    let quarantined = world();
    std::fs::create_dir_all(quarantined.service.root().join(QUARANTINE).join("old")).unwrap();
    assert_eq!(
        quarantined
            .service
            .initialize(&operator(), HARNESS)
            .unwrap_err(),
        ProvisioningError::Conflict("provisioning_history_exists")
    );

    let fresh = world();
    for bad in [
        "",
        "hns_",
        "hns_short",
        "HNS_fixtureharnessalpha0000001",
        "hns_UPPERCASE00000",
    ] {
        assert_eq!(
            fresh.service.initialize(&operator(), bad).unwrap_err(),
            ProvisioningError::Invalid("invalid_harness_id"),
            "{bad}"
        );
    }
    assert_eq!(
        fresh
            .service
            .initialize(&fresh.site(), HARNESS)
            .unwrap_err(),
        ProvisioningError::Denied("operator_required")
    );
    // Registrations made before initialization are kept.
    let stamp = fresh.service.initialize(&operator(), HARNESS).unwrap();
    assert_eq!(stamp.harness_id, HARNESS);
    assert_eq!(stamp.continuity_id.len(), 32);
    assert!(
        fresh
            .service
            .load()
            .unwrap()
            .provisioner(&principal("control-site"))
            .is_some()
    );
    assert_eq!(fresh.verdict(), Ok(()));
    // A second initialization elsewhere gets another continuity id.
    assert_ne!(initialized().stamp().continuity_id, stamp.continuity_id);
}

// --- retire ----------------------------------------------------------------------

// Requirement 7: retirement is permanent, stops every new operation, and
// deletes nothing.
#[test]
fn continuity_retire_is_permanent_and_deletes_nothing() {
    let world = initialized();
    let done = world.create("idk_fixturecreate00000000001", P1);
    assert_eq!(
        world
            .service
            .retire(&operator(), OTHER_HARNESS)
            .unwrap_err(),
        ProvisioningError::Invalid("harness_id_mismatch")
    );
    assert_eq!(
        world.service.retire(&world.site(), HARNESS).unwrap_err(),
        ProvisioningError::Denied("operator_required")
    );
    let profile_dir = world.profile_dir(P1);
    let profile_files: Vec<_> = std::fs::read_dir(&profile_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    let principals: Vec<_> = world.identity().principals().cloned().collect();
    let credentials = format!("{:?}", world.identity().credentials());
    let ownership = world.effective();

    world.service.retire(&operator(), HARNESS).unwrap();
    world.service.retire(&operator(), HARNESS).unwrap();
    let stamp = world.stamp();
    assert!(stamp.retired);
    assert_eq!(stamp.retired_unix, T0);
    assert_eq!(world.verdict(), Err("RETIRED"));

    // No new operation, by the operator API or any other route.
    let operations = world.service.load().unwrap().operations().count();
    assert_eq!(
        world
            .provision("idk_fixturecreate00000000002", &web_request(P2))
            .unwrap_err(),
        ProvisioningError::Denied("harness_retired")
    );
    assert_eq!(
        world.service.load().unwrap().operations().count(),
        operations
    );
    assert!(!world.profile_dir(P2).exists());
    assert_eq!(
        world.service.initialize(&operator(), HARNESS).unwrap_err(),
        ProvisioningError::Conflict("already_initialized")
    );

    // Nothing was deleted or reassigned.
    for path in &profile_files {
        assert!(path.exists(), "{}", path.display());
    }
    assert_eq!(
        world.identity().principals().cloned().collect::<Vec<_>>(),
        principals
    );
    assert_eq!(format!("{:?}", world.identity().credentials()), credentials);
    assert_eq!(world.effective(), ownership);

    // The recorded outcome stays readable, and access can still be removed.
    let status = world
        .service
        .status(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(status.state, OperationState::Completed);
    let revoked = world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(revoked.state, OperationState::Revoked);
    assert!(world.stamp().retired);
    assert_eq!(world.verdict(), Err("RETIRED"));

    // Retiring nothing creates nothing.
    let empty =
        Home(std::env::temp_dir().join(format!("lightagent-provision-{}", RunId::new().as_str())));
    std::fs::create_dir_all(&empty.0).unwrap();
    let service = service(&empty.0);
    assert_eq!(
        service.retire(&operator(), HARNESS).unwrap_err(),
        ProvisioningError::Invalid("not_initialized")
    );
    assert!(!service.root().join(STATE).exists());
}

// --- limits ----------------------------------------------------------------------

// What a local check cannot detect (documented in
// `docs/architecture/provisioning-ledger-continuity.md`): every store rolled
// back together is consistent, and a home wiped completely can be
// initialized again under the same harness id. Both are excluded only by
// the operator-controlled restore procedure, the fence raise (L3) and Web's
// registry.
#[test]
fn continuity_cannot_detect_a_consistent_rollback_of_every_store() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let ledger = read(&world.ledger_file());
    let identity = read(&world.identity_file());
    world.create("idk_fixturecreate00000000002", P2);
    restore(&world.ledger_file(), &ledger);
    restore(&world.identity_file(), &identity);
    std::fs::remove_dir_all(world.profile_dir(P2)).unwrap();
    assert_eq!(world.verdict(), Ok(()));

    let wiped = initialized();
    wiped.create("idk_fixturecreate00000000001", P1);
    for dir in ["provisioning", "identity", "profiles"] {
        std::fs::remove_dir_all(wiped.root().join(dir)).unwrap();
    }
    wiped.service.initialize(&operator(), HARNESS).unwrap();
    assert_eq!(wiped.verdict(), Ok(()));
}

// A missing ledger is reported without creating one.
#[test]
fn continuity_check_of_a_missing_ledger_creates_nothing() {
    let home =
        Home(std::env::temp_dir().join(format!("lightagent-provision-{}", RunId::new().as_str())));
    std::fs::create_dir_all(&home.0).unwrap();
    let service = service(&home.0);
    assert_eq!(
        service.check_continuity(HARNESS),
        Err(ContinuityError::Failed(ContinuityFailure::NoLedger))
    );
    assert!(!service.root().exists());
}

#[test]
fn continuity_harness_ids_follow_the_protocol_pattern() {
    assert!(valid_harness_id(HARNESS));
    assert!(valid_harness_id(&format!("hns_{}", "a".repeat(60))));
    assert!(!valid_harness_id(&format!("hns_{}", "a".repeat(61))));
    assert!(!valid_harness_id("hns_abcdefg"));
    assert!(valid_harness_id("hns_abcdefgh"));
    assert!(!valid_harness_id("hns_abcdefg-"));
}

// --- final review (PR #29) ---------------------------------------------------

/// Replace the identity store's marker with `marker`.
fn forge_marker(world: &World, marker: serde_json::Value) {
    edit_json(&world.identity_file(), |doc| {
        doc["provisioning_continuity"] = marker;
    });
}

fn marker_json(world: &World, sequence: u64, operation: &str, action: &str) -> serde_json::Value {
    serde_json::json!({
        "continuity_id": world.stamp().continuity_id,
        "sequence": sequence,
        "ledger_generation_seen": world.service.load().unwrap().generation(),
        "last_operation": operation,
        "last_action": action,
    })
}

// Review 1: one transaction ahead is accepted only for the same operation,
// recorded in the state before that transition, with the identity store
// holding its effect. Every other shape is LEDGER_BEHIND_STORES.
#[test]
fn continuity_one_ahead_is_accepted_only_for_the_operation_in_flight() {
    // The genuine states, from real interruptions.
    let world = initialized();
    world.service.inject(Fault::IdentityCommitted);
    assert!(
        world
            .provision("idk_fixturecreate00000000001", &web_request(P1))
            .is_err()
    );
    assert_eq!(world.verdict(), Ok(()));
    let done = world.create("idk_fixturecreate00000000001", P1);
    world.service.inject(Fault::RevokeIdentityCommitted);
    assert!(
        world
            .service
            .revoke(&world.site(), &done.operation_id)
            .is_err()
    );
    assert_eq!(operation_for(&world, P1).state, OperationState::Completed);
    assert_eq!(world.marker().unwrap().sequence, 2);
    assert_eq!(world.stamp().identity_sequence_seen, 1);
    assert_eq!(world.verdict(), Ok(()));

    // Forged shapes on top of real stores.
    let completed = initialized();
    let a = completed.create("idk_fixturecreate00000000001", P1);
    completed.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        completed
            .provision("idk_fixturecreate00000000002", &web_request(P2))
            .is_err()
    );
    completed.service.inject(Fault::PendingRecorded);
    assert!(
        completed
            .provision("idk_fixturecreate00000000003", &web_request(P3))
            .is_err()
    );
    let b = operation_for(&completed, P2).id; // profile_created, not bound
    let c = operation_for(&completed, P3).id; // pending
    let seen = completed.stamp().identity_sequence_seen;
    assert_eq!(completed.verdict(), Ok(()));
    let original = read(&completed.identity_file());
    for (sequence, operation, action, why) in [
        (
            seen + 1,
            a.operation_id.as_str(),
            "bind",
            "bind ahead of a completed operation",
        ),
        (
            seen + 1,
            a.operation_id.as_str(),
            "revoke",
            "revoke ahead, ownership still held",
        ),
        (
            seen + 1,
            b.as_str(),
            "bind",
            "bind ahead, but the store holds no binding",
        ),
        (
            seen + 1,
            c.as_str(),
            "bind",
            "bind ahead of a pending operation",
        ),
        (
            seen + 1,
            "0123456789abcdef0123456789abcdef",
            "bind",
            "unknown operation",
        ),
        (seen + 2, a.operation_id.as_str(), "bind", "two ahead"),
        (
            seen,
            b.as_str(),
            "bind",
            "settled on an operation in flight",
        ),
        (
            seen,
            a.operation_id.as_str(),
            "revoke",
            "settled revoke of a completed operation",
        ),
    ] {
        restore(&completed.identity_file(), &original);
        forge_marker(
            &completed,
            marker_json(&completed, sequence, operation, action),
        );
        assert_eq!(completed.verdict(), Err("LEDGER_BEHIND_STORES"), "{why}");
    }
    restore(&completed.identity_file(), &original);
    assert_eq!(completed.verdict(), Ok(()));
}

// Review 2: every crash point of bind and revoke, recovered by resend and by
// reconcile. While one transaction is in flight, no other operation can
// advance, so the stores are never two apart; recovery is idempotent; every
// profile has exactly its own owner; the sequence counts identity commits.
#[test]
fn continuity_every_interruption_recovers_once_and_never_completes_another_operation() {
    for fault in [
        Fault::PendingRecorded,
        Fault::ProfileRenamed,
        Fault::ProfileCreatedRecorded,
        Fault::IdentityCommitted,
    ] {
        for by_reconcile in [false, true] {
            let world = initialized();
            let key_a = "idk_fixturecreate00000000001";
            let key_b = "idk_fixturecreate00000000002";
            world.service.inject(fault);
            assert!(
                world.provision(key_a, &web_request(P1)).is_err(),
                "{fault:?}"
            );
            assert_eq!(world.verdict(), Ok(()), "{fault:?} after the crash");
            let committed = fault == Fault::IdentityCommitted;

            // Another operation meanwhile.
            let other = world.provision(key_b, &web_request(P2));
            if committed {
                // Refused in its identity transaction, retryable, unbound.
                assert_eq!(other.unwrap_err(), ProvisioningError::Unavailable);
                assert_eq!(
                    operation_for(&world, P2).state,
                    OperationState::ProfileCreated
                );
                assert!(world.effective().get(&profile(P2)).is_none());
                assert_eq!(
                    world.marker().unwrap().last_operation,
                    operation_for(&world, P1).id
                );
            } else {
                assert_eq!(other.unwrap().status.state, OperationState::Completed);
            }
            assert_ne!(operation_for(&world, P1).state, OperationState::Completed);
            assert_eq!(world.verdict(), Ok(()), "{fault:?} with another operation");

            if by_reconcile {
                for _ in 0..2 {
                    world.service.reconcile(&operator()).unwrap();
                }
            } else {
                world.create(key_a, P1);
                world.create(key_b, P2);
            }
            for (id, key) in [(P1, key_a), (P2, key_b)] {
                let op = operation_for(&world, id);
                assert_eq!(op.state, OperationState::Completed, "{fault:?} {id}");
                assert_eq!(
                    world.effective().get(&profile(id)).map(|r| r.owner.clone()),
                    Some(op.principal.clone())
                );
                // Idempotent: a resend changes nothing.
                let sequence = world.marker().unwrap().sequence;
                let again = world.provision(key, &web_request(id)).unwrap();
                assert!(again.replayed);
                assert_eq!(again.status.operation_id, op.id);
                assert_eq!(world.marker().unwrap().sequence, sequence);
            }
            assert_eq!(world.service.load().unwrap().operations().count(), 2);
            assert_eq!(
                world.marker().unwrap().sequence,
                2,
                "{fault:?}: one commit each"
            );
            assert_eq!(world.stamp().identity_sequence_seen, 2);
            assert_eq!(world.verdict(), Ok(()), "{fault:?} recovered");
        }
    }

    // A revoke interrupted after its identity commit.
    let world = initialized();
    let a = world.create("idk_fixturecreate00000000001", P1);
    world.service.inject(Fault::RevokeIdentityCommitted);
    assert!(
        world
            .service
            .revoke(&world.site(), &a.operation_id)
            .is_err()
    );
    assert!(
        world.effective().get(&profile(P1)).is_none(),
        "access is already gone"
    );
    assert_eq!(world.verdict(), Ok(()));
    // Nothing else advances until it settles.
    assert_eq!(
        world
            .provision("idk_fixturecreate00000000002", &web_request(P2))
            .unwrap_err(),
        ProvisioningError::Unavailable
    );
    let revoked = world
        .service
        .revoke(&world.site(), &a.operation_id)
        .unwrap();
    assert_eq!(revoked.state, OperationState::Revoked);
    let again = world
        .service
        .revoke(&world.site(), &a.operation_id)
        .unwrap();
    assert_eq!(again.state, OperationState::Revoked);
    assert_eq!(world.marker().unwrap().sequence, 2);
    world.create("idk_fixturecreate00000000002", P2);
    assert_eq!(world.marker().unwrap().sequence, 3);
    assert_eq!(world.verdict(), Ok(()));
}

// Review 3: an uninitialized installation's documents have exactly the
// top-level members they had before L1.
#[test]
fn continuity_uninitialized_documents_keep_their_shape() {
    let world = world();
    let done = world.create("idk_fixturecreate00000000001", P1);
    world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    let keys = |path: &Path| -> Vec<String> {
        let value: serde_json::Value = serde_json::from_slice(&read(path)).unwrap();
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&world.ledger_file()),
        ["generation", "keys", "operations", "provisioners", "schema"]
    );
    assert_eq!(
        keys(&world.identity_file()),
        [
            "bindings",
            "credentials",
            "generation",
            "idempotency",
            "ownership",
            "principals",
            "reassignments",
            "schema",
            "trusted_issuers"
        ]
    );
    // The bytes are exactly what the documents serialize to without the
    // new members.
    let ledger = world.service.load().unwrap();
    assert_eq!(
        read(&world.ledger_file()),
        serde_json::to_vec_pretty(&ledger).unwrap()
    );
}

// Review 3: retirement stops an operation recorded before it, too: no
// profile is created and nothing is bound, by resend or reconcile.
#[test]
fn continuity_retirement_stops_operations_recorded_before_it() {
    let retired = initialized();
    retired.service.inject(Fault::PendingRecorded);
    assert!(
        retired
            .provision("idk_fixturecreate00000000001", &web_request(P1))
            .is_err()
    );
    retired.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        retired
            .provision("idk_fixturecreate00000000002", &web_request(P2))
            .is_err()
    );
    retired.service.retire(&operator(), HARNESS).unwrap();
    for (key, id) in [
        ("idk_fixturecreate00000000001", P1),
        ("idk_fixturecreate00000000002", P2),
    ] {
        assert_eq!(
            retired.provision(key, &web_request(id)).unwrap_err(),
            ProvisioningError::Denied("harness_retired")
        );
    }
    let report = retired.service.reconcile(&operator()).unwrap();
    assert_eq!(report.still_pending, 2);
    assert_eq!(operation_for(&retired, P1).state, OperationState::Pending);
    assert_eq!(
        operation_for(&retired, P2).state,
        OperationState::ProfileCreated
    );
    assert!(!retired.profile_dir(P1).exists());
    assert!(retired.profile_dir(P2).exists(), "kept, never deleted");
    assert!(retired.effective().get(&profile(P2)).is_none());
    assert_eq!(retired.marker(), None);

    let staged = world();
    std::fs::create_dir_all(staged.service.root().join(STAGING).join("old")).unwrap();
    assert_eq!(
        staged.service.initialize(&operator(), HARNESS).unwrap_err(),
        ProvisioningError::Conflict("provisioning_history_exists")
    );
}

// Review 4: an identity rollback whose marker is then forged forward to the
// ledger's view. When the lost transaction created a user, that principal
// is missing, and principals are never deleted.
#[test]
fn continuity_detects_a_forged_marker_over_a_rolled_back_identity_store() {
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    let older = read(&world.identity_file());
    let second = world
        .provision(
            "idk_fixturecreate00000000002",
            &request("synthetic-subject-0002", P2),
        )
        .unwrap()
        .status;
    let forged = marker_json(&world, 2, &second.operation_id, "bind");
    restore(&world.identity_file(), &older);
    forge_marker(&world, forged);
    edit_json(&world.identity_file(), |doc| {
        doc["generation"] = 1000.into();
    });
    assert_eq!(world.verdict(), Err("STORES_BEHIND_LEDGER"));
}

// Review 5: malformed values fail closed. Nothing malformed reads as an
// authenticated ledger.
#[test]
fn continuity_malformed_values_fail_closed() {
    for (field, value, expected) in [
        ("retired", serde_json::json!("no"), "storage_corrupt"),
        ("retired", serde_json::Value::Null, "storage_corrupt"),
        (
            "identity_sequence_seen",
            serde_json::json!(-1),
            "storage_corrupt",
        ),
        ("harness_id", serde_json::json!(7), "storage_corrupt"),
        ("continuity_id", serde_json::json!(""), "UNSTAMPED_LEDGER"),
        (
            "continuity_id",
            serde_json::json!("not-hex"),
            "UNSTAMPED_LEDGER",
        ),
        (
            "harness_id",
            serde_json::json!("hns_UPPER0000000"),
            "UNSTAMPED_LEDGER",
        ),
        ("retired_unix", serde_json::json!(5), "RETIRED"),
    ] {
        let world = initialized();
        world.create("idk_fixturecreate00000000001", P1);
        edit_json(&world.ledger_file(), |doc| {
            doc["continuity"][field] = value.clone();
        });
        assert_eq!(world.verdict(), Err(expected), "{field} = {value}");
    }
    // An empty continuity id in both stores still does not authenticate.
    let world = initialized();
    world.create("idk_fixturecreate00000000001", P1);
    edit_json(&world.ledger_file(), |doc| {
        doc["continuity"]["continuity_id"] = "".into();
    });
    edit_json(&world.identity_file(), |doc| {
        doc["provisioning_continuity"]["continuity_id"] = "".into();
    });
    assert_eq!(world.verdict(), Err("UNSTAMPED_LEDGER"));
    // A malformed identity marker does not parse.
    for (field, value) in [
        ("sequence", serde_json::json!("1")),
        ("last_action", serde_json::json!("delete")),
    ] {
        let world = initialized();
        world.create("idk_fixturecreate00000000001", P1);
        edit_json(&world.identity_file(), |doc| {
            doc["provisioning_continuity"][field] = value.clone();
        });
        assert_eq!(world.verdict(), Err("storage_unavailable"), "{field}");
    }
}
