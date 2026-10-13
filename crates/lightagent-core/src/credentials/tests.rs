//! The credential vault against a scratch home. Synthetic secrets only.

use super::*;
use crate::identity::{PrincipalSource, Restriction};
use crate::{AgentProfile, ProfileStore, RunId};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

const SECRET: &str = "synthetic-remote-secret-0123456789";
const T0: u64 = 1_000_000;

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn home() -> Home {
    let root = std::env::temp_dir().join(format!("lightagent-vault-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    for id in ["alice-bot", "bob-bot"] {
        ProfileStore::new(&root)
            .create(&AgentProfile::new(
                ProfileId::new(id).unwrap(),
                id,
                "Persona",
                "m",
            ))
            .unwrap();
    }
    Home(root)
}

fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn spec(bot: &str, service: RemoteService) -> GrantSpec {
    GrantSpec {
        profile: profile(bot),
        service,
        operations: None,
        destinations: ["https://search.test".to_owned()].into(),
        expires_unix: None,
        max_requests_per_run: None,
    }
}

fn clock(at: &Arc<AtomicU64>) -> Clock {
    let at = Arc::clone(at);
    Arc::new(move || at.load(Ordering::SeqCst))
}

fn secret(value: &str) -> ExternalSecret {
    ExternalSecret::new(value).unwrap()
}

fn persisted(dir: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![dir.to_path_buf()];
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

// 14. No plaintext secret anywhere the vault writes; the key is not there
// either; secrets and keys never appear in Debug output.
#[test]
fn nothing_persisted_holds_the_secret_or_the_key() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let id = vault
        .add(
            &operator(),
            T0,
            &key,
            spec("alice-bot", RemoteService::WebSearch),
            &secret(SECRET),
        )
        .unwrap();
    let on_disk = persisted(&home.0);
    assert!(!on_disk.contains(SECRET), "plaintext persisted");
    assert!(!on_disk.contains(&key.to_hex()), "key persisted");
    assert!(on_disk.contains(&id));
    let grant = vault.load().unwrap().get(&id).unwrap().clone();
    assert_eq!(vault.open(&key, &grant).unwrap().expose(), SECRET);
    for rendered in [
        format!("{key:?}"),
        format!("{:?}", secret(SECRET)),
        format!("{grant:?}"),
        format!("{:?}", vault.load().unwrap()),
    ] {
        assert!(
            !rendered.contains(SECRET) && !rendered.contains(&key.to_hex()),
            "{rendered}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for file in vault.files() {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{}", file.display());
        }
    }
}

// 10, 11. The wrong key, tampering, a record moved to another profile or
// credential, a corrupt or newer document: all fail closed.
#[test]
fn opening_fails_closed() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let alice = vault
        .add(
            &operator(),
            T0,
            &key,
            spec("alice-bot", RemoteService::WebSearch),
            &secret(SECRET),
        )
        .unwrap();
    let bob = vault
        .add(
            &operator(),
            T0,
            &key,
            spec("bob-bot", RemoteService::WebSearch),
            &secret("bob-secret-value"),
        )
        .unwrap();
    let document = vault.load().unwrap();
    let alice_grant = document.get(&alice).unwrap().clone();
    let other = MasterKey::generate().unwrap();
    assert_eq!(
        vault.open(&other, &alice_grant).err(),
        Some(VaultError::KeyMismatch)
    );

    let mut tampered = alice_grant.clone();
    let mut bytes = unhex(&tampered.sealed.ciphertext).unwrap();
    bytes[0] ^= 1;
    tampered.sealed.ciphertext = hex(&bytes);
    assert_eq!(
        vault.open(&key, &tampered).err(),
        Some(VaultError::Integrity)
    );

    // Bob's sealed secret moved onto alice's record does not open.
    let mut moved = alice_grant.clone();
    moved.sealed = document.get(&bob).unwrap().sealed.clone();
    assert_eq!(vault.open(&key, &moved).err(), Some(VaultError::Integrity));
    // Nor alice's record relabelled for bob's profile.
    let mut relabelled = alice_grant.clone();
    relabelled.profile = profile("bob-bot");
    assert_eq!(
        vault.open(&key, &relabelled).err(),
        Some(VaultError::Integrity)
    );

    let state = home.0.join("credentials").join("state.json");
    for (broken, expected) in [
        (b"{not json".to_vec(), VaultError::Corrupt),
        (
            br#"{"schema": 9, "generation": 1}"#.to_vec(),
            VaultError::IncompatibleSchema(9),
        ),
        (br#"{"generation": 1}"#.to_vec(), VaultError::Corrupt),
    ] {
        std::fs::write(&state, &broken).unwrap();
        assert_eq!(vault.load().err(), Some(expected));
        assert!(
            vault
                .add(
                    &operator(),
                    T0,
                    &key,
                    spec("alice-bot", RemoteService::WebSearch),
                    &secret(SECRET)
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&state).unwrap(), broken, "never replaced");
        let at = Arc::new(AtomicU64::new(T0));
        assert!(matches!(
            resolve(
                &home.0,
                Some(&key),
                &profile("alice-bot"),
                &RemoteService::WebSearch,
                clock(&at)
            ),
            Resolved::Denied(CredentialDenial::Unavailable)
        ));
    }
}

