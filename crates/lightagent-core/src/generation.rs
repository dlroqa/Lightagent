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
//! - The **output allowance** is what the operator asked for. It is sent as
//!   `max_tokens`, never above the window, and the server clamps it again to
//!   the room its own count leaves. Reasoning and the visible answer share it:
//!   a model that thinks first spends the same budget, and nothing here can
//!   set part of it aside for the answer.
//! - A **reasoning effort** is sent only when the operator declared it
//!   supported for the model. It is a label the model's template interprets,
//!   not a token budget.
//!
//! What the loop does with a known window is keep room for the answer: a tool
//! result that would crowd it out is shortened in the conversation the model
//! sees (the run's events keep it whole), and once the room left is below
//! [`GenerationBudget::reserve_answer_tokens`] no more tools are offered and
//! the model answers from what it has.
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
    /// The output allowance to request per turn. `None` sends no `max_tokens`
    /// and leaves the provider's default (Lightweight: all the room the
    /// prompt leaves in the window).
    pub max_output_tokens: Option<u32>,
    /// Room kept for the answer once tools have run. Zero turns the context
    /// guard off. Used only with a known window.
    pub reserve_answer_tokens: u32,
    /// What to ask of the model's reasoning.
    pub reasoning: Reasoning,
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
/// below this, a match is as likely a coincidence as a repeat.
const MIN_OVERLAP: usize = 16;

/// How much of the text so far a continuation is compared with.
pub(crate) const OVERLAP_TAIL: usize = 512;

impl GenerationBudget {
    /// Whether the budget changes anything about a run. An inactive budget
    /// leaves requests, conversations and events exactly as before.
    pub fn is_active(&self) -> bool {
        self.context_window.is_some()
            || self.max_output_tokens.is_some()
            || self.reasoning != Reasoning::Default
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
    /// larger than this run's; the window and the reasoning setting carry
    /// over only for the same model, because both were established for it.
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
            max_continuations: self.max_continuations,
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
/// continue, a model sometimes repeats its last words first. A repeat
/// shorter than [`MIN_OVERLAP`] bytes is kept, and a model that starts its
/// whole answer again is not detected.
pub(crate) fn strip_overlap<'a>(tail: &str, next: &'a str) -> &'a str {
    let longest = tail.len().min(next.len());
    for length in (MIN_OVERLAP..=longest).rev() {
        if next.is_char_boundary(length) && tail.ends_with(&next[..length]) {
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
    /// The `max_tokens` sent, if any. The server may clamp it further.
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
    /// Continuations taken so far in this run, this turn included.
    pub continuations: u32,
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
            max_continuations: 1,
        };
        assert_eq!(lead.for_worker(true), lead);
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
            strip_overlap(so_far, "agree that the bridge opened in 1932, after"),
            " 1932, after"
        );
        assert_eq!(strip_overlap(so_far, " 1932, after"), " 1932, after");
        assert_eq!(
            strip_overlap(so_far, "opened in 1932"),
            "opened in 1932",
            "a short repeat may be a coincidence and is kept"
        );
        assert_eq!(strip_overlap("", "anything"), "anything");
        let accented = "é".repeat(400);
        assert_eq!(tail(&accented).len(), OVERLAP_TAIL);
        assert_eq!(tail("short"), "short");
        let restated = format!("{}é and more", "é".repeat(20));
        assert_eq!(strip_overlap(tail(&accented), &restated), " and more");
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
}
