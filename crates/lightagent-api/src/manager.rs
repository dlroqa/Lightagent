//! The run manager: drives agent runs in the background and lets HTTP handlers
//! observe them.
//!
//! Starting a run spawns a task that drives the core loop with a live event
//! sink; every event is buffered on the run and broadcast to any SSE subscriber,
//! so a client that connects late gets the history and then the live tail. A run
//! that pauses for approval records the pending request and waits on a decision
//! channel the approvals endpoint feeds; a cancel token stops both the model and
//! the tools. Building the concrete loop is left to a [`RunFactory`], so tests
//! inject a mock provider and production injects the Lightweight one.
//!
//! The manager is also the run **supervisor** for revocation. A run started
//! with [`RunManager::start_supervised`] carries a [`RunAuthorization`] that
//! is re-checked at every enforcement point (model turn, tool launch, remote
//! request, delegated worker, approval), and the supervisor can find and
//! revoke the runs an identity change affects ([`RunManager::revoke`]) or
//! re-check every active run at once ([`RunManager::revalidate`]). A run
//! started with [`RunManager::start`] or [`RunManager::start_as`] is not
//! revocable and behaves exactly as before.
//!
//! While revocable runs are active, one shared **monitor** task re-checks
//! them on a bounded interval ([`RunManager::with_revalidation`]), so a
//! revocation reaches a run that is waiting on a long model stream, tool,
//! worker or approval without waiting for its next action. Each sweep reads
//! the identity store once, however many runs it checks. The monitor exists
//! only while a revocable run is active, so trusted single-user runs never
//! start it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::identity::{
    Caller, Capability, Denial, PrincipalId, RevocationCheck, RevocationScope, Role,
    RunAuthorization, RunTrust, authorize_capability, revoked_message, sweep,
};
use lightagent_core::provider::ProviderMessage;
use lightagent_core::{
    AgentEvent, AgentEventSink, AgentLoop, AgentProvider, ApprovalDecision, ApprovalId, ProfileId,
    RunId, RunOutcome, StopReason, ToolInvoker,
};
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::{Mutex, Notify, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The lifecycle state of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The run is executing.
    Running,
    /// The run is paused, waiting on an approval decision.
    AwaitingApproval,
    /// The run finished normally.
    Completed,
    /// The run was cancelled.
    Cancelled,
    /// The run ended with an error.
    Failed,
}

impl RunStatus {
    /// Whether the run has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

/// What a caller asks to start.
#[derive(Clone, Debug)]
pub struct StartRun {
    pub message: String,
    /// Completed user/assistant turns that precede `message` in the same
    /// session. Empty for a genuinely new session or a stateless API call.
    pub history: Vec<ProviderMessage>,
    pub profile: Option<String>,
    /// Optional model id selected for this run; validated by the provider.
    pub model: Option<String>,
    /// The working directory for this run (an ACP session's `cwd`); when set it
    /// becomes the confined workspace root instead of the profile's default.
    pub cwd: Option<String>,
}

/// Who started a run and as which profile, fixed when the run is created.
///
/// A run with no `principal` is **operator-only**: every run started through
/// [`RunManager::start`] (the single-user API, ACP, and any run that predates
/// multi-user mode). Ownership is never inferred later from a profile name or
/// from knowing the run id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOwner {
    principal: Option<PrincipalId>,
    profile: Option<ProfileId>,
    trust: RunTrust,
    /// The public key id of the credential that started the run, if one did.
    credential: Option<String>,
}

impl RunOwner {
    /// A run only operators may observe or control.
    pub fn operator_only() -> Self {
        Self::default()
    }

    /// A run started by `principal` as the authorized `profile`, with the
    /// trust its role carries.
    pub fn new(principal: PrincipalId, profile: Option<ProfileId>, trust: RunTrust) -> Self {
        Self {
            principal: Some(principal),
            profile,
            trust,
            credential: None,
        }
    }

    /// Record the public key id of the credential that started the run, so
    /// a supervisor can find the run when that credential is revoked.
    pub fn with_credential(mut self, key_id: impl Into<String>) -> Self {
        self.credential = Some(key_id.into());
        self
    }

    /// The public key id of the starting credential, if any. Never a secret.
    pub fn credential(&self) -> Option<&str> {
        self.credential.as_deref()
    }

    /// Whether an identity change described by `scope` can affect this run.
    /// Ownership and grants bound only a user's run; an operator's run is
    /// affected only through its own credential or principal.
    pub fn affected_by(&self, scope: &RevocationScope) -> bool {
        let user = self.trust == RunTrust::RemoteUser;
        match scope {
            RevocationScope::Credential(key_id) => self.credential() == Some(key_id.as_str()),
            RevocationScope::Principal(principal) => self.principal() == Some(principal),
            RevocationScope::Profile(profile) => user && self.profile() == Some(profile),
            RevocationScope::Grant { profile, principal } => {
                user && self.profile() == Some(profile) && self.principal() == Some(principal)
            }
        }
    }

    /// How far the run's starter is trusted with the server's OS account.
    pub fn trust(&self) -> RunTrust {
        self.trust
    }