// Key material: inside the home, group-readable, malformed: refused.
#[test]
fn the_key_must_be_well_formed_and_kept_apart() {
    let home = home();
    let key = MasterKey::generate().unwrap();
    let inside = home.0.join("credential.key");
    std::fs::write(&inside, key.to_hex()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(
        MasterKey::from_file(&inside, &home.0).err(),
        Some(VaultError::KeyInsideHome)
    );
    let outside_dir =
        std::env::temp_dir().join(format!("lightagent-vault-key-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("credential.key");
    std::fs::write(&outside, format!("{}\n", key.to_hex())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            MasterKey::from_file(&outside, &home.0).err(),
            Some(VaultError::KeyPermissions)
        );
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let loaded = MasterKey::from_file(&outside, &home.0).unwrap();
    assert_eq!(loaded.fingerprint(), key.fingerprint());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        // Not a regular file: refused.
        let directory = outside_dir.join("not-a-key");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            MasterKey::from_file(&directory, &home.0).err(),
            Some(VaultError::KeyPermissions)
        );
        // Owned by another account than the home's (a root-owned home
        // stands in for it): refused, whatever the mode.
        let root_owned = Path::new("/usr");
        if std::fs::metadata(&outside).unwrap().uid() != 0
            && std::fs::metadata(root_owned).is_ok_and(|meta| meta.uid() == 0)
        {
            assert_eq!(
                MasterKey::from_file(&outside, root_owned).err(),
                Some(VaultError::KeyPermissions)
            );
        }
        // An unreadable home proves no owner: refused.
        assert_eq!(
            MasterKey::from_file(&outside, &outside_dir.join("missing-home")).err(),
            Some(VaultError::KeyPermissions)
        );
    }
    for bad in ["", "zz", &key.to_hex()[..62]] {
        assert_eq!(MasterKey::from_hex(bad).err(), Some(VaultError::KeyInvalid));
    }
    std::fs::remove_dir_all(&outside_dir).ok();
}

// Only an operator holding `administer` changes the vault; invalid grants
// are refused and write nothing.
#[test]
fn changes_are_operator_only_and_validated() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let mut user = operator();
    user.principal.role = Role::User;
    assert_eq!(
        vault
            .add(
                &user,
                T0,
                &key,
                spec("alice-bot", RemoteService::WebSearch),
                &secret(SECRET)
            )
            .err(),
        Some(VaultError::Denied(Denial::MissingCapability))
    );
    let mut narrowed = operator();
    narrowed.restriction = Restriction {
        capabilities: Some([Capability::Provision].into()),
        profiles: None,
    };
    assert!(
        vault
            .add(
                &narrowed,
                T0,
                &key,
                spec("alice-bot", RemoteService::WebSearch),
                &secret(SECRET)
            )
            .is_err()
    );
    let mut cases = Vec::new();
    for destinations in [
        BTreeSet::new(),
        ["https://user:pw@search.test".to_owned()].into(),
        ["https://search.test/path".to_owned()].into(),
        ["ftp://search.test".to_owned()].into(),
    ] {
        let mut bad = spec("alice-bot", RemoteService::WebSearch);
        bad.destinations = destinations;
        cases.push(bad);
    }
    let mut expired = spec("alice-bot", RemoteService::WebSearch);
    expired.expires_unix = Some(T0);
    cases.push(expired);
    let mut zero = spec("alice-bot", RemoteService::WebSearch);
    zero.max_requests_per_run = Some(0);
    cases.push(zero);
    for bad in cases {
        assert!(
            vault
                .add(&operator(), T0, &key, bad, &secret(SECRET))
                .is_err()
        );
    }
    for bad in ["", "  ", "line\nbreak"] {
        assert!(ExternalSecret::new(bad).is_err());
    }
    assert!(!vault.exists(), "nothing written");
    for text in ["web-search", "mcp:git", "mcp:my_server-2"] {
        assert_eq!(RemoteService::parse(text).unwrap().label(), text);
    }
    for text in ["mcp:", "mcp:a.b", "web", "mcp:../x"] {
        assert!(RemoteService::parse(text).is_none(), "{text}");
    }
}

