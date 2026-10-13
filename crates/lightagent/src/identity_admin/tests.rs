//! The operator identity CLI against a scratch home. Synthetic identities
//! only; nothing leaves the process.

use super::*;
use clap::Parser as _;
use lightagent_core::identity::{Denial, Restriction};
use lightagent_core::{AgentProfile, ProfileStore, RunId};

const NOW: u64 = 1_000_000;

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn home() -> Home {
    let root =
        std::env::temp_dir().join(format!("lightagent-identity-cli-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    Home(root)
}

fn create_profile(home: &Home, id: &str) {
    ProfileStore::new(&home.0)
        .create(&AgentProfile::new(
            ProfileId::new(id).unwrap(),
            "Bot",
            "Persona",
            "fast",
        ))
        .unwrap();
}

fn run(store: &IdentityStore, action: IdentityAction) -> Result<Executed, String> {
    execute(store, &operator(), NOW, &action)
}

fn ok(store: &IdentityStore, action: IdentityAction) -> Value {
    run(store, action).unwrap().report
}

fn user(id: &str) -> IdentityAction {
    IdentityAction::User {
        action: UserAction::Create { id: id.to_owned() },
    }
}

fn issue(principal: &str) -> IdentityAction {
    IdentityAction::Credential {
        action: CredentialAction::Issue {
            principal: principal.to_owned(),
            expires_in: None,
            capabilities: Vec::new(),
            profiles: Vec::new(),
            secret_file: None,
        },
    }
}

/// Every file the store wrote, as text.
fn persisted(home: &Home) -> String {
    let mut text = String::new();
    let mut pending = vec![home.0.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
    text
}

fn issued(store: &IdentityStore, principal: &str) -> (String, IssuedToken) {
    let executed = run(store, issue(principal)).unwrap();
    let key_id = executed.report["issued"].as_str().unwrap().to_owned();
    (key_id, executed.secret.unwrap())
}

// 1, 17. A secret exists only in the returned token: never in a report, a
// persisted file or an audit record.
#[test]
fn issuing_never_persists_or_reports_the_secret() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    let executed = run(&store, issue("alice")).unwrap();
    let token = executed.secret.unwrap();
    let secret = token.expose().rsplit('_').next().unwrap().to_owned();
    assert!(token.expose().starts_with("la_"));
    let report = executed.report.to_string();
    assert!(!report.contains(token.expose()) && !report.contains(&secret));
    let on_disk = persisted(&home);
    assert!(!on_disk.contains(&secret), "the secret was persisted");
    let audit = ok(&store, IdentityAction::Audit { limit: 100 }).to_string();
    assert!(!audit.contains(&secret));
    for listing in [
        ok(
            &store,
            IdentityAction::Credential {
                action: CredentialAction::List { principal: None },
            },
        ),
        ok(&store, IdentityAction::Status),
        ok(
            &store,
            IdentityAction::User {
                action: UserAction::List,
            },
        ),
    ] {
        let text = listing.to_string();
        assert!(
            !text.contains(&secret) && !text.contains("verifier"),
            "{text}"
        );
    }
    // The token works, against the durable state.
    let document = store.load().unwrap();
    let caller = document
        .credentials()
        .authenticate_at(token.expose(), NOW)
        .unwrap();
    assert_eq!(caller.principal.id.as_str(), "alice");
}

// 2. Rotation keeps the old credential until it is revoked; expiry is
// enforced at the stated time.
#[test]
fn rotation_and_expiry() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    let (old_id, old) = issued(&store, "alice");
    let rotated = run(
        &store,
        IdentityAction::Credential {
            action: CredentialAction::Rotate {
                key_id: old_id.clone(),
                expires_in: Some(60),
                secret_file: None,
            },
        },
    )
    .unwrap();
    let new = rotated.secret.unwrap();
    assert_eq!(rotated.report["replaces"], json!(old_id));
    assert_eq!(rotated.report["expires_unix"], json!(NOW + 60));
    let document = store.load().unwrap();
    let credentials = document.credentials();
    assert!(credentials.authenticate_at(old.expose(), NOW).is_ok());
    assert!(credentials.authenticate_at(new.expose(), NOW + 59).is_ok());
    assert_eq!(
        credentials.authenticate_at(new.expose(), NOW + 60),
        Err(Denial::Expired)
    );
    let listed = ok(
        &store,
        IdentityAction::Credential {
            action: CredentialAction::List {
                principal: Some("alice".to_owned()),
            },
        },
    );
    assert_eq!(listed["credentials"].as_array().unwrap().len(), 2);
}

// 3, 4. Revocation and suspension are durable and immediate.
#[test]
fn revocation_and_suspension() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    ok(&store, user("bob"));
    let (alice_id, alice) = issued(&store, "alice");
    let (_, alice_second) = issued(&store, "alice");
    let (_, bob) = issued(&store, "bob");
    ok(
        &store,
        IdentityAction::Credential {
            action: CredentialAction::Revoke { key_id: alice_id },
        },
    );
    let credentials = store.load().unwrap().credentials().clone();
    assert_eq!(
        credentials.authenticate_at(alice.expose(), NOW),
        Err(Denial::Revoked)
    );
    assert!(
        credentials
            .authenticate_at(alice_second.expose(), NOW)
            .is_ok()
    );
    let report = ok(
        &store,
        IdentityAction::User {
            action: UserAction::Suspend {
                id: "alice".to_owned(),
            },
        },
    );
    assert_eq!(report["credentials_revoked"], json!(true));
    let credentials = store.load().unwrap().credentials().clone();
    assert_eq!(
        credentials.authenticate_at(alice_second.expose(), NOW),
        Err(Denial::Revoked)
    );
    // Unrelated users keep working.
    assert!(credentials.authenticate_at(bob.expose(), NOW).is_ok());
    // A suspended user gets no new credential.
    assert_eq!(
        run(&store, issue("alice")).err().unwrap(),
        "refused: principal_inactive"
    );
}