    pub fn principal(&self) -> Option<&PrincipalId> {
        self.principal.as_ref()
    }

    pub fn profile(&self) -> Option<&ProfileId> {
        self.profile.as_ref()
    }
}

/// The shared state of one run, observed by HTTP handlers.
pub struct RunState {
    id: String,
    owner: RunOwner,
    events: Mutex<Vec<AgentEvent>>,
    status: Mutex<RunStatus>,
    pending: Mutex<Option<PendingApproval>>,
    notify: Notify,
    cancel: CancellationToken,
    decisions: mpsc::UnboundedSender<ApprovalDecision>,
    authorization: RunAuthorization,
    /// Bumped by the supervisor so open event streams re-check their
    /// subscriber at once instead of at their next periodic check.
    recheck: watch::Sender<u64>,
    /// Test-only: a pause the next waiter takes between reading the state
    /// and waiting, so a test can land a change exactly there.
    #[cfg(test)]
    gap: StdMutex<Option<Gap>>,
}

/// A pending approval, as the approvals endpoint reports it.
#[derive(Clone, Debug, Serialize)]
pub struct PendingApproval {
    pub approval_id: ApprovalId,
    pub tool_call_id: String,
    pub tool: String,
    pub risk: String,
}

impl RunState {
    /// This run's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Who started this run, as which profile. Immutable.
    pub fn owner(&self) -> &RunOwner {
        &self.owner
    }

    /// The current status.
    pub async fn status(&self) -> RunStatus {
        *self.status.lock().await
    }

    /// A snapshot of the events buffered so far.
    pub async fn events(&self) -> Vec<AgentEvent> {
        self.events.lock().await.clone()
    }

    /// How many events are buffered, without copying them.
    pub async fn event_count(&self) -> usize {
        self.events.lock().await.len()
    }

    /// Why the run stopped, once its terminal event is buffered.
    pub async fn stop_reason(&self) -> Option<StopReason> {
        let events = self.events.lock().await;
        events.iter().rev().find_map(|event| match event {
            AgentEvent::RunCompleted { reason } => Some(*reason),
            _ => None,
        })
    }

    /// The pending approval, if the run is waiting for one.
    pub async fn pending(&self) -> Option<PendingApproval> {
        self.pending.lock().await.clone()
    }

    /// Cancel the run.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// A clone of this run's cancellation token, so a caller can wait for the run
    /// to be cancelled (e.g. while awaiting an external answer).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Submit an approval decision to a waiting run. Returns whether it was
    /// delivered (the run may already have moved on). A revoked run takes no
    /// decision; one already queued is checked again before it is used.
    pub fn decide(&self, decision: ApprovalDecision) -> bool {
        if self.authorization.revoked().is_some() {
            return false;
        }
        self.decisions.send(decision).is_ok()
    }

    /// The run's authorization, re-checked during the run.
    pub fn authorization(&self) -> &RunAuthorization {
        &self.authorization
    }

    /// Why the run lost its authorization, if it did.
    pub fn revocation(&self) -> Option<Denial> {
        self.authorization.revoked()
    }

    /// A receiver that changes whenever the supervisor asks open streams to
    /// re-check their subscriber.
    pub fn rechecks(&self) -> watch::Receiver<u64> {
        self.recheck.subscribe()
    }

    fn request_recheck(&self) {
        self.recheck
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Wait until an event beyond `seen` is available or the run is terminal.
    /// Returns the new events and the current status.
    ///
    /// The waiter registers for the next notification *before* it reads the
    /// state. `push_event` and `set_status` wake waiters with
    /// [`Notify::notify_waiters`], which reaches only waiters already
    /// registered, so a change made after the read could otherwise be missed
    /// and the waiter would sleep past the run's end. Enabling the
    /// [`Notified`](tokio::sync::futures::Notified) future first means every
    /// change is either seen by the read or delivered to the waiter.
    pub async fn wait_from(&self, seen: usize) -> (Vec<AgentEvent>, RunStatus) {
        loop {
            let mut notified = std::pin::pin!(self.notify.notified());
            notified.as_mut().enable();
            {
                let events = self.events.lock().await;
                let status = *self.status.lock().await;
                if events.len() > seen || status.is_terminal() {
                    return (events[seen.min(events.len())..].to_vec(), status);
                }
            }
            #[cfg(test)]
            self.in_the_gap().await;
            notified.await;
        }
    }

    async fn push_event(&self, event: AgentEvent) {
        self.events.lock().await.push(event);
        self.notify.notify_waiters();
    }

    async fn set_status(&self, status: RunStatus) {
        *self.status.lock().await = status;
        self.notify.notify_waiters();
    }

    async fn set_pending(&self, pending: Option<PendingApproval>) {
        *self.pending.lock().await = pending;
    }
}

/// Builds and drives the concrete agent loop for a run.
///
/// Capabilities discovered from the provider currently selected by the harness.
///
/// This is intentionally a transport-neutral API shape. The concrete run
/// factory owns the provider protocol and performs the discovery.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderCapabilities {
    pub provider: String,
    pub base_url: String,
    pub configured_model: Option<String>,
    pub models: Vec<String>,
    /// Friendly labels configured for model IDs; inference continues to use IDs.
    pub model_aliases: std::collections::BTreeMap<String, String>,
    /// Extra configured IDs that the UI should show before provider discovery.
    pub model_catalog: std::collections::BTreeMap<String, String>,
    /// Models known to the provider's optional runtime control plane. Entries
    /// that are not loaded are informational: a chat run never changes the
    /// resident model as a side effect.
    pub runtime_models: Vec<RuntimeModel>,
    pub streaming: bool,
    pub tool_calls: bool,
    pub reasoning_content: bool,
}

