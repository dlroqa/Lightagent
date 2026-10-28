//! The agent loop.
//!
//! One async state machine drives a run from the first event to the terminal
//! one. It seeds the conversation, streams a model turn, and either finishes on
//! a final answer or, when the model asks for tools, takes each call through the
//! [`ToolInvoker`]: it asks the invoker whether the call may run
//! ([`approval_for`]), and then either runs it, refuses it with a controlled
//! tool-error result the model still sees, or — when a human decision is needed
//! — emits [`AgentEvent::AwaitingApproval`] and **pauses**, returning a
//! [`Suspended`] the caller resumes once a decision arrives. Every axis of
//! [`RunLimits`] bounds the run, including repeated-identical-call detection
//! that stops a model stuck in a loop.
//!
//! The wall-clock budget is checked at each turn boundary, so a turn or tool
//! already in flight always finishes. When the budget runs out right after a
//! tool batch, the model has not read those results yet, and ending there
//! would throw away the work. [`WallClockPolicy`] decides what happens
//! instead: take one final turn with tools withheld so the model answers from
//! what it gathered, or pause the run ([`RunOutcome::OutOfTime`]) so an
//! interactive caller can ask whether to continue.
//!
//! The turn and tool-call budgets get the same treatment without a pause: the
//! last turn the budget allows is reserved, once tools have run, for that
//! tool-less answer, so a run never ends on tool results nobody read.
//!
//! A run's [`GenerationBudget`] sets each turn's output allowance and
//! reasoning label, and with a known context window keeps room for the
//! answer: a tool result that would crowd it out is shortened in the
//! conversation the model sees, and once the room left is too small the next
//! turn is that same tool-less answer, ending with
//! [`StopReason::ContextLimit`]. An answer cut off at its length limit may be
//! continued by a bounded number of tool-less turns, each spending a turn of
//! the run's budget; see [`GenerationBudget::max_continuations`]. The default
//! budget changes none of this.
//!
//! A turn the provider did not finish is never mistaken for an answer: a
//! token-limited turn ends with [`StopReason::MaxTokens`] and a stream that
//! stopped without a completion signal with [`StopReason::Incomplete`]. Either
//! way the text that arrived is kept, and a tool call that may have been cut
//! short is not run.
//!
//! [`approval_for`]: crate::invoker::ToolInvoker::approval_for

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures_util::stream::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::event::{AgentEvent, StopReason};
use crate::generation::{self, GenerationBudget, GenerationReport, Measured};
use crate::ids::RunId;
use crate::invoker::{ToolCall, ToolInvoker, ToolOutcome};
use crate::limits::RunLimits;
use crate::permissions::{ApprovalDecision, ApprovalNeed, ApprovalRequest};
use crate::profile::AgentProfile;
use crate::provider::{
    AgentProvider, FinishReason, ProviderEvent, ProviderMessage, ProviderRequest, ProviderToolCall,
    Role, Usage,
};
use crate::tool_stream::ToolCallAccumulator;

/// A channel a streaming run sends each [`AgentEvent`] to as it happens.
pub type AgentEventSink = tokio::sync::mpsc::UnboundedSender<AgentEvent>;

/// The instruction added to the final, tool-less turn a run takes when its
/// wall-clock budget runs out with tool results the model has not read yet.
/// Sent only with that one request, never kept in the conversation.
const WRAP_UP_INSTRUCTION: &str = "The time budget for this request has run out, so no more \
    tools can be called. Answer the original request now, as well as you can, from the tool \
    results above, and say briefly if anything is incomplete.";

/// The instruction for the last turn the turn budget allows, once tools have
/// run. Like [`WRAP_UP_INSTRUCTION`], it rides on that one request only.
const TURN_BUDGET_INSTRUCTION: &str = "This is the last step allowed for this request, so no \
    more tools can be called. Answer the original request now from the tool results above, \
    cite the sources you used, and say briefly what is missing or could not be checked.";

/// The instruction for the turn after the tool-call budget ran out.
const TOOL_CALL_BUDGET_INSTRUCTION: &str = "The tool-call budget for this request has run out, \
    so no more tools can be called. Answer the original request now from the tool results \
    above, cite the sources you used, and say briefly what is missing or could not be checked.";

/// The instruction for the turn after the conversation left too little of the
/// context window for more tool results.
const CONTEXT_BUDGET_INSTRUCTION: &str = "The conversation has nearly filled the model's \
    context window, so no more tools can be called. Answer the original request now from the \
    tool results above, cite the sources you used, and say briefly what is missing or could not \
    be checked.";

/// The instruction for the turn after the run's output-token budget ran low.
const OUTPUT_BUDGET_INSTRUCTION: &str = "The output budget for this request is nearly used up, \
    so no more tools can be called. Answer the original request now, briefly, from the tool \
    results above, and say what is missing or could not be checked.";
/// The instruction for a turn that continues an answer cut off at its length
/// limit. It rides on that one request only.
const CONTINUE_INSTRUCTION: &str = "Your previous reply was cut off by the output limit. \
    Continue it exactly where it stopped: do not repeat anything already written, do not \
    start again, and do not call tools.";

/// The most progress reports forwarded for one tool call, so a chatty tool
/// cannot flood the event log.
const MAX_PROGRESS_PER_CALL: usize = 64;

/// Why a run is taking its final, tool-less turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WrapUp {
    /// The wall clock ran out with unread tool results.
    WallClock,
    /// This is the last turn the turn budget allows, and tools have run.
    TurnBudget,
    /// The tool-call budget ran out inside a batch.
    ToolCallBudget,
    /// The room left in the context window is less than the answer needs.
    ContextBudget,
    /// The run's output pool has less left than the answer needs.
    OutputBudget,
}

impl WrapUp {
    fn instruction(self) -> &'static str {
        match self {
            Self::WallClock => WRAP_UP_INSTRUCTION,
            Self::TurnBudget => TURN_BUDGET_INSTRUCTION,
            Self::ToolCallBudget => TOOL_CALL_BUDGET_INSTRUCTION,
            Self::ContextBudget => CONTEXT_BUDGET_INSTRUCTION,
            Self::OutputBudget => OUTPUT_BUDGET_INSTRUCTION,
        }
    }

    /// The stop reason a wrap-up answer records: the budget, not the model,
    /// ended the run.
    fn reason(self) -> StopReason {
        match self {
            Self::WallClock => StopReason::WallClockExceeded,
            Self::TurnBudget => StopReason::MaxTurns,
            Self::ToolCallBudget => StopReason::MaxToolCalls,
            Self::ContextBudget => StopReason::ContextLimit,
            Self::OutputBudget => StopReason::OutputBudget,
        }
    }
}

/// What a run does when its wall-clock budget runs out while tool results the
/// model has not read yet are waiting.
///
/// Either way the budget stays a turn-boundary check: nothing in flight is cut
/// short. A run with no unread results (only possible before its first turn)
/// simply ends with [`StopReason::WallClockExceeded`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WallClockPolicy {
    /// Take one final turn with tools withheld so the model answers from what
    /// it already gathered, then end with [`StopReason::WallClockExceeded`].
    /// The default: right for callers with nobody to ask.
    #[default]
    WrapUp,
    /// Pause the run and return [`RunOutcome::OutOfTime`], so an interactive
    /// caller can ask whether to continue it
    /// ([`AgentLoop::continue_out_of_time`]).
    Pause,
}

/// How to continue a run that paused on its wall-clock budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continuation {
    /// Restart the wall clock and carry on with tools, exactly where the run
    /// stopped. Only time is renewed: the turn and tool-call budgets still
    /// count across the whole run.
    Extend,
    /// Take the final, tool-less turn now, so the model answers from what it
    /// already gathered, and end.
    WrapUp,
}

/// What a run needs beyond its provider and invoker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    /// The model id sent to the provider.
    pub model: String,
    /// The system prompt seeded as the first message, if any.
    pub system: Option<String>,
    /// The bounds enforced across the run.
    pub limits: RunLimits,
    /// The orchestrator run, for a delegated child (Slice 3/4).
    pub parent: Option<RunId>,
    /// What happens when the wall-clock budget runs out with unread tool
    /// results.
    pub on_wall_clock: WallClockPolicy,
    /// Each turn's output allowance, reasoning label and context guard. The
    /// default changes nothing.
    pub generation: GenerationBudget,
}

impl RunConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            limits: RunLimits::default(),
            parent: None,
            on_wall_clock: WallClockPolicy::default(),
            generation: GenerationBudget::default(),
        }
    }
}

/// Why the loop itself could not proceed (distinct from a run that ended for a
/// [`StopReason`]).
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The provider could not start or complete a turn.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    /// A decision was not for the approval this run is waiting on.
    #[error("approval decision does not match the pending tool call")]
    InvalidApproval,
}

