//! The provisioning boundary against scratch homes. Synthetic identities,
//! issuers and tokens only.

use super::*;
use crate::identity::store::Transaction;
use crate::identity::{Denial, Restriction, authorize};
use crate::{ProfileStore, RunId};

const T0: u64 = 1_000_000;
const ISSUER: &str = "https://accounts.control.example";
const OTHER_ISSUER: &str = "https://login.other.example";
const SUBJECT: &str = "synthetic-subject-0001";

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

fn principal(id: &str) -> PrincipalId {
    PrincipalId::new(id).unwrap()
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

/// A home whose identity store trusts two issuers and holds the operator
/// changes `change` makes.
fn operator_change<R>(home: &Path, change: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
    let mut value = None;
    IdentityStore::new(home)
        .transact(&operator(), T0, None, |tx| {
            value = Some(change(tx));
            Ok(())
        })
        .unwrap();
    value.unwrap()
}

fn issue(home: &Path, id: &str, capabilities: Option<&[Capability]>) -> String {
    operator_change(home, |tx| {
        tx.issue_credential(
            &principal(id),
            Restriction {
                capabilities: capabilities.map(|set| set.iter().copied().collect()),
                profiles: None,
            },
            None,
        )
        .unwrap()
        .1
        .expose()
        .to_owned()
    })
}

/// The caller a presented token authenticates as, now.
fn caller(home: &Path, token: &str) -> Caller {
    IdentityStore::new(home)
        .load()
        .unwrap()
        .credentials()
        .authenticate_at(token, T0)
        .unwrap()
}

struct World {
    home: Home,
    service: ProvisioningService,
    /// The registered control service's provisioning-only token.
    site: String,
}

fn service(home: &Path) -> ProvisioningService {
    ProvisioningService::new(home).with_clock(Arc::new(|| T0))
}

/// `control-site` is a registered provisioner for [`ISSUER`]; both issuers
/// are trusted by the identity store.
fn world() -> World {
    let root = std::env::temp_dir().join(format!("lightagent-provision-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    operator_change(&root, |tx| {
        tx.trust_issuer(ISSUER).unwrap();
        tx.trust_issuer(OTHER_ISSUER).unwrap();
        tx.create_principal(principal("control-site"), Role::Operator, None)
            .unwrap();
        tx.create_principal(principal("other-site"), Role::Operator, None)
            .unwrap();
    });
    let site = issue(&root, "control-site", Some(&[Capability::Provision]));
    let service = service(&root);
    service
        .register_provisioner(
            &operator(),
            &principal("control-site"),
            [ISSUER.to_owned()].into(),
            ["assistant".to_owned()].into(),
            10,
        )
        .unwrap();
    World {
        home: Home(root),
        service,
        site,
    }
}

impl World {
    fn root(&self) -> &Path {
        &self.home.0
    }

    fn site(&self) -> Caller {
        caller(self.root(), &self.site)
    }

    fn provision(
        &self,
        key: &str,
        request: &ProvisionRequest,
    ) -> Result<ProvisionOutcome, ProvisioningError> {
        self.service.provision(&self.site(), key, request)
    }

    fn identity(&self) -> IdentityDocument {
        IdentityStore::new(self.root()).load().unwrap()
    }

    fn effective(&self) -> crate::identity::OwnershipRegistry {
        let store = IdentityStore::new(self.root());
        store.effective_ownership(&store.load().unwrap())
    }

    fn profile_dir(&self, id: &str) -> PathBuf {
        ProfileStore::new(self.root())
            .handle(&profile(id))
            .dir()
            .to_path_buf()
    }

    /// Every byte the boundary and the identity store persisted.
    fn persisted(&self) -> String {
        let mut text = String::new();
        let mut stack = vec![self.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
                }
            }
        }
        text
    }
}

fn request(subject: &str, profile_id: &str) -> ProvisionRequest {
    ProvisionRequest {
        issuer: ISSUER.to_owned(),
        subject: subject.to_owned(),
        profile_id: profile_id.to_owned(),
        display_name: "Research assistant".to_owned(),
        template: "assistant".to_owned(),
    }
}

// --- provisioning ----------------------------------------------------------

// A profile is created from the template, with no tools and the strict
// policy, and bound to a principal the server derived from the external
// identity. Nothing persisted holds the provisioning token.
#[test]
fn provisions_a_restrictive_profile_owned_by_a_derived_principal() {
    let world = world();
    let outcome = world
        .provision("key-00000001", &request(SUBJECT, "alice-research"))
        .unwrap();
    assert!(!outcome.replayed);
    let status = outcome.status;
    assert_eq!(status.state, OperationState::Completed);
    assert!(status.owner_bound);
    let external = ExternalIdentity {
        issuer: ISSUER.to_owned(),
        subject: SUBJECT.to_owned(),
    };
    assert_eq!(status.principal_id, derived_principal(&external).unwrap());
    assert!(status.principal_id.as_str().starts_with("ext-"));

    let identity = world.identity();
    let record = identity.principal(&status.principal_id).unwrap();
    assert_eq!(record.role, Role::User);
    assert_eq!(record.external.as_ref(), Some(&external));
    assert_eq!(identity.external(&external), Some(&status.principal_id));
    assert_eq!(
        world
            .effective()
            .get(&profile("alice-research"))
            .map(|record| record.owner.clone()),
        Some(status.principal_id.clone())
    );

    let created = ProfileStore::new(world.root())
        .load(&profile("alice-research"))
        .unwrap();
    assert!(created.toolsets.is_empty(), "no tools");
    assert_eq!(created.approval_policy, ApprovalPolicy::Strict);
    assert_eq!(created.limits.max_tool_calls, 16);
    assert_eq!(created.limits.wall_clock_secs, Some(300));
    assert!(created.routing.model.is_empty(), "the installation default");
    assert_eq!(created.name, "Research assistant");

    // No staging left behind; the token appears nowhere on disk.
    assert!(
        std::fs::read_dir(world.service.root().join(STAGING))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true)
    );
    assert!(!world.persisted().contains(&world.site));
}

