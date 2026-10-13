//! Revocation during a run's lifecycle, through the run manager (the
//! supervisor) and a factory that drives the real agent loop with the real
//! bounded executor. Identities, credentials and models are synthetic; every
//! wait is on an explicit signal or a terminal status, never on elapsed
//! time (a generous timeout only guards against a hang).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use lightagent_api::manager::{
    RunFactory, RunManager, RunState, RunStatus, StartRun, drive, drive_supervised,
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
use tokio::sync::mpsc::UnboundedReceiver;
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
}

#[derive(Clone)]
struct Model {
    turns: Arc<Mutex<VecDeque<Turn>>>,
    requests: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
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

/// `alice` owns `alice-bot`, `bob` owns `bob-bot`, `ops` is an operator.
/// Expiry is judged by `clock`.
struct World {
    identity: Arc<IdentityAuthority>,
    auth: AuthConfig,
    manager: RunManager,
    lab: Arc<Lab>,
    clock: Arc<AtomicU64>,
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
    let manager = RunManager::new(lab.clone());
    World {
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

// --- the lifecycle -------------------------------------------------------------

// Revocation before the run starts: no run is created, nothing runs.
#[tokio::test]
async fn revocation_before_a_run_starts_refuses_it() {
    let world = setup();
    // Authenticated, then revoked before the run is created.
    let alice = world.caller(&world.alice);
    world.revoke_credential(&world.alice);
    let started = world
        .start_with(alice, "alice-bot", "early", Script::new(Model::new(vec![])))
        .await;
    assert_eq!(started.err(), Some(Denial::Revoked));
    // A profile the caller no longer holds is refused the same way.
    world
        .identity
        .update(|state| state.ownership.remove(&profile("bob-bot")))
        .unwrap();
    let started = world
        .start(
            &world.bob,
            "bob-bot",
            "orphaned",
            Script::new(Model::new(vec![])),
        )
        .await;
    assert_eq!(started.err(), Some(Denial::ProfileUnowned));
    assert_eq!(world.lab.supervised.load(Ordering::SeqCst), 0);
}

// Revocation during model inference: the stream in flight is torn down.
#[tokio::test]
async fn revocation_during_inference_stops_the_model() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "think",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    let revoked = world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(revoked, vec![run.id().to_owned()]);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        model.dropped.load(Ordering::SeqCst),
        "inference was abandoned"
    );
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert_eq!(model.requests(), 1);
}

// Revocation before a tool invocation, observed by the run itself (the
// identity changed underneath it, as another process would change the
// store): the tool never runs and the model is not asked again.
#[tokio::test]
async fn revocation_before_a_tool_blocks_it_and_any_further_turn() {
    let world = setup();
    let identity = world.identity.clone();
    let alice_key = key_id(&world.alice);
    let model = Model::reacting(
        vec![Turn::Call("probe.run", "{}"), Turn::Say("done")],
        move |number| {
            if number == 1 {
                identity
                    .update(|state| state.credentials.revoke(&alice_key, T0 + 1))
                    .unwrap()
                    .unwrap();
            }
        },
    );
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "use a tool",
            Script::new(model.clone()).with_tool(probe.clone()),
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the tool never ran");
    assert_eq!(model.requests(), 1, "no model request after the revocation");
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert!(says_withdrawn(&run.events().await, "revoked"));
    // The refusal is audited once, with codes only.
    let refusals = world
        .identity
        .audit_log()
        .into_iter()
        .filter(|record| record.outcome == AuditOutcome::Denied(Denial::Revoked))
        .count();
    assert_eq!(refusals, 1);
}

// Revocation while a tool executes (by suspension: every credential of the
// principal): the call is torn down.
#[tokio::test]
async fn suspension_while_a_tool_executes_tears_it_down() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.hold", "{}")]);
    let probe = Probe::new("probe.hold", RiskClass::Observe, true);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "hold a tool",
            Script::new(model).with_tool(probe.clone()),
        )
        .await
        .unwrap();
    signalled(&probe.started).await;
    world
        .identity
        .update(|state| {
            state
                .credentials
                .revoke_principal(&principal("alice"), T0 + 1)
        })
        .unwrap();
    let revoked = world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Principal(principal("alice")),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        probe.dropped.load(Ordering::SeqCst),
        "the tool was torn down"
    );
    assert_eq!(probe.calls(), 1);
}