/// A non-sensitive runtime catalog entry suitable for a model picker.
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeModel {
    pub id: String,
    pub name: Option<String>,
    pub state: String,
    pub supported: Option<bool>,
}

/// The manager is generic over how a run is executed: a test supplies a factory
/// over `MockProvider`; production supplies one over the Lightweight provider and
/// the bounded tool executor. The factory calls [`drive`] with the loop it built.
#[async_trait]
pub trait RunFactory: Send + Sync + 'static {
    /// Tools currently visible to the factory's active target.
    async fn tools(&self) -> Result<Vec<lightagent_tools::ToolDefinition>, String> {
        let registry = lightagent_tools::ToolRegistry::builtin();
        Ok(registry
            .names()
            .iter()
            .filter_map(|name| registry.get(name).map(|tool| tool.definition().clone()))
            .collect())
    }

    /// The tools a run of `profile` started with `trust` would receive: the
    /// enabled tools narrowed by that profile's tool policy. The default
    /// cannot compute it, so a restricted catalog is refused and the tool
    /// catalog stays operator-only in multi-user mode.
    async fn tools_for(
        &self,
        _profile: &ProfileId,
        _trust: RunTrust,
    ) -> Result<Vec<lightagent_tools::ToolDefinition>, String> {
        Err("a profile-scoped tool catalog is not available in this embedding".to_owned())
    }

    /// Each specialist role's readiness for a run of `profile` (the active
    /// one when `None`) started with `trust`. The default refuses, keeping it
    /// operator-only where the embedding cannot compute it.
    async fn workers_for(
        &self,
        _profile: Option<&ProfileId>,
        _trust: RunTrust,
    ) -> Result<serde_json::Value, String> {
        Err("specialist readiness is not available in this embedding".to_owned())
    }

    /// The Jev status an ordinary user of `profile` may see: that profile's
    /// own permissions and latest classification only (no endpoint,
    /// verification, threshold, model or other profile's state). The default
    /// refuses, keeping Jev status operator-only.
    async fn jev_status_for(&self, _profile: &ProfileId) -> Result<serde_json::Value, String> {
        Err("a profile-scoped Jev status is not available in this embedding".to_owned())
    }

    /// Discover capabilities from the active provider.
    async fn provider_capabilities(&self) -> Result<ProviderCapabilities, String> {
        Err("provider capability discovery is not available in this embedding".to_owned())
    }

    /// The privacy-safe Jev status for the active profile: what is configured,
    /// enabled and verified, and that profile's latest classification record.
    async fn jev_status(&self) -> Result<serde_json::Value, String> {
        Err("Jev status is not available in this embedding".to_owned())
    }

    /// Run the operator's Jev connection check with a synthetic request.
    async fn verify_jev(&self) -> Result<serde_json::Value, String> {
        Err("Jev verification is not available in this embedding".to_owned())
    }

    /// Drive `request` to a terminal [`RunStatus`], emitting events to `sink`,
    /// obeying `cancel`, and taking approval decisions from `decisions`.
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus;

    /// Drive `request` for a starter of the given `trust`. A factory that
    /// can confine an untrusted run's tools overrides this; the default
    /// runs a local operator's request as [`run`](Self::run) and **refuses**
    /// an untrusted one rather than give it operator tools.
    async fn run_as(
        &self,
        request: StartRun,
        trust: RunTrust,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        match trust {
            RunTrust::LocalOperator => self.run(request, sink, cancel, decisions).await,
            RunTrust::RemoteUser => {
                let _ = sink.send(AgentEvent::Error {
                    message: "this runtime cannot confine an untrusted run's tools".to_owned(),
                });
                RunStatus::Failed
            }
        }
    }

    /// Drive a revocable run. `authorization` shares the run's `cancel`
    /// token. The default checks it once at the start and then runs
    /// [`run_as`](Self::run_as), so a revocation still cancels the run but
    /// is not checked inside its tools; a factory that builds its own loop
    /// overrides this to put `authorization` on its executor and provider
    /// and to drive with [`drive_supervised`].
    async fn run_supervised(
        &self,
        request: StartRun,
        trust: RunTrust,
        authorization: RunAuthorization,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        if let Err(denial) = authorization.verify() {
            return revoked(&sink, denial);
        }
        self.run_as(request, trust, sink, cancel, decisions).await
    }
}

/// End a run whose authorization was withdrawn: say why (the code only) and
/// finish it as cancelled.
fn revoked(sink: &AgentEventSink, denial: Denial) -> RunStatus {
    let _ = sink.send(AgentEvent::Error {
        message: revoked_message(denial),
    });
    let _ = sink.send(AgentEvent::RunCompleted {
        reason: StopReason::Cancelled,
    });
    RunStatus::Cancelled
}

