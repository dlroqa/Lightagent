//! The operator remote-credential CLI against a scratch home. Synthetic
//! secrets only.

use super::*;
use clap::Parser as _;
use lightagent_core::identity::Role;
use lightagent_core::{AgentProfile, RunId};

const T0: u64 = 1_000_000;
const SECRET: &str = "synthetic-cli-secret-value-42";

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn home() -> Home {
    let root =
        std::env::temp_dir().join(format!("lightagent-remote-cli-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    ProfileStore::new(&root)
        .create(&AgentProfile::new(
            ProfileId::new("alice-bot").unwrap(),
            "Alice",
            "Persona",
            "m",
        ))
        .unwrap();
    Home(root)
}

fn add(service: &str, destination: &str) -> RemoteCredentialAction {
    RemoteCredentialAction::Add {
        profile: "alice-bot".into(),
        service: service.into(),
        destinations: vec![destination.into()],
        operations: Vec::new(),
        expires_in: None,
        max_requests_per_run: None,
        secret: SecretInput {
            secret_file: None,
            secret_stdin: true,
        },
    }
}

fn secret() -> Option<ExternalSecret> {
    Some(ExternalSecret::new(SECRET).unwrap())
}

#[test]
fn credentials_are_added_listed_rotated_and_revoked_without_disclosure() {
    let home = home();
    let key = MasterKey::generate().unwrap();
    let report = execute(
        &home.0,
        &operator(),
        T0,
        Some(&key),
        &add("web-search", "https://search.test"),
        secret(),
    )
    .unwrap();
    let id = report["added"].as_str().unwrap().to_owned();
    assert_eq!(report["key_fingerprint"], json!(key.fingerprint()));
    let listed = execute(
        &home.0,
        &operator(),
        T0,
        Some(&key),
        &RemoteCredentialAction::List { profile: None },
        None,
    )
    .unwrap();
    assert_eq!(listed["credentials"][0]["state"], json!("active"));
    let rotated = execute(
        &home.0,
        &operator(),
        T0 + 1,
        Some(&key),
        &RemoteCredentialAction::Rotate {
            id: id.clone(),
            secret: SecretInput {
                secret_file: None,
                secret_stdin: true,
            },
        },
        Some(ExternalSecret::new("rotated-cli-secret").unwrap()),
    )
    .unwrap();
    assert_eq!(rotated["version"], json!(2));
    execute(
        &home.0,
        &operator(),
        T0 + 2,
        None,
        &RemoteCredentialAction::Revoke { id: id.clone() },
        None,
    )
    .unwrap();
    let status = execute(
        &home.0,
        &operator(),
        T0,
        Some(&key),
        &RemoteCredentialAction::Status,
        None,
    )
    .unwrap();
    assert_eq!(status["grants"]["revoked"], json!(1));
    let audit = execute(
        &home.0,
        &operator(),
        T0,
        None,
        &RemoteCredentialAction::Audit { limit: 10 },
        None,
    )
    .unwrap();
    for text in [
        report.to_string(),
        listed.to_string(),
        rotated.to_string(),
        status.to_string(),
        audit.to_string(),
    ] {
        assert!(
            !text.contains(SECRET)
                && !text.contains("rotated-cli-secret")
                && !text.contains(&key.to_hex())
        );
        assert!(!text.contains("ciphertext"), "{text}");
    }
    assert!(audit.to_string().contains("credential_revoked"));
}

#[test]
fn invalid_commands_change_nothing() {
    let home = home();
    let key = MasterKey::generate().unwrap();
    let cases: [(RemoteCredentialAction, Option<&MasterKey>, &str); 6] = [
        (
            add("web-search", "https://search.test"),
            None,
            "no credential key is configured",
        ),
        (
            add("ftp", "https://search.test"),
            Some(&key),
            "service must be",
        ),
        (
            add("web-search", "https://search.test/path"),
            Some(&key),
            "must be scheme://host",
        ),
        (
            add("web-search", "https://u:p@search.test"),
            Some(&key),
            "must be scheme://host",
        ),
        (
            RemoteCredentialAction::Add {
                profile: "missing-bot".into(),
                service: "web-search".into(),
                destinations: vec!["https://search.test".into()],
                operations: Vec::new(),
                expires_in: None,
                max_requests_per_run: None,
                secret: SecretInput {
                    secret_file: None,
                    secret_stdin: true,
                },
            },
            Some(&key),
            "profile_missing",
        ),
        (
            RemoteCredentialAction::Revoke {
                id: "0000000000000000".into(),
            },
            Some(&key),
            "unknown_credential",
        ),
    ];
    for (action, key, expected) in cases {
        let error = execute(&home.0, &operator(), T0, key, &action, secret()).unwrap_err();
        assert!(error.contains(expected), "{action:?}: {error}");
    }
    assert!(
        !CredentialVault::new(&home.0).exists(),
        "nothing was written"
    );
    // A non-operator changes nothing.
    let mut user = operator();
    user.principal.role = Role::User;
    assert!(
        execute(
            &home.0,
            &user,
            T0,
            Some(&key),
            &add("web-search", "https://search.test"),
            secret()
        )
        .is_err()
    );
    assert!(!CredentialVault::new(&home.0).exists());
    // A secret is never accepted on the command line, and a source is required.
    let refused: [&[&str]; 3] = [
        &[
            "lightagent",
            "remote-credential",
            "add",
            "alice-bot",
            "--service",
            "web-search",
            "--destination",
            "https://search.test",
        ],
        &[
            "lightagent",
            "remote-credential",
            "add",
            "alice-bot",
            "--service",
            "web-search",
            "--destination",
            "https://search.test",
            "--secret",
            SECRET,
        ],
        &[
            "lightagent",
            "remote-credential",
            "rotate",
            "abc",
            "--secret-file",
            "a",
            "--secret-stdin",
        ],
    ];
    for argv in refused {
        assert!(
            crate::Cli::try_parse_from(argv.iter().copied()).is_err(),
            "{argv:?}"
        );
    }
    // Reading never creates the vault.
    let status = execute(
        &home.0,
        &operator(),
        T0,
        None,
        &RemoteCredentialAction::Status,
        None,
    )
    .unwrap();
    assert_eq!(status["vault"], json!("absent"));
    assert!(!home.0.join("credentials").join("state.json").exists());
}

#[test]
fn keygen_writes_a_new_owner_only_key_outside_the_home() {
    let home = home();
    let inside = home.0.join("key");
    assert!(keygen(&inside, &home.0).unwrap_err().contains("outside"));
    let outside_dir =
        std::env::temp_dir().join(format!("lightagent-remote-key-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&outside_dir).unwrap();
    let path = outside_dir.join("credential.key");
    let report = keygen(&path, &home.0).unwrap();
    let key = MasterKey::from_file(&path, &home.0).unwrap();
    assert_eq!(report["key_fingerprint"], json!(key.fingerprint()));
    assert!(!report.to_string().contains(&key.to_hex()));
    assert!(keygen(&path, &home.0).is_err(), "never overwritten");
    std::fs::remove_dir_all(&outside_dir).ok();
}