// Revocation during an approval wait ends the wait; the approval can no
// longer be delivered, and the awaited call never runs.
#[tokio::test]
async fn revocation_during_an_approval_wait_ends_it() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.mutate", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.mutate", RiskClass::Mutating, false);
    let mut script = Script::new(model).with_tool(probe.clone());
    script.strict = true;
    let run = world
        .start(&world.alice, "alice-bot", "ask first", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Profile(profile("alice-bot")),
            Denial::ProfileNotGranted,
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(!run.decide(ApprovalDecision::grant(pending.approval_id)));
    assert_eq!(probe.calls(), 0);
    assert!(says_withdrawn(&run.events().await, "profile_not_granted"));
}

// 12. A stale approval: decided (and delivered) after the identity changed
// but before the run observed it. The run checks before acting on it.
#[tokio::test]
async fn a_stale_approval_never_resumes_the_run() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.mutate", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.mutate", RiskClass::Mutating, false);
    let mut script = Script::new(model.clone()).with_tool(probe.clone());
    script.strict = true;
    let run = world
        .start(&world.alice, "alice-bot", "stale", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world.revoke_credential(&world.alice);
    assert!(
        run.decide(ApprovalDecision::grant(pending.approval_id)),
        "the change was not yet observed, so the decision is queued"
    );
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the approved call never ran");
    assert_eq!(model.requests(), 1);
    assert_eq!(run.revocation(), Some(Denial::Revoked));
}

// Profile reassignment while runs are active: the previous owner's run
// stops at the supervisor's re-check; the new owner's and unrelated runs
// continue.
#[tokio::test]
async fn reassignment_stops_only_the_previous_owners_run() {
    let world = setup();
    let alice_model = Model::new(vec![Turn::Hold]);
    let bob_model = Model::new(vec![Turn::Hold]);
    let alice_run = world
        .start(
            &world.alice,
            "alice-bot",
            "alice",
            Script::new(alice_model.clone()),
        )
        .await
        .unwrap();
    let bob_run = world
        .start(&world.bob, "bob-bot", "bob", Script::new(bob_model.clone()))
        .await
        .unwrap();
    signalled(&alice_model.started).await;
    signalled(&bob_model.started).await;
    world
        .identity
        .update(|state| {
            state
                .ownership
                .reassign(profile("alice-bot"), principal("bob"))
        })
        .unwrap();
    let revoked = world.manager.revalidate().await;
    assert_eq!(
        revoked,
        vec![(alice_run.id().to_owned(), Denial::ProfileNotGranted)]
    );
    assert_eq!(finished(&alice_run).await, RunStatus::Cancelled);
    assert_eq!(bob_run.status().await, RunStatus::Running);
    assert!(!bob_model.dropped.load(Ordering::SeqCst));
    // Nothing further is revoked by a second pass.
    assert!(world.manager.revalidate().await.is_empty());
    bob_run.cancel();
    assert_eq!(finished(&bob_run).await, RunStatus::Cancelled);
    assert_eq!(bob_run.revocation(), None, "cancelled, not revoked");
}