/// The result of driving a run until it either finished or paused.
///
/// Each variant carries the *cumulative* event log up to that point, so a
/// caller can render everything that has happened without stitching fragments
/// together: [`run`](AgentLoop::run) returns the log through the pause, and
/// [`resume`](AgentLoop::resume) returns the whole log through completion.
#[derive(Debug)]
pub enum RunOutcome {
    /// The run reached a terminal state; the last event is a
    /// [`AgentEvent::RunCompleted`].
    Completed { events: Vec<AgentEvent> },
    /// The run paused awaiting a human decision on `request`. Resume it with
    /// [`AgentLoop::resume`], passing `suspended`.
    AwaitingApproval {
        events: Vec<AgentEvent>,
        request: ApprovalRequest,
        suspended: Box<Suspended>,
    },
    /// The run used up its wall-clock budget, `elapsed` so far, with tool
    /// results the model has not read yet, and paused instead of ending. Only
    /// returned under [`WallClockPolicy::Pause`]; the last event is an
    /// [`AgentEvent::WallClockPaused`]. Continue it with
    /// [`AgentLoop::continue_out_of_time`], passing `suspended`, or drop it.
    OutOfTime {
        events: Vec<AgentEvent>,
        elapsed: Duration,
        suspended: Box<Suspended>,
    },
}

impl RunOutcome {
    /// The cumulative event log for this outcome.
    pub fn events(&self) -> &[AgentEvent] {
        match self {
            Self::Completed { events }
            | Self::AwaitingApproval { events, .. }
            | Self::OutOfTime { events, .. } => events,
        }
    }

    /// Take the cumulative event log, whichever variant this is.
    pub fn into_events(self) -> Vec<AgentEvent> {
        match self {
            Self::Completed { events }
            | Self::AwaitingApproval { events, .. }
            | Self::OutOfTime { events, .. } => events,
        }
    }

    /// Whether the run finished (rather than paused).
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    /// The pending request, when the run is awaiting approval.
    pub fn approval_request(&self) -> Option<&ApprovalRequest> {
        match self {
            Self::AwaitingApproval { request, .. } => Some(request),
            Self::Completed { .. } | Self::OutOfTime { .. } => None,
        }
    }
}

/// A paused run, resumable once a decision (or, for a run out of time, a
/// [`Continuation`]) is available.
///
/// Opaque by design: it carries exactly the state the loop needs to continue —
/// the conversation so far, the run's bounds and counters, and the tool batch
/// it stopped inside — and nothing a caller should reach into.
#[derive(Debug)]
pub struct Suspended {
    driver: Driver,
    approval: Option<ApprovalRequest>,
}

/// The tools a single model turn asked for, and how far through them the loop
/// has processed.
#[derive(Clone, Debug)]
struct Batch {
    calls: Vec<ToolCall>,
    next: usize,
}

/// The whole mutable state of a run in flight.
#[derive(Clone, Debug)]
struct Driver {
    started: Instant,
    limits: RunLimits,
    model: String,
    messages: Vec<ProviderMessage>,
    events: Vec<AgentEvent>,
    turn: u32,
    tool_calls_made: u32,
    /// How many times each identical `(name, arguments)` call has been seen, to
    /// detect a model stuck repeating itself.
    call_counts: BTreeMap<String, u32>,
    /// The tool batch in progress, if the last turn asked for tools.
    pending: Option<Batch>,
    /// When set, each emitted event is also sent here live, for a streaming
    /// caller. Persisted across a suspend so a resume keeps streaming.
    sink: Option<AgentEventSink>,
    /// Set once a budget has run out and the run owes its final, tool-less
    /// answer turn.
    wrap_up: Option<WrapUp>,
    /// The provider's prompt count for the latest turn that reported one.
    measured: Option<Measured>,
    /// Tool results shortened to fit the context window so far.
    shortened_results: u32,
    /// The `max_tokens` the latest turn was sent with.
    sent_max_tokens: Option<u32>,
    /// Continuations taken so far.
    continuations: u32,
    /// Set when the next turn continues an answer cut off at its length
    /// limit: the end of the answer so far, to trim a restated opening.
    continuing: Option<String>,
}

impl Driver {
    /// Record an event: append it to the run's log, and — when a live sink is
    /// attached — send a copy to it. A send to a closed receiver is ignored, so
    /// a client that walked away never affects the run.
    fn emit(&mut self, event: AgentEvent) {
        if let Some(sink) = &self.sink {
            let _ = sink.send(event.clone());
        }
        self.events.push(event);
    }

    /// Whether any tool result is in the conversation. Restored history holds
    /// only user and assistant messages, so a tool result is this run's.
    fn has_tool_results(&self) -> bool {
        self.messages
            .iter()
            .any(|message| message.role == Role::Tool)
    }

    /// End the run with `reason`.
    fn end(&mut self, reason: StopReason) -> TurnResult {
        self.emit(AgentEvent::RunCompleted { reason });
        TurnResult::Finished(Box::new(RunOutcome::Completed {
            events: std::mem::take(&mut self.events),
        }))
    }
}

/// What one model turn produced.
enum TurnResult {
    /// The run ended; its terminal event is already in the log.
    Finished(Box<RunOutcome>),
    /// The model asked for tools; `driver.pending` now holds the batch.
    Batch,
    /// The wall clock ran out with unread tool results, and the run pauses.
    OutOfTime(Duration),
    /// The answer was cut off at its length limit and the next turn
    /// continues it.
    Continue,
}

/// What processing a tool batch produced.
enum BatchResult {
    /// Every call in the batch was handled.
    Done,
    /// A limit ended the run; its terminal event is already in the log.
    Completed,
    /// A call needs a human decision; the run pauses on `request`.
    Suspended(ApprovalRequest),
}

/// The agent loop, bound to a provider and an invoker.
pub struct AgentLoop<P: AgentProvider, I: ToolInvoker> {
    provider: P,
    invoker: I,
    config: RunConfig,
}

impl<P: AgentProvider, I: ToolInvoker> AgentLoop<P, I> {
    pub fn new(provider: P, invoker: I, config: RunConfig) -> Self {
        Self {
            provider,
            invoker,
            config,
        }
    }

    /// Seed a run from a profile: persona as the system prompt, and the model
    /// and limits from the profile's routing.
    pub fn from_profile(provider: P, invoker: I, profile: &AgentProfile) -> Self {
        let system = if profile.persona.is_empty() {
            None
        } else {
            Some(profile.persona.clone())
        };
        let config = RunConfig {
            model: profile.routing.model.clone(),
            system,
            limits: profile.limits,
            parent: None,
            on_wall_clock: WallClockPolicy::default(),
            generation: GenerationBudget::default(),
        };
        Self::new(provider, invoker, config)
    }

    /// Choose what a run does when its wall clock runs out with unread tool
    /// results. An interactive caller that can ask whether to continue sets
    /// [`WallClockPolicy::Pause`].
    pub fn with_wall_clock_policy(mut self, policy: WallClockPolicy) -> Self {
        self.config.on_wall_clock = policy;
        self
    }

    /// Set the run's output and context budget.
    pub fn with_generation(mut self, generation: GenerationBudget) -> Self {
        self.config.generation = generation;
        self
    }

    /// Replace the output and context budget before a fresh run, as the
    /// terminal does when the model it reaches may have changed.
    pub fn set_generation(&mut self, generation: GenerationBudget) {
        self.config.generation = generation;
    }

    /// The run's configuration.
    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    /// Select inference before a fresh run while preserving the existing
    /// invoker, approval state, persona and execution limits.
    pub fn set_inference_route(&mut self, provider: P, model: impl Into<String>) {
        self.provider = provider;
        self.config.model = model.into();
    }

    /// The bound tool invoker, for session-scoped policy changes by a caller.
    pub fn invoker(&self) -> &I {
        &self.invoker
    }

    /// Run until the first terminal state or the first pause.
    ///
    /// The terminal event of a [`RunOutcome::Completed`] is always a
    /// [`AgentEvent::RunCompleted`]; an [`AgentError`] is returned only when the
    /// provider itself could not be driven.
    pub async fn run(
        &self,
        user_input: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let driver = self.new_driver(Vec::new(), user_input, None);
        self.drive(driver, cancel).await
    }

    /// Run a new turn with the preceding conversation restored.
    ///
    /// `history` contains user and assistant messages from earlier completed
    /// turns. The configured system prompt is still owned by the loop and is
    /// prepended exactly once; `user_input` is appended as the new final user
    /// message. This is the entry point for session-aware callers.
    pub async fn run_with_history(
        &self,
        history: Vec<ProviderMessage>,
        user_input: impl Into<String>,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let driver = self.new_driver(history, user_input, None);
        self.drive(driver, cancel).await
    }

    /// Like [`run`](Self::run), but also emit each [`AgentEvent`] to `sink` as it
    /// happens, for a caller streaming the run live (the HTTP API's SSE). The
    /// sink rides along through a suspend, so a [`resume`](Self::resume) of the
    /// paused run keeps streaming to it. A closed receiver is ignored — the run
    /// is unaffected by nobody listening.
    pub async fn run_streaming(
        &self,
        user_input: impl Into<String>,
        cancel: CancellationToken,
        sink: AgentEventSink,
    ) -> Result<RunOutcome, AgentError> {
        let driver = self.new_driver(Vec::new(), user_input, Some(sink));
        self.drive(driver, cancel).await
    }

