//! The canonical run event stream.
//!
//! Every observable thing a run does is one of these. The enum is
//! `#[non_exhaustive]`: later slices add variants (approvals, delegation) and a
//! consumer must already expect not to have seen them all. The variant set here
//! is the master-plan event vocabulary, named for what happened rather than for
//! the wire frame that carried it.

use crate::ids::RunId;
use crate::invoker::{ToolCall, ToolOutcome, ToolProgress};
use crate::permissions::ApprovalId;
use crate::provider::Usage;

/// Why a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The model produced a final answer.
    EndTurn,
    /// The run hit its turn budget. When tool results were waiting, the last
    /// turn was a tool-less answer from them.
    MaxTurns,
    /// The run hit its tool-call budget.
    MaxToolCalls,
    /// The run repeated one identical tool call past its budget — a loop.
    RepeatedToolCalls,
    /// The run exceeded its wall-clock budget.
    WallClockExceeded,
    /// The model's answer was cut off at its output-token limit.
    MaxTokens,
    /// The provider stream ended without saying the turn was finished, so the
    /// answer may be cut short. What arrived is kept.
    Incomplete,
    /// The run was cancelled.
    Cancelled,
    /// The run failed — see the preceding [`AgentEvent::Error`].
    Error,
}

impl StopReason {
    /// Whether the model ended its own answer. Every other reason means the
    /// answer, if any, was cut short or written under a budget.
    pub fn is_complete(self) -> bool {
        self == Self::EndTurn
    }

    /// A short sentence for a person, for every reason but a normal end.
    pub fn explanation(self) -> Option<&'static str> {
        match self {
            Self::EndTurn => None,
            Self::MaxTurns => Some("the run reached its turn limit"),
            Self::MaxToolCalls => Some("the run reached its tool-call limit"),
            Self::RepeatedToolCalls => Some("the run stopped repeating the same tool call"),
            Self::WallClockExceeded => Some("the run reached its time limit"),
            Self::MaxTokens => Some("the answer was cut off at the model's output limit"),
            Self::Incomplete => Some("the model stream ended early; the answer may be incomplete"),
            Self::Cancelled => Some("the run was cancelled"),
            Self::Error => Some("the run failed"),
        }
    }
}

/// One thing that happened during a run.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// The run began. `parent` names the orchestrator run for a delegated
    /// child (Slice 3/4); `None` for a top-level run.
    RunStarted { run: RunId, parent: Option<RunId> },
    /// A fragment of the model's reasoning trace.
    Reasoning { text: String },
    /// A fragment of the model's visible answer.
    Content { text: String },
    /// The advisory inference router selected a model for this run. This is
    /// informational only: it does not grant tools or alter permissions.
    RouteSelected {
        source: String,
        model: String,
        kept_default: bool,
        /// The run profile's model-routing permission when routing checked
        /// it: `allowed`, `compatibility` or `denied`. Fixed vocabulary only.
        permission: Option<String>,
        /// Why the configured model was kept, as a stable code (for example
        /// `profile_not_permitted`). Never message text.
        fallback: Option<String>,
    },
    /// The pre-run task classification finished, was skipped, or fell back.
    /// Sanitized and advisory: it carries no message text, key or tool output,
    /// and it never grants a tool, an approval or a model. Emitted only when
    /// classification is not `off`.
    TaskClassified {
        diagnostics: Box<ClassificationDiagnostics>,
    },
    /// The model asked for a tool.
    ToolCallRequested { call: ToolCall },
    /// A requested tool began executing.
    ToolCallStarted { id: String, name: String },
    /// A running tool reported how far it has got (a retrieval stage, say).
    /// Progress only: the result still arrives as
    /// [`ToolCallCompleted`](AgentEvent::ToolCallCompleted).
    ToolProgress { id: String, progress: ToolProgress },
    /// A tool finished, with its result.
    ToolCallCompleted { id: String, outcome: ToolOutcome },
    /// A tool call is waiting on a human approval decision. `approval_id` is
    /// the security-decision id that must be returned in an
    /// [`ApprovalDecision`](crate::permissions::ApprovalDecision); `tool_call_id`
    /// stays the model-protocol id used by the later tool events.
    AwaitingApproval {
        approval_id: ApprovalId,
        tool_call_id: String,
        name: String,
    },
    /// One model turn completed, with its token accounting when known.
    TurnCompleted { usage: Option<Usage> },
    /// The wall-clock budget ran out while tool results the model has not read
    /// yet were waiting, and the run paused rather than ended (only under
    /// [`WallClockPolicy::Pause`](crate::loop_::WallClockPolicy::Pause)). The
    /// run is not over: a continuation picks it up where it stopped.
    WallClockPaused { elapsed_secs: u64 },
    /// The run reached a terminal state.
    RunCompleted { reason: StopReason },
    /// Something went wrong. Followed by a terminal
    /// [`RunCompleted`](AgentEvent::RunCompleted) with
    /// [`StopReason::Error`].
    Error { message: String },
}

