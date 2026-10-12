//! The pre-run decision boundary for every Jev-assisted decision.
//!
//! Each API, ACP and terminal turn passes through [`preflight`] exactly once,
//! before the agent loop starts. It runs two independent decisions:
//!
//! * **Model routing** (the original, configured by `platform.jev` allowlists):
//!   may replace the configured model with an allowlisted one. It never
//!   replaces Lightweight's `Auto`, which leaves the route to the provider.
//! * **Task classification** (`platform.jev.classification`): describes what
//!   the task requires. It never chooses a model, tool, approval or profile.
//!   The typed workflow policy ([`workflow`]) decides whether an accepted
//!   category maps to an existing, configured and permitted capability; only
//!   then, in `active` mode with `workflow_guidance` on, may one fixed
//!   advisory instruction be added.
//!
//! They are separate Jev requests because TypeSafe evaluates every question
//! against one shared `state`: routing's state is the original bounded message,
//! classification's is a sanitized description, and merging them would either
//! widen classification's disclosure or change routing's inputs. They run
//! concurrently, so the preflight takes as long as the slower of the two, and
//! both stop the moment the run is cancelled.

#[cfg(test)]
mod adversarial;
mod classify;
#[cfg(test)]
pub(crate) mod evaluation;
pub(crate) mod sanitize;
mod state;
mod tool;
pub(crate) mod workflow;

use std::time::{Duration, Instant};

use lightagent_core::{
    AgentProfile, ClassificationDiagnostics, Config, JevClassificationMode, ProfileStore,
};
use tokio_util::sync::CancellationToken;

pub(crate) use classify::{ClassificationReport, Outcome};
pub(crate) use state::{JevBudget, JevState, Trigger};
pub(crate) use tool::{add_explicit_tool, status};

/// Jev stubs shared with the run-level tests in `serve`.
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use super::classify::tests::{answer, jev_stub};
}

/// Read one stub request; shared with the run-level tests in `serve`.
#[cfg(test)]
pub(crate) use classify::tests::read_request as classify_read_request;

/// What a turn asks the preflight to decide about.
pub(crate) struct PreflightRequest<'a> {
    pub config: &'a Config,
    pub store: &'a ProfileStore,
    pub profile: &'a AgentProfile,
    /// The current user message only.
    pub message: &'a str,
    /// A model the caller chose for this run; routing never overrides it.
    pub explicit_model: Option<&'a str>,
    /// The message carries a file's contents (terminal drops).
    pub attachment: bool,
}

/// The preflight's decisions.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Preflight {
    pub model: String,
    pub base_url: String,
    /// `(model, kept_default)` for the existing `routing.selected` event, set
    /// exactly as before: when Jev is enabled and no model was chosen.
    pub routing_status: Option<(String, bool)>,
    /// `None` when classification is off.
    pub classification: Option<ClassificationReport>,
    /// Jev requests made, routing and classification together.
    pub jev_calls: u32,
    pub latency: Duration,
    /// The routing-input policy applied, when routing prepared an input.
    pub routing_input: Option<&'static str>,
}

/// Decide the turn's inference route and, when enabled, classify the task.
pub(crate) async fn preflight(
    request: PreflightRequest<'_>,
    cancel: &CancellationToken,
) -> Preflight {
    let started = Instant::now();
    let PreflightRequest {
        config,
        store,
        profile,
        message,
        explicit_model,
        attachment,
    } = request;
    let base_url = profile
        .routing
        .base_url
        .clone()
        .unwrap_or_else(|| config.inference.base_url.clone());
    let default_model = explicit_model
        .map(str::to_owned)
        .unwrap_or_else(|| crate::chat::configured_model(&profile.routing.model, config));
    let report_jev = config.platform.jev.endpoint.enabled && explicit_model.is_none();
    let routing = crate::jev::route_for_turn(
        &config.platform.jev,
        message,
        default_model.clone(),
        explicit_model.is_some(),
        attachment,
        cancel,
    );
    let classification =
        classify::classify(config, profile.id.as_str(), message, attachment, cancel);
    let (report, classification) = tokio::join!(routing, classification);
    let crate::jev::RouteReport {
        decision: route,
        invoked: route_invoked,
        input: routing_input,
        ..
    } = report;
    let routed_profile = route
        .profile
        .as_ref()
        .and_then(|name| crate::chat::resolve_profile(store, config, Some(name.clone())).ok());
    let (model, base_url) = crate::jev::inference_route(
        route,
        routed_profile.as_ref(),
        config,
        default_model.clone(),
        base_url,
    );
    let routing_status = report_jev.then(|| (model.clone(), model == default_model));
    let jev_calls = u32::from(route_invoked)
        + classification
            .as_ref()
            .map_or(0, |report| u32::from(report.invoked));
    Preflight {
        model,
        base_url,
        routing_status,
        classification,
        jev_calls,
        latency: started.elapsed(),
        routing_input,
    }
}

