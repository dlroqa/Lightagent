//! Revocation inside a run's tools: the executor, remote requests and the
//! sandbox gate all ask the run's authorization again, and a revocation
//! stops work in flight. (Delegated workers: `lightagent-api`'s
//! `revocation` tests.) Synthetic identities, local stubs only;
//! every wait is on an explicit signal, never on elapsed time.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::identity::{
    Caller, Capability, Denial, OwnershipRegistry, Principal, PrincipalId, PrincipalSource,
    Restriction, RevocationCheck, Role, RunAuthorization, authorize,
};
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{
    ApprovalDecision, ApprovalNeed, PolicyEngine, ProfileId, RiskClass, RunId, ToolCall,
    ToolInvoker, ToolOutcome,
};
use lightagent_tools::egress::Resolver;
use lightagent_tools::sandbox::{
    Assurance, BackendFault, Capabilities, DenialReason, ExecOutput, ExecRequest, ExecutionContext,
    Grants, Route, SandboxBackend, SandboxError, SandboxGate, SandboxLease, authorization_check,
};
use lightagent_tools::{
    BoundedExecutor, Tool, ToolCtx, ToolDefinition, ToolRegistry, WebContext, WebPolicy,
};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// An authorization the test revokes by flipping a switch, as another
/// process changing the identity store would.
fn switched() -> (Arc<AtomicBool>, RunAuthorization) {
    let allowed = Arc::new(AtomicBool::new(true));
    let flag = allowed.clone();
    let check: RevocationCheck = Arc::new(move || {
        if flag.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Denial::Revoked)
        }
    });
    (
        allowed,
        RunAuthorization::revocable(check, CancellationToken::new(), Some(1)),
    )
}

/// Sets a flag when dropped: proves a future was torn down.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A tool that counts its calls, signals once it has started, and then
/// waits until it is torn down (or answers at once, when `instant`).
struct Probe {
    definition: ToolDefinition,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    instant: bool,
}

impl Probe {
    fn new(risk: RiskClass, instant: bool) -> Self {
        Self {
            definition: ToolDefinition::new(
                "probe.run",
                "A test probe.",
                json!({"type": "object"}),
                risk,
                Vec::new(),
            ),
            calls: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
            instant,
        }
    }
}

#[async_trait]
impl Tool for Probe {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.instant {
            return ToolOutcome::ok("done");
        }
        let _guard = DropFlag(self.dropped.clone());
        self.started.notify_one();
        std::future::pending::<ToolOutcome>().await
    }
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Answer a call's approval the way a human granting it would, if it asks.
fn approve(executor: &BoundedExecutor, call: &ToolCall) {
    if let ApprovalNeed::Require(request) = executor.approval_for(call) {
        executor.remember(&ApprovalDecision::grant(request.id), call);
    }
}

fn executor(registry: ToolRegistry, policy: ApprovalPolicy) -> BoundedExecutor {
    BoundedExecutor::new(
        registry,
        PolicyEngine::new(policy),
        Duration::from_secs(30),
        65_536,
    )
    .with_run(RunId::new())
}

// 9. Tool dispatch is blocked after a revocation: nothing is approved or
// launched, and the run is cancelled.
#[tokio::test]
async fn a_revoked_run_launches_no_tool() {
    let probe = Arc::new(Probe::new(RiskClass::Observe, true));
    let calls = probe.calls.clone();
    let (allowed, authorization) = switched();
    let executor = executor(
        ToolRegistry::new().with(probe),
        ApprovalPolicy::permissive(),
    )
    .with_authorization(authorization.clone());
    let probe_call = call("probe.run", "{}");
    let cancel = authorization.cancel_token().clone();

    // Authorized: the call runs.
    assert_eq!(
        executor.approval_for(&probe_call),
        ApprovalNeed::AutoApprove
    );
    assert!(!executor.invoke(&probe_call, cancel.clone()).await.is_error);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    allowed.store(false, Ordering::SeqCst);
    let ApprovalNeed::Deny(reason) = executor.approval_for(&probe_call) else {
        panic!("a revoked run must not be offered for approval");
    };
    assert!(reason.contains("(revoked)"), "{reason}");
    let outcome = executor.invoke(&probe_call, cancel.clone()).await;
    assert!(outcome.is_error && outcome.content.contains("withdrawn"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no further call ran");
    assert!(cancel.is_cancelled());
    // Final: restoring the state does not restore the run.
    allowed.store(true, Ordering::SeqCst);
    assert!(executor.invoke(&probe_call, cancel).await.is_error);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Revocation while a tool executes: the call is torn down at once.
#[tokio::test]
async fn revocation_while_a_tool_executes_tears_it_down() {
    let probe = Arc::new(Probe::new(RiskClass::Observe, false));
    let (started, dropped) = (probe.started.clone(), probe.dropped.clone());
    let (_allowed, authorization) = switched();
    let executor = Arc::new(
        executor(
            ToolRegistry::new().with(probe),
            ApprovalPolicy::permissive(),
        )
        .with_authorization(authorization.clone()),
    );
    let cancel = authorization.cancel_token().clone();
    let running = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.invoke(&call("probe.run", "{}"), cancel).await })
    };
    started.notified().await;
    assert!(authorization.revoke(Denial::ProfileNotGranted));
    let outcome = running.await.unwrap();
    assert!(outcome.is_error && outcome.content.contains("cancelled"));
    assert!(
        dropped.load(Ordering::SeqCst),
        "the tool future was dropped"
    );
}