// 5, 6. Ownership is assigned, moved and removed; grants are listed without
// secrets and can be revoked.
#[test]
fn ownership_assignment_reassignment_and_removal() {
    let home = home();
    create_profile(&home, "research");
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    ok(&store, user("bob"));
    let assign = |principal: &str| IdentityAction::Owner {
        action: OwnerAction::Assign {
            profile: "research".to_owned(),
            principal: principal.to_owned(),
        },
    };
    assert_eq!(ok(&store, assign("alice"))["assignment"], json!("created"));
    assert_eq!(
        ok(&store, assign("alice"))["assignment"],
        json!("unchanged")
    );
    assert!(
        run(&store, assign("bob")).is_err(),
        "a different owner conflicts"
    );
    let report = ok(
        &store,
        IdentityAction::Owner {
            action: OwnerAction::Reassign {
                profile: "research".to_owned(),
                principal: "bob".to_owned(),
            },
        },
    );
    assert_eq!(report["assignment"], json!("reassigned"));
    let listed = ok(&store, IdentityAction::Grants { action: None });
    assert_eq!(listed["profiles"][0]["owner"], json!("bob"));
    assert_eq!(listed["profiles"][0]["effective"], json!(true));
    ok(
        &store,
        IdentityAction::Grants {
            action: Some(GrantsAction::Revoke {
                profile: "research".to_owned(),
                principal: "alice".to_owned(),
                capability: Capability::RunsRead,
            }),
        },
    );
    ok(
        &store,
        IdentityAction::Owner {
            action: OwnerAction::Remove {
                profile: "research".to_owned(),
            },
        },
    );
    let listed = ok(
        &store,
        IdentityAction::Grants {
            action: Some(GrantsAction::List {
                profile: Some("research".to_owned()),
            }),
        },
    );
    assert_eq!(listed["profiles"], json!([]));
    let marker = ProfileStore::new(&home.0)
        .handle(&ProfileId::new("research").unwrap())
        .dir()
        .join(lightagent_core::identity::store::BINDING_FILE);
    assert!(!marker.exists(), "the binding marker is removed");
}