/// Drive one agent loop to a terminal status, handling approval pauses.
///
/// A generic helper a [`RunFactory`] calls with the concrete loop it built. The
/// event sink rides along through a pause, so resuming keeps streaming. On a
/// provider failure before any terminal event, it emits an `Error` and a
/// terminal `RunCompleted` so a stream always ends cleanly.
pub async fn drive<P: AgentProvider, I: ToolInvoker>(
    agent: AgentLoop<P, I>,
    history: Vec<ProviderMessage>,
    message: String,
    sink: AgentEventSink,
    cancel: CancellationToken,
    decisions: UnboundedReceiver<ApprovalDecision>,
) -> RunStatus {
    let authorization = RunAuthorization::unconditional(cancel.clone());
    drive_supervised(
        agent,
        history,
        message,
        sink,
        cancel,
        decisions,
        authorization,
    )
    .await
}

/// [`drive`] for a revocable run: `authorization` is checked before the run
/// starts and before an approval decision is acted on, so a decision made or
/// queued before a revocation never resumes the run after it. A revocation
/// during an approval wait ends the wait at once. The loop's provider and
/// invoker check the same authorization at every model turn and tool call.
pub async fn drive_supervised<P: AgentProvider, I: ToolInvoker>(
    agent: AgentLoop<P, I>,
    history: Vec<ProviderMessage>,
    message: String,
    sink: AgentEventSink,
    cancel: CancellationToken,
    mut decisions: UnboundedReceiver<ApprovalDecision>,
    authorization: RunAuthorization,
) -> RunStatus {
    if let Err(denial) = authorization.verify() {
        return revoked(&sink, denial);
    }
    let mut outcome = match agent
        .run_streaming_with_history(history, message, cancel.clone(), sink.clone())
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = sink.send(AgentEvent::Error {
                message: error.to_string(),
            });
            let _ = sink.send(AgentEvent::RunCompleted {
                reason: StopReason::Error,
            });
            return RunStatus::Failed;
        }
    };

    loop {
        match outcome {
            RunOutcome::Completed { events } => return status_from_events(&events),
            // Only a `WallClockPolicy::Pause` run pauses out of time, and the API
            // keeps the default wrap-up policy; should one arrive anyway, the
            // time budget is spent and the run is over.
            RunOutcome::OutOfTime { .. } => {
                let _ = sink.send(AgentEvent::RunCompleted {
                    reason: StopReason::WallClockExceeded,
                });
                return RunStatus::Completed;
            }
            RunOutcome::AwaitingApproval { suspended, .. } => {
                tokio::select! {
                    () = cancel.cancelled() => {
                        return match authorization.revoked() {
                            Some(denial) => revoked(&sink, denial),
                            None => RunStatus::Cancelled,
                        };
                    }
                    decision = decisions.recv() => match decision {
                        Some(decision) => {
                            // A stale approval: decided before the run lost
                            // its authorization. The call does not run.
                            if let Err(denial) = authorization.verify() {
                                return revoked(&sink, denial);
                            }
                            outcome = match agent.resume(suspended, decision, cancel.clone()).await {
                                Ok(outcome) => outcome,
                                Err(error) => {
                                    let _ = sink.send(AgentEvent::Error { message: error.to_string() });
                                    let _ = sink.send(AgentEvent::RunCompleted { reason: StopReason::Error });
                                    return RunStatus::Failed;
                                }
                            };
                        }
                        None => return RunStatus::Cancelled,
                    }
                }
            }
        }
    }
}

/// Map a completed run's terminal event to a status.
fn status_from_events(events: &[AgentEvent]) -> RunStatus {
    match events.last() {
        Some(AgentEvent::RunCompleted { reason }) => match reason {
            StopReason::Cancelled => RunStatus::Cancelled,
            StopReason::Error => RunStatus::Failed,
            _ => RunStatus::Completed,
        },
        _ => RunStatus::Completed,
    }
}

/// How often the monitor re-checks active revocable runs by default: the
/// longest a revocation waits to reach a run that takes no action, plus one
/// check.
pub const DEFAULT_REVALIDATION_INTERVAL: Duration = Duration::from_secs(5);
/// The shortest interval accepted: the monitor never spins.
pub const MIN_REVALIDATION_INTERVAL: Duration = Duration::from_millis(100);
/// The longest interval accepted: a revocation is always observed.
pub const MAX_REVALIDATION_INTERVAL: Duration = Duration::from_secs(300);

/// Paces the revalidation monitor: each `tick` returns when the next sweep
/// is due.
#[async_trait]
pub trait RevalidationClock: Send + Sync + 'static {
    async fn tick(&self);
}

/// A fixed interval, clamped to
/// [`MIN_REVALIDATION_INTERVAL`]..=[`MAX_REVALIDATION_INTERVAL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevalidationInterval(Duration);

impl RevalidationInterval {
    pub fn new(period: Duration) -> Self {
        Self(period.clamp(MIN_REVALIDATION_INTERVAL, MAX_REVALIDATION_INTERVAL))
    }