// 14. Concurrent runs: revoking one credential stops exactly the runs it
// started; another credential of the same user, other users and operators
// are unaffected.
#[tokio::test]
async fn concurrent_runs_are_revoked_precisely() {
    let world = setup();
    let mut runs = Vec::new();
    for (token, profile_id, message) in [
        (&world.alice, "alice-bot", "a1"),
        (&world.alice, "alice-bot", "a2"),
        (&world.alice_second, "alice-bot", "a3"),
        (&world.bob, "bob-bot", "b1"),
        (&world.ops, "alice-bot", "o1"),
    ] {
        let model = Model::new(vec![Turn::Hold]);
        let run = world
            .start(token, profile_id, message, Script::new(model.clone()))
            .await
            .unwrap();
        runs.push((message, run, model));
    }
    for (_, _, model) in &runs {
        signalled(&model.started).await;
    }
    let operator = world.operator();
    let mut revoked = world
        .manager
        .revoke(
            &operator,
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    revoked.sort();
    let mut expected: Vec<String> = runs[..2]
        .iter()
        .map(|(_, run, _)| run.id().to_owned())
        .collect();
    expected.sort();
    assert_eq!(revoked, expected);
    // Ownership and grants bound users' runs only: the operator's run on the
    // same profile is untouched.
    let by_profile = world
        .manager
        .affected_by(&RevocationScope::Profile(profile("alice-bot")))
        .await;
    assert!(
        by_profile
            .iter()
            .all(|run| run.owner().trust() == RunTrust::RemoteUser)
    );
    for (message, run, _) in &runs[..2] {
        assert_eq!(finished(run).await, RunStatus::Cancelled, "{message}");
    }
    for (message, run, model) in &runs[2..] {
        assert_eq!(run.status().await, RunStatus::Running, "{message}");
        assert!(!model.dropped.load(Ordering::SeqCst), "{message}");
        assert_eq!(run.revocation(), None);
    }
    // Revoking again finds nothing new.
    assert!(
        world
            .manager
            .revoke(
                &operator,
                &RevocationScope::Credential(key_id(&world.alice)),
                Denial::Revoked,
            )
            .await
            .unwrap()
            .is_empty()
    );
    for (_, run, _) in &runs[2..] {
        run.cancel();
        finished(run).await;
    }
}

// Only an operator holding `administer` may revoke through the supervisor.
#[tokio::test]
async fn only_an_operator_may_revoke_runs() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(&world.bob, "bob-bot", "mine", Script::new(model.clone()))
        .await
        .unwrap();
    signalled(&model.started).await;
    let scope = RevocationScope::Principal(principal("bob"));
    let user = world.caller(&world.alice);
    assert_eq!(
        world.manager.revoke(&user, &scope, Denial::Revoked).await,
        Err(Denial::MissingCapability)
    );
    let mut narrowed = world.operator();
    narrowed.restriction.capabilities = Some([Capability::RunsWrite].into());
    assert_eq!(
        world
            .manager
            .revoke(&narrowed, &scope, Denial::Revoked)
            .await,
        Err(Denial::MissingCapability)
    );
    assert_eq!(run.status().await, RunStatus::Running);
    // The operator's ordinary cancellation still works and is not a
    // revocation.
    run.cancel();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(run.revocation(), None);
}

