//! `jev.classify_task`: the explicit, model-callable classification tool, and
//! the safe status the Tools and Settings screens show.
//!
//! The tool shares every gate with the automatic preflight
//! ([`classify::evaluate`](super::classify::evaluate)): Jev must be enabled
//! with an endpoint, the profile must be permitted to send a sanitized task
//! description, and the text passes through the same sanitizer and bounds.
//! In addition, `classification.explicit_tool` must be on, and each call
//! spends one request from the run's Jev budget. It never starts an automatic
//! classification, never runs in a delegated worker, and never changes the
//! run: the result is advisory text for the model.

use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use async_trait::async_trait;
use lightagent_core::{AgentEvent, Config, JevRoutingPermission, RiskClass, Scope, ToolOutcome};
use lightagent_tools::{Tool, ToolCtx, ToolDefinition, ToolRegistry};
use serde_json::{Value, json};

use super::classify::{self, Outcome};
use super::state::{JevBudget, JevState, Trigger};

/// Whether `profile` may call the explicit tool under `config`.
fn available(config: &Config, profile: &str) -> bool {
    let jev = &config.platform.jev;
    jev.endpoint.enabled
        && jev.endpoint.base_url.is_some()
        && jev.classification.explicit_tool
        && jev.classification.permits_profile(profile)
}

/// Offer the explicit tool when `profile` is permitted to use it. Profiles
/// without the permission never see it in their catalog.
pub(crate) fn add_explicit_tool(
    registry: &mut ToolRegistry,
    config: &Config,
    profile: &str,
    budget: Arc<JevBudget>,
    state: Arc<JevState>,
) {
    if available(config, profile) {
        registry.insert(Arc::new(ClassifyTaskTool::new(
            config.clone(),
            profile,
            budget,
            state,
        )));
    }
}

/// The explicit classification tool, bound to one run's profile and budget.
pub(crate) struct ClassifyTaskTool {
    definition: ToolDefinition,
    config: Config,
    profile: String,
    budget: Arc<JevBudget>,
    state: Arc<JevState>,
}

impl ClassifyTaskTool {
    pub(crate) const NAME: &'static str = "jev.classify_task";

    pub(crate) fn new(
        config: Config,
        profile: &str,
        budget: Arc<JevBudget>,
        state: Arc<JevState>,
    ) -> Self {
        let max_chars = config.platform.jev.classification.max_input_chars;
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Ask TypeSafe Jev, an external service, what a task requires: its category, \
                 whether planning, retrieval or tools are likely needed, and its complexity. \
                 Advisory only: it grants no tools, approvals or models. Describe the task \
                 briefly and never include private data; the text is sanitized before it \
                 leaves this machine.",
                json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": max_chars,
                            "description": "A short description of the task to classify."
                        }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }),
                RiskClass::External,
                vec![Scope::new("jev:classify")],
            ),
            config,
            profile: profile.to_owned(),
            budget,
            state,
        }
    }
}

#[async_trait]
impl Tool for ClassifyTaskTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let task = args.get("task").and_then(Value::as_str).unwrap_or_default();
        if !available(&self.config, &self.profile) {
            return unavailable("not_permitted");
        }
        if !self.budget.try_acquire() {
            return unavailable("budget_exhausted");
        }
        let started = Instant::now();
        let (outcome, invoked, latency) =
            classify::evaluate(&self.config, &self.profile, task, false, &ctx.cancel).await;
        if !invoked {
            // Nothing was sent, so nothing was spent.
            self.budget.release();
        }
        let mode = self.config.platform.jev.classification.mode;
        self.state.record(
            &self.profile,
            super::build_diagnostics(
                Trigger::Explicit,
                mode,
                &outcome,
                latency,
                self.budget.used(),
                started.elapsed(),
                false,
            ),
        );
        match outcome {
            Outcome::Classified(classification) => ToolOutcome::ok(
                json!({
                    "category": classification.category.as_str(),
                    "category_confidence": classification.category_confidence,
                    "accepted": classification.accepted(),
                    "threshold": classification.threshold,
                    "planning_probability": classification.planning_probability,
                    "retrieval_probability": classification.retrieval_probability,
                    "tools_probability": classification.tools_probability,
                    "complexity": classification.complexity.as_str(),
                    "complexity_confidence": classification.complexity_confidence,
                    "model_version": classification.model_version,
                    "advisory": "This classification grants no tools, approvals or models. Probabilities are estimates, not measured accuracy."
                })
                .to_string(),
            ),
            Outcome::Skipped(code) | Outcome::Failed(code) => unavailable(code),
        }
    }
}