    pub fn period(&self) -> Duration {
        self.0
    }
}

#[async_trait]
impl RevalidationClock for RevalidationInterval {
    async fn tick(&self) {
        tokio::time::sleep(self.0).await;
    }
}

/// What the monitor has done, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorState {
    /// Whether the monitor task is running.
    pub running: bool,
    /// How many times a monitor task was started.
    pub started: u64,
    /// Sweeps completed.
    pub sweeps: u64,
    /// Run checks made by those sweeps.
    pub checks: u64,
    /// Runs those sweeps revoked.
    pub revoked: u64,
}

/// The shared revalidation monitor of one manager.
struct Monitor {
    clock: Arc<dyn RevalidationClock>,
    /// The monitor task, while one runs. Changed only under this lock, so a
    /// run starting while the monitor stops is never left unwatched.
    task: StdMutex<Option<JoinHandle<()>>>,
    /// Revocable runs not yet terminal.
    active: AtomicUsize,
    state: watch::Sender<MonitorState>,
}

impl Monitor {
    fn new(clock: Arc<dyn RevalidationClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            task: StdMutex::new(None),
            active: AtomicUsize::new(0),
            state: watch::Sender::new(MonitorState::default()),
        })
    }
}

type Runs = Mutex<HashMap<String, Arc<RunState>>>;

/// Re-check every active revocable run in one sweep (one identity-store
/// read), then ask every open event stream to re-check its subscriber.
/// Returns how many runs were checked and which were revoked now.
async fn sweep_runs(runs: &Runs) -> (u64, Vec<(String, Denial)>) {
    let all: Vec<Arc<RunState>> = runs.lock().await.values().cloned().collect();
    let mut active = Vec::new();
    for run in &all {
        if run.authorization.is_revocable()
            && run.authorization.revoked().is_none()
            && !run.status().await.is_terminal()
        {
            active.push(Arc::clone(run));
        }
    }
    let revoked = sweep(|| {
        active
            .iter()
            .filter_map(|run| {
                run.authorization
                    .verify()
                    .err()
                    .map(|denial| (run.id.clone(), denial))
            })
            .collect::<Vec<_>>()
    });
    for run in &all {
        run.request_recheck();
    }
    (active.len() as u64, revoked)
}

/// The monitor task: sweep on every tick until no revocable run is active
/// (or the manager is gone), then stop.
async fn watch_runs(runs: Weak<Runs>, monitor: Arc<Monitor>) {
    loop {
        monitor.clock.tick().await;
        let Some(strong) = runs.upgrade() else {
            break;
        };
        let (checked, revoked) = sweep_runs(&strong).await;
        drop(strong);
        monitor.state.send_modify(|state| {
            state.sweeps += 1;
            state.checks += checked;
            state.revoked += revoked.len() as u64;
        });
        if let Ok(mut task) = monitor.task.lock()
            && monitor.active.load(Ordering::SeqCst) == 0
        {
            *task = None;
            monitor.state.send_modify(|state| state.running = false);
            return;
        }
    }
    if let Ok(mut task) = monitor.task.lock() {
        *task = None;
        monitor.state.send_modify(|state| state.running = false);
    }
}

/// Owns the runs and starts new ones.
#[derive(Clone)]
pub struct RunManager {
    factory: Arc<dyn RunFactory>,
    runs: Arc<Runs>,
    monitor: Arc<Monitor>,
}

impl RunManager {
    /// A manager that drives runs with `factory`.
    pub fn new(factory: Arc<dyn RunFactory>) -> Self {
        Self {
            factory,
            runs: Arc::new(Mutex::new(HashMap::new())),
            monitor: Monitor::new(Arc::new(RevalidationInterval::new(
                DEFAULT_REVALIDATION_INTERVAL,
            ))),
        }
    }

    /// Pace the revalidation monitor with `clock` (an interval, or a test's
    /// controlled clock). Set it before starting runs.
    pub fn with_revalidation(mut self, clock: Arc<dyn RevalidationClock>) -> Self {
        self.monitor = Monitor::new(clock);
        self
    }

    /// Re-check active revocable runs every `period` (clamped).
    pub fn with_revalidation_interval(self, period: Duration) -> Self {
        self.with_revalidation(Arc::new(RevalidationInterval::new(period)))
    }

    /// The monitor's state, updated after every sweep.
    pub fn monitor_state(&self) -> watch::Receiver<MonitorState> {
        self.monitor.state.subscribe()
    }

    /// Start the monitor unless one is running.
    fn ensure_monitor(&self) {
        let Ok(mut task) = self.monitor.task.lock() else {
            return;
        };
        if task.as_ref().is_some_and(|running| !running.is_finished()) {
            return;
        }
        self.monitor.state.send_modify(|state| {
            state.running = true;
            state.started += 1;
        });
        *task = Some(tokio::spawn(watch_runs(
            Arc::downgrade(&self.runs),
            Arc::clone(&self.monitor),
        )));
    }

