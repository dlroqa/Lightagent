//! The sandbox gate over synthetic profiles, grants and a deterministic mock
//! backend. The mock is a **simulation**: it runs no process and isolates
//! nothing. These tests prove the gate's decisions and lifecycle, never OS
//! isolation, which no backend in this build provides.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lightagent_core::identity::{
    Caller, OwnershipRegistry, Principal, PrincipalSource, Restriction, Role, authorize,
};
use lightagent_core::{ProfileId, RunId};

use super::*;
use crate::test_dir::TestDir;

/// What the mock does when asked to execute.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behavior {
    Succeed,
    FailStart,
    FailExecute,
    Hang,
    Flood,
}

/// A deterministic, in-process stand-in for a backend. It counts each
/// lifecycle call and records the environment it would hand a child.
struct MockBackend {
    behavior: Behavior,
    assurance: Assurance,
    enforced: BTreeSet<Guarantee>,
    server_env: BTreeMap<String, String>,
    starts: AtomicUsize,
    executes: AtomicUsize,
    releases: AtomicUsize,
    child_env: Mutex<Option<BTreeMap<String, String>>>,
}

impl MockBackend {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            assurance: Assurance::Simulated,
            enforced: Guarantee::ALL.into_iter().collect(),
            server_env: BTreeMap::from([
                ("PROVIDER_API_KEY".to_owned(), "sk-server-secret".to_owned()),
                ("LIGHTAGENT_API_KEY".to_owned(), "operator-key".to_owned()),
                ("HOME".to_owned(), "/srv/lightagent".to_owned()),
                ("REPORT_TOKEN".to_owned(), "granted-value".to_owned()),
            ]),
            starts: AtomicUsize::new(0),
            executes: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            child_env: Mutex::new(None),
        }
    }

    fn experimental(mut self) -> Self {
        self.assurance = Assurance::Experimental;
        self
    }

    fn without(mut self, guarantee: Guarantee) -> Self {
        self.enforced.remove(&guarantee);
        self
    }

    fn count(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SandboxBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn assurance(&self) -> Assurance {
        self.assurance
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            enforced: self.enforced.clone(),
            demonstrated_on: BTreeSet::new(),
        }
    }

    async fn start(&self, _ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault> {
        let id = self.starts.fetch_add(1, Ordering::SeqCst) as u64;
        if self.behavior == Behavior::FailStart {
            return Err(BackendFault::StartFailed);
        }
        Ok(SandboxLease::new(id))
    }

    async fn execute(
        &self,
        _lease: &SandboxLease,
        ctx: &ExecutionContext,
        _request: &ExecRequest,
    ) -> Result<ExecOutput, BackendFault> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        let env = ctx.child_environment(&|name: &str| self.server_env.get(name).cloned());
        if let Ok(mut seen) = self.child_env.lock() {
            *seen = Some(env);
        }
        match self.behavior {
            Behavior::Succeed => Ok(ExecOutput {
                exit_code: Some(0),
                stdout: "ok".to_owned(),
                ..ExecOutput::default()
            }),
            Behavior::FailExecute => Err(BackendFault::ExecutionFailed),
            // Never finishes: only the gate's cancellation or timeout ends it.
            Behavior::Hang => std::future::pending().await,
            Behavior::Flood => Ok(ExecOutput {
                exit_code: Some(0),
                stdout: "é".repeat(4096),
                stderr: "x".repeat(4096),
                truncated: false,
            }),
            Behavior::FailStart => Err(BackendFault::ExecutionFailed),
        }
    }

    async fn release(&self, _lease: SandboxLease) {
        self.releases.fetch_add(1, Ordering::SeqCst);
    }
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn always() -> AuthorizationCheck {
    Arc::new(|_: &ExecutionContext| true)
}

fn grants() -> Grants {
    Grants {
        roots: BTreeMap::from([("workspace".to_owned(), Access::ReadWrite)]),
        network: NetworkGrant::Deny,
        secrets: BTreeSet::from(["REPORT_TOKEN".to_owned()]),
        limits: ExecutionLimits::default(),
    }
}

/// Proof that user `alice` may take `capability` on the profile she owns.
fn alice_authorized(capability: Capability) -> AuthorizedProfile {
    let alice = PrincipalId::new("alice").unwrap();
    let mut ownership = OwnershipRegistry::default();
    ownership
        .assign(profile("research"), alice.clone())
        .unwrap();
    let caller = Caller {
        principal: Principal {
            id: alice,
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "k1".to_owned(),
            },
        },
        restriction: Restriction::default(),
    };
    authorize(&caller, capability, &profile("research"), &ownership).unwrap()
}

