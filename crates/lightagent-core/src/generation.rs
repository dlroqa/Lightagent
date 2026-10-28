//! Output and context budgeting for model turns.
//!
//! What a run knows about its model's room is limited, and this module keeps
//! what is known apart from what is estimated:
//!
//! - The **context window** is the loaded window one request gets, reported by
//!   the provider or set by the operator. It is never inferred from a model
//!   name and never sent to the server: the loop budgets against it but cannot
//!   enlarge it, and the server's memory admission is untouched.
//! - The **prompt size** is measured when the provider reported the previous
//!   turn's `prompt_tokens`, with only what was added since estimated; before
//!   that, the whole prompt — system text, tool schemas, history, tool results
//!   — is estimated from its bytes ([`estimate_text`]) plus a tenth. An
//!   estimate can still be wrong for an unusual tokenizer or script; the
//!   server's own count is what finally decides.
//! - The **output allowance** is a ceiling the operator may set. It is sent as
//!   `max_tokens`, never above the window, and the server clamps it again to
//!   the room its own count leaves. Unset, nothing is sent and Lightweight
//!   lets a turn use all the room the prompt leaves, so setting one can only
//!   lower a turn's allowance, never raise it. Reasoning and the visible
//!   answer share it: a model that thinks first spends the same budget, and
//!   nothing here can set part of it aside for the answer.
//! - A **reasoning effort** is sent only when the operator declared it
//!   supported for the model. It is a label the model's template interprets,
//!   not a token budget.
//!
//! What the loop does with a known window is keep generation headroom: a tool
//! result that would crowd it out is shortened in the conversation the model
//! sees (the run's events keep it whole), and once the room left is below
//! [`GenerationBudget::reserve_answer_tokens`] no more tools are offered and
//! the model answers from what it has. That headroom is room to generate in,
//! not a promise of visible answer: where reasoning and answer share one
//! budget, a model may spend it thinking.
//!
//! An optional [`OutputPool`] bounds what a whole run may generate: the lead,
//! every worker it delegates to and every continuation draw on one pool, each
//! turn reserving its `max_tokens` before it starts.
//!
//! Session history is packed against a [`HistoryWindow`], resolved the same
//! way for every entry point.
//!
//! An answer cut off at its length limit may be **continued**: one more
//! tool-less turn asks the model to carry on from where it stopped, at most
//! [`GenerationBudget::max_continuations`] times, and only with a known
//! window that still has room once the partial answer is in. Each
//! continuation spends a turn of the run's own budget, so the run's turn,
//! time and tree limits bound it too.
//!
//! The default budget knows nothing and changes nothing: no `max_tokens`, no
//! reasoning field, no early answer, no shortened result, no continuation and
//! no report.

use std::sync::{Arc, Mutex};

use crate::invoker::ToolSchema;
use crate::provider::{ProviderMessage, Usage};

/// Where a context window figure came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowSource {
    /// The provider reported the window its loaded model serves.
    Provider,
    /// The operator configured it, and the provider reported nothing smaller.
    Configured,
}

impl WindowSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Configured => "configured",
        }
    }
}

/// The context window one request gets, in tokens, and where that came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextWindow {
    pub tokens: u32,
    pub source: WindowSource,
}

impl ContextWindow {
    /// The window to budget against: the smaller of what the provider
    /// reported and what the operator configured, since a configured figure
    /// may narrow the budget but never stretch the window the server loaded.
    pub fn resolve(reported: Option<u32>, configured: Option<u32>) -> Option<Self> {
        let reported = reported.filter(|tokens| *tokens > 0);
        let configured = configured.filter(|tokens| *tokens > 0);
        match (reported, configured) {
            (Some(reported), Some(configured)) if configured < reported => Some(Self {
                tokens: configured,
                source: WindowSource::Configured,
            }),
            (Some(tokens), _) => Some(Self {
                tokens,
                source: WindowSource::Provider,
            }),
            (None, Some(tokens)) => Some(Self {
                tokens,
                source: WindowSource::Configured,
            }),
            (None, None) => None,
        }
    }
}

/// Where the window a session's history is packed against came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// The window the provider reported serving the model with.
    Provider,
    /// The operator's `generation.context_window`.
    Configured,
    /// `runtime.n_ctx`: the window a placement asks for, not one verified to
    /// be loaded.
    Placement,
    /// Nothing was known; [`FALLBACK_WINDOW`] is assumed.
    Fallback,
}

impl HistorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Configured => "configured",
            Self::Placement => "placement",
            Self::Fallback => "fallback",
        }
    }
}

/// The window assumed for packing history when nothing better is known.
pub const FALLBACK_WINDOW: u32 = 4_096;

/// The window a session's history is packed against, and its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryWindow {
    pub tokens: u32,
    pub source: HistorySource,
}

impl HistoryWindow {
    /// The run's budgeted window when one is known (a reported window,
    /// narrowed by any configured one); otherwise the window a placement asks
    /// for; otherwise [`FALLBACK_WINDOW`]. A reported window is never cut down
    /// to the fallback or to the placement figure.
    pub fn resolve(window: Option<ContextWindow>, placement: Option<u32>) -> Self {
        if let Some(window) = window {
            let source = match window.source {
                WindowSource::Provider => HistorySource::Provider,
                WindowSource::Configured => HistorySource::Configured,
            };
            return Self {
                tokens: window.tokens,
                source,
            };
        }
        match placement.filter(|tokens| *tokens > 0) {
            Some(tokens) => Self {
                tokens,
                source: HistorySource::Placement,
            },
            None => Self {
                tokens: FALLBACK_WINDOW,
                source: HistorySource::Fallback,
            },
        }
    }