/// The privacy-safe record of this preflight's classification.
pub(crate) fn diagnostics(
    preflight: &Preflight,
    influenced: bool,
) -> Option<ClassificationDiagnostics> {
    let report = preflight.classification.as_ref()?;
    Some(build_diagnostics(
        Trigger::Preflight,
        report.mode,
        &report.outcome,
        report.latency,
        preflight.jev_calls,
        preflight.latency,
        influenced,
    ))
}

/// The record shared by the preflight and the explicit tool: fixed vocabulary,
/// numbers and the answering model version only.
pub(crate) fn build_diagnostics(
    trigger: Trigger,
    mode: JevClassificationMode,
    outcome: &Outcome,
    classification_latency: Option<Duration>,
    jev_calls: u32,
    total_latency: Duration,
    influenced: bool,
) -> ClassificationDiagnostics {
    let mut diagnostics = ClassificationDiagnostics {
        schema: 2,
        trigger: trigger.as_str().to_owned(),
        mode: mode.as_str().to_owned(),
        outcome: String::new(),
        source: None,
        category: None,
        category_confidence: None,
        confidence_available: false,
        threshold: None,
        planning_probability: None,
        retrieval_probability: None,
        tools_probability: None,
        complexity: None,
        complexity_confidence: None,
        model_version: None,
        input_tokens: None,
        output_tokens: None,
        jev_calls,
        classification_latency_ms: classification_latency.map(state::millis),
        preflight_latency_ms: state::millis(total_latency),
        fallback_reason: None,
        influenced,
        ..ClassificationDiagnostics::default()
    };
    match outcome {
        Outcome::Classified(classification) => {
            let accepted = classification.accepted();
            diagnostics.outcome = if accepted {
                "classified"
            } else {
                "low_confidence"
            }
            .to_owned();
            diagnostics.fallback_reason = (!accepted).then(|| "low_confidence".to_owned());
            diagnostics.accepted = Some(accepted);
            diagnostics.source = Some("jev".to_owned());
            diagnostics.category = Some(classification.category.as_str().to_owned());
            diagnostics.category_confidence = Some(classification.category_confidence);
            diagnostics.confidence_available = true;
            diagnostics.threshold = Some(classification.threshold);
            diagnostics.planning_probability = Some(classification.planning_probability);
            diagnostics.retrieval_probability = Some(classification.retrieval_probability);
            diagnostics.tools_probability = Some(classification.tools_probability);
            diagnostics.complexity = Some(classification.complexity.as_str().to_owned());
            diagnostics.complexity_confidence = Some(classification.complexity_confidence);
            diagnostics.model_version = classification.model_version.clone();
            diagnostics.input_tokens = classification.usage.and_then(|usage| usage.input_tokens);
            diagnostics.output_tokens = classification.usage.and_then(|usage| usage.output_tokens);
        }
        Outcome::Skipped(reason) => {
            diagnostics.outcome = "skipped".to_owned();
            diagnostics.fallback_reason = Some((*reason).to_owned());
        }
        Outcome::Failed(reason) => {
            diagnostics.outcome = "failed".to_owned();
            diagnostics.fallback_reason = Some((*reason).to_owned());
        }
    }
    diagnostics
}

/// Add the workflow decision and routing input to a classification record.
pub(crate) fn annotate(
    diagnostics: &mut ClassificationDiagnostics,
    decision: &workflow::WorkflowDecision,
    selection: Duration,
    routing_input: Option<&str>,
) {
    diagnostics.workflow = decision
        .workflow
        .map(|workflow| workflow.as_str().to_owned());
    diagnostics.workflow_capability = decision
        .workflow
        .map(|workflow| workflow.capability().to_owned());
    diagnostics.workflow_eligibility = Some(decision.eligibility.as_str().to_owned());
    diagnostics.workflow_applied = decision.applied;
    diagnostics.influenced = decision.applied;
    diagnostics.available_tools = decision.workflow.map(|_| decision.available_tools);
    diagnostics.approval_required = decision.workflow.map(|_| decision.approval_required);
    diagnostics.workflow_selection_us =
        Some(u64::try_from(selection.as_micros()).unwrap_or(u64::MAX));
    diagnostics.routing_input = routing_input.map(str::to_owned);
}