    /// Streaming counterpart to [`run_with_history`](Self::run_with_history).
    pub async fn run_streaming_with_history(
        &self,
        history: Vec<ProviderMessage>,
        user_input: impl Into<String>,
        cancel: CancellationToken,
        sink: AgentEventSink,
    ) -> Result<RunOutcome, AgentError> {
        let driver = self.new_driver(history, user_input, Some(sink));
        self.drive(driver, cancel).await
    }

    /// Build the initial driver, emitting the opening `RunStarted`.
    fn new_driver(
        &self,
        history: Vec<ProviderMessage>,
        user_input: impl Into<String>,
        sink: Option<AgentEventSink>,
    ) -> Driver {
        let run = RunId::new();
        let mut messages = Vec::new();
        if let Some(system) = &self.config.system {
            messages.push(ProviderMessage::system(system.clone()));
        }
        messages.extend(history);
        messages.push(ProviderMessage::user(user_input));

        let mut driver = Driver {
            started: Instant::now(),
            limits: self.config.limits,
            model: self.config.model.clone(),
            messages,
            events: Vec::new(),
            turn: 0,
            tool_calls_made: 0,
            call_counts: BTreeMap::new(),
            pending: None,
            sink,
            wrap_up: None,
            measured: None,
            shortened_results: 0,
            sent_max_tokens: None,
            continuations: 0,
            continuing: None,
        };
        driver.emit(AgentEvent::RunStarted {
            run,
            parent: self.config.parent.clone(),
        });
        driver
    }

    /// Resume a paused run with a human's decision.
    ///
    /// On a grant the loop persists the decision when it asked to be remembered
    /// ([`ToolInvoker::remember`]), runs the call, and continues. On a denial it
    /// appends a controlled tool-error result the model still sees, and
    /// continues. Either way the run proceeds from exactly where it paused.
    pub async fn resume(
        &self,
        suspended: Box<Suspended>,
        decision: ApprovalDecision,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        if suspended
            .approval
            .as_ref()
            .is_none_or(|request| request.id != decision.id)
        {
            return Err(AgentError::InvalidApproval);
        }
        let mut driver = suspended.driver;

        // The awaited call is the one the pending batch stopped on.
        let awaited = driver
            .pending
            .as_ref()
            .and_then(|batch| batch.calls.get(batch.next).cloned());

        if let Some(call) = awaited {
            if decision.granted {
                self.invoker.remember(&decision, &call);
                self.run_call(&mut driver, &call, &cancel).await;
            } else {
                let outcome = ToolOutcome::error(format!(
                    "the approval for '{}' was denied; the tool did not run",
                    call.name
                ));
                driver.messages.push(ProviderMessage::tool_result(
                    call.id.clone(),
                    outcome.content.clone(),
                ));
                driver.emit(AgentEvent::ToolCallCompleted {
                    id: call.id.clone(),
                    outcome,
                });
            }
            if let Some(batch) = driver.pending.as_mut() {
                batch.next += 1;
            }
        }

        self.drive(driver, cancel).await
    }

    /// Continue a run that paused on its wall-clock budget
    /// ([`RunOutcome::OutOfTime`]), from exactly where it stopped: the whole
    /// conversation, including the tool results the model has not read yet, is
    /// carried over.
    ///
    /// [`Continuation::Extend`] restarts the wall clock and lets the run carry
    /// on with tools; [`Continuation::WrapUp`] takes the final, tool-less turn
    /// now. The returned log is cumulative, as with [`resume`](Self::resume).
    pub async fn continue_out_of_time(
        &self,
        suspended: Box<Suspended>,
        how: Continuation,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let mut driver = suspended.driver;
        match how {
            Continuation::Extend => driver.started = Instant::now(),
            Continuation::WrapUp => driver.wrap_up = Some(WrapUp::WallClock),
        }
        self.drive(driver, cancel).await
    }

    /// The main turn/batch loop, shared by [`run`](Self::run),
    /// [`resume`](Self::resume) and
    /// [`continue_out_of_time`](Self::continue_out_of_time).
    async fn drive(
        &self,
        mut driver: Driver,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        loop {
            // A pending batch (from a fresh turn or a just-handled decision) is
            // processed before another turn is taken.
            if driver.pending.is_none() {
                match self.take_turn(&mut driver, &cancel).await? {
                    TurnResult::Finished(outcome) => return Ok(*outcome),
                    TurnResult::Batch => {}
                    TurnResult::Continue => continue,
                    TurnResult::OutOfTime(elapsed) => {
                        return Ok(RunOutcome::OutOfTime {
                            events: driver.events.clone(),
                            elapsed,
                            suspended: Box::new(Suspended {
                                driver,
                                approval: None,
                            }),
                        });
                    }
                }
            }

            match self.process_batch(&mut driver, &cancel).await {
                BatchResult::Done => {
                    driver.pending = None;
                    if driver.turn >= driver.limits.max_turns {
                        driver.emit(AgentEvent::RunCompleted {
                            reason: StopReason::MaxTurns,
                        });
                        return Ok(RunOutcome::Completed {
                            events: driver.events,
                        });
                    }
                    // Otherwise, loop for another turn.
                }
                BatchResult::Completed => {
                    return Ok(RunOutcome::Completed {
                        events: driver.events,
                    });
                }
                BatchResult::Suspended(request) => {
                    return Ok(RunOutcome::AwaitingApproval {
                        events: driver.events.clone(),
                        suspended: Box::new(Suspended {
                            driver,
                            approval: Some(request.clone()),
                        }),
                        request,
                    });
                }
            }
        }
    }

