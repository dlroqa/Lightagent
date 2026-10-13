//! The durable identity store over temporary homes and synthetic identities.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::*;
use crate::identity::{
    Caller, OwnershipRegistry, Principal, PrincipalSource, Restriction, authorize,
};
use crate::ids::RunId;
use crate::profile::{AgentProfile, ProfileStore};

const NOW: u64 = 1_800_000_000;

struct Home {
    root: PathBuf,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn home() -> Home {
    let root = std::env::temp_dir().join(format!("lightagent-identity-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    Home { root }
}

fn id(name: &str) -> PrincipalId {
    PrincipalId::new(name).unwrap()
}

fn profile(name: &str) -> ProfileId {
    ProfileId::new(name).unwrap()
}

fn create_profile(home: &Home, name: &str) {
    ProfileStore::new(&home.root)
        .create(&AgentProfile::new(profile(name), name, "Persona", "fast"))
        .unwrap();
}

/// The local operator: every capability.
fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

/// An operator credential limited to `capabilities` (a future provisioning
/// service gets `provision` only).
fn limited_operator(capabilities: &[Capability]) -> Caller {
    Caller {
        principal: Principal {
            id: id("provisioner"),
            role: Role::Operator,
            source: PrincipalSource::Credential {
                key_id: "0000000000000001".to_owned(),
            },
        },
        restriction: Restriction {
            capabilities: Some(capabilities.iter().copied().collect()),
            profiles: None,
        },
    }
}

fn user(name: &str) -> Caller {
    Caller {
        principal: Principal {
            id: id(name),
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "0000000000000002".to_owned(),
            },
        },
        restriction: Restriction::default(),
    }
}

fn applied<R>(outcome: Outcome<R>) -> R {
    match outcome {
        Outcome::Applied { value, .. } => value,
        Outcome::Replayed { .. } => panic!("expected an applied change"),
    }
}

/// Create `alice` (user) and issue her a credential. Returns its token.
fn alice_with_token(store: &IdentityStore) -> (String, String) {
    applied(
        store
            .transact(&operator(), NOW, None, |tx| {
                tx.create_principal(id("alice"), Role::User, None)?;
                let (key_id, token) =
                    tx.issue_credential(&id("alice"), Restriction::default(), None)?;
                Ok((key_id, token.expose().to_owned()))
            })
            .unwrap(),
    )
}

// 1, 2, 6. Persistence across restart; issuance and verification; ownership.
#[test]
fn identity_and_ownership_survive_a_restart() {
    let home = home();
    create_profile(&home, "research");
    let (key_id, token) = {
        let store = IdentityStore::new(&home.root);
        let issued = alice_with_token(&store);
        store
            .transact(&operator(), NOW, None, |tx| {
                tx.assign_owner(&profile("research"), &id("alice"))
            })
            .unwrap();
        issued
    };
    // A new store over the same home: a restarted server.
    let store = IdentityStore::new(&home.root);
    let document = store.load().unwrap();
    assert_eq!(document.generation(), 2);
    let caller = document.credentials().authenticate_at(&token, NOW).unwrap();
    assert_eq!(caller.principal.id, id("alice"));
    assert_eq!(
        caller.principal.source,
        PrincipalSource::Credential { key_id }
    );
    let ownership = store.effective_ownership(&document);
    assert!(
        authorize(
            &caller,
            Capability::RunsWrite,
            &profile("research"),
            &ownership
        )
        .is_ok()
    );
    // A wrong secret for a real key id is refused.
    let mut forged = token.clone();
    forged.pop();
    forged.push(if token.ends_with('0') { '1' } else { '0' });
    assert_eq!(
        document.credentials().authenticate_at(&forged, NOW),
        Err(Denial::Unauthenticated)
    );
}

// 3, 4, 5. Expiry, rotation with overlap, revocation, across restart.
#[test]
fn expiry_rotation_and_revocation_are_durable() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    applied(
        store
            .transact(&operator(), NOW, None, |tx| {
                tx.create_principal(id("alice"), Role::User, None)
            })
            .unwrap(),
    );
    let (old_id, old) = applied(
        store
            .transact(&operator(), NOW, None, |tx| {
                let (key, token) =
                    tx.issue_credential(&id("alice"), Restriction::default(), Some(NOW + 60))?;
                Ok((key, token.expose().to_owned()))
            })
            .unwrap(),
    );
    let credentials = || {
        IdentityStore::new(&home.root)
            .load()
            .unwrap()
            .credentials()
            .clone()
    };
    assert!(credentials().authenticate_at(&old, NOW + 59).is_ok());
    assert_eq!(
        credentials().authenticate_at(&old, NOW + 60),
        Err(Denial::Expired)
    );
    // An expiry in the past is refused at issue.
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.issue_credential(&id("alice"), Restriction::default(), Some(NOW))
        }),
        Err(IdentityStoreError::Credential(CredentialError::Expiry))
    ));
    // Rotate: both work until the old one is revoked.
    let (new_id, new) = applied(
        store
            .transact(&operator(), NOW + 1, None, |tx| {
                let (key, token) = tx.rotate_credential(&old_id, None)?;
                Ok((key, token.expose().to_owned()))
            })
            .unwrap(),
    );
    assert!(credentials().authenticate_at(&old, NOW + 2).is_ok());
    assert!(credentials().authenticate_at(&new, NOW + 2).is_ok());
    assert_eq!(
        credentials().get(&new_id).unwrap().replaces.as_deref(),
        Some(old_id.as_str())
    );
    store
        .transact(&operator(), NOW + 3, None, |tx| {
            tx.revoke_credential(&old_id)
        })
        .unwrap();
    assert_eq!(
        credentials().authenticate_at(&old, NOW + 4),
        Err(Denial::Revoked)
    );
    assert!(credentials().authenticate_at(&new, NOW + 4).is_ok());
    // A revoked credential cannot be rotated back to life.
    assert!(matches!(
        store.transact(&operator(), NOW + 5, None, |tx| tx
            .rotate_credential(&old_id, None)),
        Err(IdentityStoreError::Credential(CredentialError::Revoked))
    ));
    // Suspension revokes every credential; reactivation restores none.
    store
        .transact(&operator(), NOW + 6, None, |tx| {
            tx.set_status(&id("alice"), PrincipalStatus::Suspended)
        })
        .unwrap();
    assert_eq!(
        credentials().authenticate_at(&new, NOW + 7),
        Err(Denial::Revoked)
    );
    store
        .transact(&operator(), NOW + 8, None, |tx| {
            tx.set_status(&id("alice"), PrincipalStatus::Active)
        })
        .unwrap();
    assert_eq!(
        credentials().authenticate_at(&new, NOW + 9),
        Err(Denial::Revoked)
    );
}