/// The privacy-safe record of one run's classification preflight.
///
/// Every string is either a fixed vocabulary value or the versioned Jev model
/// id; nothing derived from the user's text is included. Schema 2 added the
/// workflow and routing-input fields; every schema-1 field is unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassificationDiagnostics {
    /// The schema of this record, so consumers can evolve with it.
    pub schema: u32,
    /// `preflight` for the automatic classification, `explicit` for a call
    /// of the `jev.classify_task` tool.
    pub trigger: String,
    /// `shadow` or `active`.
    pub mode: String,
    /// `classified`, `low_confidence`, `skipped` or `failed`.
    pub outcome: String,
    /// `jev` when Jev answered; absent otherwise.
    pub source: Option<String>,
    /// The primary task category, when Jev answered validly.
    pub category: Option<String>,
    /// TypeSafe's confidence for the category choice.
    pub category_confidence: Option<f64>,
    /// Whether any reported decision carried a TypeSafe confidence.
    pub confidence_available: bool,
    /// The threshold applied to `category_confidence`.
    pub threshold: Option<f32>,
    /// Yes-probabilities of the independent noul questions. They carry no
    /// confidence of their own and none is derived.
    pub planning_probability: Option<f64>,
    pub retrieval_probability: Option<f64>,
    pub tools_probability: Option<f64>,
    /// `simple`, `moderate` or `complex`, with its own confidence.
    pub complexity: Option<String>,
    pub complexity_confidence: Option<f64>,
    /// The versioned Jev model that answered.
    pub model_version: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Jev requests made in this run's preflight, routing included.
    pub jev_calls: u32,
    /// Duration of the classification request, when one was made.
    pub classification_latency_ms: Option<u64>,
    /// Duration of the whole preflight (routing and classification run
    /// concurrently).
    pub preflight_latency_ms: u64,
    /// Why the classification was not applied, as a stable code.
    pub fallback_reason: Option<String>,
    /// Whether the classification changed anything about the run.
    pub influenced: bool,
    /// Whether the category cleared its confidence threshold.
    pub accepted: Option<bool>,
    /// The workflow the category maps to.
    pub workflow: Option<String>,
    /// `eligible`, `default_behavior`, `advisory_only`, `unsupported`,
    /// `not_configured`, `unauthorized`, `insufficient_budget`, `uncertain`,
    /// `not_classified` or `cancelled`.
    pub workflow_eligibility: Option<String>,
    /// The existing capability the workflow relies on.
    pub workflow_capability: Option<String>,
    /// Whether the workflow's guidance was applied (otherwise advisory only).
    pub workflow_applied: bool,
    /// Supporting tools present in the run and not refused by its policy.
    pub available_tools: Option<u32>,
    /// Whether any supporting tool would still ask for approval.
    pub approval_required: Option<bool>,
    /// Time spent deciding the workflow, in microseconds.
    pub workflow_selection_us: Option<u64>,
    /// The routing-input policy used when routing consulted Jev.
    pub routing_input: Option<String>,
}
