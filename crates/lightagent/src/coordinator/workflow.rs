//! The workflow policy: which existing capability an accepted classification
//! may point a run at.
//!
//! The policy is a pure inspection. It reads the tools the run actually has,
//! asks the run's own approval policy what it would do with each (without
//! raising, granting or remembering anything), and checks the run's limits.
//! Eligibility is not authorization: an eligible workflow may only add one
//! fixed advisory instruction, in `active` mode with `workflow_guidance` on.
//! The registry, the approval policy, the limits and the provider request are
//! never changed. Anything unsupported, unconfigured, refused, uncertain or
//! over budget keeps the run exactly as it would be without Jev.

use std::time::SystemTime;

use lightagent_core::{
    ApprovalNeed, ApprovalRequest, Config, JevClassificationMode, PolicyEngine, RiskClass,
    RunLimits,
};
use lightagent_tools::ToolRegistry;

use super::classify::{ClassificationReport, Outcome, TaskCategory};

/// A workflow an accepted category maps to, one-to-one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Workflow {
    Conversation,
    Research,
    Coding,
    Analysis,
    Automation,
    MultiStep,
}

impl Workflow {
    fn from_category(category: TaskCategory) -> Self {
        match category {
            TaskCategory::Conversation => Self::Conversation,
            TaskCategory::Research => Self::Research,
            TaskCategory::Coding => Self::Coding,
            TaskCategory::Analysis => Self::Analysis,
            TaskCategory::Automation => Self::Automation,
            TaskCategory::MultiStep => Self::MultiStep,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Research => "research",
            Self::Coding => "coding",
            Self::Analysis => "analysis",
            Self::Automation => "automation",
            Self::MultiStep => "multi_step",
        }
    }

    /// The existing Lightagent capability this workflow relies on.
    pub(crate) fn capability(self) -> &'static str {
        match self {
            Self::Conversation => "agent_loop",
            Self::Research => "retrieval_tools",
            Self::Coding => "workspace_tools",
            Self::Analysis => "none",
            Self::Automation => "action_tools",
            Self::MultiStep => "agent_delegate",
        }
    }
}

/// Why a workflow may or may not shape the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Eligibility {
    /// Supported, present, permitted and within budget.
    Eligible,
    /// The ordinary agent loop already serves it; nothing to add.
    DefaultBehavior,
    /// Present and permitted, but only ever advisory (external side effects).
    AdvisoryOnly,
    /// Lightagent has no dedicated capability for it.
    Unsupported,
    /// The capability exists but is not configured for this run.
    NotConfigured,
    /// Every supporting tool is refused by the run's approval policy.
    Unauthorized,
    /// The run's limits leave too little room for the workflow.
    InsufficientBudget,
    /// The category did not clear its confidence threshold.
    Uncertain,
    /// There is no valid classification.
    NotClassified,
    /// The run was cancelled before the workflow was decided.
    Cancelled,
}

impl Eligibility {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::DefaultBehavior => "default_behavior",
            Self::AdvisoryOnly => "advisory_only",
            Self::Unsupported => "unsupported",
            Self::NotConfigured => "not_configured",
            Self::Unauthorized => "unauthorized",
            Self::InsufficientBudget => "insufficient_budget",
            Self::Uncertain => "uncertain",
            Self::NotClassified => "not_classified",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The typed outcome of the workflow policy for one run.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkflowDecision {
    pub workflow: Option<Workflow>,
    pub eligibility: Eligibility,
    /// Supporting tools present in the run and not refused by its policy.
    pub available_tools: u32,
    /// Whether any of them would still ask for approval.
    pub approval_required: bool,
    /// The fixed instruction an eligible workflow offers. Never generated.
    pub guidance: Option<&'static str>,
    /// Whether the guidance is added to the run (otherwise advisory only).
    pub applied: bool,
}

impl WorkflowDecision {
    fn without(workflow: Option<Workflow>, eligibility: Eligibility) -> Self {
        Self {
            workflow,
            eligibility,
            available_tools: 0,
            approval_required: false,
            guidance: None,
            applied: false,
        }
    }
}

/// What the policy inspects. Every field is read-only.
pub(crate) struct WorkflowInputs<'a> {
    pub report: &'a ClassificationReport,
    pub registry: &'a ToolRegistry,
    pub policy: &'a PolicyEngine,
    pub limits: &'a RunLimits,
    pub config: &'a Config,
    pub cancelled: bool,
}