// 7, 16. Reassignment is administer-only, transactional, and drops grants.
#[test]
fn reassignment_is_transactional_and_drops_old_grants() {
    let home = home();
    create_profile(&home, "research");
    let store = IdentityStore::new(&home.root);
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.create_principal(id("alice"), Role::User, None)?;
            tx.create_principal(id("bob"), Role::User, None)?;
            tx.create_principal(id("carol"), Role::User, None)?;
            tx.assign_owner(&profile("research"), &id("alice"))?;
            tx.grant(&profile("research"), &id("carol"), Capability::RunsRead)
        })
        .unwrap();
    // A provisioning credential may not reassign.
    let before = store.load().unwrap();
    assert!(matches!(
        store.transact(
            &limited_operator(&[Capability::Provision]),
            NOW,
            None,
            |tx| { tx.reassign_owner(&profile("research"), &id("bob")) }
        ),
        Err(IdentityStoreError::Denied(Denial::MissingCapability))
    ));
    assert_eq!(store.load().unwrap(), before, "nothing written");
    // A failure midway writes nothing either.
    assert!(
        store
            .transact(&operator(), NOW, None, |tx| {
                tx.reassign_owner(&profile("research"), &id("bob"))?;
                tx.reassign_owner(&profile("research"), &id("nobody"))
            })
            .is_err()
    );
    assert_eq!(store.load().unwrap(), before);
    // The operator reassigns: carol's grant from alice's era is gone.
    assert_eq!(
        applied(
            store
                .transact(&operator(), NOW + 1, None, |tx| {
                    tx.reassign_owner(&profile("research"), &id("bob"))
                })
                .unwrap()
        ),
        Assignment::Reassigned
    );
    let document = store.load().unwrap();
    let record = document
        .recorded_ownership()
        .get(&profile("research"))
        .unwrap();
    assert_eq!(record.owner, id("bob"));
    assert!(record.grants.is_empty());
    let history: Vec<_> = document.reassignments().collect();
    assert_eq!(history.len(), 1);
    assert_eq!(
        (&history[0].from, &history[0].to),
        (&id("alice"), &id("bob"))
    );
    // An ordinary assignment to a different owner is a conflict, never a move.
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.assign_owner(&profile("research"), &id("carol"))
        }),
        Err(IdentityStoreError::Ownership(OwnershipError::Conflict))
    ));
}