/// One terminal line describing the classification, or `None` when off.
pub(crate) fn terminal_summary(report: &ClassificationReport) -> String {
    match &report.outcome {
        Outcome::Classified(classification) => format!(
            "· Task assessment ({}): {}, confidence {:.2}{}",
            report.mode.as_str(),
            classification.category.as_str(),
            classification.category_confidence,
            if classification.accepted() {
                ", advisory only"
            } else {
                ", below threshold"
            }
        ),
        Outcome::Skipped(reason) | Outcome::Failed(reason) => format!(
            "· Task assessment ({}) not used: {}",
            report.mode.as_str(),
            reason.replace('_', " ")
        ),
    }
}

/// One terminal line describing a turn's workflow decision, or `None` when
/// there was no valid classification to decide on.
pub(crate) fn terminal_workflow_summary(decision: &workflow::WorkflowDecision) -> Option<String> {
    let workflow = decision.workflow?;
    let effect = if decision.applied {
        "fixed guidance added to this turn only"
    } else {
        "advisory only, this turn is unchanged"
    };
    Some(format!(
        "· Workflow: {} ({}), {effect}",
        workflow.as_str(),
        decision.eligibility.as_str().replace('_', " ")
    ))
}

#[cfg(test)]
mod tests {
    use super::classify::TaskCategory;
    use super::classify::tests::{answer, config, jev_stub};
    use super::*;
    use lightagent_core::ProfileId;

    fn profile(id: &str, model: &str) -> AgentProfile {
        AgentProfile::new(ProfileId::new(id).unwrap(), "Test", "Persona", model)
    }