fn remote(grants: Grants) -> ExecutionContext {
    ExecutionContext::remote_user(
        RunId::new(),
        &alice_authorized(Capability::RunsWrite),
        grants,
        CancellationToken::new(),
    )
    .unwrap()
}

fn local() -> ExecutionContext {
    ExecutionContext::local_operator(
        RunId::new(),
        profile("research"),
        grants(),
        CancellationToken::new(),
    )
    .unwrap()
}

fn request() -> ExecRequest {
    ExecRequest {
        program: "report".to_owned(),
        args: vec!["--quiet".to_owned()],
        root: "workspace".to_owned(),
        cwd: None,
    }
}

fn test_gate(backend: &Arc<MockBackend>) -> SandboxGate {
    SandboxGate::accepting_simulated_for_tests(backend.clone(), always())
}

// 1. Untrusted execution is denied without an approved sandbox.
#[tokio::test]
async fn untrusted_execution_is_denied_without_an_approved_sandbox() {
    let gate = SandboxGate::deny_by_default(always());
    let ctx = remote(grants());
    assert_eq!(
        gate.route(&ctx),
        Err(SandboxError::Denied(DenialReason::NoBackend))
    );
    assert_eq!(
        gate.execute(&ctx, &request()).await,
        Err(SandboxError::Denied(DenialReason::NoBackend))
    );
    // The deny backend refuses even if called directly.
    assert_eq!(
        DenyBackend.start(&ctx).await,
        Err(BackendFault::Unavailable)
    );
}

// 2. Trusted single-user execution keeps the existing local executor.
#[tokio::test]
async fn a_trusted_run_routes_to_the_existing_host_executor() {
    let mock = Arc::new(MockBackend::new(Behavior::Succeed));
    for gate in [
        SandboxGate::deny_by_default(always()),
        SandboxGate::with_backend(mock.clone(), always()),
    ] {
        let ctx = local();
        assert_eq!(gate.route(&ctx), Ok(Route::Host));
        assert!(ctx.required().is_empty());
        // The gate never runs a trusted call itself; the caller keeps using
        // the local executor exactly as before.
        assert_eq!(
            gate.execute(&ctx, &request()).await,
            Err(SandboxError::NotSandboxed)
        );
    }
    assert_eq!(MockBackend::count(&mock.starts), 0);
}

// 3. Mock backends cannot become production executors.
#[tokio::test]
async fn a_simulated_backend_is_never_a_production_executor() {
    // The mock claims every guarantee; the production gate still refuses it.
    let mock = Arc::new(MockBackend::new(Behavior::Succeed));
    let gate = SandboxGate::with_backend(mock.clone(), always());
    let ctx = remote(grants());
    assert_eq!(
        gate.route(&ctx),
        Err(SandboxError::Denied(DenialReason::NotVerified))
    );
    assert_eq!(
        gate.execute(&ctx, &request()).await,
        Err(SandboxError::Denied(DenialReason::NotVerified))
    );
    assert_eq!(MockBackend::count(&mock.starts), 0);
    assert_eq!(mock.assurance(), Assurance::Simulated);
}

// 4. A missing guarantee causes denial.
#[tokio::test]
async fn an_unsupported_isolation_requirement_is_denied() {
    for missing in Guarantee::ALL {
        let mock = Arc::new(MockBackend::new(Behavior::Succeed).without(missing));
        let gate = test_gate(&mock);
        assert_eq!(
            gate.execute(&remote(grants()), &request()).await,
            Err(SandboxError::Denied(DenialReason::Unsupported(missing))),
            "{missing:?}"
        );
        assert_eq!(MockBackend::count(&mock.starts), 0);
    }
}

