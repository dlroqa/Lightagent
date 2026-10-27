//! Mapping the canonical [`AgentEvent`] stream to named SSE events.
//!
//! The wire names follow the master plan's vocabulary (`run.started`,
//! `model.delta`, `tool.requested`, `approval.required`, `run.completed`, …), so
//! a browser or a script reads the run by the `event:` name and the JSON `data:`
//! without knowing the Rust enum.
//!
//! Each frame also carries an `id:`, the event's position in the run's buffer
//! (see [`to_sse_with_id`]), so a client that reconnects resumes after the last
//! event it saw instead of receiving the run again.

use axum::response::sse::Event;
use lightagent_core::{AgentEvent, StopReason};
use serde_json::json;

/// The SSE event name for an [`AgentEvent`].
pub fn name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::RunStarted { .. } => "run.started",
        AgentEvent::Reasoning { .. } | AgentEvent::Content { .. } => "model.delta",
        AgentEvent::RouteSelected { .. } => "routing.selected",
        AgentEvent::TaskClassified { .. } => "classification.completed",
        AgentEvent::ToolCallRequested { .. } => "tool.requested",
        AgentEvent::ToolCallStarted { .. } => "tool.started",
        AgentEvent::ToolProgress { .. } => "tool.progress",
        AgentEvent::ToolCallCompleted { outcome, .. } => {
            if outcome.is_error {
                "tool.failed"
            } else {
                "tool.output"
            }
        }
        AgentEvent::AwaitingApproval { .. } => "approval.required",
        AgentEvent::GenerationReport { .. } => "generation.report",
        AgentEvent::TurnCompleted { .. } => "turn.completed",
        AgentEvent::RunCompleted { reason } => match reason {
            StopReason::Cancelled => "run.cancelled",
            StopReason::Error => "run.failed",
            _ => "run.completed",
        },
        AgentEvent::Error { .. } => "error",
        _ => "event",
    }
}

/// The JSON `data:` payload for an [`AgentEvent`].
pub fn data(event: &AgentEvent) -> serde_json::Value {
    match event {
        AgentEvent::RunStarted { run, parent } => json!({
            "run": run.as_str(),
            "parent": parent.as_ref().map(|id| id.as_str()),
        }),
        AgentEvent::Reasoning { text } => json!({ "reasoning": text }),
        AgentEvent::Content { text } => json!({ "content": text }),
        AgentEvent::RouteSelected {
            source,
            model,
            kept_default,
            permission,
            fallback,
        } => {
            // The original three fields, plus the permission and fallback
            // code only when routing reported them.
            let mut payload = json!({
                "source": source,
                "model": model,
                "kept_default": kept_default,
            });
            if let Some(permission) = permission {
                payload["permission"] = json!(permission);
            }
            if let Some(fallback) = fallback {
                payload["fallback"] = json!(fallback);
            }
            payload
        }
        AgentEvent::TaskClassified { diagnostics: d } => {
            let mut payload = classification_payload(d);
            if let Some(selection) = &d.specialists {
                payload["specialists"] = specialists_payload(selection);
            }
            payload
        }
        AgentEvent::ToolCallRequested { call } => json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }),
        AgentEvent::ToolCallStarted { id, name } => json!({ "id": id, "name": name }),
        AgentEvent::ToolProgress { id, progress } => json!({
            "id": id,
            "stage": progress.stage,
            "completed": progress.completed,
            "total": progress.total,
            "outcome": progress.outcome,
        }),
        AgentEvent::ToolCallCompleted { id, outcome } => json!({
            "id": id,
            "is_error": outcome.is_error,
            "content": outcome.content,
        }),
        AgentEvent::AwaitingApproval {
            approval_id,
            tool_call_id,
            name,
        } => json!({
            "approval_id": approval_id,
            "tool_call_id": tool_call_id,
            "name": name,
        }),
        AgentEvent::GenerationReport { report: r } => json!({
            "turn": r.turn,
            "purpose": r.purpose,
            "context_window": r.context_window,
            "context_source": r.context_source,
            "prompt_estimate": r.prompt_estimate,
            "prompt_basis": r.prompt_basis,
            "requested_max_tokens": r.requested_max_tokens,
            "reasoning": r.reasoning,
            "reasoning_effort": r.reasoning_effort,
            "tools_offered": r.tools_offered,
            "shortened_results": r.shortened_results,
            "finish": r.finish,
            "prompt_tokens": r.prompt_tokens,
            "completion_tokens": r.completion_tokens,
            "length_cause": r.length_cause,
        }),
        AgentEvent::TurnCompleted { usage: Some(usage) } => json!({
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
        }),
        AgentEvent::TurnCompleted { usage: None } => json!({}),
        // `reason` is unchanged; `complete` is additive: true only when the
        // model ended its own answer, so a client need not know every reason.
        AgentEvent::RunCompleted { reason } => json!({
            "reason": format!("{reason:?}"),
            "complete": reason.is_complete(),
        }),
        AgentEvent::Error { message } => json!({ "message": message }),
        _ => json!({}),
    }
}