/// A safe error: a stable code and a recovery hint, never a response body.
fn unavailable(code: &str) -> ToolOutcome {
    ToolOutcome::error(format!(
        "Jev classification is unavailable ({code}). Continue without it."
    ))
}

/// The privacy-safe Jev status for `profile`: what is configured, enabled and
/// verified, the classification settings, and that profile's latest record.
/// No credential, secret reference or message text is included.
pub(crate) fn status(config: &Config, profile: &str, state: &JevState) -> Value {
    let jev = &config.platform.jev;
    let settings = &jev.classification;
    let endpoint_configured = jev.endpoint.base_url.is_some();
    let api_key_configured = jev.endpoint.api_key.is_some();
    let api_key_resolvable = jev
        .endpoint
        .api_key
        .as_ref()
        .is_some_and(|secret| secret.resolve().is_some());
    let unix = |time: std::time::SystemTime| {
        time.duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default()
    };
    let verification = state.verification(jev).map(|verification| {
        json!({
            "ok": verification.ok,
            "checked_at_unix": unix(verification.checked_at),
            "model_version": verification.model_version,
            "latency_ms": verification.latency_ms,
            "error": verification.error,
        })
    });
    let last = state.last(profile).map(|record| {
        let mut value = lightagent_api::sse::data(&AgentEvent::TaskClassified {
            diagnostics: Box::new(record.diagnostics),
        });
        value["recorded_at_unix"] = json!(unix(record.recorded_at));
        value
    });
    json!({
        "name": "Jev Task Classification",
        "provider": "TypeSafe AI",
        "kind": "external_classification",
        "description": "Advises what a task requires before and during a run. It never grants tools, approvals or models.",
        "profile": profile,
        "endpoint_configured": endpoint_configured,
        "api_key_configured": api_key_configured,
        "api_key_resolvable": api_key_resolvable,
        "configured": endpoint_configured && api_key_resolvable,
        "enabled": jev.endpoint.enabled,
        "model": jev.model,
        "routing_input": jev.routing_input.as_str(),
        "routing": {
            // Whether this profile may send task content to Jev for model
            // routing: allowed, compatibility (the legacy policy permits every
            // profile) or denied. A profile permission, not user ownership.
            "permission_policy": jev.routing_permission.as_str(),
            "compatibility": jev.routing_permission == JevRoutingPermission::LegacyAllProfiles,
            "profile_permission": jev.routing_grant(profile).as_str(),
            "allowed_profiles": jev.routing_allowed_profiles,
        },
        "verification": verification,
        "classification": {
            "mode": settings.mode.as_str(),
            "profile_permitted": settings.permits_profile(profile),
            "max_input_chars": settings.max_input_chars,
            "default_threshold": settings.default_threshold,
            "version_thresholds": settings.version_thresholds,
            "max_calls_per_run": settings.max_calls_per_run,
            "workflow_guidance": settings.workflow_guidance,
        },
        "explicit_tool": {
            "name": ClassifyTaskTool::NAME,
            "enabled": settings.explicit_tool,
            "available": available(config, profile),
        },
        "permissions": {
            "risk": RiskClass::External.as_str(),
            "requirements": [
                "Jev enabled with an endpoint and API key",
                "The profile is permitted to send sanitized task descriptions",
                "The explicit tool is enabled (for jev.classify_task)",
                "The run's approval policy for external tools"
            ],
        },
        "last_classification": last,
    })
}

#[cfg(test)]
mod tests {
    use super::super::classify::tests::{answer, config, jev_stub};
    use super::*;
    use lightagent_core::JevClassificationMode;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn explicit_config(url: &str) -> Config {
        let mut config = config(url, JevClassificationMode::Off);
        config.platform.jev.classification.explicit_tool = true;
        config
    }