// 8. Concurrent writers are serialized; nothing is lost and one owner wins.
#[test]
fn concurrent_transactions_serialize_without_losing_writes() {
    let home = home();
    create_profile(&home, "shared");
    let store = Arc::new(IdentityStore::new(&home.root));
    let handles: Vec<_> = (0..8)
        .map(|index| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                // A separate store per thread: separate handles, as separate
                // processes would have.
                let _ = &store;
                let own = IdentityStore::new(store.root.clone());
                let name = format!("user{index}");
                own.transact(&operator(), NOW, None, |tx| {
                    tx.create_principal(id(&name), Role::User, None)
                })
                .unwrap();
                own.transact(&operator(), NOW, None, |tx| {
                    tx.assign_owner(&profile("shared"), &id(&name))
                })
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results
        .iter()
        .filter(|result| {
            matches!(
                result,
                Ok(Outcome::Applied {
                    value: Assignment::Created,
                    ..
                })
            )
        })
        .count();
    assert_eq!(winners, 1, "exactly one owner");
    let document = store.load().unwrap();
    assert_eq!(document.principals().count(), 8, "no write lost");
    assert_eq!(document.generation(), 9);
}

// 9. Duplicate provisioning identifiers and idempotency keys.
#[test]
fn duplicate_identities_and_idempotency_keys_are_handled() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    let issuer = "https://accounts.example.test";
    let external = ExternalIdentity {
        issuer: issuer.to_owned(),
        subject: "user-123".to_owned(),
    };
    // An external identity is refused until its issuer is trusted.
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.create_principal(id("alice"), Role::User, Some(external.clone()))
        }),
        Err(IdentityStoreError::Refused(Refusal::UntrustedIssuer))
    ));
    store
        .transact(&operator(), NOW, None, |tx| tx.trust_issuer(issuer))
        .unwrap();
    let provision = |key: &str, name: &str, fingerprint: &str| {
        let external = external.clone();
        store.transact(&operator(), NOW, Some((key, fingerprint)), move |tx| {
            tx.create_principal(id(name), Role::User, Some(external))
        })
    };
    assert!(matches!(
        provision("req-1", "alice", "f1"),
        Ok(Outcome::Applied { .. })
    ));
    // The same request again: replayed, nothing changed.
    let generation = store.load().unwrap().generation();
    assert!(matches!(
        provision("req-1", "alice", "f1"),
        Ok(Outcome::Replayed { .. })
    ));
    assert_eq!(store.load().unwrap().generation(), generation);
    // The same key with a different request: refused.
    assert!(matches!(
        provision("req-1", "mallory", "f2"),
        Err(IdentityStoreError::Refused(Refusal::IdempotencyConflict))
    ));
    // A new key mapping the same external identity: refused.
    assert!(matches!(
        provision("req-2", "mallory", "f3"),
        Err(IdentityStoreError::Refused(
            Refusal::DuplicateExternalIdentity
        ))
    ));
    // The same principal id again: refused.
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.create_principal(id("alice"), Role::User, None)
        }),
        Err(IdentityStoreError::Refused(Refusal::PrincipalExists))
    ));
    assert_eq!(
        store.load().unwrap().external(&external),
        Some(&id("alice"))
    );
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| tx
            .trust_issuer("http://insecure.test")),
        Err(IdentityStoreError::Refused(Refusal::InvalidIssuer))
    ));
}

// 10, 11. Corruption and interrupted writes fail closed and never overwrite.
#[test]
fn corruption_fails_closed_and_interrupted_writes_leave_the_old_state() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    let (_, token) = alice_with_token(&store);
    let state = home.root.join("identity").join(STATE);
    let good = std::fs::read(&state).unwrap();

    // An interrupted write leaves a stray temporary file: ignored.
    std::fs::write(
        home.root.join("identity").join("state.json.tmp"),
        b"{garbage",
    )
    .unwrap();
    assert!(
        store
            .load()
            .unwrap()
            .credentials()
            .authenticate_at(&token, NOW)
            .is_ok()
    );
    // The next commit replaces it and keeps the previous generation.
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.create_principal(id("bob"), Role::User, None)
        })
        .unwrap();
    assert_eq!(
        std::fs::read(home.root.join("identity").join(PREVIOUS)).unwrap(),
        good
    );

    // Corruption: refused on read and on write; never replaced by empty.
    for corrupt in [
        &b"{\"schema\":1,"[..],
        b"not json",
        b"{\"generation\":3}",
        b"",
    ] {
        std::fs::write(&state, corrupt).unwrap();
        assert!(
            matches!(store.load(), Err(IdentityStoreError::Corrupt)),
            "{corrupt:?}"
        );
        assert!(matches!(
            store.transact(&operator(), NOW, None, |tx| {
                tx.create_principal(id("carol"), Role::User, None)
            }),
            Err(IdentityStoreError::Corrupt)
        ));
        assert_eq!(std::fs::read(&state).unwrap(), corrupt, "left for recovery");
    }
    // Recovery: restore the previous generation.
    std::fs::copy(home.root.join("identity").join(PREVIOUS), &state).unwrap();
    assert!(
        store
            .load()
            .unwrap()
            .credentials()
            .authenticate_at(&token, NOW)
            .is_ok()
    );
}