    /// The window, unless it is only the fallback guess.
    pub fn known(&self) -> Option<u32> {
        (self.source != HistorySource::Fallback).then_some(self.tokens)
    }
}

/// The least a turn may reserve from an [`OutputPool`] and still be taken.
pub const MIN_GRANT: u32 = 16;

/// What a pool-backed turn reserves, and is sent as its `max_tokens`, when
/// nothing else bounds it: no output ceiling and no known window. Room for a
/// full worker report (roughly 1,500 English words), small enough that the
/// default four concurrent workers hold at most 8,192 tokens of a pool at
/// once. It says nothing about the model's context capacity, which stays
/// unknown, and it is never derived from another model's window.
pub const DEFAULT_UNKNOWN_CAPACITY_TURN_TOKENS: u32 = 2_048;

/// A ceiling on what a whole run may generate, shared by the lead, its
/// workers and its continuations.
///
/// A turn reserves its `max_tokens` before it starts, so turns running at the
/// same time are never together granted more than remains. When the turn
/// ends the reservation is settled against the provider's
/// `completion_tokens`. A turn whose usage was not reported, or that was
/// cancelled or failed before it could be settled, is charged its whole
/// reservation, so the account errs high, never low, and
/// [`OutputPool::estimated`] says that happened.
///
/// A turn never reserves the whole pool merely because nothing else bounded
/// it: the loop asks for its output ceiling, or the room its known window
/// leaves, or [`OutputPool::turn_tokens`]. The amount is sent as the turn's
/// `max_tokens`, which is what makes the reservation binding on a provider
/// that honours it. A provider that reports more than it was granted has not
/// honoured it: the excess is charged and recorded as an
/// [`overrun`](OutputPool::overrun), and the pool grants nothing more, so
/// the overspend is bounded by the turns already in flight.
#[derive(Clone, Debug)]
pub struct OutputPool {
    inner: Arc<Mutex<PoolState>>,
}

#[derive(Debug)]
struct PoolState {
    total: u32,
    spent: u32,
    reserved: u32,
    estimated: bool,
    turn_tokens: u32,
    overrun: u32,
}

/// Two handles are the same pool only if they share it.
impl PartialEq for OutputPool {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for OutputPool {}

impl OutputPool {
    pub fn new(total: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolState {
                total,
                spent: 0,
                reserved: 0,
                estimated: false,
                turn_tokens: DEFAULT_UNKNOWN_CAPACITY_TURN_TOKENS,
                overrun: 0,
            })),
        }
    }

    /// Set what a turn with nothing else to bound it reserves (at least
    /// [`MIN_GRANT`]).
    pub fn with_turn_tokens(self, tokens: u32) -> Self {
        if let Ok(mut state) = self.inner.lock() {
            state.turn_tokens = tokens.max(MIN_GRANT);
        }
        self
    }

    /// What a turn with no output ceiling and no known window reserves.
    pub fn turn_tokens(&self) -> u32 {
        self.read(|state| state.turn_tokens, MIN_GRANT)
    }

    /// Tokens reported beyond what turns were granted. Once any are, the
    /// pool grants nothing more.
    pub fn overrun(&self) -> u32 {
        self.read(|state| state.overrun, u32::MAX)
    }

    fn read<T>(&self, read: impl FnOnce(&PoolState) -> T, otherwise: T) -> T {
        self.inner.lock().map_or(otherwise, |state| read(&state))
    }

    /// The pool's size.
    pub fn total(&self) -> u32 {
        self.read(|state| state.total, 0)
    }

    /// Tokens charged so far.
    pub fn spent(&self) -> u32 {
        self.read(|state| state.spent, u32::MAX)
    }

    /// Tokens neither charged nor reserved by a turn in flight.
    pub fn available(&self) -> u32 {
        self.read(
            |state| {
                state
                    .total
                    .saturating_sub(state.spent)
                    .saturating_sub(state.reserved)
            },
            0,
        )
    }

    /// Whether any charge was a reservation standing in for unreported usage.
    pub fn estimated(&self) -> bool {
        self.read(|state| state.estimated, true)
    }

    /// Reserve up to `ask` tokens for one turn (everything available when
    /// `None`), or `None` when less than [`MIN_GRANT`] is available or a
    /// provider has overrun an earlier grant.
    pub fn reserve(&self, ask: Option<u32>) -> Option<Reservation> {
        let mut state = self.inner.lock().ok()?;
        if state.overrun > 0 {
            return None;
        }
        let available = state
            .total
            .saturating_sub(state.spent)
            .saturating_sub(state.reserved);
        if available < MIN_GRANT {
            return None;
        }
        let tokens = ask.map_or(available, |ask| ask.min(available)).max(1);
        state.reserved = state.reserved.saturating_add(tokens);
        Some(Reservation {
            pool: self.clone(),
            tokens,
            settled: false,
        })
    }

    fn charge(&self, reserved: u32, used: Option<u32>) {
        if let Ok(mut state) = self.inner.lock() {
            state.reserved = state.reserved.saturating_sub(reserved);
            state.spent = state.spent.saturating_add(used.unwrap_or(reserved));
            state.estimated |= used.is_none();
            if let Some(used) = used {
                state.overrun = state.overrun.saturating_add(used.saturating_sub(reserved));
            }
        }
    }
}