// 12. An approval granted before a revocation never lets the call run.
#[tokio::test]
async fn an_approval_granted_before_revocation_is_stale() {
    let probe = Arc::new(Probe::new(RiskClass::Mutating, true));
    let calls = probe.calls.clone();
    let (allowed, authorization) = switched();
    let executor = executor(ToolRegistry::new().with(probe), ApprovalPolicy::strict())
        .with_authorization(authorization.clone());
    let probe_call = call("probe.run", "{}");
    let ApprovalNeed::Require(request) = executor.approval_for(&probe_call) else {
        panic!("a mutating call asks first under the strict policy");
    };
    executor.remember(&ApprovalDecision::grant(request.id), &probe_call);
    allowed.store(false, Ordering::SeqCst);
    let outcome = executor
        .invoke(&probe_call, authorization.cancel_token().clone())
        .await;
    assert!(outcome.is_error && outcome.content.contains("withdrawn"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Every name resolves to loopback, so the stub is reached by name.
struct Loopback;

#[async_trait]
impl Resolver for Loopback {
    async fn resolve(&self, _host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))])
    }
}

/// A server that accepts connections, counts them, signals each, and holds
/// each open without answering until the client goes away.
struct Holding {
    port: u16,
    accepted: Arc<AtomicUsize>,
    connected: Arc<Notify>,
    closed: Arc<Notify>,
}

async fn holding_server() -> Holding {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let (connected, closed) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (count, on_connect, on_close) = (accepted.clone(), connected.clone(), closed.clone());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            on_connect.notify_one();
            let on_close = on_close.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                // Read the request and anything after it, never answering,
                // until the client closes the connection.
                while let Ok(read) = socket.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                }
                on_close.notify_one();
            });
        }
    });
    Holding {
        port,
        accepted,
        connected,
        closed,
    }
}

fn web_context() -> WebContext {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let policy = WebPolicy {
        trusted_networks: vec!["127.0.0.1/32".parse().unwrap()],
        timeout: Duration::from_secs(30),
        ..WebPolicy::default()
    };
    let mut web = WebContext::new(client, Arc::new(policy));
    web.resolver = Arc::new(Loopback);
    web
}

// 10. Remote HTTP dispatch is blocked after a revocation (no connection is
// opened), and a request in flight is abandoned when the run is revoked.
#[tokio::test]
async fn remote_http_tools_respect_revocation() {
    let server = holding_server().await;
    let url = format!(r#"{{"url":"http://stub.test:{}/page"}}"#, server.port);
    let (allowed, authorization) = switched();
    let executor = Arc::new(
        executor(ToolRegistry::builtin(), ApprovalPolicy::permissive())
            .with_web(web_context())
            .with_authorization(authorization.clone()),
    );
    let fetch = call("web.fetch", &url);

    // In flight: the stub holds the request; the revocation drops it.
    approve(&executor, &fetch);
    let running = {
        let (executor, fetch) = (executor.clone(), fetch.clone());
        let cancel = authorization.cancel_token().clone();
        tokio::spawn(async move { executor.invoke(&fetch, cancel).await })
    };
    server.connected.notified().await;
    authorization.revoke(Denial::Revoked);
    let outcome = running.await.unwrap();
    assert!(outcome.is_error && outcome.content.contains("cancelled"));
    server.closed.notified().await;
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);

    // After the revocation: no connection at all, even if the state would
    // admit the caller again.
    allowed.store(true, Ordering::SeqCst);
    let outcome = executor
        .invoke(&fetch, authorization.cancel_token().clone())
        .await;
    assert!(outcome.is_error);
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
}