// 1, 2. Resolution is per profile and service: another profile's grant,
// the same service for an ungranted profile, or another service: nothing.
#[test]
fn a_profile_resolves_only_its_own_grant() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let at = Arc::new(AtomicU64::new(T0));
    assert!(matches!(
        resolve(
            &home.0,
            Some(&key),
            &profile("alice-bot"),
            &RemoteService::WebSearch,
            clock(&at)
        ),
        Resolved::Absent
    ));
    vault
        .add(
            &operator(),
            T0,
            &key,
            spec("bob-bot", RemoteService::WebSearch),
            &secret(SECRET),
        )
        .unwrap();
    for (bot, service) in [
        ("alice-bot", RemoteService::WebSearch),
        (
            "bob-bot",
            RemoteService::Mcp {
                server: "git".into(),
            },
        ),
    ] {
        assert!(matches!(
            resolve(&home.0, Some(&key), &profile(bot), &service, clock(&at)),
            Resolved::Absent
        ));
    }
    let Resolved::Active {
        secret: opened,
        guard,
    } = resolve(
        &home.0,
        Some(&key),
        &profile("bob-bot"),
        &RemoteService::WebSearch,
        clock(&at),
    )
    else {
        panic!("bob's grant resolves for bob");
    };
    assert_eq!(opened.expose(), SECRET);
    assert_eq!(
        guard.authorize(Some("web.search"), "https://search.test/?q=x"),
        Ok(())
    );
    // No key: denied, not absent (so nothing falls back).
    assert!(matches!(
        resolve(
            &home.0,
            None,
            &profile("bob-bot"),
            &RemoteService::WebSearch,
            clock(&at)
        ),
        Resolved::Denied(CredentialDenial::Unavailable)
    ));
}

// 4, 5, 6, 9, 12. Each request re-reads the vault: expiry, revocation and
// rotation stop the next request; destinations, operations and the budget
// are enforced; concurrent use never outlives a revocation.
#[test]
fn every_request_is_checked_against_the_current_grant() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let at = Arc::new(AtomicU64::new(T0));
    let mut limited = spec(
        "alice-bot",
        RemoteService::Mcp {
            server: "git".into(),
        },
    );
    limited.destinations = ["https://mcp.test:8443".to_owned()].into();
    limited.operations = Some(["status".to_owned()].into());
    limited.max_requests_per_run = Some(2);
    limited.expires_unix = Some(T0 + 100);
    let id = vault
        .add(&operator(), T0, &key, limited, &secret(SECRET))
        .unwrap();
    let Resolved::Active { guard, .. } = resolve(
        &home.0,
        Some(&key),
        &profile("alice-bot"),
        &RemoteService::Mcp {
            server: "git".into(),
        },
        clock(&at),
    ) else {
        panic!("active");
    };
    let url = "https://mcp.test:8443/mcp";
    assert_eq!(
        guard.authorize(Some("status"), "https://other.test/mcp"),
        Err(CredentialDenial::Destination)
    );
    assert_eq!(
        guard.authorize(Some("status"), "http://mcp.test:8443/mcp"),
        Err(CredentialDenial::Destination)
    );
    assert_eq!(
        guard.authorize(Some("push"), url),
        Err(CredentialDenial::Operation)
    );
    assert_eq!(guard.authorize(Some("status"), url), Ok(()));
    assert_eq!(guard.authorize(Some("status"), url), Ok(()));
    assert_eq!(
        guard.authorize(Some("status"), url),
        Err(CredentialDenial::Budget)
    );
    // A new run has a new budget.
    let Resolved::Active { guard, .. } = resolve(
        &home.0,
        Some(&key),
        &profile("alice-bot"),
        &RemoteService::Mcp {
            server: "git".into(),
        },
        clock(&at),
    ) else {
        panic!("active");
    };
    assert_eq!(guard.authorize(Some("status"), url), Ok(()));
    // Rotation supersedes this run's version.
    vault
        .rotate(
            &operator(),
            T0 + 1,
            &key,
            &id,
            &secret("rotated-secret-value"),
        )
        .unwrap();
    assert_eq!(
        guard.authorize(Some("status"), url),
        Err(CredentialDenial::Superseded)
    );
    let Resolved::Active {
        secret: rotated,
        guard,
    } = resolve(
        &home.0,
        Some(&key),
        &profile("alice-bot"),
        &RemoteService::Mcp {
            server: "git".into(),
        },
        clock(&at),
    )
    else {
        panic!("active");
    };
    assert_eq!(rotated.expose(), "rotated-secret-value");
    // Expiry, then revocation.
    at.store(T0 + 100, Ordering::SeqCst);
    assert_eq!(
        guard.authorize(Some("status"), url),
        Err(CredentialDenial::Expired)
    );
    at.store(T0 + 50, Ordering::SeqCst);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let guard = Arc::clone(&guard);
            std::thread::spawn(move || guard.authorize(Some("status"), url))
        })
        .collect();
    vault.revoke(&operator(), T0 + 2, &id).unwrap();
    for thread in threads {
        // Before the revocation or refused; the budget (2) caps successes.
        let _ = thread.join().unwrap();
    }
    assert_eq!(
        guard.authorize(Some("status"), url),
        Err(CredentialDenial::Revoked)
    );
    assert!(matches!(
        resolve(
            &home.0,
            Some(&key),
            &profile("alice-bot"),
            &RemoteService::Mcp {
                server: "git".into()
            },
            clock(&at),
        ),
        Resolved::Denied(CredentialDenial::Revoked)
    ));
    assert_eq!(
        vault
            .rotate(&operator(), T0 + 3, &key, &id, &secret("x-value"))
            .err(),
        Some(VaultError::Refused("revoked"))
    );
    // Denials are audited with codes only.
    let audit = serde_json::to_string(&vault.audit().unwrap()).unwrap();
    for code in [
        "denied:destination",
        "denied:budget",
        "denied:superseded",
        "denied:revoked",
        "credential_rotated",
        "credential_revoked",
    ] {
        assert!(audit.contains(code), "{code}");
    }
    assert!(!audit.contains(SECRET) && !audit.contains("rotated-secret-value"));
}

