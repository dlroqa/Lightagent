//! Synthetic identities, profiles and credentials only.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use super::*;
use crate::config::JevConfig;

fn principal(id: &str) -> PrincipalId {
    PrincipalId::new(id).unwrap()
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn user(id: &str) -> Caller {
    Caller {
        principal: Principal {
            id: principal(id),
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "0000000000000000".to_owned(),
            },
        },
        restriction: Restriction::default(),
    }
}

/// `alice` owns `alice-bot`; `bob` owns `bob-bot`; `shared` is alice's with
/// a read grant for bob; `orphan` has no owner.
fn registry() -> OwnershipRegistry {
    let mut registry = OwnershipRegistry::default();
    registry
        .assign(profile("alice-bot"), principal("alice"))
        .unwrap();
    registry
        .assign(profile("bob-bot"), principal("bob"))
        .unwrap();
    registry
        .assign(profile("shared"), principal("alice"))
        .unwrap();
    registry
        .grant(
            &profile("shared"),
            principal("bob"),
            Capability::SessionsRead,
        )
        .unwrap();
    registry
}

const PROFILE_CAPABILITIES: [Capability; 6] = [
    Capability::RunsRead,
    Capability::RunsWrite,
    Capability::SessionsRead,
    Capability::SessionsWrite,
    Capability::ToolsRead,
    Capability::ApprovalsWrite,
];

#[test]
fn the_local_operator_keeps_todays_access_to_every_profile() {
    let registry = registry();
    for source in [
        PrincipalSource::LocalCompatibility,
        PrincipalSource::LocalProcess,
    ] {
        let caller = Caller::local_operator(source);
        for capability in Capability::ALL {
            assert_eq!(authorize_capability(&caller, capability), Ok(()));
            for id in ["alice-bot", "bob-bot", "orphan", "default"] {
                let granted = authorize(&caller, capability, &profile(id), &registry).unwrap();
                assert_eq!(granted.profile().as_str(), id);
                assert_eq!(granted.principal().as_str(), LOCAL_OPERATOR);
                assert_eq!(granted.capability(), capability);
            }
        }
    }
    // An empty registry, as every installation has today, changes nothing.
    let empty = OwnershipRegistry::default();
    let caller = Caller::local_operator(PrincipalSource::LocalCompatibility);
    assert!(authorize(&caller, Capability::RunsWrite, &profile("default"), &empty).is_ok());
}

#[test]
fn a_user_acts_on_its_own_profile_only() {
    let registry = registry();
    let alice = user("alice");
    for capability in PROFILE_CAPABILITIES {
        assert!(authorize(&alice, capability, &profile("alice-bot"), &registry).is_ok());
        assert_eq!(
            authorize(&alice, capability, &profile("bob-bot"), &registry),
            Err(Denial::ProfileNotGranted),
            "{capability:?}: another user's profile, including its sessions, run events, Jev diagnostics and approvals"
        );
    }
}

#[test]
fn a_grant_allows_exactly_the_granted_capability() {
    let registry = registry();
    let bob = user("bob");
    assert!(
        authorize(
            &bob,
            Capability::SessionsRead,
            &profile("shared"),
            &registry
        )
        .is_ok()
    );
    for capability in [
        Capability::SessionsWrite,
        Capability::RunsWrite,
        Capability::RunsRead,
        Capability::ApprovalsWrite,
    ] {
        assert_eq!(
            authorize(&bob, capability, &profile("shared"), &registry),
            Err(Denial::ProfileNotGranted),
            "{capability:?}"
        );
    }
}

#[test]
fn new_multi_user_installations_fail_closed_on_unowned_profiles() {
    let registry = registry();
    for id in ["orphan", "default"] {
        assert_eq!(
            authorize(
                &user("alice"),
                Capability::RunsWrite,
                &profile(id),
                &registry
            ),
            Err(Denial::ProfileUnowned)
        );
    }
    assert_eq!(
        authorize(
            &user("alice"),
            Capability::RunsWrite,
            &profile("alice-bot"),
            &OwnershipRegistry::default()
        ),
        Err(Denial::ProfileUnowned)
    );
}

#[test]
fn knowing_or_sharing_a_profile_name_is_not_ownership() {
    let registry = registry();
    // A principal named exactly like the profile gains nothing.
    assert_eq!(
        authorize(
            &user("bob-bot"),
            Capability::RunsWrite,
            &profile("bob-bot"),
            &registry
        ),
        Err(Denial::ProfileNotGranted)
    );
    // A principal is never derived from a profile id: unknown principals
    // own nothing.
    assert!(
        registry.owned_by(&principal("bob-bot")).next().is_none(),
        "no ownership by name"
    );
}

