// Shared fixtures for the revocation test binaries, included with
// `include!`: a scripted model, a probe tool, a factory that drives the
// real loop and executor, a synthetic identity world and HTTP helpers.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use lightagent_api::manager::{
    MonitorState, RevalidationClock, RunFactory, RunManager, RunState, RunStatus, StartRun, drive,
    drive_supervised,
};
use lightagent_api::{AppState, AuthConfig, IdentityAuthority, IdentityState, RunOwner, router};
use lightagent_core::identity::store::IdentityStore;
use lightagent_core::identity::{
    AuditOutcome, AuthorizedProvider, Caller, Capability, Denial, PrincipalId, PrincipalSource,
    Restriction, RevocationScope, Role, RunAuthorization, RunTrust,
};
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{
    AgentEvent, AgentEventSink, AgentLoop, AgentProfile, AgentProvider, ApprovalDecision,
    FinishReason, ModelRouting, PolicyEngine, ProfileId, ProfileStore, ProviderError,
    ProviderEvent, ProviderRequest, ProviderStream, RiskClass, RunConfig, RunId, ToolOutcome,
};
use lightagent_store::SessionStore;
use lightagent_tools::{BoundedExecutor, Delegation, Tool, ToolCtx, ToolDefinition, ToolRegistry};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

const HANG_GUARD: Duration = Duration::from_secs(30);
const T0: u64 = 1_000_000;

// --- signals ---------------------------------------------------------------

/// Sets a flag when dropped: proves a future was torn down.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A future that never completes, holding a drop flag.
fn held(dropped: &Arc<AtomicBool>) -> ProviderStream {
    let guard = DropFlag(dropped.clone());
    Box::pin(stream::unfold(guard, |guard| async move {
        std::future::pending::<()>().await;
        Some((Err(ProviderError::Transport(String::new())), guard))
    }))
}

// --- a scripted model --------------------------------------------------------

#[derive(Clone)]
enum Turn {
    Say(&'static str),
    Call(&'static str, &'static str),
    /// Stream nothing until torn down (inference in progress).
    Hold,
    /// Stream tokens without end, yielding between them as a network
    /// stream does, until torn down.
    Tokens,
    /// Never finish setting up the connection: `stream` itself never
    /// returns until torn down.
    Connect,
}

#[derive(Clone)]
struct Model {
    turns: Arc<Mutex<VecDeque<Turn>>>,
    requests: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    /// Tokens streamed by `Turn::Tokens`.
    tokens: Arc<AtomicUsize>,
    /// Runs before each request is answered, with its 1-based number: a
    /// test changes the identity state here, as another process would.
    on_request: Arc<dyn Fn(usize) + Send + Sync>,
}

impl Model {
    fn new(turns: Vec<Turn>) -> Self {
        Self::reacting(turns, |_| {})
    }