    /// The tools the factory currently offers to the active target.
    pub async fn tools(&self) -> Result<Vec<lightagent_tools::ToolDefinition>, String> {
        self.factory.tools().await
    }
    /// The tools a run of `profile` with `trust` would receive.
    pub async fn tools_for(
        &self,
        profile: &ProfileId,
        trust: RunTrust,
    ) -> Result<Vec<lightagent_tools::ToolDefinition>, String> {
        self.factory.tools_for(profile, trust).await
    }

    /// Each specialist role's readiness for a run of `profile` with `trust`.
    pub async fn workers_for(
        &self,
        profile: Option<&ProfileId>,
        trust: RunTrust,
    ) -> Result<serde_json::Value, String> {
        self.factory.workers_for(profile, trust).await
    }

    /// The Jev status an ordinary user of `profile` may see.
    pub async fn jev_status_for(&self, profile: &ProfileId) -> Result<serde_json::Value, String> {
        self.factory.jev_status_for(profile).await
    }

    /// Discover the selected provider's current model catalog and capabilities.
    pub async fn provider_capabilities(&self) -> Result<ProviderCapabilities, String> {
        self.factory.provider_capabilities().await
    }

    /// The active profile's Jev status.
    pub async fn jev_status(&self) -> Result<serde_json::Value, String> {
        self.factory.jev_status().await
    }

    /// Run the Jev connection check.
    pub async fn verify_jev(&self) -> Result<serde_json::Value, String> {
        self.factory.verify_jev().await
    }

    /// Start a run and return its shared state. The run drives in the background.
    pub async fn start(&self, request: StartRun) -> Arc<RunState> {
        self.start_as(request, RunOwner::operator_only()).await
    }

    /// Start a run recording its `owner`. The caller must already have
    /// authorized the owner for `request.profile`. The run is not
    /// revocable: only cancellation stops it.
    pub async fn start_as(&self, request: StartRun, owner: RunOwner) -> Arc<RunState> {
        self.launch(request, owner, None).await
    }

    /// Start a revocable run. `check` reads the current identity state and is
    /// consulted now (a revocation before the run starts refuses it, and no
    /// run is created) and then at every enforcement point of the run.
    /// `generation` is the identity-state generation it was authorized at.
    pub async fn start_supervised(
        &self,
        request: StartRun,
        owner: RunOwner,
        check: RevocationCheck,
        generation: Option<u64>,
    ) -> Result<Arc<RunState>, Denial> {
        check()?;
        Ok(self.launch(request, owner, Some((check, generation))).await)
    }

    async fn launch(
        &self,
        request: StartRun,
        owner: RunOwner,
        revocable: Option<(RevocationCheck, Option<u64>)>,
    ) -> Arc<RunState> {
        let id = new_run_id();
        let trust = owner.trust();
        let (decisions_tx, decisions_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let supervised = revocable.is_some();
        let authorization = match revocable {
            Some((check, generation)) => {
                RunAuthorization::revocable(check, cancel.clone(), generation)
            }
            None => RunAuthorization::unconditional(cancel.clone()),
        };
        let state = Arc::new(RunState {
            id: id.clone(),
            owner,
            events: Mutex::new(Vec::new()),
            status: Mutex::new(RunStatus::Running),
            pending: Mutex::new(None),
            notify: Notify::new(),
            cancel,
            decisions: decisions_tx,
            authorization,
            recheck: watch::Sender::new(0),
            #[cfg(test)]
            gap: StdMutex::new(None),
        });
        self.runs.lock().await.insert(id, state.clone());
        if supervised {
            self.monitor.active.fetch_add(1, Ordering::SeqCst);
            self.ensure_monitor();
        }
        let monitor = Arc::clone(&self.monitor);

        let factory = self.factory.clone();
        let run_state = state.clone();
        let cancel = state.cancel.clone();
        tokio::spawn(async move {
            let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<AgentEvent>();

            // Forward each event onto the run's buffer, deriving the pending
            // approval and status transitions from the stream.
            let forward_state = run_state.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(event) = sink_rx.recv().await {
                    match &event {
                        AgentEvent::AwaitingApproval {
                            approval_id,
                            tool_call_id,
                            name,
                        } => {
                            forward_state
                                .set_pending(Some(PendingApproval {
                                    approval_id: approval_id.clone(),
                                    tool_call_id: tool_call_id.clone(),
                                    tool: name.clone(),
                                    risk: "".to_owned(),
                                }))
                                .await;
                            forward_state.set_status(RunStatus::AwaitingApproval).await;
                        }
                        AgentEvent::ToolCallStarted { .. } | AgentEvent::Content { .. } => {
                            // Any progress past a pause means it resolved.
                            forward_state.set_pending(None).await;
                            forward_state.set_status(RunStatus::Running).await;
                        }
                        _ => {}
                    }
                    forward_state.push_event(event).await;
                }
            });

            let status = if supervised {
                let authorization = run_state.authorization.clone();
                factory
                    .run_supervised(request, trust, authorization, sink_tx, cancel, decisions_rx)
                    .await
            } else {
                factory
                    .run_as(request, trust, sink_tx, cancel, decisions_rx)
                    .await
            };
            // No longer active for the monitor once the run is over; done
            // before the terminal status is visible to anyone waiting on it.
            if supervised {
                monitor.active.fetch_sub(1, Ordering::SeqCst);
            }
            let _ = forwarder.await;
            run_state.set_pending(None).await;
            run_state.set_status(status).await;
        });