// 12, 13. Migration failures and incompatible schemas.
#[test]
fn newer_and_unmigratable_schemas_are_refused_without_writing() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    alice_with_token(&store);
    let state = home.root.join("identity").join(STATE);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    for (schema, expected) in [(SCHEMA_VERSION + 1, "newer"), (0, "migrate")] {
        value["schema"] = serde_json::json!(schema);
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(&state, &bytes).unwrap();
        let error = store.load().unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert!(
            store
                .transact(&operator(), NOW, None, |tx| tx.create_principal(
                    id("x"),
                    Role::User,
                    None
                ))
                .is_err()
        );
        assert_eq!(std::fs::read(&state).unwrap(), bytes, "never overwritten");
    }
}

// 14. Audit retention bounds storage and records fixed codes only.
#[test]
fn audit_is_bounded_and_carries_no_secret() {
    let home = home();
    let retention = AuditRetention {
        max_file_bytes: 2_000,
        max_files: 3,
    };
    let store = IdentityStore::new(&home.root).with_retention(retention);
    let (_, token) = alice_with_token(&store);
    for index in 0..60 {
        store
            .transact(&operator(), NOW + index, None, |tx| {
                tx.create_principal(id(&format!("p{index}")), Role::User, None)
            })
            .unwrap();
    }
    let total: u64 = store
        .files()
        .iter()
        .filter(|path| path.to_string_lossy().contains("audit"))
        .map(|path| std::fs::metadata(path).unwrap().len())
        .sum();
    assert!(total <= retention.max_file_bytes * retention.max_files as u64 + 1_000);
    let records = store.audit().unwrap();
    assert!(
        !records.is_empty() && records.len() < 61,
        "oldest discarded"
    );
    assert!(
        records
            .iter()
            .all(|r| r.record.action == AuditAction::PrincipalCreated)
    );
    // A refused change is audited too.
    let _ = store.transact(&user("alice"), NOW, None, |tx| {
        tx.create_principal(id("mallory"), Role::Operator, None)
    });
    let last = store.audit().unwrap().pop().unwrap();
    assert!(is_denial(&last));
    let secret = token.rsplit('_').next().unwrap();
    for path in store.files() {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!text.contains(secret), "{}", path.display());
    }
}