// 16. Invalid commands are refused with fixed codes and change nothing.
#[test]
fn invalid_operator_commands_are_refused() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    let before = store.load().unwrap().generation();
    let cases: [(IdentityAction, &str); 10] = [
        (user("Not Valid"), "invalid principal id"),
        (user("alice"), "refused: principal_exists"),
        (issue("nobody"), "refused: unknown_principal"),
        (
            IdentityAction::Credential {
                action: CredentialAction::Issue {
                    principal: "alice".to_owned(),
                    expires_in: Some(0),
                    capabilities: Vec::new(),
                    profiles: Vec::new(),
                    secret_file: None,
                },
            },
            "--expires-in must be at least one second",
        ),
        (
            IdentityAction::Credential {
                action: CredentialAction::Issue {
                    principal: "alice".to_owned(),
                    expires_in: None,
                    capabilities: vec![Capability::Administer],
                    profiles: Vec::new(),
                    secret_file: None,
                },
            },
            "refused: operator_only_restriction",
        ),
        (
            IdentityAction::Credential {
                action: CredentialAction::Issue {
                    principal: "alice".to_owned(),
                    expires_in: None,
                    capabilities: Vec::new(),
                    profiles: vec!["../escape".to_owned()],
                    secret_file: None,
                },
            },
            "invalid profile id",
        ),
        (
            IdentityAction::Credential {
                action: CredentialAction::Revoke {
                    key_id: "0000000000000000".to_owned(),
                },
            },
            "refused: unknown_credential",
        ),
        (
            IdentityAction::Credential {
                action: CredentialAction::Rotate {
                    key_id: "0000000000000000".to_owned(),
                    expires_in: None,
                    secret_file: None,
                },
            },
            "refused: unknown_credential",
        ),
        (
            IdentityAction::Owner {
                action: OwnerAction::Assign {
                    profile: "missing".to_owned(),
                    principal: "alice".to_owned(),
                },
            },
            "refused: profile_missing",
        ),
        (
            IdentityAction::User {
                action: UserAction::Suspend {
                    id: "nobody".to_owned(),
                },
            },
            "refused: unknown_principal",
        ),
    ];
    for (action, expected) in cases {
        let error = run(&store, action.clone()).err().unwrap_or_default();
        assert_eq!(error, expected, "{action:?}");
    }
    assert_eq!(
        store.load().unwrap().generation(),
        before,
        "nothing changed"
    );

    // A caller other than the local operator changes nothing.
    let mut outsider = operator();
    outsider.principal.role = Role::User;
    assert_eq!(
        execute(&store, &outsider, NOW, &user("mallory"))
            .err()
            .unwrap(),
        "not authorized: missing_capability"
    );
    let mut narrowed = operator();
    narrowed.restriction = Restriction {
        capabilities: Some([Capability::RunsRead].into()),
        profiles: None,
    };
    assert!(execute(&store, &narrowed, NOW, &user("mallory")).is_err());

    // Parsing refuses unknown capabilities and missing arguments.
    let invalid: [&[&str]; 3] = [
        &[
            "lightagent",
            "identity",
            "credential",
            "issue",
            "alice",
            "--capability",
            "everything",
        ],
        &["lightagent", "identity", "owner", "assign", "research"],
        &["lightagent", "identity", "activate"],
    ];
    for argv in invalid {
        assert!(
            crate::Cli::try_parse_from(argv.iter().copied()).is_err(),
            "{argv:?}"
        );
    }
    assert!(
        crate::Cli::try_parse_from([
            "lightagent",
            "identity",
            "credential",
            "issue",
            "alice",
            "--capability",
            "runs_read",
            "--expires-in",
            "3600",
        ])
        .is_ok()
    );
}

// Read-only commands never create the store; ordinary startup never reads
// or writes it.
#[test]
fn reading_never_initializes_the_store() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    let status = ok(&store, IdentityAction::Status);
    assert_eq!(status["store"], json!("absent"));
    assert_eq!(status["multi_user"], json!("not_activated"));
    for action in [
        IdentityAction::User {
            action: UserAction::List,
        },
        IdentityAction::Credential {
            action: CredentialAction::List { principal: None },
        },
        IdentityAction::Grants { action: None },
    ] {
        ok(&store, action);
    }
    assert!(!home.0.join("identity").exists());
    // The first change creates it, and says so.
    assert_eq!(ok(&store, user("alice"))["store_created"], json!(true));
    assert_eq!(ok(&store, user("bob"))["store_created"], json!(false));
}

// 15. A corrupt or newer store is refused, never replaced.
#[test]
fn a_corrupt_or_newer_store_fails_closed() {
    let home = home();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    let state = home.0.join("identity").join("state.json");
    for broken in [
        b"{not json".to_vec(),
        br#"{"schema": 99, "generation": 1}"#.to_vec(),
    ] {
        std::fs::write(&state, &broken).unwrap();
        assert!(run(&store, IdentityAction::Status).is_err());
        assert!(run(&store, issue("alice")).is_err());
        assert!(run(&store, user("bob")).is_err());
        assert_eq!(std::fs::read(&state).unwrap(), broken, "left for recovery");
    }
}

// 19. Identity administration touches only the identity store and the
// profile binding marker: settings (Jev routing and classification
// permissions included) are never read or changed.
#[test]
fn identity_changes_leave_settings_and_jev_permissions_alone() {
    let home = home();
    create_profile(&home, "research");
    let config = home.0.join("config.toml");
    let settings = "[platform.jev]\nenabled = true\nrouting_permission = \"strict\"\n";
    std::fs::write(&config, settings).unwrap();
    let store = IdentityStore::new(&home.0);
    ok(&store, user("alice"));
    issued(&store, "alice");
    ok(
        &store,
        IdentityAction::Owner {
            action: OwnerAction::Assign {
                profile: "research".to_owned(),
                principal: "alice".to_owned(),
            },
        },
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), settings);
}

// Delivery safety: JSON never carries a secret, an existing file is never
// overwritten, and a new one is owner-only.
#[test]
fn secret_delivery_is_guarded() {
    let home = home();
    assert!(
        delivery(None, true)
            .err()
            .unwrap()
            .contains("--json output never carries a credential")
    );
    let path = home.0.join("secret");
    let Delivery::File(file, _) = delivery(Some(&path), true).unwrap() else {
        panic!("a secret file was asked for");
    };
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(delivery(Some(&path), false).is_err(), "never overwritten");
}