// The owner gets the minimum bundle (C6) and nothing more; the provisioning
// credential gets no runtime or administrative capability at all.
#[test]
fn provisioning_never_becomes_runtime_or_operator_authority() {
    let world = world();
    let status = world
        .provision("key-00000002", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    let site = world.site();
    let target = profile("alice-research");
    let effective = world.effective();
    for capability in Capability::ALL {
        if capability == Capability::Provision {
            continue;
        }
        assert_eq!(
            authorize(&site, capability, &target, &effective),
            Err(Denial::MissingCapability),
            "{capability:?}"
        );
        assert_eq!(
            authorize_capability(&site, capability),
            Err(Denial::MissingCapability)
        );
    }
    // Identity administration beyond provisioning is refused.
    let store = IdentityStore::new(world.root());
    let denied = |change: &dyn Fn(&mut Transaction<'_>) -> Result<(), IdentityStoreError>| {
        matches!(
            store.transact(&site, T0, None, |tx| change(tx)),
            Err(IdentityStoreError::Denied(Denial::MissingCapability))
        )
    };
    assert!(denied(&|tx| tx.create_principal(
        principal("sneaky-operator"),
        Role::Operator,
        None
    )));
    assert!(denied(&|tx| tx.trust_issuer("https://evil.example")));
    assert!(denied(&|tx| tx
        .reassign_owner(&target, &principal("control-site"))
        .map(|_| ())));
    assert!(denied(&|tx| tx.grant(
        &target,
        &status.principal_id,
        Capability::ToolsRead
    )));
    // Nor can it manage provisioners or reconcile.
    assert_eq!(
        world.service.register_provisioner(
            &site,
            &principal("control-site"),
            [ISSUER.to_owned()].into(),
            ["assistant".to_owned()].into(),
            1_000,
        ),
        Err(ProvisioningError::Denied("operator_required"))
    );
    assert_eq!(
        world.service.reconcile(&site),
        Err(ProvisioningError::Denied("operator_required"))
    );
}

// Invalid and over-broad credentials are refused, and nothing is recorded.
#[test]
fn only_a_registered_provisioning_only_credential_is_admitted() {
    let world = world();
    let attempt = |caller: &Caller| {
        world
            .service
            .provision(caller, "key-00000003", &request(SUBJECT, "alice-research"))
            .err()
    };
    let unauthenticated = Some(ProvisioningError::Denied("unauthenticated"));
    // The local operator, the legacy key and the compatibility mode are not
    // provisioning credentials.
    for source in [
        PrincipalSource::LocalProcess,
        PrincipalSource::LegacyOperatorKey,
        PrincipalSource::LocalCompatibility,
    ] {
        assert_eq!(attempt(&Caller::local_operator(source)), unauthenticated);
    }
    // A forged key id, or a real one under another principal's name.
    let mut forged = world.site();
    forged.principal.source = PrincipalSource::Credential {
        key_id: "0000000000000000".into(),
    };
    assert_eq!(attempt(&forged), unauthenticated);
    let mut renamed = world.site();
    renamed.principal.id = principal("other-site");
    assert_eq!(attempt(&renamed), unauthenticated);
    // An operator credential that is not exactly `{provision}`.
    for capabilities in [
        None,
        Some(&[Capability::Provision, Capability::Administer][..]),
        Some(&[Capability::Provision, Capability::RunsWrite][..]),
    ] {
        let token = issue(world.root(), "control-site", capabilities);
        assert_eq!(
            attempt(&caller(world.root(), &token)),
            Some(ProvisioningError::Denied(
                "credential_not_provisioning_only"
            ))
        );
    }
    // A provisioning-only credential of a principal never registered.
    let unregistered = issue(world.root(), "other-site", Some(&[Capability::Provision]));
    assert_eq!(
        attempt(&caller(world.root(), &unregistered)),
        Some(ProvisioningError::Denied("not_a_provisioner"))
    );
    // A user credential.
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("mallory"), Role::User, None)
            .unwrap();
    });
    let user = issue(world.root(), "mallory", None);
    assert_eq!(
        attempt(&caller(world.root(), &user)),
        Some(ProvisioningError::Denied("not_a_provisioner"))
    );
    assert_eq!(world.service.load().unwrap().operations().count(), 0);
    assert!(!world.profile_dir("alice-research").exists());
    // Every refusal was audited by code.
    let audit = world.service.audit_log().unwrap();
    assert!(
        audit
            .iter()
            .any(|line| line.outcome == "refused:credential_not_provisioning_only")
    );
    assert!(
        audit
            .iter()
            .any(|line| line.outcome == "refused:unauthenticated")
    );
}

// An issuer is trusted only when an operator registered it in the identity
// store *and* allowed it for this provisioner. A well-known name is not
// enough.
#[test]
fn only_operator_registered_issuers_are_accepted() {
    let world = world();
    let mut other = request(SUBJECT, "alice-research");
    other.issuer = OTHER_ISSUER.to_owned();
    assert_eq!(
        world.provision("key-00000004", &other).err(),
        Some(ProvisioningError::Denied("issuer_not_allowed"))
    );
    for name in [
        "https://accounts.google.com",
        "https://github.com",
        "https://cloudflareaccess.com",
    ] {
        let mut named = request(SUBJECT, "alice-research");
        named.issuer = name.to_owned();
        assert_eq!(
            world.provision("key-00000005", &named).err(),
            Some(ProvisioningError::Denied("issuer_not_allowed"))
        );
    }
    // A provisioner cannot be registered for an issuer the identity store
    // does not trust.
    assert_eq!(
        world.service.register_provisioner(
            &operator(),
            &principal("other-site"),
            ["https://accounts.google.com".to_owned()].into(),
            ["assistant".to_owned()].into(),
            1,
        ),
        Err(ProvisioningError::Invalid("issuer_not_trusted"))
    );
    // Nor a user, nor an unknown principal.
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("mallory"), Role::User, None)
            .unwrap();
    });
    for who in ["mallory", "nobody"] {
        assert!(matches!(
            world.service.register_provisioner(
                &operator(),
                &principal(who),
                [ISSUER.to_owned()].into(),
                ["assistant".to_owned()].into(),
                1,
            ),
            Err(ProvisioningError::Invalid(_))
        ));
    }
    assert!(!world.profile_dir("alice-research").exists());
}