#[test]
fn only_operators_administer_or_provision() {
    let registry = registry();
    let alice = user("alice");
    for capability in [Capability::Administer, Capability::Provision] {
        assert_eq!(
            authorize_capability(&alice, capability),
            Err(Denial::MissingCapability)
        );
        assert_eq!(
            authorize(&alice, capability, &profile("alice-bot"), &registry),
            Err(Denial::MissingCapability),
            "owning a profile does not make a user an administrator"
        );
        let operator = Caller::local_operator(PrincipalSource::LocalProcess);
        assert_eq!(authorize_capability(&operator, capability), Ok(()));
    }
    // Even a credential listing them cannot lift a user.
    let mut lifted = user("alice");
    lifted.restriction.capabilities = Some(Capability::ALL.into_iter().collect());
    assert_eq!(
        authorize_capability(&lifted, Capability::Administer),
        Err(Denial::MissingCapability)
    );
    let mut registry = registry;
    assert_eq!(
        registry.grant(
            &profile("alice-bot"),
            principal("bob"),
            Capability::Administer
        ),
        Err(OwnershipError::OperatorOnly)
    );
}

#[test]
fn a_credential_restriction_only_narrows() {
    let registry = registry();
    let mut alice = user("alice");
    alice.restriction = Restriction {
        capabilities: Some(BTreeSet::from([Capability::SessionsRead])),
        profiles: Some(BTreeSet::from([profile("alice-bot")])),
    };
    assert!(
        authorize(
            &alice,
            Capability::SessionsRead,
            &profile("alice-bot"),
            &registry
        )
        .is_ok()
    );
    assert_eq!(
        authorize(
            &alice,
            Capability::RunsWrite,
            &profile("alice-bot"),
            &registry
        ),
        Err(Denial::MissingCapability)
    );
    assert_eq!(
        authorize(
            &alice,
            Capability::SessionsRead,
            &profile("shared"),
            &registry
        ),
        Err(Denial::ProfileOutsideCredential),
        "an owned profile outside the credential's list"
    );
    // Listing someone else's profile grants nothing.
    alice.restriction.profiles = Some(BTreeSet::from([profile("bob-bot")]));
    assert_eq!(
        authorize(
            &alice,
            Capability::SessionsRead,
            &profile("bob-bot"),
            &registry
        ),
        Err(Denial::ProfileNotGranted)
    );
    // An operator credential can be narrowed too.
    let mut operator = Caller::local_operator(PrincipalSource::LocalCompatibility);
    operator.restriction.capabilities = Some(BTreeSet::from([Capability::RunsRead]));
    assert_eq!(
        authorize_capability(&operator, Capability::Administer),
        Err(Denial::MissingCapability)
    );
}

#[test]
fn a_jev_permission_never_authorizes_a_caller() {
    // bob-bot may send content to Jev for routing and classification; alice
    // is still refused bob-bot.
    let mut jev = JevConfig::default();
    jev.routing_allowed_profiles = vec!["bob-bot".to_owned()];
    jev.classification.allowed_profiles = vec!["bob-bot".to_owned()];
    assert!(jev.routing_grant("bob-bot").permits());
    assert!(jev.classification.permits_profile("bob-bot"));
    let registry = registry();
    for capability in PROFILE_CAPABILITIES {
        assert_eq!(
            authorize(&user("alice"), capability, &profile("bob-bot"), &registry),
            Err(Denial::ProfileNotGranted)
        );
    }
    // And a profile without any Jev permission is still its owner's.
    assert!(!jev.routing_grant("alice-bot").permits());
    assert!(
        authorize(
            &user("alice"),
            Capability::RunsWrite,
            &profile("alice-bot"),
            &registry
        )
        .is_ok()
    );
}

#[test]
fn revoking_a_grant_applies_to_the_next_decision() {
    let mut registry = registry();
    let bob = user("bob");
    assert!(
        authorize(
            &bob,
            Capability::SessionsRead,
            &profile("shared"),
            &registry
        )
        .is_ok()
    );
    registry.revoke(
        &profile("shared"),
        &principal("bob"),
        Capability::SessionsRead,
    );
    assert_eq!(
        authorize(
            &bob,
            Capability::SessionsRead,
            &profile("shared"),
            &registry
        ),
        Err(Denial::ProfileNotGranted)
    );
    assert!(registry.get(&profile("shared")).unwrap().grants.is_empty());
}