        state
    }

    /// The run with this id, if it exists.
    pub async fn get(&self, id: &str) -> Option<Arc<RunState>> {
        self.runs.lock().await.get(id).cloned()
    }

    /// The runs that have not reached a terminal state.
    async fn active(&self) -> Vec<Arc<RunState>> {
        let runs: Vec<Arc<RunState>> = self.runs.lock().await.values().cloned().collect();
        let mut active = Vec::new();
        for run in runs {
            if !run.status().await.is_terminal() {
                active.push(run);
            }
        }
        active
    }

    /// The active revocable runs an identity change described by `scope`
    /// can affect. Unrelated runs, and runs that are not revocable, are
    /// never included.
    pub async fn affected_by(&self, scope: &RevocationScope) -> Vec<Arc<RunState>> {
        self.active()
            .await
            .into_iter()
            .filter(|run| run.authorization.is_revocable() && run.owner.affected_by(scope))
            .collect()
    }

    /// Revoke the active runs `scope` affects, recording `denial` as the
    /// reason, and cancel them. Only an operator holding `administer` may do
    /// this; other runs are untouched. Returns the ids of the runs this call
    /// revoked.
    pub async fn revoke(
        &self,
        actor: &Caller,
        scope: &RevocationScope,
        denial: Denial,
    ) -> Result<Vec<String>, Denial> {
        if actor.principal.role != Role::Operator {
            return Err(Denial::MissingCapability);
        }
        authorize_capability(actor, Capability::Administer)?;
        let mut revoked = Vec::new();
        for run in self.affected_by(scope).await {
            if run.authorization.revoke(denial) {
                revoked.push(run.id.clone());
            }
            run.request_recheck();
        }
        Ok(revoked)
    }

    /// Re-check every active revocable run against the current identity
    /// state, revoking and cancelling each that no longer holds, and ask
    /// every open event stream to re-check its subscriber. One bounded check
    /// per active run; nothing polls. Returns the runs revoked now, with
    /// why.
    pub async fn revalidate(&self) -> Vec<(String, Denial)> {
        sweep_runs(&self.runs).await.1
    }

    /// The runs currently awaiting an approval decision.
    pub async fn awaiting_approval(&self) -> Vec<Arc<RunState>> {
        let runs = self.runs.lock().await;
        let mut waiting = Vec::new();
        for state in runs.values() {
            if state.status().await == RunStatus::AwaitingApproval {
                waiting.push(state.clone());
            }
        }
        waiting
    }
}

fn new_run_id() -> String {
    RunId::new().as_str().to_owned()
}

/// Test-only: where a paused waiter reports that it stopped, and what lets
/// it go on.
#[cfg(test)]
struct Gap {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
impl RunState {
    /// A run with no task behind it, whose state a test changes directly.
    pub(crate) fn detached() -> Arc<Self> {
        let cancel = CancellationToken::new();
        let (decisions, _) = mpsc::unbounded_channel();
        Arc::new(Self {
            id: new_run_id(),
            owner: RunOwner::operator_only(),
            events: Mutex::new(Vec::new()),
            status: Mutex::new(RunStatus::Running),
            pending: Mutex::new(None),
            notify: Notify::new(),
            cancel: cancel.clone(),
            decisions,
            authorization: RunAuthorization::unconditional(cancel),
            recheck: watch::Sender::new(0),
            gap: StdMutex::new(None),
        })
    }

    /// Make the next `wait_from` that finds nothing new pause after its read
    /// and before its wait. Returns a receiver that fires once it has paused,
    /// and the sender that lets it go on.
    pub(crate) fn pause_next_wait(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, paused) = tokio::sync::oneshot::channel();
        let (resume, release) = tokio::sync::oneshot::channel();
        *self.gap.lock().unwrap() = Some(Gap { entered, release });
        (paused, resume)
    }

    async fn in_the_gap(&self) {
        let gap = self.gap.lock().unwrap().take();
        if let Some(Gap { entered, release }) = gap {
            let _ = entered.send(());
            let _ = release.await;
        }
    }

    pub(crate) async fn push_for_test(&self, event: AgentEvent) {
        self.push_event(event).await;
    }