// 5. Invalid profiles and execution contexts fail closed.
#[test]
fn invalid_profiles_and_contexts_fail_closed() {
    // Proof of another capability does not authorize starting execution.
    let read_only = alice_authorized(Capability::RunsRead);
    assert_eq!(
        ExecutionContext::remote_user(RunId::new(), &read_only, grants(), CancellationToken::new())
            .unwrap_err(),
        SandboxError::InvalidContext(ContextFault::NotAuthorizedToRun)
    );
    let fault = |grants: Grants| {
        ExecutionContext::remote_user(
            RunId::new(),
            &alice_authorized(Capability::RunsWrite),
            grants,
            CancellationToken::new(),
        )
        .unwrap_err()
    };
    let long = "a".repeat(65);
    for label in ["", "../etc", "/home", "Work Space", long.as_str()] {
        let mut bad = grants();
        bad.roots.insert(label.to_owned(), Access::ReadOnly);
        assert_eq!(
            fault(bad),
            SandboxError::InvalidContext(ContextFault::InvalidRoot),
            "{label:?}"
        );
    }
    for name in ["", "lower", "1ST", "A-B", "A=B"] {
        let mut bad = grants();
        bad.secrets.insert(name.to_owned());
        assert_eq!(
            fault(bad),
            SandboxError::InvalidContext(ContextFault::InvalidSecretName),
            "{name:?}"
        );
    }
    let mut bad = grants();
    bad.limits.memory_bytes = 0;
    assert_eq!(
        fault(bad),
        SandboxError::InvalidContext(ContextFault::InvalidLimits)
    );
}

#[test]
fn private_and_malformed_destinations_are_refused() {
    // Built numerically, like the SSRF guard's tests, so the repository's
    // address policy sees only range base addresses.
    let metadata = std::net::Ipv4Addr::new(169, 254, 169, 254).to_string();
    let home_lan = std::net::Ipv4Addr::new(192, 168, 1, 1).to_string();
    for host in [
        "127.0.0.1",
        "10.0.0.1",
        metadata.as_str(),
        home_lan.as_str(),
        "[::1]",
        "fd00::1",
        "::ffff:10.0.0.1",
        "localhost",
        "api.localhost",
        "0.0.0.0",
    ] {
        assert_eq!(
            Destination::new(host, 443),
            Err(SandboxError::InvalidContext(
                ContextFault::PrivateDestination
            )),
            "{host}"
        );
    }
    for (host, port) in [
        ("", 443),
        ("example.com", 0),
        ("ex ample.com", 443),
        ("-a.com", 443),
    ] {
        assert_eq!(
            Destination::new(host, port),
            Err(SandboxError::InvalidContext(
                ContextFault::InvalidDestination
            )),
            "{host}:{port}"
        );
    }
    let public = Destination::new("API.Example.com.", 443).unwrap();
    assert_eq!((public.host(), public.port()), ("api.example.com", 443));
}

// 6. Revoked authorization stops new sandbox requests.
#[tokio::test]
async fn revoked_authorization_stops_new_sandbox_requests() {
    let revoked = Arc::new(AtomicBool::new(false));
    let flag = revoked.clone();
    let mock = Arc::new(MockBackend::new(Behavior::Succeed));
    let gate = SandboxGate::accepting_simulated_for_tests(
        mock.clone(),
        Arc::new(move |_: &ExecutionContext| !flag.load(Ordering::SeqCst)),
    );
    let ctx = remote(grants());
    assert!(gate.execute(&ctx, &request()).await.is_ok());
    revoked.store(true, Ordering::SeqCst);
    assert_eq!(gate.route(&ctx), Err(SandboxError::Revoked));
    assert_eq!(
        gate.execute(&ctx, &request()).await,
        Err(SandboxError::Revoked)
    );
    assert_eq!(MockBackend::count(&mock.starts), 1);
    // A trusted run is checked too.
    assert_eq!(gate.route(&local()), Err(SandboxError::Revoked));
}

// 7. Cancellation propagates.
#[tokio::test]
async fn cancellation_propagates_and_releases_the_sandbox() {
    let mock = Arc::new(MockBackend::new(Behavior::Hang));
    let gate = test_gate(&mock);
    let ctx = remote(grants());
    let cancel = ctx.cancel().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    assert_eq!(
        gate.execute(&ctx, &request()).await,
        Err(SandboxError::Cancelled)
    );
    canceller.await.unwrap();
    assert_eq!(MockBackend::count(&mock.releases), 1);
    // Already cancelled: refused before anything starts.
    assert_eq!(gate.route(&ctx), Err(SandboxError::Cancelled));
    assert_eq!(MockBackend::count(&mock.starts), 1);
}

#[tokio::test]
async fn cancelling_a_run_cancels_its_workers() {
    let parent = remote(grants());
    let worker = parent
        .for_worker(RunId::new(), profile("research"), grants())
        .unwrap();
    let nested = worker
        .for_worker(RunId::new(), profile("research"), grants())
        .unwrap();
    parent.cancel().cancel();
    assert!(worker.cancel().is_cancelled());
    assert!(nested.cancel().is_cancelled());
}