/// The classification record's fields, unchanged since schema 2.
fn classification_payload(d: &lightagent_core::ClassificationDiagnostics) -> serde_json::Value {
    json!({
        "schema": d.schema,
        "trigger": d.trigger,
        "mode": d.mode,
        "outcome": d.outcome,
        "source": d.source,
        "category": d.category,
        "category_confidence": d.category_confidence,
        "confidence_available": d.confidence_available,
        "threshold": d.threshold,
        "planning_probability": d.planning_probability,
        "retrieval_probability": d.retrieval_probability,
        "tools_probability": d.tools_probability,
        "complexity": d.complexity,
        "complexity_confidence": d.complexity_confidence,
        "model_version": d.model_version,
        "input_tokens": d.input_tokens,
        "output_tokens": d.output_tokens,
        "jev_calls": d.jev_calls,
        "classification_latency_ms": d.classification_latency_ms,
        "preflight_latency_ms": d.preflight_latency_ms,
        "fallback_reason": d.fallback_reason,
        "influenced": d.influenced,
        "accepted": d.accepted,
        "workflow": d.workflow,
        "workflow_eligibility": d.workflow_eligibility,
        "workflow_capability": d.workflow_capability,
        "workflow_applied": d.workflow_applied,
        "available_tools": d.available_tools,
        "approval_required": d.approval_required,
        "workflow_selection_us": d.workflow_selection_us,
        "routing_input": d.routing_input,
    })
}

/// A specialist selection: fixed codes and role names only.
fn specialists_payload(selection: &lightagent_core::SpecialistSelection) -> serde_json::Value {
    json!({
        "mode": selection.mode,
        "outcome": selection.outcome,
        "fallback_reason": selection.fallback_reason,
        "delegation_probability": selection.delegation_probability,
        "structure": selection.structure,
        "steps": selection.steps.iter().map(|step| json!({
            "role": step.role,
            "reason": step.reason,
            "after": step.after,
            "worker": step.worker,
        })).collect::<Vec<_>>(),
        "restricted": selection.restricted.iter().map(|restriction| json!({
            "role": restriction.role,
            "reason": restriction.reason,
        })).collect::<Vec<_>>(),
        "concurrent": selection.concurrent,
        "concurrency": selection.concurrency,
        "tool": selection.tool,
        "applied": selection.applied,
    })
}

/// Build the axum SSE [`Event`] for an [`AgentEvent`].
pub fn to_sse(event: &AgentEvent) -> Event {
    Event::default()
        .event(name(event))
        .data(data(event).to_string())
}