// 8. Credential expiry during an open event stream: the expiring
// subscriber's stream ends at once while the run (started with another,
// longer credential) continues; the run stops when its own credential
// expires. An approval for a revoked run is refused as stale.
#[tokio::test]
async fn expiry_ends_streams_and_runs_and_stales_approvals() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "watched",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    let addr = serve(world.manager.clone(), world.auth.clone()).await;
    let mut stream = TcpStream::connect(&addr).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/lightagent/v1/runs/{}/events HTTP/1.1\r\nHost: local\r\nConnection: close\r\nAuthorization: Bearer {}\r\n\r\n",
                run.id(),
                world.alice_short
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains("run.started") {
        let read = tokio::time::timeout(HANG_GUARD, stream.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(read > 0, "the stream closed before any event");
        seen.extend_from_slice(&buffer[..read]);
    }
    // The subscriber's credential expires; the supervisor asks open
    // streams to re-check at once.
    world.clock.store(T0 + 100, Ordering::SeqCst);
    assert!(world.manager.revalidate().await.is_empty(), "the run holds");
    let mut rest = Vec::new();
    tokio::time::timeout(HANG_GUARD, stream.read_to_end(&mut rest))
        .await
        .expect("an expired subscriber's stream must end")
        .unwrap();
    assert_eq!(run.status().await, RunStatus::Running);

    // The run's own credential expires.
    world.clock.store(T0 + 1_000, Ordering::SeqCst);
    let revoked = world.manager.revalidate().await;
    assert_eq!(revoked, vec![(run.id().to_owned(), Denial::Expired)]);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    let (status, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{}", run.id()),
        &world.ops,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["revocation"],
        json!("expired")
    );
    let (status, body) = http(
        &addr,
        "POST",
        &format!("/api/lightagent/v1/approvals/{}", run.id()),
        &world.ops,
        Some(r#"{"approve":true}"#),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("withdrawn") && body.contains("expired"));
}

// 15. A persistent authority whose store becomes unreadable fails closed:
// the run stops at its next check.
#[tokio::test]
async fn an_unreadable_identity_store_stops_revocable_runs() {
    let root =
        std::env::temp_dir().join(format!("lightagent-revocation-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    ProfileStore::new(&root)
        .create(&AgentProfile::new(
            profile("research"),
            "Research",
            "Persona",
            "fast",
        ))
        .unwrap();
    let operator = Caller::local_operator(PrincipalSource::LocalProcess);
    let store = IdentityStore::new(&root);
    let token = match store
        .transact(&operator, T0, None, |tx| {
            tx.create_principal(principal("alice"), Role::User, None)?;
            tx.assign_owner(&profile("research"), &principal("alice"))?;
            tx.issue_credential(&principal("alice"), Restriction::default(), None)
                .map(|(_, token)| token.expose().to_owned())
        })
        .unwrap()
    {
        lightagent_core::identity::store::Outcome::Applied { value, .. } => value,
        lightagent_core::identity::store::Outcome::Replayed { .. } => panic!("not replayed"),
    };
    let identity = Arc::new(IdentityAuthority::persistent(Arc::new(IdentityStore::new(
        &root,
    ))));
    let auth = AuthConfig::multi_user(identity.clone(), None);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let caller = auth.caller(&headers).unwrap();
    let lab = Arc::new(Lab::default());
    let manager = RunManager::new(lab.clone());
    let model = Model::new(vec![Turn::Hold]);
    lab.script("durable", Script::new(model.clone()));
    let owner = RunOwner::new(
        principal("alice"),
        Some(profile("research")),
        RunTrust::RemoteUser,
    );
    let generation = identity.generation();
    assert_eq!(generation, Some(1));
    let run = manager
        .start_supervised(
            StartRun {
                message: "durable".to_owned(),
                history: Vec::new(),
                profile: Some("research".to_owned()),
                model: None,
                cwd: None,
            },
            owner.clone(),
            identity.run_check(caller, owner),
            generation,
        )
        .await
        .unwrap();
    assert_eq!(run.authorization().generation(), Some(1));
    signalled(&model.started).await;
    assert!(manager.revalidate().await.is_empty());
    std::fs::write(root.join("identity").join("state.json"), b"{corrupt").unwrap();
    let revoked = manager.revalidate().await;
    assert_eq!(revoked.len(), 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    std::fs::remove_dir_all(&root).ok();
}

// 18. The single-user API is unchanged: runs are not revocable, never
// checked, and report no revocation.
#[tokio::test]
async fn single_user_runs_are_unchanged() {
    let lab = Arc::new(Lab::default());
    let manager = RunManager::new(lab.clone());
    let model = Model::new(vec![Turn::Call("probe.run", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    lab.script("hello", Script::new(model.clone()).with_tool(probe.clone()));
    let addr = serve(manager.clone(), AuthConfig::open()).await;
    let (status, body) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        "",
        Some(r#"{"message":"hello"}"#),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run = manager.get(&id).await.unwrap();
    assert_eq!(finished(&run).await, RunStatus::Completed);
    assert_eq!(probe.calls(), 1);
    assert_eq!(model.requests(), 2);
    assert!(!run.authorization().is_revocable());
    assert_eq!(run.authorization().checks(), 0);
    assert_eq!(lab.plain.load(Ordering::SeqCst), 1);
    assert_eq!(lab.supervised.load(Ordering::SeqCst), 0);
    assert!(manager.revalidate().await.is_empty());
    let (_, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        "",
        None,
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(body.get("revocation").is_none(), "{body}");
}

// 11. Delegated workers: none starts for a revoked run; one in flight is
// torn down with its run; and the worker's own tools and model turns check
// the run's authorization.
#[tokio::test]
async fn delegated_workers_follow_the_runs_authorization() {
    let root =
        std::env::temp_dir().join(format!("lightagent-delegation-{}", RunId::new().as_str()));
    let profiles = ProfileStore::new(root.clone());
    let mut worker_profile = AgentProfile::new(profile("worker"), "Worker", "You help.", "w");
    worker_profile.approval_policy = lightagent_core::ApprovalPolicy::Permissive;
    profiles.save(&worker_profile).unwrap();
    let delegation = |worker: &Model, tools: ToolRegistry| {
        Delegation::new(
            Arc::new(ProfileStore::new(root.clone())),
            Arc::new(worker.clone()),
            tools,
            Duration::from_secs(60),
            65_536,
        )
    };
    let delegate = r#"{"profile":"worker","task":"help"}"#;
    let mut builtin = ToolRegistry::new();
    builtin.insert(Arc::new(lightagent_tools::builtins::AgentDelegate::new()));

    // In flight: the run approves the delegation, the worker's model holds,
    // and revoking the run tears the worker down.
    let world = setup();
    let worker = Model::new(vec![Turn::Hold]);
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate)]);
    let mut script = Script::new(parent);
    script.tools = builtin.clone();
    script.delegation = Some(delegation(&worker, ToolRegistry::new()));
    let run = world
        .start(&world.alice, "alice-bot", "delegate", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    assert!(run.decide(ApprovalDecision::grant(pending.approval_id)));
    signalled(&worker.started).await;
    world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        worker.dropped.load(Ordering::SeqCst),
        "the worker was torn down"
    );

    // Revoked before delegation is approved: no worker starts.
    let worker = Model::new(vec![Turn::Say("never")]);
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate)]);
    let mut script = Script::new(parent);
    script.tools = builtin.clone();
    script.delegation = Some(delegation(&worker, ToolRegistry::new()));
    let run = world
        .start(&world.bob, "bob-bot", "late delegate", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world.revoke_credential(&world.bob);
    run.decide(ApprovalDecision::grant(pending.approval_id));
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(worker.requests(), 0, "no worker was started");

    // Revoked mid-worker, observed by the worker's own checks.
    let world = setup();
    let identity = world.identity.clone();
    let alice_key = key_id(&world.alice);
    let worker = Model::reacting(vec![Turn::Call("probe.run", "{}")], move |_| {
        identity
            .update(|state| state.credentials.revoke(&alice_key, T0 + 1))
            .unwrap()
            .unwrap();
    });
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    let mut worker_tools = ToolRegistry::new();
    worker_tools.insert(probe.clone());
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate), Turn::Say("x")]);
    let mut script = Script::new(parent.clone());
    script.tools = builtin;
    script.delegation = Some(delegation(&worker, worker_tools));
    let run = world
        .start(&world.alice, "alice-bot", "mid worker", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    assert!(run.decide(ApprovalDecision::grant(pending.approval_id)));
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the worker's tool never ran");
    assert_eq!(worker.requests(), 1, "the worker took no second turn");
    assert_eq!(parent.requests(), 1, "the run took no second turn");
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    std::fs::remove_dir_all(&root).ok();
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