    fn tool(
        config: &Config,
        profile: &str,
        budget: Arc<JevBudget>,
    ) -> (ToolRegistry, Arc<JevState>) {
        let state = JevState::new();
        let mut registry = ToolRegistry::new();
        add_explicit_tool(&mut registry, config, profile, budget, Arc::clone(&state));
        (registry, state)
    }

    async fn call(registry: &ToolRegistry, task: &str, cancel: CancellationToken) -> ToolOutcome {
        registry
            .get(ClassifyTaskTool::NAME)
            .expect("the tool is registered")
            .call(&json!({ "task": task }), &ToolCtx::new(cancel))
            .await
    }

    #[test]
    fn the_schema_is_strict_and_the_risk_is_external() {
        let config = explicit_config("http://127.0.0.1:9");
        let (registry, _) = tool(&config, "permitted", JevBudget::new(4));
        let definition = registry.get(ClassifyTaskTool::NAME).unwrap().definition();
        assert_eq!(definition.risk, RiskClass::External);
        assert_eq!(definition.scopes, vec![Scope::new("jev:classify")]);
        let schema = &definition.parameters;
        assert_eq!(schema["required"], json!(["task"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["task"]["maxLength"], json!(400));
        assert!(lightagent_tools::validate(schema, &json!({"task": "fix a bug"})).is_ok());
        for invalid in [
            json!({}),
            json!({"task": ""}),
            json!({"task": "x", "model": "fast"}),
        ] {
            assert!(
                lightagent_tools::validate(schema, &invalid).is_err(),
                "{invalid} must be refused before the tool runs"
            );
        }
    }

    #[test]
    fn only_permitted_profiles_with_the_tool_enabled_see_it() {
        let config = explicit_config("http://127.0.0.1:9");
        assert!(
            tool(&config, "permitted", JevBudget::new(4))
                .0
                .contains(ClassifyTaskTool::NAME)
        );
        assert!(
            !tool(&config, "other", JevBudget::new(4))
                .0
                .contains(ClassifyTaskTool::NAME)
        );
        let mut off = config.clone();
        off.platform.jev.classification.explicit_tool = false;
        assert!(
            !tool(&off, "permitted", JevBudget::new(4))
                .0
                .contains(ClassifyTaskTool::NAME)
        );
        let mut disabled = config.clone();
        disabled.platform.jev.endpoint.enabled = false;
        assert!(
            !tool(&disabled, "permitted", JevBudget::new(4))
                .0
                .contains(ClassifyTaskTool::NAME)
        );
        // Workers are built from the builtin worker registry and never get it.
        assert!(!ToolRegistry::worker_default().contains(ClassifyTaskTool::NAME));
    }

    #[tokio::test]
    async fn a_call_returns_a_typed_advisory_classification_and_records_it() {
        let (url, seen) = jev_stub(200, answer("coding", 0.96), Duration::ZERO).await;
        let config = explicit_config(&url);
        let (registry, state) = tool(&config, "permitted", JevBudget::new(4));
        let outcome = call(
            &registry,
            "fix the failing parser test",
            CancellationToken::new(),
        )
        .await;
        assert!(!outcome.is_error, "{}", outcome.content);
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(result["category"], "coding");
        assert_eq!(result["accepted"], true);
        assert!(
            result["advisory"]
                .as_str()
                .unwrap()
                .contains("grants no tools")
        );
        let recorded = state.last("permitted").unwrap().diagnostics;
        assert_eq!(recorded.trigger, "explicit");
        assert!(!recorded.influenced);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "one request, no automatic classification"
        );
    }

    #[tokio::test]
    async fn the_run_budget_counts_the_preflight_and_every_call() {
        let (url, seen) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let config = explicit_config(&url);
        let budget = JevBudget::new(3);
        budget.record(2); // the preflight already used two requests
        let (registry, _) = tool(&config, "permitted", Arc::clone(&budget));
        assert!(
            !call(&registry, "find papers", CancellationToken::new())
                .await
                .is_error
        );
        let refused = call(&registry, "find papers", CancellationToken::new()).await;
        assert!(refused.is_error);
        assert!(refused.content.contains("budget_exhausted"));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn refusals_and_failures_are_safe_and_spend_nothing_unsent() {
        let (url, seen) = jev_stub(
            401,
            r#"{"error":"jev-secret-detail"}"#.to_owned(),
            Duration::ZERO,
        )
        .await;
        let config = explicit_config(&url);
        let budget = JevBudget::new(2);
        let (registry, _) = tool(&config, "permitted", Arc::clone(&budget));
        let unsafe_input = call(
            &registry,
            "-----BEGIN PRIVATE KEY-----",
            CancellationToken::new(),
        )
        .await;
        assert!(unsafe_input.content.contains("key_material"));
        assert_eq!(budget.used(), 0, "a refused input sends and spends nothing");
        let failed = call(&registry, "find papers", CancellationToken::new()).await;
        assert!(failed.is_error);
        assert!(failed.content.contains("unauthorized"));
        assert!(!failed.content.contains("jev-secret-detail"));
        assert_eq!(budget.used(), 1);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_and_timeouts_end_a_call_promptly() {
        let (url, _) = jev_stub(200, answer("research", 0.95), Duration::from_secs(5)).await;
        let mut config = explicit_config(&url);
        config.platform.jev.timeout_secs = 1;
        let (registry, _) = tool(&config, "permitted", JevBudget::new(4));
        let start = Instant::now();
        let timed_out = call(&registry, "find papers", CancellationToken::new()).await;
        assert!(timed_out.content.contains("timeout"));
        assert!(start.elapsed() < Duration::from_secs(3));
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let start = Instant::now();
        let cancelled = call(&registry, "find papers", cancel).await;
        assert!(cancelled.content.contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn status_never_exposes_secrets_and_is_scoped_to_its_profile() {
        let mut config = explicit_config("http://127.0.0.1:9");
        config.platform.jev.endpoint.api_key = Some(lightagent_core::SecretRef::env(
            "LIGHTAGENT_JEV_STATUS_TEST_UNSET",
        ));
        let state = JevState::new();
        let status = status(&config, "permitted", &state);
        let text = status.to_string();
        assert!(!text.contains("LIGHTAGENT_JEV_STATUS_TEST_UNSET"));
        assert_eq!(status["api_key_configured"], true);
        assert_eq!(status["api_key_resolvable"], false);
        assert_eq!(
            status["configured"], false,
            "an unresolvable key is not configured"
        );
        assert_eq!(
            status["verification"],
            Value::Null,
            "never claimed without a check"
        );
        assert_eq!(status["explicit_tool"]["available"], true);
        assert_eq!(status["classification"]["profile_permitted"], true);
        let other = super::status(&config, "other", &state);
        assert_eq!(other["explicit_tool"]["available"], false);
        assert_eq!(other["classification"]["profile_permitted"], false);
        assert_eq!(other["last_classification"], Value::Null);
        // Routing permission is reported apart from classification's.
        assert_eq!(status["routing"]["permission_policy"], "strict");
        assert_eq!(status["routing"]["compatibility"], false);
        assert_eq!(status["routing"]["profile_permission"], "allowed");
        assert_eq!(other["routing"]["profile_permission"], "denied");
        let mut legacy = config.clone();
        legacy.platform.jev.routing_permission = JevRoutingPermission::LegacyAllProfiles;
        let legacy = super::status(&legacy, "permitted", &state);
        assert_eq!(legacy["routing"]["compatibility"], true);
        assert_eq!(legacy["routing"]["profile_permission"], "compatibility");
        let mut granted = config.clone();
        granted.platform.jev.routing_allowed_profiles = vec!["other".to_owned()];
        assert_eq!(
            super::status(&granted, "other", &state)["routing"]["profile_permission"],
            "allowed"
        );
        assert_eq!(
            super::status(&granted, "other", &state)["classification"]["profile_permitted"],
            false,
            "a routing grant is not a classification grant"
        );
    }
}