// Every external identifier is validated; a request cannot carry profile
// configuration, a path or a tool grant.
#[test]
fn malformed_requests_and_configuration_are_refused() {
    let world = world();
    let long = "s".repeat(256);
    let cases: Vec<(&str, ProvisionRequest, &str)> = vec![
        (
            "issuer-http",
            ProvisionRequest {
                issuer: "http://accounts.control.example".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_issuer",
        ),
        (
            "issuer-userinfo",
            ProvisionRequest {
                issuer: "https://x@accounts.control.example".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_issuer",
        ),
        (
            "issuer-space",
            ProvisionRequest {
                issuer: "https://accounts .example".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_issuer",
        ),
        (
            "issuer-empty-host",
            ProvisionRequest {
                issuer: "https:///x".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_issuer",
        ),
        ("subject-empty", request("", "p1"), "invalid_subject"),
        ("subject-space", request("a b", "p1"), "invalid_subject"),
        (
            "subject-unicode",
            request("sübject", "p1"),
            "invalid_subject",
        ),
        (
            "subject-control",
            request("a\u{7}b", "p1"),
            "invalid_subject",
        ),
        ("subject-long", request(&long, "p1"), "invalid_subject"),
        (
            "profile-path",
            request(SUBJECT, "../etc"),
            "invalid_profile_id",
        ),
        (
            "profile-upper",
            request(SUBJECT, "Alice"),
            "invalid_profile_id",
        ),
        (
            "profile-abs",
            request(SUBJECT, "/tmp/x"),
            "invalid_profile_id",
        ),
        (
            "profile-reserved",
            request(SUBJECT, "default"),
            "reserved_profile_id",
        ),
        (
            "name-empty",
            ProvisionRequest {
                display_name: String::new(),
                ..request(SUBJECT, "p1")
            },
            "invalid_display_name",
        ),
        (
            "name-long",
            ProvisionRequest {
                display_name: "n".repeat(65),
                ..request(SUBJECT, "p1")
            },
            "invalid_display_name",
        ),
        (
            "name-control",
            ProvisionRequest {
                display_name: "a\nb".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_display_name",
        ),
        (
            "name-padded",
            ProvisionRequest {
                display_name: " a".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_display_name",
        ),
        (
            "template",
            ProvisionRequest {
                template: "operator".into(),
                ..request(SUBJECT, "p1")
            },
            "invalid_template",
        ),
    ];
    for (label, bad, code) in cases {
        assert_eq!(
            world.provision(&format!("key-{label}"), &bad).err(),
            Some(ProvisioningError::Invalid(code)),
            "{label}"
        );
    }
    for key in [
        "short",
        "has/slash-0001",
        "has space 0001",
        &"k".repeat(129),
    ] {
        assert_eq!(
            world.provision(key, &request(SUBJECT, "p1")).err(),
            Some(ProvisioningError::Invalid("invalid_idempotency_key"))
        );
    }
    // Configuration fields are not part of the contract: refused, not ignored.
    let base = r#""issuer":"https://accounts.control.example","subject":"s","profile_id":"p1","display_name":"n","template":"assistant""#;
    for extra in [
        r#""toolsets":["all"]"#,
        r#""approval_policy":"permissive""#,
        r#""model":"big""#,
        r#""path":"/etc""#,
        r#""owner":"control-site""#,
        r#""principal_id":"ext-0""#,
        r#""role":"operator""#,
        r#""credentials":["web-search"]"#,
    ] {
        assert!(
            serde_json::from_str::<ProvisionRequest>(&format!("{{{base},{extra}}}")).is_err(),
            "{extra}"
        );
    }
    assert!(serde_json::from_str::<ProvisionRequest>(&format!("{{{base}}}")).is_ok());
    assert_eq!(world.service.load().unwrap().operations().count(), 0);
    // The request's Debug never prints the subject.
    assert!(!format!("{:?}", request(SUBJECT, "p1")).contains(SUBJECT));
}

// --- idempotency -----------------------------------------------------------

#[test]
fn the_same_key_and_request_is_one_operation() {
    let world = world();
    let first = world
        .provision("key-00000010", &request(SUBJECT, "alice-research"))
        .unwrap();
    let generation = world.identity().generation();
    let again = world
        .provision("key-00000010", &request(SUBJECT, "alice-research"))
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.status, first.status);
    assert_eq!(
        world.identity().generation(),
        generation,
        "no identity change"
    );
    assert_eq!(world.service.load().unwrap().operations().count(), 1);
    // A different key for the same profile is a new request: refused.
    assert_eq!(
        world
            .provision("key-00000011", &request(SUBJECT, "alice-research"))
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable"))
    );
}

#[test]
fn a_reused_key_with_different_parameters_is_refused() {
    let world = world();
    world
        .provision("key-00000020", &request(SUBJECT, "alice-research"))
        .unwrap();
    for changed in [
        request("synthetic-subject-0002", "alice-research"),
        request(SUBJECT, "alice-other"),
        ProvisionRequest {
            display_name: "Renamed".into(),
            ..request(SUBJECT, "alice-research")
        },
    ] {
        assert_eq!(
            world.provision("key-00000020", &changed).err(),
            Some(ProvisioningError::Conflict("idempotency_conflict"))
        );
    }
    assert!(!world.profile_dir("alice-other").exists());
    assert_eq!(world.service.load().unwrap().operations().count(), 1);
}

// Keys are scoped to the provisioner: another provisioner's identical key
// is a different request, never a replay of someone else's operation.
#[test]
fn idempotency_keys_are_scoped_to_the_provisioner() {
    let world = world();
    world
        .service
        .register_provisioner(
            &operator(),
            &principal("other-site"),
            [OTHER_ISSUER.to_owned()].into(),
            ["assistant".to_owned()].into(),
            10,
        )
        .unwrap();
    let other = caller(
        world.root(),
        &issue(world.root(), "other-site", Some(&[Capability::Provision])),
    );
    let mine = world
        .provision("key-shared-0001", &request(SUBJECT, "alice-research"))
        .unwrap();
    let mut theirs_request = request(SUBJECT, "other-research");
    theirs_request.issuer = OTHER_ISSUER.to_owned();
    let theirs = world
        .service
        .provision(&other, "key-shared-0001", &theirs_request)
        .unwrap();
    assert!(!theirs.replayed);
    assert_ne!(theirs.status.operation_id, mine.status.operation_id);
    // Same subject, different issuer: a different person.
    assert_ne!(theirs.status.principal_id, mine.status.principal_id);
    // Neither sees the other's operation.
    assert_eq!(
        world.service.status(&other, &mine.status.operation_id),
        Err(ProvisioningError::UnknownOperation)
    );
    assert_eq!(
        world.service.revoke(&other, &mine.status.operation_id),
        Err(ProvisioningError::UnknownOperation)
    );
}

// --- concurrency -----------------------------------------------------------

// Racing requests (separate service instances, as separate processes) for one
// profile id: exactly one owner. Racing retries of one key: one operation.
#[test]
fn concurrent_provisioning_never_creates_conflicting_ownership() {
    let world = world();
    let root = world.root().to_path_buf();
    let site = world.site();
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let (root, site) = (root.clone(), site.clone());
                scope.spawn(move || {
                    service(&root).provision(
                        &site,
                        &format!("key-race-{index:04}"),
                        &request(&format!("synthetic-race-{index:04}"), "contested"),
                    )
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(winners[0].status.state, OperationState::Completed);
    assert!(outcomes.iter().all(|outcome| match outcome {
        Ok(_) => true,
        Err(error) => *error == ProvisioningError::Conflict("profile_unavailable"),
    }));
    assert_eq!(
        world
            .effective()
            .get(&profile("contested"))
            .map(|record| record.owner.clone()),
        Some(winners[0].status.principal_id.clone())
    );

    let retries: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (root, site) = (root.clone(), site.clone());
                scope.spawn(move || {
                    service(&root)
                        .provision(&site, "key-retry-0001", &request(SUBJECT, "retried"))
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        retries.iter().filter(|outcome| !outcome.replayed).count(),
        1
    );
    assert!(
        retries
            .iter()
            .all(|outcome| outcome.status.operation_id == retries[0].status.operation_id)
    );
    let principals = world
        .identity()
        .principals()
        .filter(|record| record.role == Role::User)
        .count();
    assert_eq!(principals, 2, "one per distinct subject that won");
}

// --- partial failure and recovery ------------------------------------------

// The profile directory cannot be created: the operation stays pending, no
// principal or ownership exists, and the retry completes it.
#[test]
fn a_filesystem_failure_leaves_a_retryable_operation_and_nothing_usable() {
    let world = world();
    let blocker = world.root().join("profiles");
    std::fs::write(&blocker, "not a directory").unwrap();
    assert_eq!(
        world
            .provision("key-00000030", &request(SUBJECT, "alice-research"))
            .err(),
        Some(ProvisioningError::Unavailable)
    );
    let ledger = world.service.load().unwrap();
    let pending: Vec<_> = ledger.operations().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, OperationState::Pending);
    assert_eq!(
        world
            .identity()
            .principals()
            .filter(|record| record.role == Role::User)
            .count(),
        0
    );
    assert!(world.effective().get(&profile("alice-research")).is_none());

    std::fs::remove_file(&blocker).unwrap();
    let done = world
        .provision("key-00000030", &request(SUBJECT, "alice-research"))
        .unwrap();
    assert!(done.replayed);
    assert_eq!(done.status.state, OperationState::Completed);
    assert!(done.status.owner_bound);
}

// The identity store is unavailable after the profile exists: the profile
// has no owner, so no user can use it; once the store is back, the retry
// binds it.
#[test]
fn an_identity_store_failure_leaves_no_usable_orphan() {
    let world = world();
    world.service.inject(Fault::ProfileCreatedRecorded);
    assert_eq!(
        world
            .provision("key-00000040", &request(SUBJECT, "alice-research"))
            .err(),
        Some(ProvisioningError::Unavailable)
    );
    assert!(
        world
            .profile_dir("alice-research")
            .join(MARKER_FILE)
            .exists()
    );
    // Authenticated before the store breaks, as a request already in flight.
    let site = world.site();
    let state = world.root().join("identity").join("state.json");
    let saved = std::fs::read(&state).unwrap();
    std::fs::write(&state, b"{ not json").unwrap();
    assert_eq!(
        world
            .service
            .provision(&site, "key-00000040", &request(SUBJECT, "alice-research"))
            .err(),
        Some(ProvisioningError::Unavailable)
    );
    std::fs::write(&state, &saved).unwrap();
    // Unowned: a user, whoever it is, is refused; an operator is not.
    let effective = world.effective();
    let derived = derived_principal(&ExternalIdentity {
        issuer: ISSUER.into(),
        subject: SUBJECT.into(),
    })
    .unwrap();
    let as_user = Caller {
        principal: crate::identity::Principal {
            id: derived,
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "0000000000000000".into(),
            },
        },
        restriction: Restriction::default(),
    };
    assert_eq!(
        authorize(
            &as_user,
            Capability::RunsWrite,
            &profile("alice-research"),
            &effective
        ),
        Err(Denial::ProfileUnowned)
    );
    let done = world
        .provision("key-00000040", &request(SUBJECT, "alice-research"))
        .unwrap();
    assert_eq!(done.status.state, OperationState::Completed);
    assert!(done.status.owner_bound);
}

// A crash at every point, followed by a restart (a new service instance)
// and either a retry or the operator's reconciliation, ends completed with
// exactly one profile, one principal and one binding.
#[test]
fn crash_and_restart_recovery_completes_every_interrupted_operation() {
    for (index, at) in [
        Fault::PendingRecorded,
        Fault::ProfileRenamed,
        Fault::ProfileCreatedRecorded,
        Fault::IdentityCommitted,
    ]
    .into_iter()
    .enumerate()
    {
        for by_reconcile in [false, true] {
            let world = world();
            let key = format!("key-crash-{index}-{by_reconcile}");
            let wanted = request(&format!("synthetic-crash-{index}"), "crashed");
            world.service.inject(at);
            assert_eq!(
                world.provision(&key, &wanted).err(),
                Some(ProvisioningError::Unavailable),
                "{at:?}"
            );
            // Abandoned staging from a crash elsewhere is cleared too.
            let stray = world.service.root().join(STAGING).join("f".repeat(32));
            std::fs::create_dir_all(&stray).unwrap();
            let restarted = service(world.root());
            let status = if by_reconcile {
                let report = restarted.reconcile(&operator()).unwrap();
                assert_eq!(report.completed, 1, "{at:?}");
                assert!(report.staging_removed >= 1);
                restarted
                    .provision(&world.site(), &key, &wanted)
                    .unwrap()
                    .status
            } else {
                restarted
                    .provision(&world.site(), &key, &wanted)
                    .unwrap()
                    .status
            };
            assert_eq!(status.state, OperationState::Completed, "{at:?}");
            assert!(status.owner_bound, "{at:?}");
            let identity = world.identity();
            assert_eq!(
                identity
                    .principals()
                    .filter(|record| record.role == Role::User)
                    .count(),
                1,
                "{at:?}"
            );
            assert_eq!(world.service.load().unwrap().operations().count(), 1);
            assert_eq!(stray.exists(), !by_reconcile);
        }
    }
}

// The principal is suspended between the profile's creation and its
// binding: the operation fails, and the unowned profile is moved to
// quarantine, never deleted and never left usable.
#[test]
fn a_failed_operation_quarantines_its_profile() {
    let world = world();
    let first = world
        .provision("key-00000050", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    world.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        world
            .provision("key-00000051", &request(SUBJECT, "alice-second"))
            .is_err()
    );
    operator_change(world.root(), |tx| {
        tx.set_status(&first.principal_id, PrincipalStatus::Suspended)
            .unwrap();
    });
    let failed = world
        .provision("key-00000051", &request(SUBJECT, "alice-second"))
        .unwrap()
        .status;
    assert_eq!(failed.state, OperationState::Failed);
    assert_eq!(failed.failure.as_deref(), Some("principal_inactive"));
    assert!(!failed.owner_bound);
    assert!(!world.profile_dir("alice-second").exists());
    assert!(
        world
            .service
            .root()
            .join(QUARANTINE)
            .join(&failed.operation_id)
            .join(MARKER_FILE)
            .exists()
    );
    // The outcome is stable: the same request answers the same failure.
    assert_eq!(
        world
            .provision("key-00000051", &request(SUBJECT, "alice-second"))
            .unwrap()
            .status,
        failed
    );
    // A new provisioning for the suspended principal is refused up front.
    assert_eq!(
        world
            .provision("key-00000052", &request(SUBJECT, "alice-third"))
            .err(),
        Some(ProvisioningError::Conflict("principal_inactive"))
    );
}

// --- takeover, reuse and revocation -----------------------------------------

// An existing profile, an operator-owned one, or one with stale ownership is
// never provisioned over; a deleted provisioned profile is not recreated and
// its id cannot be re-provisioned to someone else.
#[test]
fn existing_and_reused_profiles_are_never_taken_over() {
    let world = world();
    let store = ProfileStore::new(world.root());
    store
        .create(&AgentProfile::new(
            profile("ops-bot"),
            "Ops",
            "Persona",
            "m",
        ))
        .unwrap();
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("bob"), Role::User, None)
            .unwrap();
    });
    assert_eq!(
        world
            .provision("key-00000060", &request(SUBJECT, "ops-bot"))
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable"))
    );
    // Owned by bob, then its directory deleted: a stale record.
    store
        .create(&AgentProfile::new(
            profile("bob-bot"),
            "Bob",
            "Persona",
            "m",
        ))
        .unwrap();
    operator_change(world.root(), |tx| {
        tx.assign_owner(&profile("bob-bot"), &principal("bob"))
            .unwrap();
    });
    store.delete(&profile("bob-bot")).unwrap();
    assert_eq!(
        world
            .provision("key-00000061", &request(SUBJECT, "bob-bot"))
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable"))
    );
    assert!(!world.profile_dir("bob-bot").exists());

    // A provisioned profile deleted by the operator: the original request
    // replays its outcome without recreating it, and nobody else gets the id.
    let done = world
        .provision("key-00000062", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    store.delete(&profile("alice-research")).unwrap();
    let replay = world
        .provision("key-00000062", &request(SUBJECT, "alice-research"))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.status.operation_id, done.operation_id);
    assert!(!replay.status.owner_bound);
    assert!(!world.profile_dir("alice-research").exists());
    assert_eq!(
        world
            .provision(
                "key-00000063",
                &request("synthetic-subject-other", "alice-research")
            )
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable"))
    );
    // bob's ownership was never touched.
    assert_eq!(
        world
            .identity()
            .recorded_ownership()
            .get(&profile("bob-bot"))
            .map(|record| record.owner.clone()),
        Some(principal("bob"))
    );
}

// Revoking access removes the user's ownership through the identity store,
// so the durable authority refuses the user at once; it is idempotent, the
// profile is kept, and a profile an operator reassigned is not revocable by
// the provisioner.
#[test]
fn revoking_access_is_enforced_by_the_identity_store() {
    let world = world();
    let done = world
        .provision("key-00000070", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    let user_token = issue(world.root(), done.principal_id.as_str(), None);
    let user = caller(world.root(), &user_token);
    let target = profile("alice-research");
    for capability in crate::identity::OWNER_DEFAULT {
        assert!(authorize(&user, capability, &target, &world.effective()).is_ok());
    }
    // The owner's minimum bundle only: no tool catalog, no administration.
    assert_eq!(
        authorize(&user, Capability::ToolsRead, &target, &world.effective()),
        Err(Denial::ProfileNotGranted)
    );
    let revoked = world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(revoked.state, OperationState::Revoked);
    assert!(!revoked.owner_bound);
    assert_eq!(
        authorize(&user, Capability::RunsWrite, &target, &world.effective()),
        Err(Denial::ProfileUnowned)
    );
    assert!(
        world.profile_dir("alice-research").exists(),
        "kept for the operator"
    );
    assert_eq!(
        world
            .service
            .revoke(&world.site(), &done.operation_id)
            .unwrap(),
        revoked
    );
    assert_eq!(
        world
            .service
            .status(&world.site(), &done.operation_id)
            .unwrap(),
        revoked
    );

    // Reassigned by an operator: no longer the provisioner's to revoke.
    let second = world
        .provision(
            "key-00000071",
            &request("synthetic-subject-0002", "alice-other"),
        )
        .unwrap()
        .status;
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("carol"), Role::User, None)
            .unwrap();
        tx.reassign_owner(&profile("alice-other"), &principal("carol"))
            .unwrap();
    });
    assert_eq!(
        world.service.revoke(&world.site(), &second.operation_id),
        Err(ProvisioningError::Conflict("ownership_changed"))
    );
    assert_eq!(
        world
            .effective()
            .get(&profile("alice-other"))
            .map(|record| record.owner.clone()),
        Some(principal("carol"))
    );
    // Malformed or unknown operation ids reveal nothing.
    for id in ["../x", "", &"0".repeat(32)] {
        assert_eq!(
            world.service.status(&world.site(), id),
            Err(ProvisioningError::UnknownOperation)
        );
    }
}

// The provisioning credential is revoked, rotated or expires; its principal
// is suspended; its registration is withdrawn: each stops the next call.
#[test]
fn provisioning_authority_follows_the_current_stores() {
    let world = world();
    let site = world.site();
    let PrincipalSource::Credential { key_id } = site.principal.source.clone() else {
        panic!("credential");
    };
    // Rotation: the new credential works; the old one until revoked.
    let rotated = operator_change(world.root(), |tx| {
        tx.rotate_credential(&key_id, None)
            .unwrap()
            .1
            .expose()
            .to_owned()
    });
    let fresh = caller(world.root(), &rotated);
    world
        .service
        .provision(&fresh, "key-00000080", &request(SUBJECT, "p-rotated"))
        .unwrap();
    operator_change(world.root(), |tx| tx.revoke_credential(&key_id).unwrap());
    assert_eq!(
        world
            .service
            .provision(&site, "key-00000081", &request(SUBJECT, "p-old"))
            .err(),
        Some(ProvisioningError::Denied("unauthenticated"))
    );
    // Provisioning access withdrawn by the operator.
    world
        .service
        .revoke_provisioner(&operator(), &principal("control-site"))
        .unwrap();
    assert_eq!(
        world
            .service
            .provision(&fresh, "key-00000082", &request(SUBJECT, "p-withdrawn"))
            .err(),
        Some(ProvisioningError::Denied("provisioner_revoked"))
    );
    // Re-registered, then the principal suspended: every credential stops.
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
    world
        .service
        .provision(&fresh, "key-00000083", &request(SUBJECT, "p-again"))
        .unwrap();
    operator_change(world.root(), |tx| {
        tx.set_status(&principal("control-site"), PrincipalStatus::Suspended)
            .unwrap();
    });
    assert_eq!(
        world
            .service
            .provision(&fresh, "key-00000084", &request(SUBJECT, "p-suspended"))
            .err(),
        Some(ProvisioningError::Denied("unauthenticated"))
    );
    // Expiry, judged by the service clock.
    let world = world_with_expiring_credential();
    assert_eq!(
        world
            .provision("key-00000085", &request(SUBJECT, "p-expired"))
            .err(),
        Some(ProvisioningError::Denied("unauthenticated"))
    );
}

fn world_with_expiring_credential() -> World {
    let mut world = world();
    let token = operator_change(world.root(), |tx| {
        tx.issue_credential(
            &principal("control-site"),
            Restriction {
                capabilities: Some([Capability::Provision].into()),
                profiles: None,
            },
            Some(T0 + 10),
        )
        .unwrap()
        .1
        .expose()
        .to_owned()
    });
    world.site = token;
    world.service = ProvisioningService::new(world.root()).with_clock(Arc::new(|| T0 + 10));
    world
}

// A withdrawn provisioner's interrupted operations fail at reconciliation
// instead of completing, and their unowned profiles are quarantined.
#[test]
fn reconciliation_fails_operations_of_a_withdrawn_provisioner() {
    let world = world();
    world.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        world
            .provision("key-00000090", &request(SUBJECT, "alice-research"))
            .is_err()
    );
    world
        .service
        .revoke_provisioner(&operator(), &principal("control-site"))
        .unwrap();
    let report = world.service.reconcile(&operator()).unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.completed, 0);
    let ledger = world.service.load().unwrap();
    let operation = ledger.operations().next().unwrap();
    assert_eq!(operation.state, OperationState::Failed);
    assert_eq!(operation.failure.as_deref(), Some("provisioner_revoked"));
    assert!(!world.profile_dir("alice-research").exists());
    assert!(world.effective().get(&profile("alice-research")).is_none());
    assert_eq!(
        world
            .identity()
            .principals()
            .filter(|record| record.role == Role::User)
            .count(),
        0
    );
}