    fn reacting(turns: Vec<Turn>, on_request: impl Fn(usize) + Send + Sync + 'static) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            requests: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
            tokens: Arc::new(AtomicUsize::new(0)),
            on_request: Arc::new(on_request),
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AgentProvider for Model {
    async fn stream(
        &self,
        _request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        (self.on_request)(number);
        self.started.notify_one();
        let turn = self.turns.lock().unwrap().pop_front().unwrap_or(Turn::Hold);
        let events = match turn {
            Turn::Hold => return Ok(held(&self.dropped)),
            Turn::Connect => {
                let _guard = DropFlag(self.dropped.clone());
                return std::future::pending().await;
            }
            Turn::Tokens => {
                let state = (DropFlag(self.dropped.clone()), self.tokens.clone());
                return Ok(Box::pin(stream::unfold(state, |state| async move {
                    tokio::task::yield_now().await;
                    state.1.fetch_add(1, Ordering::SeqCst);
                    Some((Ok(ProviderEvent::Content("tok ".into())), state))
                })));
            }
            Turn::Say(text) => vec![
                ProviderEvent::Content(text.into()),
                ProviderEvent::Finished {
                    reason: FinishReason::Stop,
                    usage: None,
                },
            ],
            Turn::Call(name, arguments) => vec![
                ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: Some(format!("call_{number}")),
                    name: Some(name.into()),
                    arguments: Some(arguments.into()),
                },
                ProviderEvent::Finished {
                    reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
        };
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

impl lightagent_core::ProviderFactory for Model {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.clone()))
    }
}

// --- a probe tool ------------------------------------------------------------

/// Counts its calls; either answers at once or holds until torn down.
struct Probe {
    definition: ToolDefinition,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    hold: bool,
}

impl Probe {
    fn new(name: &str, risk: RiskClass, hold: bool) -> Arc<Self> {
        Arc::new(Self {
            definition: ToolDefinition::new(
                name,
                "A test probe.",
                json!({"type": "object"}),
                risk,
                Vec::new(),
            ),
            calls: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
            hold,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for Probe {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.hold {
            return ToolOutcome::ok("probed");
        }
        let _guard = DropFlag(self.dropped.clone());
        self.started.notify_one();
        std::future::pending::<ToolOutcome>().await
    }
}

// --- the factory -------------------------------------------------------------

/// What one run (keyed by its message) is driven with.
#[derive(Clone)]
struct Script {
    model: Model,
    tools: ToolRegistry,
    strict: bool,
    delegation: Option<Delegation>,
}

impl Script {
    fn new(model: Model) -> Self {
        Self {
            model,
            tools: ToolRegistry::new(),
            strict: false,
            delegation: None,
        }
    }

    fn with_tool(mut self, tool: Arc<Probe>) -> Self {
        self.tools.insert(tool);
        self
    }

    fn executor(&self, authorization: Option<&RunAuthorization>) -> BoundedExecutor {
        let policy = if self.strict {
            ApprovalPolicy::strict()
        } else {
            ApprovalPolicy::permissive()
        };
        let mut executor = BoundedExecutor::new(
            self.tools.clone(),
            PolicyEngine::new(policy),
            Duration::from_secs(60),
            65_536,
        )
        .with_run(RunId::new());
        if let Some(delegation) = &self.delegation {
            executor = executor.with_delegation(delegation.clone());
        }
        if let Some(authorization) = authorization {
            executor = executor.with_authorization(authorization.clone());
        }
        executor
    }
}

/// Drives each run with the script registered for its message: the real
/// loop, the real executor, and (when supervised) the run's authorization on
/// both, exactly as the production factory wires it.
#[derive(Default)]
struct Lab {
    scripts: Mutex<HashMap<String, Script>>,
    supervised: AtomicUsize,
    plain: AtomicUsize,
}

impl Lab {
    fn script(&self, message: &str, script: Script) {
        self.scripts
            .lock()
            .unwrap()
            .insert(message.to_owned(), script);
    }

    fn take(&self, message: &str) -> Script {
        self.scripts.lock().unwrap()[message].clone()
    }
}

#[async_trait]
impl RunFactory for Lab {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.plain.fetch_add(1, Ordering::SeqCst);
        let script = self.take(&request.message);
        let agent = AgentLoop::new(
            script.model.clone(),
            script.executor(None),
            RunConfig::new("lab"),
        );
        drive(agent, Vec::new(), request.message, sink, cancel, decisions).await
    }

    async fn run_supervised(
        &self,
        request: StartRun,
        _trust: RunTrust,
        authorization: RunAuthorization,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.supervised.fetch_add(1, Ordering::SeqCst);
        let script = self.take(&request.message);
        let provider = AuthorizedProvider::new(script.model.clone(), authorization.clone());
        let agent = AgentLoop::new(
            provider,
            script.executor(Some(&authorization)),
            RunConfig::new("lab"),
        );
        drive_supervised(
            agent,
            Vec::new(),
            request.message,
            sink,
            cancel,
            decisions,
            authorization,
        )
        .await
    }
}

// --- the identity world -------------------------------------------------------

fn principal(id: &str) -> PrincipalId {
    PrincipalId::new(id).unwrap()
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn key_id(token: &str) -> String {
    token[3..19].to_owned()
}

/// A revalidation clock the test drives: each `tick` waits for one message.
struct ManualClock {
    ticks: tokio::sync::Mutex<UnboundedReceiver<()>>,
}

#[async_trait]
impl RevalidationClock for ManualClock {
    async fn tick(&self) {
        let mut ticks = self.ticks.lock().await;
        if ticks.recv().await.is_none() {
            drop(ticks);
            std::future::pending::<()>().await;
        }
    }
}

fn manual_clock() -> (Arc<ManualClock>, UnboundedSender<()>) {
    let (sender, receiver) = unbounded_channel();
    (
        Arc::new(ManualClock {
            ticks: tokio::sync::Mutex::new(receiver),
        }),
        sender,
    )
}

/// Let the monitor run one sweep, and wait until it has.
async fn tick(manager: &RunManager, ticks: &UnboundedSender<()>) -> MonitorState {
    let mut state = manager.monitor_state();
    let before = state.borrow().sweeps;
    ticks.send(()).unwrap();
    *tokio::time::timeout(HANG_GUARD, state.wait_for(|now| now.sweeps > before))
        .await
        .expect("the monitor never swept")
        .unwrap()
}

/// `alice` owns `alice-bot`, `bob` owns `bob-bot`, `ops` is an operator.
/// Expiry is judged by `clock`.
struct World {
    identity: Arc<IdentityAuthority>,
    auth: AuthConfig,
    manager: RunManager,
    lab: Arc<Lab>,
    clock: Arc<AtomicU64>,
    /// Drives the manager's revalidation monitor.
    ticks: UnboundedSender<()>,
    alice: String,
    alice_short: String,
    alice_second: String,
    bob: String,
    ops: String,
}

fn setup() -> World {
    let mut state = IdentityState::default();
    let mut issue = |id: &str, role: Role, expires: Option<u64>| {
        state
            .credentials
            .issue_expiring(principal(id), role, Restriction::default(), T0, expires)
            .unwrap()
            .1
            .expose()
            .to_owned()
    };
    let alice = issue("alice", Role::User, Some(T0 + 1_000));
    let alice_short = issue("alice", Role::User, Some(T0 + 100));
    let alice_second = issue("alice", Role::User, None);
    let bob = issue("bob", Role::User, None);
    let ops = issue("ops", Role::Operator, None);
    state
        .ownership
        .assign(profile("alice-bot"), principal("alice"))
        .unwrap();
    state
        .ownership
        .assign(profile("bob-bot"), principal("bob"))
        .unwrap();
    let clock = Arc::new(AtomicU64::new(T0));
    let now = clock.clone();
    let identity = Arc::new(
        IdentityAuthority::new(state).with_clock(Arc::new(move || now.load(Ordering::SeqCst))),
    );
    let lab = Arc::new(Lab::default());
    let (paced, ticks) = manual_clock();
    let manager = RunManager::new(lab.clone()).with_revalidation(paced);
    World {
        ticks,
        auth: AuthConfig::multi_user(identity.clone(), None),
        identity,
        manager,
        lab,
        clock,
        alice,
        alice_short,
        alice_second,
        bob,
        ops,
    }
}

impl World {
    fn caller(&self, token: &str) -> Caller {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        self.auth.caller(&headers).unwrap()
    }

    /// The local operator, as the CLI or an embedding supervisor acts.
    fn operator(&self) -> Caller {
        Caller::local_operator(PrincipalSource::LocalProcess)
    }

    /// Start a supervised run of `message` on `profile` as `token`, as the
    /// multi-user API does.
    async fn start(
        &self,
        token: &str,
        profile_id: &str,
        message: &str,
        script: Script,
    ) -> Result<Arc<RunState>, Denial> {
        self.start_with(self.caller(token), profile_id, message, script)
            .await
    }

    /// [`start`](Self::start) for a caller authenticated earlier.
    async fn start_with(
        &self,
        caller: Caller,
        profile_id: &str,
        message: &str,
        script: Script,
    ) -> Result<Arc<RunState>, Denial> {
        self.lab.script(message, script);
        let trust = caller.principal.role.run_trust();
        let mut owner = RunOwner::new(
            caller.principal.id.clone(),
            Some(profile(profile_id)),
            trust,
        );
        if let PrincipalSource::Credential { key_id } = &caller.principal.source {
            owner = owner.with_credential(key_id.clone());
        }
        let check = self.identity.run_check(caller, owner.clone());
        self.manager
            .start_supervised(
                StartRun {
                    message: message.to_owned(),
                    history: Vec::new(),
                    profile: Some(profile_id.to_owned()),
                    model: None,
                    cwd: None,
                },
                owner,
                check,
                self.identity.generation(),
            )
            .await
    }

    fn revoke_credential(&self, token: &str) {
        self.identity
            .update(|state| state.credentials.revoke(&key_id(token), T0 + 1))
            .unwrap()
            .unwrap();
    }
}

/// Wait for the run to reach a terminal status.
async fn finished(run: &RunState) -> RunStatus {
    tokio::time::timeout(HANG_GUARD, async {
        let mut seen = 0;
        loop {
            let (events, status) = run.wait_from(seen).await;
            seen += events.len();
            if status.is_terminal() {
                return status;
            }
        }
    })
    .await
    .expect("the run never finished")
}

/// Wait for the run to pause for an approval.
async fn awaiting_approval(run: &RunState) {
    tokio::time::timeout(HANG_GUARD, async {
        let mut seen = 0;
        loop {
            let (events, status) = run.wait_from(seen).await;
            seen += events.len();
            if status == RunStatus::AwaitingApproval && run.pending().await.is_some() {
                return;
            }
            assert!(!status.is_terminal(), "finished instead of pausing");
        }
    })
    .await
    .expect("the run never paused")
}

async fn signalled(notify: &Notify) {
    tokio::time::timeout(HANG_GUARD, notify.notified())
        .await
        .expect("the signal never came");
}

fn says_withdrawn(events: &[AgentEvent], code: &str) -> bool {
    events.iter().any(|event| {
        matches!(event, AgentEvent::Error { message } if message.contains("withdrawn") && message.contains(code))
            || matches!(event, AgentEvent::ToolCallCompleted { outcome, .. } if outcome.content.contains("withdrawn"))
    })
}

// --- HTTP helpers ----------------------------------------------------------

async fn serve(manager: RunManager, auth: AuthConfig) -> String {
    let dir = std::env::temp_dir().join(format!("lightagent-revocation-{}", RunId::new().as_str()));
    let state = AppState {
        manager,
        auth,
        sessions: SessionStore::new(dir),
        session_profile: "default".into(),
        context_limit: 4_096,
        config_store: None,
        busy_sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
        web_root: None,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(state).into_make_service()).await;
    });
    format!("{addr}")
}

/// (status, body) for one request; an empty token sends none.
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n");
    if !token.is_empty() {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    request.push_str("\r\n");
    request.push_str(payload);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = Vec::new();
    tokio::time::timeout(HANG_GUARD, stream.read_to_end(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default();
    (status, body)
}