/// One turn's claim on an [`OutputPool`]. Dropping it unsettled charges it
/// in full.
#[derive(Debug)]
pub struct Reservation {
    pool: OutputPool,
    tokens: u32,
    settled: bool,
}

impl Reservation {
    /// The tokens reserved, which the turn sends as its `max_tokens`.
    pub fn tokens(&self) -> u32 {
        self.tokens
    }

    /// Charge what the turn generated: the provider's count, or the whole
    /// reservation when it reported none.
    pub fn settle(mut self, used: Option<u32>) {
        self.pool.charge(self.tokens, used);
        self.settled = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.settled {
            self.pool.charge(self.tokens, None);
        }
    }
}

/// What a run asks of the model's reasoning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Reasoning {
    /// Send nothing: the model behaves as its own template intends.
    #[default]
    Default,
    /// Send this effort label; the operator declared it supported.
    Applied(String),
    /// This effort was configured but not declared supported for the model,
    /// so nothing is sent.
    Unsupported(String),
}

impl Reasoning {
    /// Choose from a configured effort and the efforts the operator declared
    /// the model supports. Nothing is assumed supported.
    pub fn resolve(effort: Option<&str>, supported: &[String]) -> Self {
        match effort.map(str::trim).filter(|effort| !effort.is_empty()) {
            None => Self::Default,
            Some(effort) if supported.iter().any(|known| known.trim() == effort) => {
                Self::Applied(effort.to_owned())
            }
            Some(effort) => Self::Unsupported(effort.to_owned()),
        }
    }

    /// The effort to send, if any.
    pub fn effort(&self) -> Option<&str> {
        match self {
            Self::Applied(effort) => Some(effort),
            Self::Default | Self::Unsupported(_) => None,
        }
    }

    /// The configured label, whether or not it is sent.
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Applied(effort) | Self::Unsupported(effort) => Some(effort),
            Self::Default => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Applied(_) => "applied",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// A run's output and context budget. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenerationBudget {
    /// The window one request gets, when known.
    pub context_window: Option<ContextWindow>,
    /// The output ceiling per request. `None` sends no `max_tokens` and
    /// leaves the provider's default (Lightweight: all the room the prompt
    /// leaves in the window), so a value here can only lower it.
    pub max_output_tokens: Option<u32>,
    /// Generation headroom kept once tools have run: room to generate in,
    /// which reasoning may consume, not a guaranteed visible answer. Zero
    /// turns the context guard off. Used only with a known window.
    pub reserve_answer_tokens: u32,
    /// What to ask of the model's reasoning.
    pub reasoning: Reasoning,
    /// What the whole run, workers and continuations included, may
    /// generate. `None`: no run-wide token bound.
    pub output_pool: Option<OutputPool>,
    /// How many times an answer cut off at its length limit may be
    /// continued. Used only with a known window.
    pub max_continuations: u32,
}

/// The tokens added to every estimate of a message, for its role and framing.
const MESSAGE_OVERHEAD: u32 = 5;

/// The tokens a shortened result's note may take.
const NOTE_TOKENS: u32 = 48;

/// The least room a continuation must leave for the model to write in.
pub const CONTINUE_MIN_TOKENS: u32 = 256;

/// The most continuations a budget may allow.
pub const MAX_CONTINUATIONS: u32 = 3;

/// The shortest restatement of earlier text a continuation is trimmed of:
/// below this, a match is as likely a deliberate repeat as a restated
/// ending.
const MIN_OVERLAP: usize = 32;

/// How much of the text so far a continuation is compared with.
pub(crate) const OVERLAP_TAIL: usize = 512;

impl GenerationBudget {
    /// Whether the budget changes anything about a run. An inactive budget
    /// leaves requests, conversations and events exactly as before.
    pub fn is_active(&self) -> bool {
        self.context_window.is_some()
            || self.max_output_tokens.is_some()
            || self.reasoning != Reasoning::Default
            || self.output_pool.is_some()
    }

    /// The `max_tokens` to send: the operator's allowance, never above what
    /// the window could hold. The server clamps it again by its own count.
    pub fn requested_max_tokens(&self) -> Option<u32> {
        let allowance = self.max_output_tokens.filter(|tokens| *tokens > 0)?;
        Some(match self.context_window {
            Some(window) => allowance.min(window.tokens.saturating_sub(1).max(1)),
            None => allowance,
        })
    }

    /// The tokens left for generation once a prompt of `prompt` tokens is in,
    /// after a safety margin. `None` when the window is unknown.
    pub fn room(&self, prompt: u32) -> Option<u32> {
        let window = self.context_window?;
        Some(
            window
                .tokens
                .saturating_sub(prompt)
                .saturating_sub(margin(window.tokens)),
        )
    }

    /// Whether the context guard is on: a known window and room to keep.
    pub fn guards_context(&self) -> bool {
        self.context_window.is_some() && self.reserve_answer_tokens > 0
    }