#[tokio::test]
async fn a_wall_clock_overrun_is_stopped_by_the_gate_and_released() {
    let mock = Arc::new(MockBackend::new(Behavior::Hang));
    let gate = test_gate(&mock);
    let mut short = grants();
    short.limits.wall_clock = Duration::from_millis(20);
    assert_eq!(
        gate.execute(&remote(short), &request()).await,
        Err(SandboxError::TimedOut)
    );
    assert_eq!(MockBackend::count(&mock.releases), 1);
}

// 8. Failed sandbox initialization cannot fall back to host execution.
#[tokio::test]
async fn a_failed_sandbox_never_falls_back_to_the_host() {
    for (behavior, expected) in [
        (Behavior::FailStart, SandboxError::Unavailable),
        (Behavior::FailExecute, SandboxError::ExecutionFailed),
    ] {
        let mock = Arc::new(MockBackend::new(behavior));
        let gate = test_gate(&mock);
        // The request names a program that would leave a mark on the host if
        // anything ran it there.
        let host = TestDir::new("sandbox-fallback");
        let marker = host.path().join("ran-on-host");
        let probe = ExecRequest {
            program: "touch".to_owned(),
            args: vec![marker.display().to_string()],
            ..request()
        };
        assert_eq!(gate.execute(&remote(grants()), &probe).await, Err(expected));
        assert!(!marker.exists(), "nothing ran on the host");
        // A lease that started is released; one that never started is not.
        let released = usize::from(behavior == Behavior::FailExecute);
        assert_eq!(MockBackend::count(&mock.releases), released);
    }
}

// 9. Delegated workers cannot escape their originating limits.
#[test]
fn a_worker_cannot_exceed_its_originating_run() {
    let mut parent_grants = grants();
    parent_grants
        .roots
        .insert("reference".to_owned(), Access::ReadOnly);
    parent_grants.network =
        NetworkGrant::Allow(BTreeSet::from([
            Destination::new("api.example.com", 443).unwrap()
        ]));
    let parent = remote(parent_grants.clone());
    let widen = |change: &dyn Fn(&mut Grants)| {
        let mut requested = parent_grants.clone();
        change(&mut requested);
        parent
            .for_worker(RunId::new(), profile("research"), requested)
            .unwrap_err()
    };
    let widened = SandboxError::InvalidContext(ContextFault::WidensParent);
    assert_eq!(
        widen(&|g: &mut Grants| {
            g.roots.insert("other".to_owned(), Access::ReadOnly);
        }),
        widened
    );
    assert_eq!(
        widen(&|g: &mut Grants| {
            g.roots.insert("reference".to_owned(), Access::ReadWrite);
        }),
        widened
    );
    assert_eq!(
        widen(&|g: &mut Grants| {
            g.network =
                NetworkGrant::Allow(BTreeSet::from([
                    Destination::new("other.example.com", 443).unwrap()
                ]));
        }),
        widened
    );
    assert_eq!(
        widen(&|g: &mut Grants| {
            g.secrets.insert("PROVIDER_API_KEY".to_owned());
        }),
        widened
    );
    // Another profile is refused for an untrusted run.
    assert_eq!(
        parent
            .for_worker(RunId::new(), profile("billing"), grants())
            .unwrap_err(),
        SandboxError::InvalidContext(ContextFault::CrossProfileWorker)
    );
    // Limits are capped, trust and principal kept, isolation not weakened.
    let mut generous = grants();
    generous.limits.wall_clock = Duration::from_secs(3600);
    generous.limits.memory_bytes = u64::MAX;
    let worker = parent
        .for_worker(RunId::new(), profile("research"), generous)
        .unwrap();
    assert_eq!(worker.grants().limits, parent.grants().limits);
    assert_eq!(worker.trust(), RunTrust::RemoteUser);
    assert_eq!(worker.principal(), parent.principal());
    assert!(worker.required().is_superset(&parent.required()));
    // Narrowing is allowed: read-only of a read-write root, no network.
    let narrower = Grants {
        roots: BTreeMap::from([("workspace".to_owned(), Access::ReadOnly)]),
        network: NetworkGrant::Deny,
        secrets: BTreeSet::new(),
        limits: ExecutionLimits::default(),
    };
    assert!(
        parent
            .for_worker(RunId::new(), profile("research"), narrower)
            .is_ok()
    );
    // Depth is bounded.
    let mut deepest = parent;
    for _ in 0..MAX_DEPTH {
        deepest = deepest
            .for_worker(RunId::new(), profile("research"), grants())
            .unwrap();
    }
    assert_eq!(
        deepest
            .for_worker(RunId::new(), profile("research"), grants())
            .unwrap_err(),
        SandboxError::InvalidContext(ContextFault::DepthExceeded)
    );
}