// A credential limited to some profiles provisions only those; a quota
// bounds how many profiles a provisioner holds; a template not allowed for
// it is refused.
#[test]
fn credential_profile_limits_quotas_and_templates_are_enforced() {
    let world = world();
    let limited = operator_change(world.root(), |tx| {
        tx.issue_credential(
            &principal("control-site"),
            Restriction {
                capabilities: Some([Capability::Provision].into()),
                profiles: Some([profile("only-this")].into()),
            },
            None,
        )
        .unwrap()
        .1
        .expose()
        .to_owned()
    });
    let limited = caller(world.root(), &limited);
    assert_eq!(
        world
            .service
            .provision(&limited, "key-00000100", &request(SUBJECT, "not-this"))
            .err(),
        Some(ProvisioningError::Denied("profile_outside_credential"))
    );
    let only = world
        .service
        .provision(&limited, "key-00000101", &request(SUBJECT, "only-this"))
        .unwrap()
        .status;
    let elsewhere = world
        .provision("key-00000104", &request(SUBJECT, "not-this"))
        .unwrap()
        .status;
    // The limited credential sees and revokes only operations inside its
    // limit; the rest are answered like missing ones.
    assert_eq!(
        world.service.status(&limited, &only.operation_id).unwrap(),
        only
    );
    for attempt in [
        world.service.status(&limited, &elsewhere.operation_id),
        world.service.revoke(&limited, &elsewhere.operation_id),
    ] {
        assert_eq!(attempt, Err(ProvisioningError::UnknownOperation));
    }
    assert!(
        world
            .effective()
            .get(&profile("not-this"))
            .is_some_and(|record| record.owner == elsewhere.principal_id)
    );
    world
        .service
        .register_provisioner(
            &operator(),
            &principal("control-site"),
            [ISSUER.to_owned()].into(),
            ["assistant".to_owned()].into(),
            3,
        )
        .unwrap();
    world
        .provision("key-00000102", &request(SUBJECT, "second"))
        .unwrap();
    assert_eq!(
        world
            .provision("key-00000103", &request(SUBJECT, "third"))
            .err(),
        Some(ProvisioningError::Denied("quota_exceeded"))
    );
    assert!(!world.profile_dir("third").exists());
}