    /// Whether a prompt of `prompt` tokens leaves less room than the answer
    /// needs, so the model should answer now rather than gather more.
    pub fn needs_answer_now(&self, prompt: u32) -> bool {
        self.guards_context()
            && self
                .room(prompt)
                .is_some_and(|room| room < self.reserve_answer_tokens)
    }

    /// The tokens a new tool result may take with a prompt of `prompt` tokens
    /// already in, keeping the answer's room. `None` when the guard is off.
    pub fn result_allowance(&self, prompt: u32) -> Option<u32> {
        if !self.guards_context() {
            return None;
        }
        self.room(prompt)
            .map(|room| room.saturating_sub(self.reserve_answer_tokens))
    }

    /// The budget a delegated worker runs under. Its allowance is never
    /// larger than this run's and it draws on this run's output pool; the
    /// window and the reasoning setting carry over only for the same model,
    /// because both were established for it.
    pub fn for_worker(&self, same_model: bool) -> Self {
        Self {
            context_window: self.context_window.filter(|_| same_model),
            max_output_tokens: self.max_output_tokens,
            reserve_answer_tokens: self.reserve_answer_tokens,
            reasoning: if same_model {
                self.reasoning.clone()
            } else {
                Reasoning::Default
            },
            output_pool: self.output_pool.clone(),
            max_continuations: self.max_continuations,
        }
    }

    /// The budget a worker routed to another model runs under. `own` is that
    /// model's budget, resolved from the operator's settings and the window
    /// its provider reports: the worker keeps that window (per-request
    /// capacity, which may be larger than this run's), its reserve and its
    /// own reasoning declaration. Its allowance is never larger than this
    /// run's, its continuations never more, and it draws on this run's
    /// output pool, never on one of its own.
    pub fn for_routed_worker(&self, own: Self) -> Self {
        let max_output_tokens = match (own.max_output_tokens, self.max_output_tokens) {
            (Some(own), Some(lead)) => Some(own.min(lead)),
            (own, lead) => own.or(lead),
        };
        Self {
            context_window: own.context_window,
            max_output_tokens,
            reserve_answer_tokens: own.reserve_answer_tokens,
            reasoning: own.reasoning,
            output_pool: self.output_pool.clone(),
            max_continuations: own.max_continuations.min(self.max_continuations),
        }
    }

    /// Whether a continuation would leave enough room once a prompt of
    /// `prompt` tokens, the partial answer included, is in.
    pub fn may_continue_with(&self, prompt: u32) -> bool {
        self.room(prompt)
            .is_some_and(|room| room >= CONTINUE_MIN_TOKENS)
    }
}

/// The share of a window kept free for estimate error: 2%, at least 16 tokens.
pub fn margin(window: u32) -> u32 {
    (window / 50).max(16)
}

/// A conservative token estimate for `text`: one token per three bytes,
/// rounded up. Most tokenizers average closer to four bytes per token in
/// English and code; text in some scripts, or unusual symbols, can cost more,
/// which is why a measured count is preferred whenever one is available.
pub fn estimate_text(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(3)).unwrap_or(u32::MAX)
}

/// An estimate for a conversation, tool calls and per-message framing
/// included.
pub fn estimate_messages(messages: &[ProviderMessage]) -> u32 {
    messages.iter().fold(0_u32, |total, message| {
        let calls = message.tool_calls.iter().fold(0_u32, |sum, call| {
            sum.saturating_add(estimate_text(&call.id))
                .saturating_add(estimate_text(&call.name))
                .saturating_add(estimate_text(&call.arguments))
                .saturating_add(MESSAGE_OVERHEAD)
        });
        total
            .saturating_add(estimate_text(&message.content))
            .saturating_add(calls)
            .saturating_add(MESSAGE_OVERHEAD)
    })
}

/// An estimate for the tool schemas declared to the model.
pub fn estimate_tools(tools: &[ToolSchema]) -> u32 {
    tools.iter().fold(0_u32, |total, tool| {
        total
            .saturating_add(estimate_text(&tool.name))
            .saturating_add(estimate_text(&tool.description))
            .saturating_add(estimate_text(&tool.parameters.to_string()))
            .saturating_add(MESSAGE_OVERHEAD)
    })
}

/// An estimate with a tenth added for estimate error.
fn padded(estimate: u32) -> u32 {
    estimate.saturating_add(estimate / 10)
}

/// The prompt count the provider reported for an earlier turn, and how many
/// of the run's messages that turn's request began with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Measured {
    pub(crate) messages: usize,
    pub(crate) prompt_tokens: u32,
}

/// How a prompt figure was arrived at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptBasis {
    /// The provider's count for an earlier turn, plus an estimate of what
    /// was added since.
    Measured,
    /// An estimate of the whole prompt.
    Estimated,
}

impl PromptBasis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Estimated => "estimated",
        }
    }
}

/// The prompt size of a request of `messages` and `tools`, measured where
/// the provider reported an earlier turn's count. The measured request's
/// messages must be a prefix of `messages`, which holds for one run's
/// conversation: it only grows.
pub(crate) fn prompt_tokens(
    measured: Option<Measured>,
    messages: &[ProviderMessage],
    tools: &[ToolSchema],
) -> (u32, PromptBasis) {
    match measured.filter(|m| m.prompt_tokens > 0 && m.messages <= messages.len()) {
        Some(m) => (
            m.prompt_tokens
                .saturating_add(padded(estimate_messages(&messages[m.messages..]))),
            PromptBasis::Measured,
        ),
        None => (
            padded(estimate_messages(messages).saturating_add(estimate_tools(tools))),
            PromptBasis::Estimated,
        ),
    }
}