// 15. No plaintext credential anywhere the store writes; owner-only files.
#[test]
fn nothing_persisted_holds_a_plaintext_credential() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    let (key_id, token) = alice_with_token(&store);
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.rotate_credential(&key_id, None).map(|_| ())
        })
        .unwrap();
    let secret = token.rsplit('_').next().unwrap();
    for path in store.files() {
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(secret) && !text.contains(&token),
            "{}",
            path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(home.root.join("identity"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
    // Debug of the document and of errors carries no secret either.
    let document = store.load().unwrap();
    assert!(!format!("{document:?}").contains(secret));
}

// 16, 17. Users change nothing; credentials cannot escalate.
#[test]
fn users_cannot_change_identity_and_credentials_cannot_escalate() {
    let home = home();
    create_profile(&home, "research");
    let store = IdentityStore::new(&home.root);
    alice_with_token(&store);
    let before = store.load().unwrap();
    for attempt in [
        store.transact(&user("alice"), NOW, None, |tx| {
            tx.assign_owner(&profile("research"), &id("alice"))
                .map(|_| ())
        }),
        store.transact(&user("alice"), NOW, None, |tx| {
            tx.grant(&profile("research"), &id("alice"), Capability::ToolsRead)
        }),
        store.transact(&user("alice"), NOW, None, |tx| {
            tx.issue_credential(&id("alice"), Restriction::default(), None)
                .map(|_| ())
        }),
    ] {
        assert!(matches!(
            attempt,
            Err(IdentityStoreError::Denied(Denial::MissingCapability))
        ));
    }
    assert_eq!(store.load().unwrap(), before);
    // A provisioning credential cannot create operators or issue them keys.
    let provisioner = limited_operator(&[Capability::Provision]);
    assert!(matches!(
        store.transact(&provisioner, NOW, None, |tx| {
            tx.create_principal(id("root2"), Role::Operator, None)
        }),
        Err(IdentityStoreError::Denied(Denial::MissingCapability))
    ));
    // A user credential cannot carry operator-only capabilities.
    let escalate = Restriction {
        capabilities: Some(BTreeSet::from([
            Capability::RunsWrite,
            Capability::Administer,
        ])),
        profiles: None,
    };
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.issue_credential(&id("alice"), escalate, None)
                .map(|_| ())
        }),
        Err(IdentityStoreError::Refused(
            Refusal::OperatorOnlyRestriction
        ))
    ));
    // Grants never include operator-only capabilities; owners are users.
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.assign_owner(&profile("research"), &id("alice"))
        })
        .unwrap();
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.grant(&profile("research"), &id("alice"), Capability::Provision)
        }),
        Err(IdentityStoreError::Ownership(OwnershipError::OperatorOnly))
    ));
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.create_principal(id("ops"), Role::Operator, None)?;
            tx.reassign_owner(&profile("research"), &id("ops"))
        }),
        Err(IdentityStoreError::Refused(Refusal::OwnerMustBeUser))
    ));
}

// 18. Cross-profile authorization, missing and re-created profiles.
#[test]
fn ownership_counts_only_for_the_same_existing_profile() {
    let home = home();
    create_profile(&home, "research");
    create_profile(&home, "billing");
    let store = IdentityStore::new(&home.root);
    let (_, token) = alice_with_token(&store);
    // A profile that does not exist cannot be assigned.
    assert!(matches!(
        store.transact(&operator(), NOW, None, |tx| {
            tx.assign_owner(&profile("ghost"), &id("alice"))
        }),
        Err(IdentityStoreError::Refused(Refusal::ProfileMissing))
    ));
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.assign_owner(&profile("research"), &id("alice"))
        })
        .unwrap();
    let check = |name: &str| {
        let document = store.load().unwrap();
        let caller = document.credentials().authenticate_at(&token, NOW).unwrap();
        authorize(
            &caller,
            Capability::RunsWrite,
            &profile(name),
            &store.effective_ownership(&document),
        )
    };
    assert!(check("research").is_ok());
    assert_eq!(check("billing"), Err(Denial::ProfileUnowned));
    // Deleted, then re-created under the same id: ownership does not follow.
    let profiles = ProfileStore::new(&home.root);
    profiles.delete(&profile("research")).unwrap();
    assert_eq!(check("research"), Err(Denial::ProfileUnowned));
    create_profile(&home, "research");
    assert_eq!(check("research"), Err(Denial::ProfileUnowned));
    // A copied marker from another profile does not bind either.
    std::fs::write(
        profiles
            .handle(&profile("research"))
            .dir()
            .join(BINDING_FILE),
        "0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    assert_eq!(check("research"), Err(Denial::ProfileUnowned));
    // The operator releases and re-assigns explicitly.
    store
        .transact(&operator(), NOW, None, |tx| {
            tx.remove_profile(&profile("research"))?;
            tx.assign_owner(&profile("research"), &id("alice"))
        })
        .unwrap();
    assert!(check("research").is_ok());
}

// 19. Single-user installations: nothing read, nothing written.
#[test]
fn an_installation_without_identity_state_is_untouched() {
    let home = home();
    let store = IdentityStore::new(&home.root);
    assert!(!store.exists());
    let document = store.load().unwrap();
    assert_eq!(document.generation(), 0);
    assert!(document.principals().next().is_none());
    assert!(
        !home.root.join("identity").exists(),
        "loading creates nothing"
    );
    // Unowned profiles stay operator-only.
    let empty = OwnershipRegistry::default();
    assert!(
        authorize(
            &operator(),
            Capability::RunsWrite,
            &profile("default"),
            &empty
        )
        .is_ok()
    );
    assert_eq!(
        authorize(
            &user("alice"),
            Capability::RunsWrite,
            &profile("default"),
            &empty
        ),
        Err(Denial::ProfileUnowned)
    );
}