// Cross-profile: a provisioned user reaches only its own profile.
#[test]
fn provisioned_users_reach_only_their_own_profiles() {
    let world = world();
    let alice = world
        .provision("key-00000110", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    let bob = world
        .provision(
            "key-00000111",
            &request("synthetic-subject-bob", "bob-research"),
        )
        .unwrap()
        .status;
    let alice_user = caller(
        world.root(),
        &issue(world.root(), alice.principal_id.as_str(), None),
    );
    let effective = world.effective();
    assert!(
        authorize(
            &alice_user,
            Capability::RunsWrite,
            &profile("alice-research"),
            &effective
        )
        .is_ok()
    );
    for capability in Capability::ALL {
        assert!(
            authorize(
                &alice_user,
                capability,
                &profile("bob-research"),
                &effective
            )
            .is_err(),
            "{capability:?}"
        );
    }
    assert_ne!(alice.principal_id, bob.principal_id);
}

// Audit lines hold identifiers and fixed codes only: never the subject, the
// issuer URL, the display name or a token.
#[test]
fn audit_records_hold_metadata_and_codes_only() {
    let world = world();
    let done = world
        .provision("key-00000120", &request(SUBJECT, "alice-research"))
        .unwrap()
        .status;
    let _ = world.provision(
        "key-00000120",
        &request("synthetic-subject-0002", "alice-research"),
    );
    world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    let audit = world.service.audit_log().unwrap();
    let text = serde_json::to_string(&audit).unwrap();
    for secret in [SUBJECT, ISSUER, "Research assistant", world.site.as_str()] {
        assert!(!text.contains(secret), "{secret}");
    }
    for code in [
        "completed",
        "refused:idempotency_conflict",
        "revoked",
        "provisioner_registered",
    ] {
        assert!(text.contains(code), "{code}");
    }
    let identity_audit =
        serde_json::to_string(&IdentityStore::new(world.root()).audit().unwrap()).unwrap();
    assert!(!identity_audit.contains(SUBJECT) && !identity_audit.contains(&world.site));
    // Error text holds codes only.
    for error in [
        ProvisioningError::Denied("unauthenticated"),
        ProvisioningError::Invalid("invalid_subject"),
        ProvisioningError::Conflict("profile_unavailable"),
        ProvisioningError::UnknownOperation,
        ProvisioningError::Unavailable,
    ] {
        assert!(!error.to_string().contains('/'));
    }
}

// Nothing is created for an installation that never provisions.
#[test]
fn an_unused_service_writes_nothing() {
    let root = std::env::temp_dir().join(format!("lightagent-provision-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    let home = Home(root);
    let service = service(&home.0);
    assert_eq!(service.load().unwrap().operations().count(), 0);
    assert!(!service.root().exists());
}

// --- LA-WEB-PROVISIONING/1 semantics (L0 review) ------------------------------
//
// The Web protocol candidate (`docs/integrations/web/`) relies on these
// properties of the service exactly as it is. They were established by
// reading the code. These tests assert them, with the protocol's own id
// shapes (`agt_…` profiles, `usr_…` subjects).

const WEB_SUBJECT: &str = "usr_fixtureuser00000000000001";
const WEB_PROFILE: &str = "agt_fixtureprofile000000000001";

fn web_request(profile_id: &str) -> ProvisionRequest {
    request(WEB_SUBJECT, profile_id)
}

fn operation_for(world: &World, profile_id: &str) -> Operation {
    world
        .service
        .load()
        .unwrap()
        .operations()
        .find(|op| op.profile.as_str() == profile_id)
        .unwrap()
        .clone()
}

// Exact redelivery is one operation. After revocation the same key returns
// the recorded `revoked` operation and binds nothing (S1, S11). A new key, or
// another subject, cannot take the id (S2). The same key with a different
// payload changes nothing (IDEMPOTENCY_CONFLICT).
#[test]
fn web_redelivery_and_revocation_never_restore_access() {
    let world = world();
    let first = world
        .provision("idk_fixturecreate00000000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(first.status.state, OperationState::Completed);
    let again = world
        .provision("idk_fixturecreate00000000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.status, first.status);
    assert_eq!(world.service.load().unwrap().operations().count(), 1);

    let mut changed = web_request(WEB_PROFILE);
    changed.display_name = "Changed".to_owned();
    let before = world.service.load().unwrap();
    assert_eq!(
        world
            .provision("idk_fixturecreate00000000001", &changed)
            .err(),
        Some(ProvisioningError::Conflict("idempotency_conflict"))
    );
    assert_eq!(world.service.load().unwrap(), before);

    let id = first.status.operation_id.clone();
    let revoked = world.service.revoke(&world.site(), &id).unwrap();
    assert_eq!(revoked.state, OperationState::Revoked);
    assert!(!revoked.owner_bound);

    // A delayed copy of the create, after the revoke.
    let late = world
        .provision("idk_fixturecreate00000000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(late.replayed);
    assert_eq!(late.status, revoked);
    assert!(
        world.effective().get(&profile(WEB_PROFILE)).is_none(),
        "no binding is restored"
    );

    for (key, subject) in [
        ("idk_fixturecreate00000000002", WEB_SUBJECT),
        (
            "idk_fixturecreate00000000003",
            "usr_fixtureuser00000000000002",
        ),
    ] {
        assert_eq!(
            world.provision(key, &request(subject, WEB_PROFILE)).err(),
            Some(ProvisioningError::Conflict("profile_unavailable")),
            "{subject}"
        );
    }
    assert_eq!(world.service.load().unwrap().operations().count(), 1);
    assert_eq!(world.service.status(&world.site(), &id).unwrap(), revoked);
}

// A revoke that races an unfinished create is refused without effect, and
// succeeds once the create completes (S3).
#[test]
fn web_revoke_of_an_unfinished_create_is_operation_in_progress() {
    let world = world();
    world.service.inject(Fault::PendingRecorded);
    assert!(
        world
            .provision("idk_fixtureinprogress00000001", &web_request(WEB_PROFILE))
            .is_err()
    );
    let pending = operation_for(&world, WEB_PROFILE);
    assert_eq!(pending.state, OperationState::Pending);
    assert_eq!(
        world.service.revoke(&world.site(), &pending.id).err(),
        Some(ProvisioningError::Conflict("operation_in_progress"))
    );
    assert_eq!(
        operation_for(&world, WEB_PROFILE).state,
        OperationState::Pending
    );

    let done = world
        .provision("idk_fixtureinprogress00000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(done.replayed);
    assert_eq!(done.status.state, OperationState::Completed);
    assert_eq!(
        world
            .service
            .revoke(&world.site(), &pending.id)
            .unwrap()
            .state,
        OperationState::Revoked
    );
}

// A refusal on the merits happens after the idempotency lookup and records
// nothing, so the same key can still succeed once the cause is gone. The
// protocol's R3 rule depends on this.
#[test]
fn web_merits_refusals_record_nothing() {
    let world = world();
    for n in 0..10 {
        world
            .provision(
                &format!("idk_fixturequota{n:014}"),
                &request(&format!("usr_quota{n:020}"), &format!("agt_quota{n:020}")),
            )
            .unwrap();
    }
    let before = world.service.load().unwrap();
    assert_eq!(
        world
            .provision("idk_fixturemerits00000000001", &web_request(WEB_PROFILE))
            .err(),
        Some(ProvisioningError::Denied("quota_exceeded"))
    );
    assert_eq!(world.service.load().unwrap(), before, "nothing recorded");
    let last = operation_for(&world, "agt_quota00000000000000000009");
    world.service.revoke(&world.site(), &last.id).unwrap();
    let created = world
        .provision("idk_fixturemerits00000000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(!created.replayed, "the refused key had no operation");
    assert_eq!(created.status.state, OperationState::Completed);
}

// Spec open point 3: after an operation for an id ends `failed`, a new key
// may provision that id again. Nothing was bound, the profile went to
// quarantine, and only non-failed operations reserve an id. The failed key
// keeps answering its failure.
#[test]
fn web_a_failed_create_leaves_its_profile_id_available_to_a_new_key() {
    let world = world();
    let owner = world
        .provision(
            "idk_fixtureowner000000000001",
            &web_request("agt_fixtureprofile000000000009"),
        )
        .unwrap()
        .status;
    world.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        world
            .provision("idk_fixturefailed00000000001", &web_request(WEB_PROFILE))
            .is_err()
    );
    operator_change(world.root(), |tx| {
        tx.set_status(&owner.principal_id, PrincipalStatus::Suspended)
            .unwrap();
    });
    let failed = world
        .provision("idk_fixturefailed00000000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    assert_eq!(failed.state, OperationState::Failed);
    assert_eq!(failed.failure.as_deref(), Some("principal_inactive"));
    assert!(!world.profile_dir(WEB_PROFILE).exists());

    operator_change(world.root(), |tx| {
        tx.set_status(&owner.principal_id, PrincipalStatus::Active)
            .unwrap();
    });
    let retried = world
        .provision("idk_fixtureretry000000000001", &web_request(WEB_PROFILE))
        .unwrap();
    assert!(!retried.replayed);
    assert_ne!(retried.status.operation_id, failed.operation_id);
    assert_eq!(retried.status.state, OperationState::Completed);
    assert!(retried.status.owner_bound);
    // The failed key still answers its failure. Its `owner_bound` is now
    // true: it reports the principal's current ownership of the profile,
    // which the new operation established (see the next test).
    let replay = world
        .provision("idk_fixturefailed00000000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    assert_eq!(replay.operation_id, failed.operation_id);
    assert_eq!(replay.state, OperationState::Failed);
    assert_eq!(replay.failure.as_deref(), Some("principal_inactive"));
    assert!(replay.owner_bound);
}

// `owner_bound` is the effective ownership of (operation principal, profile)
// when asked, not a function of the operation's state. The candidate
// protocol (§12.1, §12.2, fixture `invalid-owner-bound-pending`) requires
// `ownerBound` if and only if `completed`. These three reachable states
// break that rule, so the protocol has to change, not the service.
#[test]
fn web_owner_bound_is_current_ownership_not_operation_state() {
    // `profile_created` and bound: the identity transaction committed, and
    // the process stopped before the ledger recorded `completed`.
    let crashed = world();
    crashed.service.inject(Fault::IdentityCommitted);
    assert!(
        crashed
            .provision("idk_fixturecommitted00000001", &web_request(WEB_PROFILE))
            .is_err()
    );
    let interrupted = operation_for(&crashed, WEB_PROFILE);
    let status = crashed
        .service
        .status(&crashed.site(), &interrupted.id)
        .unwrap();
    assert_eq!(status.state, OperationState::ProfileCreated);
    assert!(status.owner_bound);

    // `completed` and not bound: an operator reassigned the profile.
    let world = world();
    let done = world
        .provision("idk_fixturereassigned0000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("carol"), Role::User, None)
            .unwrap();
        tx.reassign_owner(&profile(WEB_PROFILE), &principal("carol"))
            .unwrap();
    });
    let status = world
        .service
        .status(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(status.state, OperationState::Completed);
    assert!(!status.owner_bound);

    // `failed` and bound: shown in the previous test.
}

// r2 §13.1 against the service. After an operator removes ownership outright,
// revoke answers `revoked`; after the operator gives a revoked profile back
// to the same principal, status answers `revoked`/`true`. A reassigned
// profile is never re-bound by a replay of its create, and a new key cannot
// take it.
#[test]
fn web_ownership_changes_by_the_operator_are_reported_not_undone() {
    let world = world();
    let done = world
        .provision("idk_fixtureremoved0000000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    operator_change(world.root(), |tx| {
        tx.remove_profile(&profile(WEB_PROFILE)).unwrap();
    });
    let status = world
        .service
        .status(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(
        (status.state, status.owner_bound),
        (OperationState::Completed, false)
    );
    let revoked = world
        .service
        .revoke(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(
        (revoked.state, revoked.owner_bound),
        (OperationState::Revoked, false),
        "no owner remained"
    );
    operator_change(world.root(), |tx| {
        tx.reassign_owner(&profile(WEB_PROFILE), &done.principal_id)
            .unwrap();
    });
    let status = world
        .service
        .status(&world.site(), &done.operation_id)
        .unwrap();
    assert_eq!(
        (status.state, status.owner_bound),
        (OperationState::Revoked, true)
    );

    // Reassigned to someone else after completion.
    let other = "agt_fixtureprofile000000000002";
    let second = world
        .provision("idk_fixturereassign000000001", &web_request(other))
        .unwrap()
        .status;
    operator_change(world.root(), |tx| {
        tx.create_principal(principal("carol"), Role::User, None)
            .unwrap();
        tx.reassign_owner(&profile(other), &principal("carol"))
            .unwrap();
    });
    let replay = world
        .provision("idk_fixturereassign000000001", &web_request(other))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        (replay.status.state, replay.status.owner_bound),
        (OperationState::Completed, false)
    );
    assert_eq!(
        world
            .effective()
            .get(&profile(other))
            .map(|o| o.owner.clone()),
        Some(principal("carol")),
        "the replay did not re-bind"
    );
    assert_eq!(
        world
            .provision("idk_fixturereassign000000002", &web_request(other))
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable"))
    );
    assert_eq!(
        world
            .service
            .revoke(&world.site(), &second.operation_id)
            .err(),
        Some(ProvisioningError::Conflict("ownership_changed"))
    );
}

// RC3: `invalid_template` is a real stored failure. A create interrupted at
// `pending` by a build that knew its template, then resumed by a build that
// does not, fails in `materialize` with `invalid_template` (simulated by
// renaming the recorded template). The operation's status then reports it.
#[test]
fn web_an_unknown_recorded_template_fails_the_operation() {
    let world = world();
    world.service.inject(Fault::PendingRecorded);
    assert!(
        world
            .provision("idk_fixtureretired0000000001", &web_request(WEB_PROFILE))
            .is_err()
    );
    let path = world.service.root().join(STATE);
    let ledger = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        ledger.matches("\"assistant\"").count(),
        2,
        "registration and operation"
    );
    let mut value: serde_json::Value = serde_json::from_str(&ledger).unwrap();
    for operation in value["operations"].as_object_mut().unwrap().values_mut() {
        operation["template"] = "retired".into();
    }
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    let report = world.service.reconcile(&operator()).unwrap();
    assert_eq!(report.failed, 1);
    let operation = operation_for(&world, WEB_PROFILE);
    let status = world.service.status(&world.site(), &operation.id).unwrap();
    assert_eq!(status.state, OperationState::Failed);
    assert_eq!(status.failure.as_deref(), Some("invalid_template"));
    assert!(!status.owner_bound);
    assert!(!world.profile_dir(WEB_PROFILE).exists());
}

// r3 review counterexample. r3 §10.5 lets a harness "initialize a new ledger
// for an existing harness id" and only raises the fence. The identity store
// and profile directory outlive the ledger, so a fresh resend of a job whose
// create bound the user finds no operation for its key, and `begin` refuses
// it on the merits (`profile_unavailable`). Web's R3 would conclude `failed`
// while the user owns the profile. This is the service behaviour that makes
// r3's restore rule insufficient.
#[test]
fn web_a_fresh_ledger_answers_a_merits_refusal_for_a_bound_profile() {
    let world = world();
    let done = world
        .provision("idk_fixturelostledger0000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    assert!(done.owner_bound);

    // A new ledger for the same home: the operator re-registers the
    // provisioner; the identity store and the profile are untouched.
    std::fs::remove_file(world.service.root().join(STATE)).unwrap();
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

    assert_eq!(
        world
            .provision("idk_fixturelostledger0000001", &web_request(WEB_PROFILE))
            .err(),
        Some(ProvisioningError::Conflict("profile_unavailable")),
        "a merits-class refusal for the job's own key"
    );
    assert_eq!(
        world
            .effective()
            .get(&profile(WEB_PROFILE))
            .map(|o| o.owner.clone()),
        Some(done.principal_id),
        "the user still owns the profile"
    );
}

// r4 §10.6 against the real stores (test-only; no continuity check exists in
// the service yet). C2 and C3 need, per harness: which operations the ledger
// knows, which ones the profiles carry, and whether the identity store is
// older than the ledger. Profile markers hold each operation's random
// `marker` token (not its id), and they survive revocation, reassignment and
// crashes. The identity store's generation is monotone and drops when an
// older copy is restored. Identity idempotency records cannot serve C3: they
// are capped at 10,000 and evicted oldest first (`store.rs`
// `MAX_IDEMPOTENCY`), and nothing exposes them.

fn ledger_markers(world: &World) -> BTreeSet<String> {
    world
        .service
        .load()
        .unwrap()
        .operations()
        .map(|op| op.marker.clone())
        .collect()
}

fn profile_markers(world: &World) -> BTreeSet<String> {
    let profiles = world
        .profile_dir(WEB_PROFILE)
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::read_dir(profiles)
        .unwrap()
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path().join(MARKER_FILE)).ok())
        .map(|marker| marker.trim().to_owned())
        .collect()
}

#[test]
fn web_continuity_inputs_are_observable_on_the_real_stores() {
    let harness = world();
    // Completed, revoked, operator-reassigned, and interrupted after the
    // profile was created (a crash-recovery marker without a binding).
    let completed = harness
        .provision("idk_fixturecontinuity000001", &web_request(WEB_PROFILE))
        .unwrap()
        .status;
    let revoked = harness
        .provision(
            "idk_fixturecontinuity000002",
            &web_request("agt_fixtureprofile000000000002"),
        )
        .unwrap()
        .status;
    harness
        .service
        .revoke(&harness.site(), &revoked.operation_id)
        .unwrap();
    harness
        .provision(
            "idk_fixturecontinuity000003",
            &web_request("agt_fixtureprofile000000000003"),
        )
        .unwrap();
    operator_change(harness.root(), |tx| {
        tx.create_principal(principal("carol"), Role::User, None)
            .unwrap();
        tx.reassign_owner(
            &profile("agt_fixtureprofile000000000003"),
            &principal("carol"),
        )
        .unwrap();
    });
    harness.service.inject(Fault::ProfileCreatedRecorded);
    assert!(
        harness
            .provision(
                "idk_fixturecontinuity000004",
                &web_request("agt_fixtureprofile000000000004")
            )
            .is_err()
    );

    // Consistent: every marker on disk names a ledger operation (C2 passes),
    // for historical revoked, reassigned and interrupted operations alike.
    let on_disk = profile_markers(&harness);
    assert_eq!(on_disk.len(), 4);
    assert!(on_disk.is_subset(&ledger_markers(&harness)));
    let identity_generation = harness.identity().generation();

    // An older identity store restored beside the ledger: its generation
    // drops below what the ledger's last bind produced (C3, as L1 records it).
    let identity_state = harness.root().join("identity").join("state.json");
    let newer = std::fs::read(&identity_state).unwrap();
    let older = {
        let fresh = world();
        fresh
            .provision("idk_fixturecontinuity000005", &web_request(WEB_PROFILE))
            .unwrap();
        std::fs::read(fresh.root().join("identity").join("state.json")).unwrap()
    };
    std::fs::write(&identity_state, &older).unwrap();
    assert!(
        harness.identity().generation() < identity_generation,
        "detectable"
    );
    std::fs::write(&identity_state, &newer).unwrap();

    // A fresh ledger beside the kept profiles (RC7): no marker on disk is
    // known any more (C2 fails), so a gateway can refuse before serving.
    std::fs::remove_file(harness.service.root().join(STATE)).unwrap();
    harness
        .service
        .register_provisioner(
            &operator(),
            &principal("control-site"),
            [ISSUER.to_owned()].into(),
            ["assistant".to_owned()].into(),
            10,
        )
        .unwrap();
    assert!(ledger_markers(&harness).is_empty());
    assert!(!profile_markers(&harness).is_subset(&ledger_markers(&harness)));
    // Nothing was altered by looking: the user still owns the profile.
    assert_eq!(
        harness
            .effective()
            .get(&profile(WEB_PROFILE))
            .map(|o| o.owner.clone()),
        Some(completed.principal_id)
    );
}

mod continuity;

#[test]
fn a_preset_sets_only_toolsets_and_approval_and_templates_are_unchanged() {
    assert_eq!(TEMPLATES, ["assistant"]);
    let id = ProfileId::new("visitor").unwrap();
    let mut profile = template_profile("assistant", id, "Visitor").unwrap();
    let before = profile.clone();
    apply_preset(&mut profile, "research@1").unwrap();
    assert!(profile.toolsets.contains(&"agent.research".to_owned()));
    assert!(!profile.toolsets.iter().any(|tool| tool == "agent.delegate"
        || tool.starts_with("fs.")
        || tool.starts_with("terminal")
        || tool == "all"));
    assert_eq!(profile.approval_policy, ApprovalPolicy::Balanced);
    assert_eq!(profile.limits, before.limits);
    assert_eq!(profile.persona, before.persona);
    assert_eq!(profile.routing, before.routing);
    let mut other = before.clone();
    assert_eq!(
        apply_preset(&mut other, "everything@1"),
        Err(ProvisioningError::Invalid("invalid_preset"))
    );
    assert_eq!(other, before);
}