#[test]
fn reassignment_moves_access_and_drops_the_previous_owners_grants() {
    let mut registry = registry();
    assert_eq!(
        registry.reassign(profile("shared"), principal("carol")),
        Assignment::Reassigned
    );
    let shared = profile("shared");
    assert!(authorize(&user("carol"), Capability::RunsWrite, &shared, &registry).is_ok());
    assert_eq!(
        authorize(&user("alice"), Capability::RunsWrite, &shared, &registry),
        Err(Denial::ProfileNotGranted),
        "the previous owner loses access"
    );
    assert_eq!(
        authorize(&user("bob"), Capability::SessionsRead, &shared, &registry),
        Err(Denial::ProfileNotGranted),
        "grants made by the previous owner are dropped"
    );
    assert_eq!(
        registry.reassign(shared.clone(), principal("carol")),
        Assignment::Unchanged
    );
    // A deleted profile's ownership does not pass to a new profile reusing the id.
    registry.remove(&shared);
    assert_eq!(
        authorize(&user("carol"), Capability::RunsWrite, &shared, &registry),
        Err(Denial::ProfileUnowned)
    );
}

#[test]
fn provisioning_is_idempotent_and_conflicts_are_refused() {
    let mut registry = OwnershipRegistry::default();
    assert_eq!(
        registry.assign(profile("new-bot"), principal("alice")),
        Ok(Assignment::Created)
    );
    assert_eq!(
        registry.assign(profile("new-bot"), principal("alice")),
        Ok(Assignment::Unchanged)
    );
    assert_eq!(
        registry.assign(profile("new-bot"), principal("bob")),
        Err(OwnershipError::Conflict)
    );
    assert_eq!(
        registry.get(&profile("new-bot")).unwrap().owner,
        principal("alice")
    );
    assert_eq!(
        registry.grant(
            &profile("new-bot"),
            principal("alice"),
            Capability::RunsRead
        ),
        Err(OwnershipError::GrantToOwner)
    );
    assert_eq!(
        registry.grant(&profile("missing"), principal("bob"), Capability::RunsRead),
        Err(OwnershipError::Unowned)
    );
}

#[test]
fn concurrent_conflicting_assignments_have_exactly_one_winner() {
    let registry = Arc::new(Mutex::new(OwnershipRegistry::default()));
    let handles: Vec<_> = (0..16)
        .map(|index| {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry
                    .lock()
                    .unwrap()
                    .assign(profile("contested"), principal(&format!("user-{index}")))
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Ok(Assignment::Created))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(OwnershipError::Conflict))
            .count(),
        15
    );
    // Concurrent readers see one owner throughout.
    let owner = registry
        .lock()
        .unwrap()
        .get(&profile("contested"))
        .unwrap()
        .owner
        .clone();
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let registry = Arc::clone(&registry);
            let owner = owner.clone();
            std::thread::spawn(move || {
                let registry = registry.lock().unwrap();
                let caller = Caller {
                    principal: Principal {
                        id: owner,
                        role: Role::User,
                        source: PrincipalSource::LocalProcess,
                    },
                    restriction: Restriction::default(),
                };
                authorize(
                    &caller,
                    Capability::RunsRead,
                    &profile("contested"),
                    &registry,
                )
                .is_ok()
            })
        })
        .collect();
    assert!(readers.into_iter().all(|reader| reader.join().unwrap()));
}

#[test]
fn issued_credentials_authenticate_and_store_only_a_digest() {
    let mut store = CredentialStore::default();
    let token = store
        .issue(
            principal("alice"),
            Role::User,
            Restriction::default(),
            1_700_000_000,
        )
        .unwrap();
    let text = token.expose().to_owned();
    assert!(text.starts_with("la_"));
    assert_eq!(text.len(), 3 + 16 + 1 + 64);
    let caller = store.authenticate(&text).unwrap();
    assert_eq!(caller.principal.id, principal("alice"));
    assert_eq!(caller.principal.role, Role::User);
    let PrincipalSource::Credential { key_id } = &caller.principal.source else {
        panic!("a credential caller");
    };
    // Nothing persisted or printed holds the secret.
    let secret = text.rsplit('_').next().unwrap();
    let persisted = serde_json::to_string(&store).unwrap();
    assert!(!persisted.contains(secret));
    assert!(persisted.contains(key_id.as_str()));
    assert_eq!(format!("{token:?}"), "IssuedToken(<redacted>)");
    let record = store.get(key_id).unwrap();
    assert_eq!(record.secret_sha256.len(), 64);
    assert_ne!(record.secret_sha256, secret);
    // A reloaded store authenticates the same token.
    let reloaded: CredentialStore = serde_json::from_str(&persisted).unwrap();
    assert!(reloaded.authenticate(&text).is_ok());
}

#[test]
fn invalid_and_tampered_credentials_are_unauthenticated() {
    let mut store = CredentialStore::default();
    let token = store
        .issue(principal("alice"), Role::User, Restriction::default(), 1)
        .unwrap();
    let text = token.expose();
    let mut tampered = text.to_owned();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    let other = CredentialStore::default()
        .issue(principal("alice"), Role::User, Restriction::default(), 1)
        .map(|token| token.expose().to_owned())
        .unwrap();
    let (key_part, _) = text[3..].split_once('_').unwrap();
    let swapped = format!("la_{key_part}_{}", &other[3 + 16 + 1..]);
    for presented in [
        "",
        "la_",
        "secret",
        "Bearer la_x",
        text.to_uppercase().as_str(),
        &text[..text.len() - 1],
        format!("{text}0").as_str(),
        tampered.as_str(),
        other.as_str(),
        swapped.as_str(),
    ] {
        assert_eq!(
            store.authenticate(presented),
            Err(Denial::Unauthenticated),
            "{presented:?}"
        );
    }
}