    pub(crate) async fn finish_for_test(&self, status: RunStatus) {
        self.set_status(status).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bound on each wait: a waiter that is never woken fails the test
    /// instead of hanging it. Nothing here waits for time to pass.
    const GUARD: Duration = Duration::from_secs(10);

    fn content(text: &str) -> AgentEvent {
        AgentEvent::Content {
            text: text.to_owned(),
        }
    }

    fn cancelled() -> AgentEvent {
        AgentEvent::RunCompleted {
            reason: StopReason::Cancelled,
        }
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(GUARD, future)
            .await
            .expect("a waiter was never woken")
    }

    /// Wait from `seen` until the run is terminal, as every observer does.
    async fn until_terminal(run: Arc<RunState>, mut seen: usize) -> (Vec<AgentEvent>, RunStatus) {
        let mut collected = Vec::new();
        loop {
            let (events, status) = run.wait_from(seen).await;
            seen += events.len();
            collected.extend(events);
            if status.is_terminal() {
                return (collected, status);
            }
        }
    }

    // The run finishes after the waiter read "running" and before it waits:
    // the interleaving of the hang in check run 39728608914.
    #[tokio::test]
    async fn a_run_finishing_between_the_read_and_the_wait_wakes_the_waiter() {
        let run = RunState::detached();
        let (paused, resume) = run.pause_next_wait();
        let waiter = tokio::spawn({
            let run = Arc::clone(&run);
            async move { run.wait_from(0).await }
        });
        paused.await.unwrap();
        run.set_status(RunStatus::Cancelled).await;
        resume.send(()).unwrap();
        let (events, status) = within(waiter).await.unwrap();
        assert!(events.is_empty());
        assert_eq!(status, RunStatus::Cancelled);
    }

    // The same gap, with an event instead of the end.
    #[tokio::test]
    async fn an_event_arriving_between_the_read_and_the_wait_wakes_the_waiter() {
        let run = RunState::detached();
        run.push_event(content("before")).await;
        let (paused, resume) = run.pause_next_wait();
        let waiter = tokio::spawn({
            let run = Arc::clone(&run);
            async move { run.wait_from(1).await }
        });
        paused.await.unwrap();
        run.push_event(content("after")).await;
        resume.send(()).unwrap();
        let (events, status) = within(waiter).await.unwrap();
        assert_eq!(events, vec![content("after")]);
        assert_eq!(status, RunStatus::Running);
    }

    // A run that already ended answers at once, from any position, and a
    // position past the end (a stale resume cursor) is clamped.
    #[tokio::test]
    async fn a_finished_run_answers_at_once_from_any_position() {
        let run = RunState::detached();
        run.push_event(content("a")).await;
        run.push_event(content("b")).await;
        run.set_status(RunStatus::Completed).await;
        assert_eq!(
            within(run.wait_from(0)).await,
            (vec![content("a"), content("b")], RunStatus::Completed)
        );
        assert_eq!(
            within(run.wait_from(1)).await,
            (vec![content("b")], RunStatus::Completed)
        );
        assert_eq!(
            within(run.wait_from(2)).await,
            (Vec::new(), RunStatus::Completed)
        );
        assert_eq!(
            within(run.wait_from(9)).await,
            (Vec::new(), RunStatus::Completed)
        );
    }

    // The waiter is already waiting when the run produces its last events and
    // ends.
    #[tokio::test]
    async fn a_run_finishing_while_the_waiter_waits_wakes_it() {
        let run = RunState::detached();
        let waiter = tokio::spawn(until_terminal(Arc::clone(&run), 0));
        // Single-threaded runtime: the waiter runs until it waits.
        tokio::task::yield_now().await;
        run.push_event(content("a")).await;
        run.push_event(cancelled()).await;
        run.set_status(RunStatus::Cancelled).await;
        assert_eq!(
            within(waiter).await.unwrap(),
            (vec![content("a"), cancelled()], RunStatus::Cancelled)
        );
    }

    // Many waiters from different positions, one of them caught in the gap,
    // while events arrive and the run ends: each sees every event after its
    // position once, in order, and the end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_waiter_sees_every_event_once_in_order_and_the_end() {
        let run = RunState::detached();
        run.push_event(content("0")).await;
        let (paused, resume) = run.pause_next_wait();
        let caught = tokio::spawn(until_terminal(Arc::clone(&run), 1));
        paused.await.unwrap();
        let others: Vec<_> = (0..8)
            .map(|index| tokio::spawn(until_terminal(Arc::clone(&run), index % 2)))
            .collect();
        let mut expected = vec![content("0")];
        for index in 1..=5 {
            let event = content(&index.to_string());
            run.push_event(event.clone()).await;
            expected.push(event);
        }
        run.push_event(cancelled()).await;
        expected.push(cancelled());
        run.set_status(RunStatus::Cancelled).await;
        resume.send(()).unwrap();
        assert_eq!(
            within(caught).await.unwrap(),
            (expected[1..].to_vec(), RunStatus::Cancelled)
        );
        for (index, other) in others.into_iter().enumerate() {
            assert_eq!(
                within(other).await.unwrap(),
                (expected[index % 2..].to_vec(), RunStatus::Cancelled)
            );
        }
    }

    // A waiter that gives up (a stream dropped mid-wait, a `select!` that took
    // another branch) takes nothing from the others.
    #[tokio::test]
    async fn a_waiter_that_gives_up_takes_nothing_from_the_others() {
        let run = RunState::detached();
        let abandoned = tokio::spawn({
            let run = Arc::clone(&run);
            async move { run.wait_from(0).await }
        });
        let kept = tokio::spawn(until_terminal(Arc::clone(&run), 0));
        tokio::task::yield_now().await;
        abandoned.abort();
        assert!(abandoned.await.unwrap_err().is_cancelled());
        run.set_status(RunStatus::Cancelled).await;
        assert_eq!(
            within(kept).await.unwrap(),
            (Vec::new(), RunStatus::Cancelled)
        );
    }
}