const RESEARCH_TOOLS: &[&str] = &["rag.realtime", "web.search", "web.fetch", "rag.search"];
/// Coding guidance asks the model to read first, so a read tool must be usable.
const CODING_READ_TOOLS: &[&str] = &["fs.read", "fs.list"];
const CODING_TOOLS: &[&str] = &[
    "fs.read",
    "fs.list",
    "fs.write",
    "terminal.run",
    "open_terminal.run",
];
const DELEGATE_TOOL: &str = "agent.delegate";

pub(crate) const RESEARCH_GUIDANCE: &str = "# Task assessment (advisory)\n\
    This request appears to need current or external information. Where it helps, gather \
    evidence with the retrieval tools you already have before answering, and cite the sources \
    you used. This assessment grants no additional permissions.";
pub(crate) const CODING_GUIDANCE: &str = "# Task assessment (advisory)\n\
    This request appears to involve code or configuration in the workspace. Inspect the \
    relevant files with the workspace tools you already have before proposing changes. Running \
    commands and changing files remain subject to the usual approval.";
pub(crate) const MULTI_STEP_GUIDANCE: &str = "# Task assessment (advisory)\n\
    This request appears to have several distinct parts. Outline the steps before acting, and \
    delegate a part only where delegation is already permitted and clearly helps. Delegation \
    limits and approvals still apply.";

