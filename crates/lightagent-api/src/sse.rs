//! Mapping the canonical [`AgentEvent`] stream to named SSE events.
//!
//! The wire names follow the master plan's vocabulary (`run.started`,
//! `model.delta`, `tool.requested`, `approval.required`, `run.completed`, …), so
//! a browser or a script reads the run by the `event:` name and the JSON `data:`
//! without knowing the Rust enum.

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
        AgentEvent::ToolCallCompleted { outcome, .. } => {
            if outcome.is_error {
                "tool.failed"
            } else {
                "tool.output"
            }
        }
        AgentEvent::AwaitingApproval { .. } => "approval.required",
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
        } => json!({
            "source": source,
            "model": model,
            "kept_default": kept_default,
        }),
        AgentEvent::TaskClassified { diagnostics: d } => json!({
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
        }),
        AgentEvent::ToolCallRequested { call } => json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }),
        AgentEvent::ToolCallStarted { id, name } => json!({ "id": id, "name": name }),
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
        AgentEvent::TurnCompleted { usage: Some(usage) } => json!({
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
        }),
        AgentEvent::TurnCompleted { usage: None } => json!({}),
        AgentEvent::RunCompleted { reason } => json!({ "reason": format!("{reason:?}") }),
        AgentEvent::Error { message } => json!({ "message": message }),
        _ => json!({}),
    }
}

/// Build the axum SSE [`Event`] for an [`AgentEvent`].
pub fn to_sse(event: &AgentEvent) -> Event {
    Event::default()
        .event(name(event))
        .data(data(event).to_string())
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
        };
        assert_eq!(name(&route), "routing.selected");
        assert_eq!(
            data(&route),
            json!({"source": "Jev", "model": "Auto", "kept_default": true})
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
            }),
        };
        assert_eq!(name(&classified), "classification.completed");
        let payload = data(&classified);
        assert_eq!(payload["schema"], 1);
        assert_eq!(payload["category"], "research");
        assert_eq!(payload["fallback_reason"], serde_json::Value::Null);
        assert_eq!(payload["trigger"], "preflight");
        assert_eq!(payload.as_object().unwrap().len(), 22);
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
    fn content_data_carries_the_text() {
        let value = data(&AgentEvent::Content {
            text: "hello".into(),
        });
        assert_eq!(value["content"], "hello");
    }
}