/// `content` shortened to fit `allowance` tokens, with a note saying so, or
/// `None` when it already fits or costs no more than the note would. The cut
/// falls on a character boundary.
pub(crate) fn fit_result(content: &str, allowance: u32) -> Option<String> {
    if padded(estimate_text(content)) <= allowance.max(NOTE_TOKENS) {
        return None;
    }
    // Three bytes per estimated token, less the padding the estimate adds.
    let budget = allowance.saturating_sub(NOTE_TOKENS) as usize;
    let mut keep = budget.saturating_mul(3).saturating_mul(10) / 11;
    keep = keep.min(content.len());
    while keep > 0 && !content.is_char_boundary(keep) {
        keep -= 1;
    }
    Some(format!(
        "{}\n\n[Lightagent shortened this result to fit the model's context window: \
         the first {keep} of {} bytes are shown.]",
        &content[..keep],
        content.len()
    ))
}

/// The end of `text`, at most [`OVERLAP_TAIL`] bytes, cut on a character
/// boundary.
pub(crate) fn tail(text: &str) -> &str {
    let mut start = text.len().saturating_sub(OVERLAP_TAIL);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// `next` without any opening it restates from the end of `tail`: asked to
/// continue, a model sometimes repeats its last words first. Only a clear
/// restatement is trimmed: at least [`MIN_OVERLAP`] bytes, more than one
/// word, starting where a word starts in the earlier text. Anything less —
/// a short refrain, a repeated identifier, a match beginning mid-word — is
/// kept, so legitimate repetition is never removed; the cost is that a short
/// restatement may show twice. A model that starts its whole answer again
/// is not detected.
pub(crate) fn strip_overlap<'a>(tail: &str, next: &'a str) -> &'a str {
    let longest = tail.len().min(next.len());
    for length in (MIN_OVERLAP..=longest).rev() {
        if !next.is_char_boundary(length) || !tail.ends_with(&next[..length]) {
            continue;
        }
        let restated = &next[..length];
        let starts_a_word = tail[..tail.len() - length]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if starts_a_word && restated.trim().contains(char::is_whitespace) {
            return &next[length..];
        }
    }
    next
}

/// Why a turn that stopped at its length limit most likely stopped, from the
/// provider's counts: the requested allowance was spent, or prompt and output
/// filled the window. Unknown without the counts to tell.
pub(crate) fn length_cause(
    requested: Option<u32>,
    window: Option<u32>,
    usage: Option<Usage>,
) -> &'static str {
    let Some(usage) = usage else {
        return "unknown";
    };
    if requested.is_some_and(|requested| usage.completion_tokens >= requested) {
        return "output_allowance";
    }
    let used = usage
        .prompt_tokens
        .saturating_add(usage.completion_tokens)
        .saturating_add(1);
    if window.is_some_and(|window| used >= window) {
        return "context_window";
    }
    "unknown"
}