    /// Stream one model turn, folding its events into the driver.
    async fn take_turn(
        &self,
        driver: &mut Driver,
        cancel: &CancellationToken,
    ) -> Result<TurnResult, AgentError> {
        // The budget is checked before the turn is counted, so a pause that is
        // later continued does not spend a turn.
        if driver.wrap_up.is_none()
            && let Some(budget) = driver.limits.wall_clock()
            && driver.started.elapsed() >= budget
        {
            // Every call in a batch leaves a result, so a tool message last
            // means the model has not read what its tools returned.
            let unread_results = driver
                .messages
                .last()
                .is_some_and(|message| message.role == Role::Tool);
            if !unread_results {
                // An answer the clock stopped from continuing stays cut off.
                let reason = if driver.continuing.is_some() {
                    StopReason::MaxTokens
                } else {
                    StopReason::WallClockExceeded
                };
                driver.emit(AgentEvent::RunCompleted { reason });
                return Ok(TurnResult::Finished(Box::new(RunOutcome::Completed {
                    events: std::mem::take(&mut driver.events),
                })));
            }
            match self.config.on_wall_clock {
                WallClockPolicy::Pause => {
                    let elapsed = driver.started.elapsed();
                    driver.emit(AgentEvent::WallClockPaused {
                        elapsed_secs: elapsed.as_secs(),
                    });
                    return Ok(TurnResult::OutOfTime(elapsed));
                }
                WallClockPolicy::WrapUp => driver.wrap_up = Some(WrapUp::WallClock),
            }
        }

        // The last turn the budget allows is kept for an answer once tools
        // have run: offered tools there, a model would gather more evidence
        // and the run would end before anyone read it. The budget is not
        // extended; with no tool results yet, the turn is an ordinary one.
        if driver.wrap_up.is_none()
            && driver.continuing.is_none()
            && driver.turn.saturating_add(1) >= driver.limits.max_turns
            && driver.has_tool_results()
        {
            driver.wrap_up = Some(WrapUp::TurnBudget);
        }

        // With a known window, a conversation that leaves less room than the
        // answer needs is answered now: offered tools, the model would add
        // results the window cannot hold, and the answer would be cut off.
        let budget = &self.config.generation;
        if driver.wrap_up.is_none()
            && driver.continuing.is_none()
            && budget.guards_context()
            && driver.has_tool_results()
        {
            let schemas = self.invoker.schemas();
            let (prompt, _) =
                generation::prompt_tokens(driver.measured, &driver.messages, &schemas);
            if budget.needs_answer_now(prompt) {
                driver.wrap_up = Some(WrapUp::ContextBudget);
            }
        }

        // A run-wide output budget too low for more research is answered
        // now, from what the tools returned, with what is left.
        if driver.wrap_up.is_none()
            && driver.continuing.is_none()
            && driver.has_tool_results()
            && let Some(pool) = &budget.output_pool
            && pool.available() < budget.reserve_answer_tokens.max(generation::MIN_GRANT)
        {
            driver.wrap_up = Some(WrapUp::OutputBudget);
        }

        // Every turn reserves what it may generate from the run's output
        // pool before it starts, and is sent no more than it was granted.
        // With nothing left to grant, no turn is taken.
        let ceiling = budget.requested_max_tokens();
        let ask = ceiling.or_else(|| budget.context_window.map(|window| window.tokens));
        let reservation = match &budget.output_pool {
            Some(pool) => match pool.reserve(ask) {
                Some(reservation) => Some(reservation),
                None => return Ok(driver.end(StopReason::OutputBudget)),
            },
            None => None,
        };
        driver.sent_max_tokens = reservation
            .as_ref()
            .map_or(ceiling, |reservation| Some(reservation.tokens()));

        driver.turn += 1;

        // The final turn after a budget ran out offers no tools and says why,
        // so the model answers from what it has instead of asking for more.
        // A continuation offers none either. The instruction rides on this
        // one request only.
        let (messages, tools) = if driver.continuing.is_some() {
            let mut messages = driver.messages.clone();
            messages.push(ProviderMessage::user(CONTINUE_INSTRUCTION));
            (messages, Vec::new())
        } else if let Some(wrap_up) = driver.wrap_up {
            let mut messages = driver.messages.clone();
            messages.push(ProviderMessage::user(wrap_up.instruction()));
            (messages, Vec::new())
        } else {
            (driver.messages.clone(), self.invoker.schemas())
        };
        // Measured against the request actually sent, before it is moved.
        let planned = budget.is_active().then(|| {
            let (prompt, basis) = generation::prompt_tokens(driver.measured, &messages, &tools);
            (
                prompt,
                basis,
                u32::try_from(tools.len()).unwrap_or(u32::MAX),
            )
        });
        let measured_from = driver.messages.len();
        let request = ProviderRequest {
            model: driver.model.clone(),
            messages,
            tools,
            temperature: None,
            max_tokens: driver.sent_max_tokens,
            reasoning_effort: budget.reasoning.effort().map(str::to_owned),
        };

        let mut stream = self.provider.stream(request, cancel.clone()).await?;
        let mut accumulator = ToolCallAccumulator::new();
        let mut assistant_text = String::new();
        let mut finish: Option<(FinishReason, Option<Usage>)> = None;
        // A continuation's opening is held until it can be compared with
        // the end of the answer so far, so a restated ending is not shown
        // twice.
        let mut held = driver.continuing.clone().map(|tail| (tail, String::new()));

        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    release(driver, &mut held, &mut assistant_text);
                    driver.emit(AgentEvent::RunCompleted { reason: StopReason::Cancelled });
                    return Ok(TurnResult::Finished(Box::new(RunOutcome::Completed {
                        events: std::mem::take(&mut driver.events),
                    })));
                }
                item = stream.next() => item,
            };
            let Some(item) = next else { break };
            match item {
                Ok(ProviderEvent::RoleStarted) => {}
                Ok(ProviderEvent::Reasoning(text)) => {
                    driver.emit(AgentEvent::Reasoning { text });
                }
                Ok(ProviderEvent::Content(text)) => match held.as_mut() {
                    Some((tail, opening)) => {
                        opening.push_str(&text);
                        if opening.len() >= tail.len() {
                            release(driver, &mut held, &mut assistant_text);
                        }
                    }
                    None => {
                        assistant_text.push_str(&text);
                        driver.emit(AgentEvent::Content { text });
                    }
                },
                Ok(ProviderEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                }) => {
                    accumulator.push(index, id, name, arguments);
                }
                Ok(ProviderEvent::Finished { reason, usage }) => {
                    finish = Some((reason, usage));
                }
                Err(err) => {
                    release(driver, &mut held, &mut assistant_text);
                    driver.emit(AgentEvent::Error {
                        message: err.to_string(),
                    });
                    driver.emit(AgentEvent::RunCompleted {
                        reason: StopReason::Error,
                    });
                    return Ok(TurnResult::Finished(Box::new(RunOutcome::Completed {
                        events: std::mem::take(&mut driver.events),
                    })));
                }
            }
        }

        release(driver, &mut held, &mut assistant_text);
        // A stream that ended without its terminal event did not say the turn
        // was finished.
        let (reason, usage) = finish.unwrap_or((FinishReason::Incomplete, None));
        if let Some(reservation) = reservation {
            reservation.settle(usage.map(|usage| usage.completion_tokens));
        }
        if let Some(usage) = usage.filter(|usage| usage.prompt_tokens > 0) {
            driver.measured = Some(Measured {
                messages: measured_from,
                prompt_tokens: usage.prompt_tokens,
            });
        }
        if let Some((prompt, basis, tools_offered)) = planned {
            let report = self.report(driver, reason, usage, prompt, basis, tools_offered);
            driver.emit(AgentEvent::GenerationReport {
                report: Box::new(report),
            });
        }
        driver.emit(AgentEvent::TurnCompleted { usage });
        let continued = driver.continuing.take().is_some();

        match reason {
            // A turn cut short keeps its text, already in the log, and never
            // runs a tool call it was streaming, because its arguments may be
            // truncated. Its answer is continued when the budget allows;
            // otherwise the run ends here.
            FinishReason::Length => {
                if self.may_continue(driver, &assistant_text, !accumulator.is_empty(), cancel) {
                    if continued
                        && let Some(answer) = driver.messages.last_mut()
                        && answer.role == Role::Assistant
                    {
                        answer.content.push_str(&assistant_text);
                    } else {
                        driver
                            .messages
                            .push(ProviderMessage::assistant(assistant_text));
                    }
                    let so_far = driver.messages.last().map_or("", |m| m.content.as_str());
                    driver.continuing = Some(generation::tail(so_far).to_owned());
                    driver.continuations = driver.continuations.saturating_add(1);
                    return Ok(TurnResult::Continue);
                }
                Ok(driver.end(StopReason::MaxTokens))
            }
            FinishReason::Incomplete => Ok(driver.end(StopReason::Incomplete)),
            // The wrap-up turn is the last one whatever it produced: its
            // answer stands, and any tool calls it asked for anyway are not
            // run. The stop reason still records that a budget, not the
            // model, ended the run. A continuation is tool-less the same way.
            FinishReason::Stop | FinishReason::ToolCalls
                if driver.wrap_up.is_some() || continued =>
            {
                let reason = driver.wrap_up.map_or(StopReason::EndTurn, WrapUp::reason);
                Ok(driver.end(reason))
            }
            FinishReason::Stop => Ok(driver.end(StopReason::EndTurn)),
            FinishReason::Error => {
                driver.emit(AgentEvent::Error {
                    message: "the provider ended the turn with an error".to_owned(),
                });
                driver.emit(AgentEvent::RunCompleted {
                    reason: StopReason::Error,
                });
                Ok(TurnResult::Finished(Box::new(RunOutcome::Completed {
                    events: std::mem::take(&mut driver.events),
                })))
            }
            FinishReason::ToolCalls => {
                let calls = accumulator.into_calls();
                // Replay the assistant's tool-call message so the next turn sees
                // exactly what it asked for.
                driver.messages.push(ProviderMessage::assistant_tool_calls(
                    assistant_text,
                    calls
                        .iter()
                        .map(|call| ProviderToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        })
                        .collect(),
                ));
                driver.pending = Some(Batch { calls, next: 0 });
                Ok(TurnResult::Batch)
            }
        }
    }

    /// Process the pending tool batch from where it left off.
    async fn process_batch(&self, driver: &mut Driver, cancel: &CancellationToken) -> BatchResult {
        loop {
            // The next unhandled call, cloned so nothing borrows `driver` while
            // its events and counters are mutated below.
            let (index, call) = match &driver.pending {
                Some(batch) if batch.next < batch.calls.len() => {
                    (batch.next, batch.calls[batch.next].clone())
                }
                _ => return BatchResult::Done,
            };

            driver.emit(AgentEvent::ToolCallRequested { call: call.clone() });

            driver.tool_calls_made += 1;
            if driver.tool_calls_made > driver.limits.max_tool_calls {
                return out_of_tool_calls(driver, index);
            }

            let key = format!("{}\u{0}{}", call.name, call.arguments);
            let count = driver.call_counts.entry(key).or_insert(0);
            *count += 1;
            if *count > driver.limits.max_repeated_identical_calls {
                driver.emit(AgentEvent::RunCompleted {
                    reason: StopReason::RepeatedToolCalls,
                });
                return BatchResult::Completed;
            }

            match self.invoker.approval_for(&call) {
                ApprovalNeed::AutoApprove => {
                    self.run_call(driver, &call, cancel).await;
                }
                ApprovalNeed::Deny(reason) => {
                    let outcome = ToolOutcome::error(reason);
                    driver.messages.push(ProviderMessage::tool_result(
                        call.id.clone(),
                        outcome.content.clone(),
                    ));
                    driver.emit(AgentEvent::ToolCallCompleted {
                        id: call.id.clone(),
                        outcome,
                    });
                }
                ApprovalNeed::Require(request) => {
                    driver.emit(AgentEvent::AwaitingApproval {
                        approval_id: request.id.clone(),
                        tool_call_id: call.id.clone(),
                        name: call.name.clone(),
                    });
                    // Leave `next` pointing at this call so a resume acts on it.
                    return BatchResult::Suspended(request);
                }
            }

            if let Some(batch) = driver.pending.as_mut() {
                batch.next = index + 1;
            }
        }
    }

    /// Run one approved call: start, invoke, append its result, complete.
    /// Progress the tool reports while it runs is forwarded as
    /// [`AgentEvent::ToolProgress`], up to [`MAX_PROGRESS_PER_CALL`] reports.
    async fn run_call(&self, driver: &mut Driver, call: &ToolCall, cancel: &CancellationToken) {
        driver.emit(AgentEvent::ToolCallStarted {
            id: call.id.clone(),
            name: call.name.clone(),
        });
        let (progress, mut reports) = tokio::sync::mpsc::unbounded_channel();
        let invocation = self
            .invoker
            .invoke_with_progress(call, cancel.clone(), progress);
        tokio::pin!(invocation);
        let mut forwarded = 0;
        let mut forward = |driver: &mut Driver, progress| {
            if forwarded < MAX_PROGRESS_PER_CALL {
                forwarded += 1;
                driver.emit(AgentEvent::ToolProgress {
                    id: call.id.clone(),
                    progress,
                });
            }
        };
        let outcome = loop {
            tokio::select! {
                outcome = &mut invocation => break outcome,
                Some(report) = reports.recv() => forward(driver, report),
            }
        };
        // Reports sent just before the tool returned still precede its result.
        while let Ok(report) = reports.try_recv() {
            forward(driver, report);
        }
        let content = self.fit_to_context(driver, &outcome.content);
        driver
            .messages
            .push(ProviderMessage::tool_result(call.id.clone(), content));
        driver.emit(AgentEvent::ToolCallCompleted {
            id: call.id.clone(),
            outcome,
        });
    }

    /// A tool result as the model will see it: whole, unless the context
    /// guard is on and the result would leave less room than the answer
    /// needs, in which case it is shortened with a note saying so and the
    /// next turn is the answer, since the window is now at its reserve. The
    /// run's events keep the whole result either way.
    fn fit_to_context(&self, driver: &mut Driver, content: &str) -> String {
        let budget = &self.config.generation;
        if !budget.guards_context() {
            return content.to_owned();
        }
        let schemas = self.invoker.schemas();
        let (prompt, _) = generation::prompt_tokens(driver.measured, &driver.messages, &schemas);
        let fitted = budget
            .result_allowance(prompt)
            .and_then(|allowance| generation::fit_result(content, allowance));
        match fitted {
            Some(fitted) => {
                driver.shortened_results = driver.shortened_results.saturating_add(1);
                driver.wrap_up.get_or_insert(WrapUp::ContextBudget);
                fitted
            }
            None => content.to_owned(),
        }
    }

    /// Whether an answer cut off at its length limit may be continued: the
    /// budget allows another continuation and the run another turn and more
    /// time, it was not cancelled, it asked for no tool, some answer text
    /// arrived, and the window still has room once that text is in.
    fn may_continue(
        &self,
        driver: &Driver,
        text: &str,
        asked_for_tools: bool,
        cancel: &CancellationToken,
    ) -> bool {
        let budget = &self.config.generation;
        if driver.continuations >= budget.max_continuations.min(generation::MAX_CONTINUATIONS)
            || asked_for_tools
            || text.trim().is_empty()
            || cancel.is_cancelled()
            || driver.turn >= driver.limits.max_turns
            || budget
                .output_pool
                .as_ref()
                .is_some_and(|pool| pool.available() < generation::CONTINUE_MIN_TOKENS)
            || driver
                .limits
                .wall_clock()
                .is_some_and(|limit| driver.started.elapsed() >= limit)
        {
            return false;
        }
        let mut messages = driver.messages.clone();
        messages.push(ProviderMessage::assistant(text));
        messages.push(ProviderMessage::user(CONTINUE_INSTRUCTION));
        let (prompt, _) = generation::prompt_tokens(driver.measured, &messages, &[]);
        budget.may_continue_with(prompt)
    }

    /// The diagnostics record for a finished turn.
    fn report(
        &self,
        driver: &Driver,
        reason: FinishReason,
        usage: Option<Usage>,
        prompt: u32,
        basis: generation::PromptBasis,
        tools_offered: u32,
    ) -> GenerationReport {
        let budget = &self.config.generation;
        let window = budget.context_window;
        let tokens = window.map(|window| window.tokens);
        let requested = driver.sent_max_tokens;
        let pool = budget.output_pool.as_ref();
        let purpose = if driver.continuing.is_some() {
            "continuation"
        } else if driver.wrap_up.is_some() {
            "answer"
        } else {
            "turn"
        };
        let length_cause = (reason == FinishReason::Length)
            .then(|| generation::length_cause(requested, tokens, usage).to_owned());
        GenerationReport {
            turn: driver.turn,
            purpose: purpose.to_owned(),
            context_window: tokens,
            context_source: window.map(|window| window.source.as_str().to_owned()),
            prompt_estimate: prompt,
            prompt_basis: basis.as_str().to_owned(),
            configured_max_tokens: budget.max_output_tokens,
            requested_max_tokens: requested,
            reasoning: budget.reasoning.as_str().to_owned(),
            reasoning_effort: budget.reasoning.label().map(str::to_owned),
            tools_offered,
            shortened_results: driver.shortened_results,
            finish: reason.as_str().to_owned(),
            prompt_tokens: usage.map(|usage| usage.prompt_tokens),
            completion_tokens: usage.map(|usage| usage.completion_tokens),
            length_cause,
            output_budget: pool.map(generation::OutputPool::total),
            output_spent: pool.map(generation::OutputPool::spent),
            output_estimated: pool.is_some_and(generation::OutputPool::estimated),
            continuations: driver.continuations,
        }
    }
}