/// [`to_sse`] with the event's position in the run's buffer as its SSE `id`.
/// Ids start at 0 and increase by one per event, so they are both a resume
/// cursor (`Last-Event-ID`) and a duplicate check.
pub fn to_sse_with_id(event: &AgentEvent, position: usize) -> Event {
    to_sse(event).id(position.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::ids::RunId;

    #[test]
    fn routing_and_classification_events_are_separate_and_additive() {
        let route = AgentEvent::RouteSelected {
            source: "Jev".into(),
            model: "Auto".into(),
            kept_default: true,
            permission: None,
            fallback: None,
        };
        assert_eq!(name(&route), "routing.selected");
        assert_eq!(
            data(&route),
            json!({"source": "Jev", "model": "Auto", "kept_default": true})
        );
        let denied = AgentEvent::RouteSelected {
            source: "Jev".into(),
            model: "configured".into(),
            kept_default: true,
            permission: Some("denied".into()),
            fallback: Some("profile_not_permitted".into()),
        };
        assert_eq!(name(&denied), "routing.selected");
        assert_eq!(
            data(&denied),
            json!({"source": "Jev", "model": "configured", "kept_default": true,
                   "permission": "denied", "fallback": "profile_not_permitted"})
        );
        let classified = AgentEvent::TaskClassified {
            diagnostics: Box::new(lightagent_core::ClassificationDiagnostics {
                schema: 1,
                trigger: "preflight".into(),
                mode: "shadow".into(),
                outcome: "classified".into(),
                source: Some("jev".into()),
                category: Some("research".into()),
                category_confidence: Some(0.93),
                confidence_available: true,
                threshold: Some(0.9),
                planning_probability: Some(0.2),
                retrieval_probability: Some(0.8),
                tools_probability: Some(0.6),
                complexity: Some("moderate".into()),
                complexity_confidence: Some(0.7),
                model_version: Some("jev-1.13.0".into()),
                input_tokens: Some(120),
                output_tokens: Some(30),
                jev_calls: 1,
                classification_latency_ms: Some(42),
                preflight_latency_ms: 43,
                fallback_reason: None,
                influenced: false,
                accepted: Some(true),
                workflow: Some("research".into()),
                workflow_eligibility: Some("eligible".into()),
                ..Default::default()
            }),
        };
        assert_eq!(name(&classified), "classification.completed");
        let payload = data(&classified);
        assert_eq!(payload["schema"], 1);
        assert_eq!(payload["category"], "research");
        assert_eq!(payload["fallback_reason"], serde_json::Value::Null);
        assert_eq!(payload["trigger"], "preflight");
        assert_eq!(payload["workflow_eligibility"], "eligible");
        assert_eq!(payload["workflow_applied"], false);
        assert_eq!(payload["routing_input"], serde_json::Value::Null);
        // Every schema-1 field is still present, unchanged, beside the
        // additive schema-2 fields.
        assert_eq!(payload.as_object().unwrap().len(), 31);
    }

    #[test]
    fn names_follow_the_canonical_vocabulary() {
        assert_eq!(
            name(&AgentEvent::RunStarted {
                run: RunId::new(),
                parent: None
            }),
            "run.started"
        );
        assert_eq!(
            name(&AgentEvent::Content { text: "hi".into() }),
            "model.delta"
        );
        assert_eq!(
            name(&AgentEvent::RunCompleted {
                reason: StopReason::Cancelled
            }),
            "run.cancelled"
        );
        assert_eq!(
            name(&AgentEvent::RunCompleted {
                reason: StopReason::Error
            }),
            "run.failed"
        );
        assert_eq!(
            name(&AgentEvent::RunCompleted {
                reason: StopReason::EndTurn
            }),
            "run.completed"
        );
    }

    #[test]
    fn completion_says_whether_the_model_finished_its_answer() {
        let done = data(&AgentEvent::RunCompleted {
            reason: StopReason::EndTurn,
        });
        assert_eq!(done, json!({"reason": "EndTurn", "complete": true}));
        for reason in [
            StopReason::MaxTokens,
            StopReason::Incomplete,
            StopReason::MaxTurns,
            StopReason::MaxToolCalls,
            StopReason::ContextLimit,
        ] {
            let event = AgentEvent::RunCompleted { reason };
            assert_eq!(name(&event), "run.completed");
            assert_eq!(data(&event)["complete"], false);
        }
        assert_eq!(
            data(&AgentEvent::RunCompleted {
                reason: StopReason::Incomplete
            })["reason"],
            "Incomplete"
        );
    }

    #[test]
    fn a_generation_report_carries_only_counts_and_codes() {
        let event = AgentEvent::GenerationReport {
            report: Box::new(lightagent_core::GenerationReport {
                turn: 2,
                purpose: "answer".into(),
                context_window: Some(8_192),
                context_source: Some("provider".into()),
                prompt_estimate: 7_000,
                prompt_basis: "measured".into(),
                requested_max_tokens: None,
                reasoning: "default".into(),
                reasoning_effort: None,
                tools_offered: 0,
                shortened_results: 1,
                finish: "length".into(),
                prompt_tokens: Some(7_000),
                completion_tokens: Some(1_191),
                length_cause: Some("context_window".into()),
            }),
        };
        assert_eq!(name(&event), "generation.report");
        let payload = data(&event);
        assert_eq!(payload["context_window"], 8_192);
        assert_eq!(
            payload["requested_max_tokens"],
            json!(null),
            "unknown is null"
        );
        assert_eq!(payload["length_cause"], "context_window");
        assert_eq!(payload.as_object().map(|object| object.len()), Some(15));
    }

    #[test]
    fn tool_progress_is_its_own_named_event() {
        let event = AgentEvent::ToolProgress {
            id: "call_1".into(),
            progress: lightagent_core::ToolProgress::stage("fetching").counted(2, 5),
        };
        assert_eq!(name(&event), "tool.progress");
        assert_eq!(
            data(&event),
            json!({"id": "call_1", "stage": "fetching", "completed": 2, "total": 5, "outcome": null})
        );
    }

    #[test]
    fn content_data_carries_the_text() {
        let value = data(&AgentEvent::Content {
            text: "hello".into(),
        });
        assert_eq!(value["content"], "hello");
    }

    #[test]
    fn a_specialist_selection_is_an_additive_key_of_fixed_codes() {
        let selection = lightagent_core::SpecialistSelection {
            mode: "active".into(),
            outcome: "plan".into(),
            delegation_probability: Some(0.91),
            structure: Some("sequential".into()),
            steps: vec![lightagent_core::SpecialistStep {
                role: "coder".into(),
                reason: "recommended".into(),
                after: Vec::new(),
                worker: "builtin:coder@1".into(),
            }],
            restricted: vec![lightagent_core::SpecialistRestriction {
                role: "reviewer".into(),
                reason: "worker_cap".into(),
            }],
            concurrency: Some("workers_run_one_at_a_time".into()),
            tool: Some("agent.delegate".into()),
            applied: true,
            ..Default::default()
        };
        let event = AgentEvent::TaskClassified {
            diagnostics: Box::new(lightagent_core::ClassificationDiagnostics {
                schema: 2,
                outcome: "classified".into(),
                specialists: Some(selection),
                ..Default::default()
            }),
        };
        let payload = data(&event);
        assert_eq!(payload.as_object().unwrap().len(), 32);
        let specialists = &payload["specialists"];
        assert_eq!(specialists["outcome"], "plan");
        assert_eq!(specialists["steps"][0]["role"], "coder");
        assert_eq!(specialists["restricted"][0]["reason"], "worker_cap");
        assert_eq!(specialists["concurrent"], false);
        assert_eq!(specialists["applied"], true);
        let without = data(&AgentEvent::TaskClassified {
            diagnostics: Box::default(),
        });
        assert!(without.get("specialists").is_none());
    }
}