/// Decide the workflow for one run. Pure: nothing it reads is changed.
pub(crate) fn evaluate(inputs: &WorkflowInputs<'_>) -> WorkflowDecision {
    if inputs.cancelled {
        return WorkflowDecision::without(None, Eligibility::Cancelled);
    }
    let Outcome::Classified(classification) = &inputs.report.outcome else {
        return WorkflowDecision::without(None, Eligibility::NotClassified);
    };
    let workflow = Workflow::from_category(classification.category);
    if !classification.accepted() {
        return WorkflowDecision::without(Some(workflow), Eligibility::Uncertain);
    }
    let (supporting, guidance, minimum): (Vec<String>, Option<&'static str>, (u32, u32)) =
        match workflow {
            Workflow::Conversation => {
                return WorkflowDecision::without(Some(workflow), Eligibility::DefaultBehavior);
            }
            Workflow::Analysis => {
                return WorkflowDecision::without(Some(workflow), Eligibility::Unsupported);
            }
            Workflow::Research => (named(RESEARCH_TOOLS), Some(RESEARCH_GUIDANCE), (2, 1)),
            Workflow::Coding => (named(CODING_TOOLS), Some(CODING_GUIDANCE), (2, 1)),
            Workflow::Automation => (action_tools(inputs.registry), None, (2, 1)),
            Workflow::MultiStep => {
                if !delegation_bounded(inputs.config) {
                    return WorkflowDecision::without(Some(workflow), Eligibility::NotConfigured);
                }
                (
                    vec![DELEGATE_TOOL.to_owned()],
                    Some(MULTI_STEP_GUIDANCE),
                    (3, 2),
                )
            }
        };
    let present: Vec<&String> = supporting
        .iter()
        .filter(|name| inputs.registry.contains(name))
        .collect();
    if present.is_empty() {
        return WorkflowDecision::without(Some(workflow), Eligibility::NotConfigured);
    }
    let mut usable = Vec::new();
    let mut approval_required = false;
    for name in present {
        match need(inputs.registry, inputs.policy, name) {
            Some(ApprovalNeed::AutoApprove) => usable.push(name.as_str()),
            Some(ApprovalNeed::Require(_)) => {
                approval_required = true;
                usable.push(name.as_str());
            }
            Some(ApprovalNeed::Deny(_)) | None => {}
        }
    }
    let usable_reads =
        workflow != Workflow::Coding || usable.iter().any(|name| CODING_READ_TOOLS.contains(name));
    if usable.is_empty() || !usable_reads {
        return WorkflowDecision::without(Some(workflow), Eligibility::Unauthorized);
    }
    let (min_turns, min_tool_calls) = minimum;
    if inputs.limits.max_turns < min_turns || inputs.limits.max_tool_calls < min_tool_calls {
        return WorkflowDecision::without(Some(workflow), Eligibility::InsufficientBudget);
    }
    let (eligibility, guidance) = match workflow {
        // External side effects stay the model's and the user's decision.
        Workflow::Automation => (Eligibility::AdvisoryOnly, None),
        _ => (Eligibility::Eligible, guidance),
    };
    let classification_settings = &inputs.config.platform.jev.classification;
    let applied = guidance.is_some()
        && inputs.report.mode == JevClassificationMode::Active
        && classification_settings.workflow_guidance;
    WorkflowDecision {
        workflow: Some(workflow),
        eligibility,
        available_tools: u32::try_from(usable.len()).unwrap_or(u32::MAX),
        approval_required,
        guidance,
        applied,
    }
}

fn named(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Tools that change something: the run's mutating and executable tools,
/// built-in or MCP.
fn action_tools(registry: &ToolRegistry) -> Vec<String> {
    registry
        .names()
        .into_iter()
        .filter(|name| {
            registry.get(name).is_some_and(|tool| {
                matches!(
                    tool.definition().risk,
                    RiskClass::Mutating | RiskClass::Executable
                )
            })
        })
        .collect()
}

/// Delegation is bounded by depth, fan-out and time: the legacy path is one
/// level deep, and autonomous subagents need a positive depth and fan-out.
fn delegation_bounded(config: &Config) -> bool {
    !config.subagents.enabled
        || (config.subagents.max_depth >= 1 && config.subagents.max_children_per_agent >= 1)
}

/// What the run's policy would decide for `name`, without raising anything.
fn need(registry: &ToolRegistry, policy: &PolicyEngine, name: &str) -> Option<ApprovalNeed> {
    let definition = registry.get(name)?.definition();
    let request = ApprovalRequest::new(
        &definition.name,
        definition.risk,
        definition.scopes.clone(),
        "{}",
    );
    Some(policy.evaluate(&request, SystemTime::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::classify::{Complexity, TaskClassification};
    use lightagent_core::permissions::ApprovalPolicy;
    use std::time::Duration;

    fn report(
        mode: JevClassificationMode,
        category: TaskCategory,
        confidence: f64,
    ) -> ClassificationReport {
        ClassificationReport {
            mode,
            outcome: Outcome::Classified(TaskClassification {
                category,
                category_confidence: confidence,
                planning_probability: 0.5,
                retrieval_probability: 0.5,
                tools_probability: 0.5,
                complexity: Complexity::Moderate,
                complexity_confidence: 0.6,
                model_version: Some("jev-1.13.0".to_owned()),
                usage: None,
                threshold: 0.9,
            }),
            invoked: true,
            latency: Some(Duration::from_millis(5)),
        }
    }

    fn registry(names: &[&str]) -> ToolRegistry {
        ToolRegistry::builtin().scoped(
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    fn guided() -> Config {
        let mut config = Config::default();
        config.platform.jev.classification.workflow_guidance = true;
        config
    }

    struct Case {
        mode: JevClassificationMode,
        category: TaskCategory,
        confidence: f64,
        tools: &'static [&'static str],
        policy: ApprovalPolicy,
        limits: RunLimits,
        config: Config,
        cancelled: bool,
    }

    impl Case {
        fn new(category: TaskCategory, tools: &'static [&'static str]) -> Self {
            Self {
                mode: JevClassificationMode::Active,
                category,
                confidence: 0.95,
                tools,
                policy: ApprovalPolicy::balanced(),
                limits: RunLimits::default(),
                config: guided(),
                cancelled: false,
            }
        }

        fn decide(&self) -> WorkflowDecision {
            let report = report(self.mode, self.category, self.confidence);
            let registry = registry(self.tools);
            let policy = PolicyEngine::new(self.policy.clone());
            evaluate(&WorkflowInputs {
                report: &report,
                registry: &registry,
                policy: &policy,
                limits: &self.limits,
                config: &self.config,
                cancelled: self.cancelled,
            })
        }
    }

    #[test]
    fn active_mode_with_guidance_applies_only_eligible_fixed_guidance() {
        let research = Case::new(TaskCategory::Research, &["web.search", "web.fetch"]).decide();
        assert_eq!(research.eligibility, Eligibility::Eligible);
        assert!(research.applied);
        assert_eq!(research.guidance, Some(RESEARCH_GUIDANCE));
        assert_eq!(research.available_tools, 2);
        let coding = Case::new(TaskCategory::Coding, &["fs.read", "terminal.run"]).decide();
        assert_eq!(coding.eligibility, Eligibility::Eligible);
        assert!(coding.applied);
        let steps = Case::new(TaskCategory::MultiStep, &["agent.delegate"]).decide();
        assert_eq!(steps.eligibility, Eligibility::Eligible);
        assert!(steps.applied);
    }

    #[test]
    fn shadow_mode_and_the_default_setting_never_apply_guidance() {
        let mut shadow = Case::new(TaskCategory::Research, &["web.search"]);
        shadow.mode = JevClassificationMode::Shadow;
        let decision = shadow.decide();
        assert_eq!(
            decision.eligibility,
            Eligibility::Eligible,
            "evaluated in shadow"
        );
        assert!(!decision.applied, "but never applied");
        let mut unguided = Case::new(TaskCategory::Research, &["web.search"]);
        unguided.config = Config::default();
        assert!(
            !unguided.decide().applied,
            "workflow_guidance is off by default"
        );
    }

    #[test]
    fn research_cannot_reach_retrieval_the_run_does_not_have_or_may_not_use() {
        let absent = Case::new(TaskCategory::Research, &["datetime.now", "fs.read"]).decide();
        assert_eq!(absent.eligibility, Eligibility::NotConfigured);
        assert!(!absent.applied && absent.guidance.is_none());
        let mut refused = Case::new(TaskCategory::Research, &["web.search", "web.fetch"]);
        refused.policy.deny.push(RiskClass::External);
        let decision = refused.decide();
        assert_eq!(decision.eligibility, Eligibility::Unauthorized);
        assert!(!decision.applied);
    }

    #[test]
    fn coding_reports_approval_and_never_relaxes_it() {
        let policy = PolicyEngine::new(ApprovalPolicy::balanced());
        let registry = registry(&["fs.read", "terminal.run"]);
        let before = need(&registry, &policy, "terminal.run");
        let mut case = Case::new(TaskCategory::Coding, &["fs.read", "terminal.run"]);
        case.policy = ApprovalPolicy::balanced();
        let decision = case.decide();
        assert!(decision.approval_required, "terminal.run still asks");
        assert!(
            decision
                .guidance
                .unwrap()
                .contains("remain subject to the usual approval")
        );
        assert!(matches!(before, Some(ApprovalNeed::Require(_))));
        assert!(
            matches!(
                need(&registry, &policy, "terminal.run"),
                Some(ApprovalNeed::Require(_))
            ),
            "deciding the workflow left the policy's answer unchanged"
        );
        // Without a read tool the coding guidance cannot be followed.
        let write_only = Case::new(TaskCategory::Coding, &["terminal.run"]).decide();
        assert_eq!(write_only.eligibility, Eligibility::Unauthorized);
    }

    #[test]
    fn automation_is_only_ever_advisory() {
        let decision = Case::new(TaskCategory::Automation, &["fs.write", "terminal.run"]).decide();
        assert_eq!(decision.eligibility, Eligibility::AdvisoryOnly);
        assert!(!decision.applied);
        assert!(decision.guidance.is_none());
        assert!(decision.approval_required, "every action still asks");
        let none = Case::new(TaskCategory::Automation, &["fs.read", "web.search"]).decide();
        assert_eq!(none.eligibility, Eligibility::NotConfigured);
    }

    #[test]
    fn multi_step_needs_bounded_delegation_and_budget() {
        let mut unbounded = Case::new(TaskCategory::MultiStep, &["agent.delegate"]);
        unbounded.config.subagents.enabled = true;
        unbounded.config.subagents.max_children_per_agent = 0;
        assert_eq!(unbounded.decide().eligibility, Eligibility::NotConfigured);
        let absent = Case::new(TaskCategory::MultiStep, &["fs.read"]).decide();
        assert_eq!(absent.eligibility, Eligibility::NotConfigured);
        let mut tight = Case::new(TaskCategory::MultiStep, &["agent.delegate"]);
        tight.limits.max_turns = 2;
        assert_eq!(tight.decide().eligibility, Eligibility::InsufficientBudget);
        let config = Config::default();
        let before = (
            config.subagents.max_depth,
            config.subagents.max_children_per_agent,
        );
        let _ = Case::new(TaskCategory::MultiStep, &["agent.delegate"]).decide();
        assert_eq!(
            before,
            (
                Config::default().subagents.max_depth,
                Config::default().subagents.max_children_per_agent
            ),
            "no limit is raised"
        );
    }

    #[test]
    fn unsupported_uncertain_and_failed_classifications_fall_back() {
        assert_eq!(
            Case::new(TaskCategory::Analysis, &["fs.read"])
                .decide()
                .eligibility,
            Eligibility::Unsupported
        );
        assert_eq!(
            Case::new(TaskCategory::Conversation, &[])
                .decide()
                .eligibility,
            Eligibility::DefaultBehavior
        );
        let mut unsure = Case::new(TaskCategory::Research, &["web.search"]);
        unsure.confidence = 0.5;
        let decision = unsure.decide();
        assert_eq!(decision.eligibility, Eligibility::Uncertain);
        assert!(!decision.applied);
        let failed = ClassificationReport {
            mode: JevClassificationMode::Active,
            outcome: Outcome::Failed("unauthorized"),
            invoked: true,
            latency: None,
        };
        let registry = registry(&["web.search"]);
        let policy = PolicyEngine::new(ApprovalPolicy::permissive());
        let limits = RunLimits::default();
        let config = guided();
        let decision = evaluate(&WorkflowInputs {
            report: &failed,
            registry: &registry,
            policy: &policy,
            limits: &limits,
            config: &config,
            cancelled: false,
        });
        assert_eq!(decision.eligibility, Eligibility::NotClassified);
        assert!(!decision.applied, "a Jev failure never applies anything");
    }

    #[test]
    fn cancellation_and_budgets_stop_the_workflow() {
        let mut cancelled = Case::new(TaskCategory::Research, &["web.search"]);
        cancelled.cancelled = true;
        assert_eq!(cancelled.decide().eligibility, Eligibility::Cancelled);
        let mut no_tools = Case::new(TaskCategory::Research, &["web.search"]);
        no_tools.limits.max_tool_calls = 0;
        assert_eq!(
            no_tools.decide().eligibility,
            Eligibility::InsufficientBudget
        );
    }

    #[test]
    fn the_registry_is_never_changed() {
        let registry = registry(&["web.search", "web.fetch"]);
        let before = registry.names();
        let report = report(JevClassificationMode::Active, TaskCategory::Research, 0.99);
        let policy = PolicyEngine::new(ApprovalPolicy::balanced());
        let limits = RunLimits::default();
        let config = guided();
        let _ = evaluate(&WorkflowInputs {
            report: &report,
            registry: &registry,
            policy: &policy,
            limits: &limits,
            config: &config,
            cancelled: false,
        });
        assert_eq!(registry.names(), before);
    }
}