/// Emit a continuation's held opening, less any ending of the answer so far
/// it restates. Nothing is held outside a continuation.
fn release(driver: &mut Driver, held: &mut Option<(String, String)>, text: &mut String) {
    if let Some((tail, opening)) = held.take() {
        let fresh = generation::strip_overlap(&tail, &opening);
        if !fresh.is_empty() {
            text.push_str(fresh);
            driver.emit(AgentEvent::Content {
                text: fresh.to_owned(),
            });
        }
    }
}

/// The tool-call budget ran out at the call at `index` of the pending
/// batch. That call and the rest of the batch do not run; each gets a
/// controlled result so the conversation stays well-formed. When the turn
/// budget leaves room, the run owes a tool-less answer from what earlier
/// calls returned; otherwise it ends here.
fn out_of_tool_calls(driver: &mut Driver, index: usize) -> BatchResult {
    let unrun: Vec<ToolCall> = driver
        .pending
        .as_ref()
        .map(|batch| batch.calls[index..].to_vec())
        .unwrap_or_default();
    for (offset, call) in unrun.iter().enumerate() {
        if offset > 0 {
            driver.emit(AgentEvent::ToolCallRequested { call: call.clone() });
        }
        let outcome = ToolOutcome::error(format!(
            "'{}' did not run: the tool-call budget for this request is used up",
            call.name
        ));
        driver.messages.push(ProviderMessage::tool_result(
            call.id.clone(),
            outcome.content.clone(),
        ));
        driver.emit(AgentEvent::ToolCallCompleted {
            id: call.id.clone(),
            outcome,
        });
    }
    if let Some(batch) = driver.pending.as_mut() {
        batch.next = batch.calls.len();
    }
    // An answer turn the batch already owes (a result filled the context
    // window) stays the one taken.
    if driver.turn < driver.limits.max_turns {
        driver.wrap_up.get_or_insert(WrapUp::ToolCallBudget);
        return BatchResult::Done;
    }
    driver.emit(AgentEvent::RunCompleted {
        reason: StopReason::MaxToolCalls,
    });
    BatchResult::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoker::{NullInvoker, ToolSchema};
    use crate::mock::MockProvider;
    use crate::permissions::{ApprovalRequest, RiskClass};
    use crate::profile::{AgentProfile, ProfileId};
    use crate::provider::{ProviderEvent, Role, Usage};
    use async_trait::async_trait;

    #[test]
    fn inference_route_change_retains_execution_configuration_and_invoker() {
        let mut profile = AgentProfile::new(
            ProfileId::new("original").unwrap(),
            "Original",
            "Keep this persona",
            "original-model",
        );
        profile.limits.max_turns = 2;
        let mut agent = AgentLoop::from_profile(MockProvider::new(vec![]), NullInvoker, &profile);
        let before = agent.config().clone();
        let invoker = agent.invoker() as *const NullInvoker;
        agent.set_inference_route(MockProvider::new(vec![]), "routed-model");
        let mut expected = before;
        expected.model = "routed-model".to_owned();
        assert_eq!(agent.config(), &expected);
        assert_eq!(agent.invoker() as *const NullInvoker, invoker);
    }

    #[tokio::test]
    async fn run_streaming_emits_each_event_live() {
        let mock = MockProvider::new(vec![stop_turn("hi")]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let agent = AgentLoop::new(mock, NullInvoker, RunConfig::new("m"));
        let outcome = agent
            .run_streaming("go", CancellationToken::new(), tx)
            .await
            .unwrap();

        let mut streamed = Vec::new();
        while let Ok(event) = rx.try_recv() {
            streamed.push(event);
        }
        assert!(matches!(
            streamed.first(),
            Some(AgentEvent::RunStarted { .. })
        ));
        // The live stream and the recorded log are the same events, in order.
        assert_eq!(streamed, outcome.into_events());
    }

    #[tokio::test]
    async fn new_run_replays_only_the_given_session_history() {
        let mock = MockProvider::new(vec![stop_turn("reply"), stop_turn("fresh")]);
        let agent = AgentLoop::new(mock.clone(), NullInvoker, RunConfig::new("m"));
        agent
            .run_with_history(
                vec![
                    ProviderMessage::user("first"),
                    ProviderMessage::assistant("answer"),
                ],
                "second",
                CancellationToken::new(),
            )
            .await
            .unwrap();
        agent
            .run("new session", CancellationToken::new())
            .await
            .unwrap();

        let requests = mock.requests();
        assert_eq!(requests[0].messages.len(), 3);
        assert_eq!(requests[0].messages[0], ProviderMessage::user("first"));
        assert_eq!(
            requests[0].messages[1],
            ProviderMessage::assistant("answer")
        );
        assert_eq!(requests[0].messages[2], ProviderMessage::user("second"));
        assert_eq!(
            requests[1].messages,
            vec![ProviderMessage::user("new session")]
        );
    }

    fn stop_turn(content: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::RoleStarted,
            ProviderEvent::Content(content.to_owned()),
            ProviderEvent::Finished {
                reason: FinishReason::Stop,
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    total_tokens: 12,
                }),
            },
        ]
    }

    fn tool_turn() -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::RoleStarted,
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("datetime.now".into()),
                arguments: Some("{}".into()),
            },
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls,
                usage: None,
            },
        ]
    }

    /// An invoker that declares one tool and answers a fixed approval need.
    struct ScriptedInvoker {
        need: ApprovalNeed,
    }

    #[async_trait]
    impl ToolInvoker for ScriptedInvoker {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema::new(
                "datetime.now",
                "the time",
                serde_json::json!({"type": "object"}),
            )]
        }

        fn approval_for(&self, _call: &ToolCall) -> ApprovalNeed {
            self.need.clone()
        }

        async fn invoke(&self, _call: &ToolCall, _cancel: CancellationToken) -> ToolOutcome {
            ToolOutcome::ok("2026-09-01T00:00:00Z")
        }
    }

    #[tokio::test]
    async fn mock_drives_a_toolless_turn() {
        let mock = MockProvider::new(vec![stop_turn("hello")]);
        let agent = AgentLoop::new(mock, NullInvoker, RunConfig::new("m@8k"));
        let events = agent
            .run("hi", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        assert!(matches!(events[0], AgentEvent::RunStarted { .. }));
        assert!(matches!(events[1], AgentEvent::Content { ref text } if text == "hello"));
        assert!(matches!(events[2], AgentEvent::TurnCompleted { .. }));
        assert!(matches!(
            events[3],
            AgentEvent::RunCompleted {
                reason: StopReason::EndTurn
            }
        ));
        assert_eq!(events.len(), 4);
    }

    #[tokio::test]
    async fn limit_max_turns_stops_the_run() {
        let mock = MockProvider::looping(vec![tool_turn()]);
        let mut config = RunConfig::new("m@8k");
        config.limits = RunLimits {
            max_turns: 2,
            max_tool_calls: 100,
            wall_clock_secs: None,
            ..RunLimits::default()
        };
        let agent = AgentLoop::new(mock, NullInvoker, config);
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunCompleted {
                reason: StopReason::MaxTurns
            })
        ));
        // The second, last turn was the reserved answer turn: offered no
        // tools, and the call it asked for anyway did not run.
        let declined = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallCompleted { outcome, .. } if outcome.is_error))
            .count();
        assert_eq!(declined, 1);
    }

    #[tokio::test]
    async fn cancellation_before_a_turn_stops_the_run() {
        let mock = MockProvider::looping(vec![tool_turn()]);
        let agent = AgentLoop::new(mock, NullInvoker, RunConfig::new("m@8k"));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let events = agent.run("go", cancel).await.expect("run").into_events();
        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunCompleted {
                reason: StopReason::Cancelled
            })
        ));
    }

    #[tokio::test]
    async fn from_profile_seeds_persona_as_system() {
        let profile = AgentProfile::new(
            ProfileId::new("aria").expect("id"),
            "Aria",
            "You are Aria.",
            "lfm2@8k",
        );
        let mock = MockProvider::new(vec![stop_turn("done")]);
        let inspector = mock.clone();
        let agent = AgentLoop::from_profile(mock, NullInvoker, &profile);

        assert_eq!(agent.config().model, "lfm2@8k");
        assert_eq!(agent.config().system.as_deref(), Some("You are Aria."));

        let _ = agent
            .run("hi", CancellationToken::new())
            .await
            .expect("run");

        let requests = inspector.requests();
        let first = &requests[0];
        assert_eq!(first.model, "lfm2@8k");
        assert_eq!(first.messages[0].role, Role::System);
        assert_eq!(first.messages[0].content, "You are Aria.");
        assert_eq!(first.messages[1].role, Role::User);
    }

    #[tokio::test]
    async fn awaiting_approval_pauses_then_resumes() {
        // Turn 1 asks for the tool; turn 2 stops. The invoker requires approval,
        // so the run pauses, and resuming with a grant runs the tool and lets
        // the run finish.
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("all done")]);
        let request = ApprovalRequest::new("datetime.now", RiskClass::Mutating, vec![], "{}");
        let invoker = ScriptedInvoker {
            need: ApprovalNeed::Require(request),
        };
        let agent = AgentLoop::new(mock, invoker, RunConfig::new("m@8k"));

        let paused = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run");
        assert!(!paused.is_completed(), "the run must pause");
        let request = paused.approval_request().expect("a request").clone();
        assert!(matches!(
            paused.events().last(),
            Some(AgentEvent::AwaitingApproval { .. })
        ));
        let RunOutcome::AwaitingApproval { suspended, .. } = paused else {
            panic!("expected a paused run");
        };

        let decision = ApprovalDecision::grant(request.id);
        let done = agent
            .resume(suspended, decision, CancellationToken::new())
            .await
            .expect("resume");
        assert!(done.is_completed());
        let events = done.into_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
            "the granted tool must have started"
        );
        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunCompleted {
                reason: StopReason::EndTurn
            })
        ));
    }

    #[tokio::test]
    async fn a_decision_for_another_request_cannot_resume_a_tool() {
        let mock = MockProvider::new(vec![tool_turn()]);
        let request = ApprovalRequest::new("datetime.now", RiskClass::Mutating, vec![], "{}");
        let agent = AgentLoop::new(
            mock,
            ScriptedInvoker {
                need: ApprovalNeed::Require(request),
            },
            RunConfig::new("m@8k"),
        );
        let paused = agent.run("go", CancellationToken::new()).await.unwrap();
        let RunOutcome::AwaitingApproval { suspended, .. } = paused else {
            panic!("expected approval pause");
        };
        let wrong = ApprovalDecision::grant(
            ApprovalRequest::new("other", RiskClass::Mutating, vec![], "{}").id,
        );
        assert!(matches!(
            agent
                .resume(suspended, wrong, CancellationToken::new())
                .await,
            Err(AgentError::InvalidApproval)
        ));
    }

    #[tokio::test]
    async fn denied_tool_becomes_a_tool_error_message() {
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("ok")]);
        let invoker = ScriptedInvoker {
            need: ApprovalNeed::Deny("policy forbids this".to_owned()),
        };
        let agent = AgentLoop::new(mock, invoker, RunConfig::new("m@8k"));

        let outcome = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run");
        assert!(outcome.is_completed(), "a denial does not pause the run");
        let events = outcome.into_events();

        // The denied call never started, but produced an error result.
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
            "a denied tool must not start"
        );
        let denied = events.iter().any(|event| {
            matches!(event, AgentEvent::ToolCallCompleted { outcome, .. }
                if outcome.is_error && outcome.content.contains("policy forbids this"))
        });
        assert!(denied, "the model must see a controlled error result");
        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunCompleted {
                reason: StopReason::EndTurn
            })
        ));
    }

    #[tokio::test]
    async fn repeated_identical_calls_stop_the_run() {
        // The model asks for the same call forever; auto-approved, so it runs,
        // but the repeat detector stops it once the identical call recurs past
        // its budget.
        let mock = MockProvider::looping(vec![tool_turn()]);
        let mut config = RunConfig::new("m@8k");
        config.limits = RunLimits {
            max_turns: 100,
            max_tool_calls: 100,
            max_repeated_identical_calls: 2,
            wall_clock_secs: None,
            ..RunLimits::default()
        };
        let invoker = ScriptedInvoker {
            need: ApprovalNeed::AutoApprove,
        };
        let agent = AgentLoop::new(mock, invoker, config);
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        assert!(matches!(
            events.last(),
            Some(AgentEvent::RunCompleted {
                reason: StopReason::RepeatedToolCalls
            })
        ));
        // The 3rd identical call trips the detector, so exactly two ran.
        let started = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallStarted { .. }))
            .count();
        assert_eq!(started, 2);
    }

    /// An auto-approving invoker whose one tool outlasts a one-second budget,
    /// so the wall clock runs out right after its result lands — the moment a
    /// fetched page would otherwise be thrown away.
    struct SlowInvoker;

    #[async_trait]
    impl ToolInvoker for SlowInvoker {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema::new(
                "datetime.now",
                "the time",
                serde_json::json!({"type": "object"}),
            )]
        }

        fn approval_for(&self, _call: &ToolCall) -> ApprovalNeed {
            ApprovalNeed::AutoApprove
        }

        async fn invoke(&self, _call: &ToolCall, _cancel: CancellationToken) -> ToolOutcome {
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            ToolOutcome::ok("the fetched evidence")
        }
    }

    fn one_second_budget(model: &str) -> RunConfig {
        let mut config = RunConfig::new(model);
        config.limits.wall_clock_secs = Some(1);
        config
    }

    fn stop_reason(events: &[AgentEvent]) -> Option<StopReason> {
        match events.last() {
            Some(AgentEvent::RunCompleted { reason }) => Some(*reason),
            _ => None,
        }
    }

    #[tokio::test]
    async fn out_of_time_before_any_tool_ends_without_a_turn() {
        let mock = MockProvider::new(vec![stop_turn("never")]);
        let inspector = mock.clone();
        let mut config = RunConfig::new("m@8k");
        config.limits.wall_clock_secs = Some(0);
        let agent = AgentLoop::new(mock, NullInvoker, config);
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        assert_eq!(stop_reason(&events), Some(StopReason::WallClockExceeded));
        assert!(inspector.requests().is_empty(), "no turn is taken");
    }

    #[tokio::test]
    async fn out_of_time_with_unread_results_wraps_up_without_tools() {
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("answer from evidence")]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(mock, SlowInvoker, one_second_budget("m@8k"));
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        let requests = inspector.requests();
        assert_eq!(requests.len(), 2, "one final turn after the clock ran out");
        let wrap_up = &requests[1];
        assert!(wrap_up.tools.is_empty(), "the final turn offers no tools");
        assert!(
            wrap_up
                .messages
                .iter()
                .any(|m| m.role == Role::Tool && m.content == "the fetched evidence"),
            "the model reads what its tool returned"
        );
        let last = wrap_up.messages.last().expect("a message");
        assert_eq!(last.role, Role::User);
        assert_eq!(last.content, WRAP_UP_INSTRUCTION);
        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::Content { text } if text == "answer from evidence")
        ));
        assert_eq!(stop_reason(&events), Some(StopReason::WallClockExceeded));
    }

    #[tokio::test]
    async fn a_wrap_up_turn_that_asks_for_tools_runs_none() {
        let mock = MockProvider::new(vec![tool_turn(), tool_turn()]);
        let agent = AgentLoop::new(mock, SlowInvoker, one_second_budget("m@8k"));
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        let started = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallStarted { .. }))
            .count();
        assert_eq!(started, 1, "the wrap-up turn's tool call is not run");
        assert_eq!(stop_reason(&events), Some(StopReason::WallClockExceeded));
    }

    #[tokio::test]
    async fn out_of_time_pauses_then_extends_where_it_stopped() {
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("all done")]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(mock, SlowInvoker, one_second_budget("m@8k"))
            .with_wall_clock_policy(WallClockPolicy::Pause);

        let paused = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run");
        let RunOutcome::OutOfTime {
            events,
            elapsed,
            suspended,
        } = paused
        else {
            panic!("expected the run to pause out of time");
        };
        assert!(elapsed >= Duration::from_secs(1));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::WallClockPaused { elapsed_secs: 1.. })
        ));
        assert_eq!(inspector.requests().len(), 1, "no turn after the pause");

        let done = agent
            .continue_out_of_time(suspended, Continuation::Extend, CancellationToken::new())
            .await
            .expect("continue");
        let requests = inspector.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[1].tools.is_empty(),
            "an extended run keeps its tools"
        );
        assert_eq!(
            requests[1]
                .messages
                .last()
                .map(|m| (m.role, m.content.as_str())),
            Some((Role::Tool, "the fetched evidence")),
            "the run picks up with the unread result, and no wrap-up instruction"
        );
        let events = done.into_events();
        assert!(
            matches!(events[0], AgentEvent::RunStarted { .. }),
            "cumulative log"
        );
        assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn a_paused_run_can_wrap_up_on_request() {
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("best effort")]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(mock, SlowInvoker, one_second_budget("m@8k"))
            .with_wall_clock_policy(WallClockPolicy::Pause);

        let RunOutcome::OutOfTime { suspended, .. } = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
        else {
            panic!("expected the run to pause out of time");
        };
        let events = agent
            .continue_out_of_time(suspended, Continuation::WrapUp, CancellationToken::new())
            .await
            .expect("continue")
            .into_events();

        assert!(inspector.requests()[1].tools.is_empty());
        assert_eq!(stop_reason(&events), Some(StopReason::WallClockExceeded));
    }

    fn content(events: &[AgentEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Content { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn turn_ending(text: &str, reason: Option<FinishReason>) -> Vec<ProviderEvent> {
        let mut turn = vec![
            ProviderEvent::RoleStarted,
            ProviderEvent::Content(text.to_owned()),
        ];
        if let Some(reason) = reason {
            turn.push(ProviderEvent::Finished {
                reason,
                usage: None,
            });
        }
        turn
    }

    fn tool_runs(events: &[AgentEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCallStarted { .. }))
            .count()
    }

    #[tokio::test]
    async fn a_token_limited_answer_is_kept_and_reported_as_cut_off() {
        let mock = MockProvider::new(vec![turn_ending(
            "The first half of",
            Some(FinishReason::Length),
        )]);
        let agent = AgentLoop::new(mock, NullInvoker, RunConfig::new("m"));
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert_eq!(content(&events), "The first half of");
        assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
        assert!(!StopReason::MaxTokens.is_complete());
    }

    #[tokio::test]
    async fn a_token_limited_tool_call_is_not_run() {
        let truncated = vec![
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("datetime.now".into()),
                arguments: Some("{\"zo".into()),
            },
            ProviderEvent::Finished {
                reason: FinishReason::Length,
                usage: None,
            },
        ];
        let agent = AgentLoop::new(
            MockProvider::new(vec![truncated]),
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            RunConfig::new("m"),
        );
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert_eq!(tool_runs(&events), 0);
        assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    }

    #[tokio::test]
    async fn an_unfinished_stream_keeps_its_text_and_is_not_an_answer() {
        let mock = MockProvider::new(vec![turn_ending("Partial evidence says", None)]);
        let agent = AgentLoop::new(mock, NullInvoker, RunConfig::new("m"));
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert_eq!(content(&events), "Partial evidence says");
        assert_eq!(stop_reason(&events), Some(StopReason::Incomplete));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::Error { .. })),
            "an early end is reported by its stop reason, not as a failure"
        );
    }

    #[tokio::test]
    async fn an_incomplete_turn_with_partial_tool_arguments_runs_no_tool() {
        let partial = vec![
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("datetime.now".into()),
                arguments: Some("{\"q\":\"half".into()),
            },
            ProviderEvent::Finished {
                reason: FinishReason::Incomplete,
                usage: None,
            },
        ];
        let agent = AgentLoop::new(
            MockProvider::new(vec![partial]),
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            RunConfig::new("m"),
        );
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert_eq!(
            tool_runs(&events),
            0,
            "a possibly truncated call never runs"
        );
        assert_eq!(stop_reason(&events), Some(StopReason::Incomplete));
    }

    fn limited(max_turns: u32, max_tool_calls: u32) -> RunConfig {
        let mut config = RunConfig::new("m");
        config.limits = RunLimits {
            max_turns,
            max_tool_calls,
            wall_clock_secs: None,
            ..RunLimits::default()
        };
        config
    }

    #[tokio::test]
    async fn the_last_permitted_turn_answers_from_the_gathered_evidence() {
        let mock = MockProvider::new(vec![
            tool_turn(),
            tool_turn_with("call_2"),
            stop_turn("answer with sources"),
        ]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(
            mock,
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            limited(3, 100),
        );
        let events = agent
            .run("research", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        let requests = inspector.requests();
        assert_eq!(requests.len(), 3, "the budget is not extended");
        assert!(
            !requests[1].tools.is_empty(),
            "earlier turns keep their tools"
        );
        assert!(
            requests[2].tools.is_empty(),
            "the last turn offers no tools"
        );
        assert_eq!(
            requests[2].messages.last().map(|m| m.content.as_str()),
            Some(TURN_BUDGET_INSTRUCTION)
        );
        assert!(
            requests[2]
                .messages
                .iter()
                .any(|m| m.role == Role::Tool && m.content == "2026-09-01T00:00:00Z"),
            "the answer turn reads every tool result"
        );
        assert_eq!(tool_runs(&events), 2);
        assert_eq!(content(&events), "answer with sources");
        assert_eq!(stop_reason(&events), Some(StopReason::MaxTurns));
    }

    #[tokio::test]
    async fn a_last_turn_without_tool_results_is_an_ordinary_turn() {
        let mock = MockProvider::new(vec![stop_turn("plain")]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(
            mock,
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            limited(1, 100),
        );
        let events = agent
            .run("hi", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert!(!inspector.requests()[0].tools.is_empty());
        assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn a_token_limited_final_answer_reports_the_token_limit() {
        let mock = MockProvider::new(vec![
            tool_turn(),
            turn_ending("cut", Some(FinishReason::Length)),
        ]);
        let agent = AgentLoop::new(
            mock,
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            limited(2, 100),
        );
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        assert_eq!(content(&events), "cut");
        assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    }

    #[tokio::test]
    async fn an_exhausted_tool_call_budget_still_gets_an_answer() {
        let two_calls = vec![
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                name: Some("datetime.now".into()),
                arguments: Some("{}".into()),
            },
            ProviderEvent::ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                name: Some("datetime.now".into()),
                arguments: Some("{\"again\":true}".into()),
            },
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls,
                usage: None,
            },
        ];
        let mock = MockProvider::new(vec![two_calls, stop_turn("from one result")]);
        let inspector = mock.clone();
        let agent = AgentLoop::new(
            mock,
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            limited(8, 1),
        );
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();

        assert_eq!(
            tool_runs(&events),
            1,
            "the call past the budget does not run"
        );
        let requests = inspector.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty());
        assert_eq!(
            requests[1].messages.last().map(|m| m.content.as_str()),
            Some(TOOL_CALL_BUDGET_INSTRUCTION)
        );
        // Every requested call has a result, so the replay is well-formed.
        let results = requests[1]
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count();
        assert_eq!(results, 2);
        assert_eq!(content(&events), "from one result");
        assert_eq!(stop_reason(&events), Some(StopReason::MaxToolCalls));
    }

    fn tool_turn_with(id: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::RoleStarted,
            ProviderEvent::ToolCallDelta {
                index: 0,
                id: Some(id.to_owned()),
                name: Some("datetime.now".into()),
                arguments: Some(format!("{{\"n\":\"{id}\"}}")),
            },
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls,
                usage: None,
            },
        ]
    }

    /// A provider that plays `scripts` and then never answers, so a test can
    /// cancel a run in the middle of a model turn.
    struct StallingProvider {
        inner: MockProvider,
        scripted: usize,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl AgentProvider for StallingProvider {
        async fn stream(
            &self,
            request: ProviderRequest,
            cancel: CancellationToken,
        ) -> Result<crate::provider::ProviderStream, crate::provider::ProviderError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call < self.scripted {
                return self.inner.stream(request, cancel).await;
            }
            Ok(futures_util::stream::pending().boxed())
        }
    }

    #[tokio::test]
    async fn cancelling_during_the_final_answer_keeps_the_evidence() {
        let provider = StallingProvider {
            inner: MockProvider::new(vec![tool_turn()]),
            scripted: 1,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let agent = AgentLoop::new(
            provider,
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            },
            limited(2, 100),
        );
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        let events = tokio::time::timeout(Duration::from_secs(5), agent.run("go", cancel))
            .await
            .expect("cancellation ends the stalled answer turn")
            .expect("run")
            .into_events();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { outcome, .. } if !outcome.is_error
        )));
        assert_eq!(stop_reason(&events), Some(StopReason::Cancelled));
    }

    /// An invoker whose tool reports two stages before it returns.
    struct ReportingInvoker;

    #[async_trait]
    impl ToolInvoker for ReportingInvoker {
        fn schemas(&self) -> Vec<ToolSchema> {
            ScriptedInvoker {
                need: ApprovalNeed::AutoApprove,
            }
            .schemas()
        }

        async fn invoke(&self, _call: &ToolCall, _cancel: CancellationToken) -> ToolOutcome {
            ToolOutcome::ok("no progress")
        }

        async fn invoke_with_progress(
            &self,
            _call: &ToolCall,
            _cancel: CancellationToken,
            progress: crate::invoker::ToolProgressSink,
        ) -> ToolOutcome {
            let _ = progress.send(crate::invoker::ToolProgress::stage("searching"));
            tokio::task::yield_now().await;
            let _ = progress
                .send(crate::invoker::ToolProgress::stage("completed").with_outcome("evidence"));
            ToolOutcome::ok("evidence")
        }
    }

    #[tokio::test]
    async fn tool_progress_is_forwarded_in_order_before_the_result() {
        let mock = MockProvider::new(vec![tool_turn(), stop_turn("done")]);
        let agent = AgentLoop::new(mock, ReportingInvoker, RunConfig::new("m"));
        let events = agent
            .run("go", CancellationToken::new())
            .await
            .expect("run")
            .into_events();
        let order: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCallStarted { .. } => Some("started"),
                AgentEvent::ToolProgress { id, progress } => {
                    assert_eq!(id, "call_1");
                    Some(progress.stage.as_str())
                }
                AgentEvent::ToolCallCompleted { .. } => Some("result"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["started", "searching", "completed", "result"]);
    }

    /// A turn that asks for `size` distinct tool calls at once.
    fn batch_turn(size: u32) -> Vec<ProviderEvent> {
        let mut turn = vec![ProviderEvent::RoleStarted];
        for index in 0..size {
            turn.push(ProviderEvent::ToolCallDelta {
                index,
                id: Some(format!("call_{index}")),
                name: Some("datetime.now".into()),
                arguments: Some(format!("{{\"n\":{index}}}")),
            });
        }
        turn.push(ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        });
        turn
    }

    /// Every assistant message that asked for tools is followed by exactly
    /// one result per call, in order.
    fn well_formed(messages: &[ProviderMessage]) -> bool {
        for (at, message) in messages.iter().enumerate() {
            for (offset, call) in message.tool_calls.iter().enumerate() {
                let Some(result) = messages.get(at + 1 + offset) else {
                    return false;
                };
                if result.role != Role::Tool || result.tool_call_id.as_ref() != Some(&call.id) {
                    return false;
                }
            }
        }
        true
    }

    /// Whether `message` is the instruction of a budget's answer turn.
    fn says_why(message: Option<&ProviderMessage>) -> bool {
        let said = message.map_or("", |message| message.content.as_str());
        said == TURN_BUDGET_INSTRUCTION || said == TOOL_CALL_BUDGET_INSTRUCTION
    }

    #[tokio::test]
    async fn the_reserved_answer_turn_never_exceeds_any_budget() {
        for max_turns in 0..=4 {
            for max_tool_calls in 0..=4 {
                for size in 1..=3 {
                    let case = format!("turns {max_turns}, calls {max_tool_calls}, batch {size}");
                    let mock = MockProvider::looping(vec![batch_turn(size)]);
                    let inspector = mock.clone();
                    let agent = AgentLoop::new(
                        mock,
                        ScriptedInvoker {
                            need: ApprovalNeed::AutoApprove,
                        },
                        limited(max_turns, max_tool_calls),
                    );
                    let events = agent
                        .run("go", CancellationToken::new())
                        .await
                        .expect("run")
                        .into_events();
                    let requests = inspector.requests();

                    // A run always takes its first turn, as before this change.
                    let turns = u32::try_from(requests.len()).unwrap();
                    assert!(turns <= max_turns.max(1), "{case}: {turns} requests");
                    let ran = u32::try_from(tool_runs(&events)).unwrap();
                    assert!(ran <= max_tool_calls, "{case}: {ran} tools ran");
                    assert!(stop_reason(&events).is_some(), "{case}: no terminal event");
                    for (at, request) in requests.iter().enumerate() {
                        assert!(well_formed(&request.messages), "{case}: request {at}");
                        assert_eq!(request.max_tokens, None, "{case}: token settings");
                    }
                    let answer_turns = requests.iter().filter(|r| r.tools.is_empty()).count();
                    assert!(answer_turns <= 1, "{case}: {answer_turns} tool-less turns");
                    let toolless_last = requests.last().is_some_and(|r| r.tools.is_empty());
                    assert!(
                        answer_turns == 0 || toolless_last,
                        "{case}: answer turn not last"
                    );
                    if let Some(last) = requests.last().filter(|r| r.tools.is_empty()) {
                        assert!(says_why(last.messages.last()), "{case}: says why");
                    }
                }
            }
        }
    }
}