// 10. No server secrets are inherited by untrusted execution.
#[tokio::test]
async fn untrusted_execution_inherits_no_server_secret() {
    let mock = Arc::new(MockBackend::new(Behavior::Succeed));
    let gate = test_gate(&mock);
    gate.execute(&remote(grants()), &request()).await.unwrap();
    let env = mock.child_env.lock().unwrap().clone().unwrap();
    assert_eq!(
        env,
        BTreeMap::from([("REPORT_TOKEN".to_owned(), "granted-value".to_owned())])
    );
    // With no grants the environment is empty, even though the server has
    // provider and operator keys.
    let mut none = grants();
    none.secrets.clear();
    gate.execute(&remote(none), &request()).await.unwrap();
    assert!(mock.child_env.lock().unwrap().clone().unwrap().is_empty());
}

// 11. Errors expose no sensitive path or credential.
#[test]
fn sandbox_errors_carry_no_paths_or_credentials() {
    let mut errors = vec![
        SandboxError::Revoked,
        SandboxError::Cancelled,
        SandboxError::Unavailable,
        SandboxError::ExecutionFailed,
        SandboxError::TimedOut,
        SandboxError::NotSandboxed,
        SandboxError::Denied(DenialReason::NoBackend),
        SandboxError::Denied(DenialReason::NotVerified),
        SandboxError::InvalidRequest(RequestFault::InvalidProgram),
        SandboxError::InvalidRequest(RequestFault::UngrantedRoot),
        SandboxError::InvalidRequest(RequestFault::InvalidPath),
    ];
    errors.extend(Guarantee::ALL.map(|g| SandboxError::Denied(DenialReason::Unsupported(g))));
    errors.extend(Guarantee::ALL.map(SandboxError::LimitExceeded));
    for fault in [
        ContextFault::NotAuthorizedToRun,
        ContextFault::InvalidRoot,
        ContextFault::InvalidDestination,
        ContextFault::PrivateDestination,
        ContextFault::InvalidSecretName,
        ContextFault::InvalidLimits,
        ContextFault::WidensParent,
        ContextFault::CrossProfileWorker,
        ContextFault::DepthExceeded,
    ] {
        errors.push(SandboxError::InvalidContext(fault));
    }
    for error in errors {
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendered.contains('/') && !rendered.contains('\\'),
                "{rendered}"
            );
            assert!(!rendered.contains('='), "{rendered}");
        }
    }
    // An invalid request naming a host path does not echo it.
    let ctx = remote(grants());
    let escaping = ExecRequest {
        cwd: Some("../../srv/operator/.ssh".to_owned()),
        ..request()
    };
    let error = escaping.validate(&ctx).unwrap_err();
    assert_eq!(
        error,
        SandboxError::InvalidRequest(RequestFault::InvalidPath)
    );
    assert!(!error.to_string().contains(".ssh"));
    // A backend fault carries no text to pass through.
    assert_eq!(
        SandboxError::from(BackendFault::StartFailed),
        SandboxError::Unavailable
    );
}

#[tokio::test]
async fn requests_outside_the_grants_never_reach_the_backend() {
    let mock = Arc::new(MockBackend::new(Behavior::Succeed));
    let gate = test_gate(&mock);
    let ctx = remote(grants());
    for (bad, fault) in [
        (
            ExecRequest {
                root: "home".to_owned(),
                ..request()
            },
            RequestFault::UngrantedRoot,
        ),
        (
            ExecRequest {
                cwd: Some("/etc".to_owned()),
                ..request()
            },
            RequestFault::InvalidPath,
        ),
        (
            ExecRequest {
                program: " ".to_owned(),
                ..request()
            },
            RequestFault::InvalidProgram,
        ),
        (
            ExecRequest {
                args: vec!["a\0b".to_owned()],
                ..request()
            },
            RequestFault::InvalidProgram,
        ),
    ] {
        assert_eq!(
            gate.execute(&ctx, &bad).await,
            Err(SandboxError::InvalidRequest(fault))
        );
    }
    assert_eq!(MockBackend::count(&mock.starts), 0);
}