/// The privacy-safe record of one model turn's budget and how it ended.
///
/// Numbers and fixed codes only, plus the operator's own reasoning label:
/// nothing from the conversation, the tools' output or the model's reasoning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenerationReport {
    /// The run's turn number, from one.
    pub turn: u32,
    /// `turn` for an ordinary turn, `answer` for a final tool-less one.
    pub purpose: String,
    /// The window budgeted against, when known, and its source.
    pub context_window: Option<u32>,
    pub context_source: Option<String>,
    /// The prompt size this request was budgeted with, and whether it rests
    /// on a measured count (`measured`) or only an estimate (`estimated`).
    pub prompt_estimate: u32,
    pub prompt_basis: String,
    /// The operator's ceiling, as configured.
    pub configured_max_tokens: Option<u32>,
    /// The `max_tokens` actually sent, if any: the ceiling, capped at the
    /// window and at what the run's output pool granted. The server may clamp
    /// it further; the allowance it applied is not reported to the client.
    pub requested_max_tokens: Option<u32>,
    /// `default`, `applied` or `unsupported`, with the configured label.
    pub reasoning: String,
    pub reasoning_effort: Option<String>,
    /// Tool schemas offered with this request.
    pub tools_offered: u32,
    /// Tool results shortened to fit the window so far in this run.
    pub shortened_results: u32,
    /// The provider's finish reason: `stop`, `tool_calls`, `length`, `error`
    /// or `incomplete` (the stream ended without one).
    pub finish: String,
    /// The provider's counts for this turn, when it reported them.
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// For a `length` finish: `output_allowance`, `context_window` or
    /// `unknown`.
    pub length_cause: Option<String>,
    /// The run's output pool, when it has one: its size, what has been
    /// charged so far, and whether any charge stood in for unreported usage.
    pub output_budget: Option<u32>,
    pub output_spent: Option<u32>,
    pub output_estimated: bool,
    /// Continuations taken so far in this run, this turn included.
    pub continuations: u32,
    /// What bounded this turn's reservation from the pool: `ceiling` (an
    /// explicit or inherited `max_output_tokens`), `remaining_context` (the
    /// known window less the estimated prompt) or `unknown_capacity` (the
    /// pool's per-turn default; the window is unknown and stays so). `None`
    /// without a pool.
    pub reservation_basis: Option<String>,
    /// Tokens the provider reported beyond what turns were granted, so far.
    pub output_overrun: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(tokens: u32) -> Option<ContextWindow> {
        Some(ContextWindow {
            tokens,
            source: WindowSource::Provider,
        })
    }

    #[test]
    fn the_default_budget_changes_nothing() {
        let budget = GenerationBudget::default();
        assert!(!budget.is_active());
        assert_eq!(budget.requested_max_tokens(), None);
        assert_eq!(budget.room(10), None);
        assert!(!budget.needs_answer_now(u32::MAX));
        assert_eq!(budget.result_allowance(0), None);
        assert_eq!(budget.reasoning.effort(), None);
    }

    #[test]
    fn a_configured_window_narrows_but_never_stretches_the_reported_one() {
        let narrowed = ContextWindow::resolve(Some(8_192), Some(4_096)).unwrap();
        assert_eq!(narrowed.tokens, 4_096);
        assert_eq!(narrowed.source, WindowSource::Configured);
        let kept = ContextWindow::resolve(Some(8_192), Some(32_768)).unwrap();
        assert_eq!(kept.tokens, 8_192, "a larger configured window is ignored");
        assert_eq!(kept.source, WindowSource::Provider);
        let configured = ContextWindow::resolve(None, Some(4_096)).unwrap();
        assert_eq!(configured.source, WindowSource::Configured);
        assert_eq!(ContextWindow::resolve(None, None), None);
        assert_eq!(ContextWindow::resolve(Some(0), Some(0)), None);
    }

    #[test]
    fn the_allowance_is_sent_as_asked_but_never_above_the_window() {
        let mut budget = GenerationBudget {
            max_output_tokens: Some(2_048),
            ..GenerationBudget::default()
        };
        assert_eq!(budget.requested_max_tokens(), Some(2_048));
        budget.context_window = window(8_192);
        assert_eq!(budget.requested_max_tokens(), Some(2_048));
        budget.max_output_tokens = Some(65_536);
        assert_eq!(budget.requested_max_tokens(), Some(8_191));
        budget.max_output_tokens = Some(0);
        assert_eq!(budget.requested_max_tokens(), None, "zero is not a limit");
    }

    #[test]
    fn reasoning_is_sent_only_when_declared_supported() {
        let supported = vec!["none".to_owned(), "low".to_owned()];
        assert_eq!(
            Reasoning::resolve(Some("low"), &supported),
            Reasoning::Applied("low".into())
        );
        let unsupported = Reasoning::resolve(Some("high"), &supported);
        assert_eq!(unsupported, Reasoning::Unsupported("high".into()));
        assert_eq!(unsupported.effort(), None);
        assert_eq!(
            Reasoning::resolve(Some("low"), &[]),
            Reasoning::Unsupported("low".into()),
            "nothing is assumed supported"
        );
        assert_eq!(Reasoning::resolve(None, &supported), Reasoning::Default);
        assert_eq!(
            Reasoning::resolve(Some("  "), &supported),
            Reasoning::Default
        );
    }

    #[test]
    fn the_guard_keeps_the_answer_room_inside_a_known_window() {
        let budget = GenerationBudget {
            context_window: window(8_192),
            reserve_answer_tokens: 1_024,
            ..GenerationBudget::default()
        };
        let margin = margin(8_192);
        assert_eq!(budget.room(1_000), Some(8_192 - 1_000 - margin));
        assert!(!budget.needs_answer_now(1_000));
        assert!(budget.needs_answer_now(8_192 - margin - 1_023));
        assert_eq!(
            budget.result_allowance(1_000),
            Some(8_192 - 1_000 - margin - 1_024)
        );
        assert_eq!(budget.result_allowance(8_192), Some(0));
        let off = GenerationBudget {
            reserve_answer_tokens: 0,
            ..budget
        };
        assert!(!off.needs_answer_now(8_192));
        assert_eq!(off.result_allowance(8_000), None);
    }

    #[test]
    fn a_worker_never_gets_a_larger_allowance_or_another_models_window() {
        let lead = GenerationBudget {
            context_window: window(8_192),
            max_output_tokens: Some(1_024),
            reserve_answer_tokens: 512,
            reasoning: Reasoning::Applied("low".into()),
            output_pool: Some(OutputPool::new(10_000)),
            max_continuations: 1,
        };
        assert_eq!(lead.for_worker(true), lead);
        assert_eq!(
            lead.for_worker(false).output_pool,
            lead.output_pool,
            "every worker draws on the lead's pool"
        );
        let other = lead.for_worker(false);
        assert_eq!(other.context_window, None);
        assert_eq!(other.reasoning, Reasoning::Default);
        assert_eq!(other.max_output_tokens, Some(1_024));
        assert_eq!(other.max_continuations, 1);
    }

    #[test]
    fn a_continuation_needs_a_known_window_with_room_left() {
        let unknown = GenerationBudget {
            max_continuations: 1,
            ..GenerationBudget::default()
        };
        assert!(!unknown.may_continue_with(0), "no window, no continuation");
        let known = GenerationBudget {
            context_window: window(4_096),
            ..unknown
        };
        let margin = margin(4_096);
        assert!(known.may_continue_with(4_096 - margin - CONTINUE_MIN_TOKENS));
        assert!(!known.may_continue_with(4_096 - margin - CONTINUE_MIN_TOKENS + 1));
    }

    #[test]
    fn a_restated_ending_is_trimmed_from_a_continuation() {
        let so_far = "The three sources agree that the bridge opened in";
        assert_eq!(
            strip_overlap(
                so_far,
                "three sources agree that the bridge opened in 1932, after"
            ),
            " 1932, after"
        );
        assert_eq!(strip_overlap(so_far, " 1932, after"), " 1932, after");
        assert_eq!(
            strip_overlap(so_far, "agree that the bridge opened in 1932"),
            "agree that the bridge opened in 1932",
            "a repeat under 32 bytes is kept"
        );
        let ward = "They toured the warehouse district by the river opened in";
        let mid_word = "house district by the river opened in 1932";
        assert_eq!(strip_overlap(ward, mid_word), mid_word, "starts mid-word");
        let key = "the key is 0123456789abcdef0123456789abcdef";
        let again = "0123456789abcdef0123456789abcdef, again";
        assert_eq!(
            strip_overlap(key, again),
            again,
            "one word is not a restatement"
        );
        assert_eq!(strip_overlap("", "anything"), "anything");
        let accented = "é".repeat(400);
        assert_eq!(tail(&accented).len(), OVERLAP_TAIL);
        assert_eq!(tail("short"), "short");
        let french = "le café crème était très bon et le thé aussi";
        assert_eq!(
            strip_overlap(french, "café crème était très bon et le thé aussi, merci"),
            ", merci"
        );
    }

    #[test]
    fn history_is_packed_against_the_best_known_window() {
        let provider = HistoryWindow::resolve(window(131_072), Some(4_096));
        assert_eq!(provider.tokens, 131_072, "never cut to a smaller setting");
        assert_eq!(provider.source, HistorySource::Provider);
        let narrowed =
            HistoryWindow::resolve(ContextWindow::resolve(Some(32_768), Some(8_192)), None);
        assert_eq!(
            (narrowed.tokens, narrowed.source),
            (8_192, HistorySource::Configured)
        );
        let placement = HistoryWindow::resolve(None, Some(16_384));
        assert_eq!(placement.source, HistorySource::Placement);
        assert_eq!(placement.known(), Some(16_384));
        let fallback = HistoryWindow::resolve(None, None);
        assert_eq!(fallback.tokens, FALLBACK_WINDOW);
        assert_eq!(fallback.known(), None, "a guess is not shown as known");
        assert_eq!(
            HistoryWindow::resolve(None, Some(0)).source,
            HistorySource::Fallback
        );
    }

    #[test]
    fn a_pool_reserves_up_front_and_settles_against_reported_usage() {
        let pool = OutputPool::new(1_000);
        let first = pool.reserve(Some(600)).unwrap();
        let second = pool.reserve(Some(600)).unwrap();
        assert_eq!(first.tokens(), 600);
        assert_eq!(second.tokens(), 400, "never more than remains unreserved");
        assert!(pool.reserve(Some(10)).is_none(), "nothing left to grant");
        first.settle(Some(120));
        assert_eq!(pool.spent(), 120);
        assert_eq!(pool.available(), 1_000 - 120 - 400);
        assert!(!pool.estimated());
        // Unreported usage is charged in full, and said to be.
        second.settle(None);
        assert_eq!(pool.spent(), 520);
        assert!(pool.estimated());
        // A turn that never settles (cancelled, failed) is charged in full.
        drop(pool.reserve(Some(300)).unwrap());
        assert_eq!(pool.spent(), 820);
        // A provider that reports more than it was granted is charged it,
        // and the excess is recorded.
        pool.reserve(None).unwrap().settle(Some(400));
        assert_eq!(pool.available(), 0);
        assert_eq!(pool.overrun(), 220);
        assert!(pool.reserve(None).is_none());
    }

    #[test]
    fn an_overrun_stops_further_grants_even_with_tokens_left() {
        let pool = OutputPool::new(10_000);
        assert_eq!(pool.turn_tokens(), DEFAULT_UNKNOWN_CAPACITY_TURN_TOKENS);
        assert_eq!(
            OutputPool::new(10).with_turn_tokens(4).turn_tokens(),
            MIN_GRANT
        );
        pool.reserve(Some(100)).unwrap().settle(Some(150));
        assert_eq!((pool.spent(), pool.overrun()), (150, 50));
        assert!(pool.available() > 9_000);
        assert!(
            pool.reserve(Some(100)).is_none(),
            "the ceiling was not honoured"
        );
    }

    #[test]
    fn concurrent_reservations_never_exceed_the_pool() {
        let pool = OutputPool::new(10_000);
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let pool = pool.clone();
                std::thread::spawn(move || pool.reserve(Some(1_000)))
            })
            .collect();
        let granted: Vec<Reservation> = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect();
        let total: u32 = granted.iter().map(Reservation::tokens).sum();
        assert!(total <= 10_000, "{total} granted");
        assert_eq!(granted.len(), 10);
        for reservation in granted {
            reservation.settle(Some(10));
        }
        assert_eq!(pool.spent(), 100);
        assert_eq!(pool.available(), 9_900);
    }

    #[test]
    fn estimates_cover_every_part_of_the_prompt() {
        let messages = vec![
            ProviderMessage::system("s".repeat(300)),
            ProviderMessage::user("u".repeat(30)),
        ];
        assert_eq!(estimate_messages(&messages), 100 + 5 + 10 + 5);
        let tools = vec![ToolSchema::new(
            "rag.search",
            "find passages",
            json!({"type": "object"}),
        )];
        assert!(estimate_tools(&tools) > 0);
        let (whole, basis) = prompt_tokens(None, &messages, &tools);
        assert_eq!(basis, PromptBasis::Estimated);
        let raw = estimate_messages(&messages) + estimate_tools(&tools);
        assert_eq!(whole, raw + raw / 10);
    }

    #[test]
    fn a_measured_count_replaces_the_estimate_of_what_it_covered() {
        let mut messages = vec![ProviderMessage::user("x".repeat(3_000))];
        let measured = Measured {
            messages: 1,
            prompt_tokens: 700,
        };
        let (same, basis) = prompt_tokens(Some(measured), &messages, &[]);
        assert_eq!((same, basis), (700, PromptBasis::Measured));
        messages.push(ProviderMessage::tool_result("call_1", "y".repeat(30)));
        let (grown, _) = prompt_tokens(Some(measured), &messages, &[]);
        assert_eq!(grown, 700 + 15 + 1);
        // A count that does not fit this conversation is not trusted.
        let stale = Measured {
            messages: 5,
            prompt_tokens: 700,
        };
        assert_eq!(
            prompt_tokens(Some(stale), &messages, &[]).1,
            PromptBasis::Estimated
        );
    }

    #[test]
    fn a_result_too_large_for_the_room_is_shortened_on_a_character_boundary() {
        assert_eq!(fit_result("small", 100), None);
        assert_eq!(fit_result("ok", 0), None, "never replaced by a longer note");
        let large = "é".repeat(10_000);
        let fitted = fit_result(&large, 200).unwrap();
        assert!(fitted.contains("shortened this result"));
        assert!(fitted.contains("of 20000 bytes"));
        assert!(padded(estimate_text(&fitted)) <= 200 + NOTE_TOKENS);
        let nothing = fit_result(&large, 0).unwrap();
        assert!(nothing.starts_with("\n\n[Lightagent shortened"));
    }

    #[test]
    fn a_length_stop_is_attributed_only_from_the_providers_counts() {
        let usage = |prompt, completion| {
            Some(Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            })
        };
        assert_eq!(
            length_cause(Some(512), Some(8_192), usage(100, 512)),
            "output_allowance"
        );
        assert_eq!(
            length_cause(None, Some(8_192), usage(7_000, 1_191)),
            "context_window"
        );
        assert_eq!(
            length_cause(Some(4_096), Some(8_192), usage(7_000, 1_191)),
            "context_window",
            "a server clamp below the allowance is the window's doing"
        );
        assert_eq!(length_cause(None, None, usage(100, 100)), "unknown");
        assert_eq!(length_cause(Some(10), Some(100), None), "unknown");
    }

    #[test]
    fn a_routed_worker_keeps_its_own_window_and_the_leads_bounds() {
        let pool = OutputPool::new(4_000);
        let lead = GenerationBudget {
            context_window: window(8_192),
            max_output_tokens: Some(1_024),
            reserve_answer_tokens: 512,
            reasoning: Reasoning::Applied("low".into()),
            output_pool: Some(pool.clone()),
            max_continuations: 1,
        };
        // The worker's model: a larger window, a larger ceiling of its own, a
        // pool of its own from the same settings, and more continuations.
        let own = GenerationBudget {
            context_window: window(32_768),
            max_output_tokens: Some(4_096),
            reserve_answer_tokens: 1_024,
            reasoning: Reasoning::Applied("none".into()),
            output_pool: Some(OutputPool::new(4_000)),
            max_continuations: 3,
        };
        let worker = lead.for_routed_worker(own.clone());
        // Per-request capacity is the worker model's own, even when larger.
        assert_eq!(worker.context_window, window(32_768));
        assert_eq!(worker.reserve_answer_tokens, 1_024);
        assert_eq!(worker.reasoning, Reasoning::Applied("none".into()));
        // Never a larger allowance or more continuations than the lead's.
        assert_eq!(worker.max_output_tokens, Some(1_024));
        assert_eq!(worker.max_continuations, 1);
        // The lead's pool, shared: what the worker reserves the lead loses.
        let held = worker
            .output_pool
            .as_ref()
            .unwrap()
            .reserve(Some(1_000))
            .unwrap();
        assert_eq!(pool.available(), 3_000);
        drop(held);
        // A smaller ceiling of its own wins; with none, the lead's applies.
        let tighter = lead.for_routed_worker(GenerationBudget {
            max_output_tokens: Some(256),
            ..own.clone()
        });
        assert_eq!(tighter.max_output_tokens, Some(256));
        let unset = GenerationBudget::default().for_routed_worker(GenerationBudget {
            max_output_tokens: None,
            ..own
        });
        assert_eq!(unset.max_output_tokens, None);
        assert_eq!(unset.output_pool, None, "no pool of its own, ever");
        // An unknown window stays unknown: no guard, no continuation.
        let unknown = lead.for_routed_worker(GenerationBudget::default());
        assert_eq!(unknown.context_window, None);
        assert!(!unknown.guards_context());
        assert!(!unknown.may_continue_with(0));
        assert_eq!(unknown.max_output_tokens, Some(1_024));
    }
}