// Racing requests of one run never exceed its budget: exactly the budget
// succeeds, every other request is refused with `budget`.
#[test]
fn concurrent_requests_never_exceed_the_budget() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let key = MasterKey::generate().unwrap();
    let at = Arc::new(AtomicU64::new(T0));
    let service = RemoteService::Mcp {
        server: "git".into(),
    };
    let mut limited = spec("alice-bot", service.clone());
    limited.destinations = ["https://mcp.test:8443".to_owned()].into();
    limited.max_requests_per_run = Some(5);
    vault
        .add(&operator(), T0, &key, limited, &secret(SECRET))
        .unwrap();
    let Resolved::Active { guard, .. } = resolve(
        &home.0,
        Some(&key),
        &profile("alice-bot"),
        &service,
        clock(&at),
    ) else {
        panic!("active");
    };
    let start = Arc::new(std::sync::Barrier::new(64));
    let threads: Vec<_> = (0..64)
        .map(|_| {
            let guard = Arc::clone(&guard);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                guard.authorize(None, "https://mcp.test:8443/mcp")
            })
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 5);
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Ok(()) | Err(CredentialDenial::Budget)))
    );
}

// Controlled key rotation: every active grant is re-sealed or none is.
#[test]
fn rekeying_is_all_or_nothing() {
    let home = home();
    let vault = CredentialVault::new(&home.0);
    let old = MasterKey::generate().unwrap();
    let new = MasterKey::generate().unwrap();
    let a = vault
        .add(
            &operator(),
            T0,
            &old,
            spec("alice-bot", RemoteService::WebSearch),
            &secret(SECRET),
        )
        .unwrap();
    let b = vault
        .add(
            &operator(),
            T0,
            &old,
            spec("bob-bot", RemoteService::WebSearch),
            &secret("bob-secret-value"),
        )
        .unwrap();
    let wrong = MasterKey::generate().unwrap();
    assert_eq!(
        vault.rekey(&operator(), T0, &wrong, &new).err(),
        Some(VaultError::KeyMismatch)
    );
    let before = vault.load().unwrap();
    assert_eq!(vault.rekey(&operator(), T0, &old, &new).unwrap(), 2);
    let after = vault.load().unwrap();
    assert_eq!(after.generation(), before.generation() + 1);
    for (id, value) in [(&a, SECRET), (&b, "bob-secret-value")] {
        let grant = after.get(id).unwrap();
        assert_eq!(vault.open(&new, grant).unwrap().expose(), value);
        assert_eq!(vault.open(&old, grant).err(), Some(VaultError::KeyMismatch));
    }
}