// 12. Resource and output limits are enforced or reported unsupported.
#[tokio::test]
async fn output_is_bounded_by_the_gate_whatever_the_backend_returns() {
    let mock = Arc::new(MockBackend::new(Behavior::Flood));
    let gate = test_gate(&mock);
    let mut small = grants();
    small.limits.max_output_bytes = 1001;
    let output = gate.execute(&remote(small), &request()).await.unwrap();
    assert!(output.truncated);
    assert!(output.stdout.len() + output.stderr.len() <= 1001);
    // Cut on a character boundary: "é" is two bytes, so 1000 are kept.
    assert_eq!(output.stdout.len(), 1000);
    assert_eq!(MockBackend::count(&mock.releases), 1);
}

#[tokio::test]
async fn os_resource_limits_a_backend_does_not_enforce_are_reported_and_denied() {
    for limit in [
        Guarantee::CpuLimit,
        Guarantee::MemoryLimit,
        Guarantee::ProcessLimit,
    ] {
        let mock = Arc::new(MockBackend::new(Behavior::Succeed).without(limit));
        assert!(!mock.capabilities().enforced.contains(&limit));
        assert_eq!(
            test_gate(&mock).route(&remote(grants())),
            Err(SandboxError::Denied(DenialReason::Unsupported(limit)))
        );
    }
}

// 13. Interface mocks are never mistaken for OS isolation, on any platform.
#[test]
fn no_backend_in_this_build_claims_verified_isolation() {
    // A function that accepts the witness compiles only because it can never
    // be called: `VerifiedIsolation` has no values.
    fn _no_witness_exists(witness: VerifiedIsolation) -> ! {
        match witness {}
    }
    let backends: [Arc<dyn SandboxBackend>; 2] = [
        Arc::new(DenyBackend),
        Arc::new(MockBackend::new(Behavior::Succeed)),
    ];
    for backend in backends {
        assert!(!matches!(backend.assurance(), Assurance::Verified(_)));
        assert!(
            backend.capabilities().demonstrated_on.is_empty(),
            "{} demonstrates nothing on {:?}",
            backend.name(),
            Platform::current()
        );
    }
    assert!(DenyBackend.capabilities().enforced.is_empty());
}

/// The tool policy and the gate agree: what the policy withholds from an
/// untrusted run for lack of isolation, the production gate also refuses.
#[test]
fn the_policy_and_the_gate_agree_on_untrusted_execution() {
    let isolated_reaches = [
        crate::policy::reach_of("fs.read", &BTreeSet::new()),
        crate::policy::reach_of("terminal.run", &BTreeSet::new()),
        crate::policy::reach_of("open_terminal.run", &BTreeSet::new()),
        crate::policy::reach_of("agent.delegate", &BTreeSet::new()),
        crate::policy::reach_of("mcp.shell.exec", &BTreeSet::new()),
    ];
    assert!(
        isolated_reaches
            .iter()
            .all(|reach| reach.requires_isolation())
    );
    assert_eq!(
        SandboxGate::deny_by_default(always()).route(&remote(grants())),
        Err(SandboxError::Denied(DenialReason::NoBackend))
    );
}

/// An experimental backend (real processes, unverified guarantees) is never
/// eligible: not in production, and not even through the test-only gate
/// that accepts simulations.
#[tokio::test]
async fn an_experimental_backend_is_never_eligible_for_untrusted_runs() {
    let backend = Arc::new(MockBackend::new(Behavior::Succeed).experimental());
    for gate in [
        SandboxGate::with_backend(backend.clone(), always()),
        test_gate(&backend),
    ] {
        assert_eq!(
            gate.route(&remote(grants())),
            Err(SandboxError::Denied(DenialReason::NotVerified))
        );
        assert_eq!(
            gate.execute(&remote(grants()), &request()).await,
            Err(SandboxError::Denied(DenialReason::NotVerified))
        );
        assert_eq!(gate.route(&local()), Ok(Route::Host));
    }
    assert_eq!(MockBackend::count(&backend.starts), 0);
    assert_eq!(
        SandboxError::from(BackendFault::TimedOut),
        SandboxError::TimedOut
    );
    assert_eq!(
        SandboxError::from(BackendFault::Cancelled),
        SandboxError::Cancelled
    );
}

/// macOS and Windows have no execution backend at all: the bubblewrap
/// module is not compiled there, and the production gate denies.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn untrusted_execution_is_denied_on_macos_and_windows() {
    assert_ne!(Platform::current(), Platform::Linux);
    let gate = SandboxGate::deny_by_default(always());
    assert_eq!(gate.backend_name(), "deny");
    assert_eq!(
        gate.execute(&remote(grants()), &request()).await,
        Err(SandboxError::Denied(DenialReason::NoBackend))
    );
}