#[test]
fn a_revoked_credential_never_authenticates_again() {
    let mut store = CredentialStore::default();
    let token = store
        .issue(principal("alice"), Role::User, Restriction::default(), 1)
        .unwrap();
    let caller = store.authenticate(token.expose()).unwrap();
    let PrincipalSource::Credential { key_id } = caller.principal.source else {
        panic!("a credential caller");
    };
    store.revoke(&key_id, 2).unwrap();
    assert_eq!(store.authenticate(token.expose()), Err(Denial::Revoked));
    assert_eq!(Denial::Revoked.http_status(), 401);
    // Revoking again keeps the first revocation time.
    store.revoke(&key_id, 3).unwrap();
    assert_eq!(store.get(&key_id).unwrap().revoked_unix, Some(2));
    assert_eq!(
        store.revoke("ffffffffffffffff", 4),
        Err(CredentialError::Unknown)
    );
    // Other credentials are unaffected.
    let second = store
        .issue(principal("alice"), Role::User, Restriction::default(), 5)
        .unwrap();
    assert!(store.authenticate(second.expose()).is_ok());
}

#[test]
fn credential_restrictions_survive_authentication() {
    let mut store = CredentialStore::default();
    let restriction = Restriction {
        capabilities: Some(BTreeSet::from([Capability::RunsRead])),
        profiles: Some(BTreeSet::from([profile("alice-bot")])),
    };
    let token = store
        .issue(principal("alice"), Role::User, restriction.clone(), 1)
        .unwrap();
    let caller = store.authenticate(token.expose()).unwrap();
    assert_eq!(caller.restriction, restriction);
    assert_eq!(
        authorize(
            &caller,
            Capability::RunsWrite,
            &profile("alice-bot"),
            &registry()
        ),
        Err(Denial::MissingCapability)
    );
}

#[test]
fn audit_records_hold_identifiers_and_codes_only() {
    let mut store = CredentialStore::default();
    let token = store
        .issue(principal("alice"), Role::User, Restriction::default(), 1)
        .unwrap();
    let caller = store.authenticate(token.expose()).unwrap();
    let denied = AuditRecord::decision(
        10,
        Some(&caller),
        AuditAction::Request(Capability::RunsWrite),
        Some(&profile("bob-bot")),
        authorize(
            &caller,
            Capability::RunsWrite,
            &profile("bob-bot"),
            &registry(),
        )
        .map(|_| ()),
    );
    assert_eq!(
        denied.outcome,
        AuditOutcome::Denied(Denial::ProfileNotGranted)
    );
    let line = serde_json::to_string(&denied).unwrap();
    let secret = token.expose().rsplit('_').next().unwrap();
    assert!(!line.contains(secret), "{line}");
    assert!(line.contains("profile_not_granted"), "{line}");
    let anonymous = AuditRecord::decision(
        11,
        None,
        AuditAction::Request(Capability::SessionsRead),
        None,
        Err(Denial::Unauthenticated),
    );
    assert_eq!(anonymous.principal, None);
    assert_eq!(anonymous.key_id, None);
    // Every value in a record is an id, a code, a number or null.
    let value = serde_json::to_value(&denied).unwrap();
    for field in [
        "at_unix",
        "principal",
        "key_id",
        "action",
        "profile",
        "outcome",
    ] {
        assert!(value.get(field).is_some(), "{field}");
    }
    assert_eq!(
        value.as_object().unwrap().len(),
        6,
        "no field beyond the fixed set"
    );
}

#[test]
fn principal_ids_are_validated_and_denials_map_to_statuses() {
    for invalid in ["", "Alice", "../x", "-a", "a b", "a".repeat(65).as_str()] {
        assert_eq!(
            PrincipalId::new(invalid),
            Err(IdentityError::InvalidPrincipal),
            "{invalid:?}"
        );
        assert!(serde_json::from_str::<PrincipalId>(&format!("{invalid:?}")).is_err());
    }
    assert_eq!(Denial::Unauthenticated.http_status(), 401);
    for denial in [
        Denial::MissingCapability,
        Denial::ProfileOutsideCredential,
        Denial::ProfileUnowned,
        Denial::ProfileNotGranted,
    ] {
        assert_eq!(denial.http_status(), 403, "{denial:?}");
    }
}