    fn route_answer() -> String {
        r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"model:fast","confidence":0.99,"probabilities":{"default":0.01,"model:fast":0.99}}}}"#.to_owned()
    }

    async fn run(config: &Config, profile: &AgentProfile, explicit: Option<&str>) -> Preflight {
        let store = ProfileStore::new(std::env::temp_dir());
        preflight(
            PreflightRequest {
                config,
                store: &store,
                profile,
                message: "find recent papers on batteries",
                explicit_model: explicit,
                attachment: false,
            },
            &CancellationToken::new(),
        )
        .await
    }

    fn questions(body: &str) -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        value["questions"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn auto_is_preserved_in_every_classification_mode() {
        for mode in [
            JevClassificationMode::Off,
            JevClassificationMode::Shadow,
            JevClassificationMode::Active,
        ] {
            let (url, seen) = jev_stub(200, route_answer(), Duration::ZERO).await;
            let mut config = config(&url, mode);
            config.platform.jev.allowed_models = vec!["fast".to_owned()];
            let auto = profile("permitted", "Auto");
            let preflight = run(&config, &auto, None).await;
            assert_eq!(preflight.model, "Auto", "{mode:?}");
            assert_eq!(preflight.routing_status, Some(("Auto".to_owned(), true)));
            let requests = seen.lock().unwrap().clone();
            assert!(
                requests
                    .iter()
                    .all(|body| !questions(body).contains(&"route".to_owned())),
                "Lightagent never asks Jev to route an Auto model"
            );
            // Only the classification request remains, and only when enabled.
            let expected = usize::from(mode != JevClassificationMode::Off);
            assert_eq!(requests.len(), expected, "{mode:?}");
            assert_eq!(preflight.jev_calls as usize, expected);
        }
    }

    #[tokio::test]
    async fn non_auto_routing_is_unchanged_and_runs_beside_one_classification() {
        // The stub answers every request with a route answer, so the
        // classification request fails while routing succeeds: each decision
        // is independent of the other.
        let (url, seen) = jev_stub(200, route_answer(), Duration::ZERO).await;
        let mut config = config(&url, JevClassificationMode::Shadow);
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        let preflight = run(&config, &profile("permitted", "original"), None).await;
        assert_eq!(preflight.model, "fast");
        assert_eq!(preflight.routing_status, Some(("fast".to_owned(), false)));
        assert_eq!(preflight.jev_calls, 2);
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        let routes = requests
            .iter()
            .filter(|body| questions(body) == ["route"])
            .count();
        let classifications = requests
            .iter()
            .filter(|body| questions(body).contains(&"task".to_owned()))
            .count();
        assert_eq!(
            (routes, classifications),
            (1, 1),
            "no duplicate classification"
        );
        assert_eq!(
            preflight.classification.unwrap().outcome,
            Outcome::Failed("malformed")
        );
    }

    #[tokio::test]
    async fn an_explicit_model_is_kept_and_classification_still_runs() {
        let (url, seen) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let mut config = config(&url, JevClassificationMode::Shadow);
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        let preflight = run(&config, &profile("permitted", "original"), Some("chosen")).await;
        assert_eq!(preflight.model, "chosen");
        assert_eq!(preflight.routing_status, None);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(matches!(
            preflight.classification.unwrap().outcome,
            Outcome::Classified(_)
        ));
    }

    #[tokio::test]
    async fn classification_off_leaves_the_preflight_exactly_as_before() {
        let (url, seen) = jev_stub(200, route_answer(), Duration::ZERO).await;
        let mut config = config(&url, JevClassificationMode::Off);
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        let preflight = run(&config, &profile("permitted", "original"), None).await;
        assert_eq!(preflight.model, "fast");
        assert!(preflight.classification.is_none());
        assert_eq!(diagnostics(&preflight, false), None);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_preflight_returns_at_once_with_the_configured_model() {
        let (url, seen) = jev_stub(200, answer("research", 0.95), Duration::from_secs(5)).await;
        let mut config = config(&url, JevClassificationMode::Shadow);
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        config.platform.jev.timeout_secs = 30;
        let store = ProfileStore::new(std::env::temp_dir());
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let start = Instant::now();
        let result = preflight(
            PreflightRequest {
                config: &config,
                store: &store,
                profile: &profile("permitted", "original"),
                message: "find papers",
                explicit_model: None,
                attachment: false,
            },
            &cancel,
        )
        .await;
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(result.model, "original");
        assert_eq!(
            result.classification.unwrap().outcome,
            Outcome::Failed("cancelled")
        );
        assert!(seen.lock().unwrap().len() <= 2);
    }

    fn report(
        mode: JevClassificationMode,
        category: TaskCategory,
        confidence: f64,
    ) -> ClassificationReport {
        ClassificationReport {
            mode,
            outcome: Outcome::Classified(classify::TaskClassification {
                category,
                category_confidence: confidence,
                planning_probability: 0.5,
                retrieval_probability: 0.5,
                tools_probability: 0.5,
                complexity: classify::Complexity::Moderate,
                complexity_confidence: 0.6,
                model_version: Some("jev-1.13.0".to_owned()),
                usage: None,
                threshold: 0.9,
            }),
            invoked: true,
            latency: Some(Duration::from_millis(5)),
        }
    }

    #[test]
    fn diagnostics_hold_only_fixed_vocabulary_and_numbers() {
        let preflight = Preflight {
            model: "m".to_owned(),
            base_url: "u".to_owned(),
            routing_status: None,
            classification: Some(report(
                JevClassificationMode::Active,
                TaskCategory::Research,
                0.95,
            )),
            jev_calls: 1,
            latency: Duration::from_millis(7),
            routing_input: None,
        };
        let diagnostics = diagnostics(&preflight, true).unwrap();
        assert_eq!(diagnostics.outcome, "classified");
        assert_eq!(diagnostics.category.as_deref(), Some("research"));
        assert_eq!(diagnostics.fallback_reason, None);
        assert!(diagnostics.influenced);
        assert_eq!(diagnostics.jev_calls, 1);
        let mut low = preflight.clone();
        low.classification = Some(report(
            JevClassificationMode::Shadow,
            TaskCategory::Research,
            0.4,
        ));
        let low = super::diagnostics(&low, false).unwrap();
        assert_eq!(low.outcome, "low_confidence");
        assert_eq!(low.fallback_reason.as_deref(), Some("low_confidence"));
        let mut skipped = preflight.clone();
        skipped.classification = Some(ClassificationReport {
            mode: JevClassificationMode::Shadow,
            outcome: Outcome::Skipped("profile_not_permitted"),
            invoked: false,
            latency: None,
        });
        let skipped = super::diagnostics(&skipped, false).unwrap();
        assert_eq!(skipped.outcome, "skipped");
        assert_eq!(skipped.category, None);
        assert!(!skipped.confidence_available);
    }
}