// The per-hop check: a fetch whose run is revoked before it starts opens no
// connection, whichever way the tool is reached.
#[tokio::test]
async fn a_revoked_fetch_opens_no_connection() {
    let server = holding_server().await;
    let (allowed, authorization) = switched();
    allowed.store(false, Ordering::SeqCst);
    let ctx = ToolCtx::new(authorization.cancel_token().clone())
        .with_web(web_context())
        .with_authorization(authorization.clone());
    let web = web_context();
    let error = lightagent_tools::builtins::fetch_text_authorized(
        &web,
        &format!("http://stub.test:{}/page", server.port),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(error.contains("withdrawn"), "{error}");
    let search = lightagent_tools::builtins::WebSearch::new()
        .call(&json!({"query": "anything"}), &ctx)
        .await;
    assert!(search.is_error && search.content.contains("withdrawn"));
    assert_eq!(server.accepted.load(Ordering::SeqCst), 0);
}

/// A backend that reports experimental assurance, like the bubblewrap
/// backend, and would run anything if asked.
struct Experimental;

#[async_trait]
impl SandboxBackend for Experimental {
    fn name(&self) -> &'static str {
        "experimental-test"
    }

    fn assurance(&self) -> Assurance {
        Assurance::Experimental
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn start(&self, _ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault> {
        Ok(SandboxLease::new(1))
    }

    async fn execute(
        &self,
        _lease: &SandboxLease,
        _ctx: &ExecutionContext,
        _request: &ExecRequest,
    ) -> Result<ExecOutput, BackendFault> {
        Ok(ExecOutput::default())
    }

    async fn release(&self, _lease: SandboxLease) {}
}

// 20. The experimental sandbox stays unreachable for untrusted work, with or
// without a valid authorization, and the gate follows the run's
// authorization like every other enforcement point.
#[test]
fn the_experimental_sandbox_stays_inaccessible_to_untrusted_runs() {
    let research = ProfileId::new("research").unwrap();
    let alice = PrincipalId::new("alice").unwrap();
    let mut ownership = OwnershipRegistry::default();
    ownership.assign(research.clone(), alice.clone()).unwrap();
    let caller = Caller {
        principal: Principal {
            id: alice,
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "0000000000000001".to_owned(),
            },
        },
        restriction: Restriction::default(),
    };
    let authorized = authorize(&caller, Capability::RunsWrite, &research, &ownership).unwrap();
    let (allowed, authorization) = switched();
    let gate = SandboxGate::with_backend(
        Arc::new(Experimental),
        authorization_check(authorization.clone()),
    );
    // A separate token, so the authorization path is what refuses.
    let ctx = ExecutionContext::remote_user(
        RunId::new(),
        &authorized,
        Grants::default(),
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        gate.route(&ctx),
        Err(SandboxError::Denied(DenialReason::NotVerified))
    );
    let operator = ExecutionContext::local_operator(
        RunId::new(),
        research,
        Grants::default(),
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(gate.route(&operator), Ok(Route::Host));
    allowed.store(false, Ordering::SeqCst);
    assert_eq!(gate.route(&ctx), Err(SandboxError::Revoked));
    assert_eq!(gate.route(&operator), Err(SandboxError::Revoked));
    assert!(authorization.cancel_token().is_cancelled());
}

// 18. A run that is not revocable behaves as before: no checks, no change.
#[tokio::test]
async fn an_unrevocable_run_is_unchanged() {
    let probe = Arc::new(Probe::new(RiskClass::Observe, true));
    let calls = probe.calls.clone();
    let cancel = CancellationToken::new();
    let authorization = RunAuthorization::unconditional(cancel.clone());
    let plain = executor(
        ToolRegistry::new().with(probe.clone()),
        ApprovalPolicy::permissive(),
    );
    let with_unconditional = executor(
        ToolRegistry::new().with(probe),
        ApprovalPolicy::permissive(),
    )
    .with_authorization(authorization.clone());
    let probe_call = call("probe.run", "{}");
    for executor in [&plain, &with_unconditional] {
        assert_eq!(
            executor.approval_for(&probe_call),
            ApprovalNeed::AutoApprove
        );
        assert!(!executor.invoke(&probe_call, cancel.clone()).await.is_error);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(authorization.checks(), 0);
    assert!(!cancel.is_cancelled());
}
