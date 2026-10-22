//! `agent.research` — hand a research question to the researcher specialist.
//!
//! A narrowly scoped, read-only delegation: it names no role and no profile,
//! only a task, and always runs the requesting profile's researcher (its
//! assignment, or the built-in). The worker's tools stay inside the
//! researcher's read-only ceiling *and* the run's own tools, so it can never
//! reach a filesystem, a command or another profile; every one of its calls
//! is still checked by the run's policy, authorization and budget. This is
//! what an untrusted run may be offered where generic `agent.delegate` is
//! withheld until execution isolation exists.

use async_trait::async_trait;
use lightagent_core::workers::{self, WorkerRole};
use lightagent_core::{RiskClass, Scope, ToolOutcome};
use serde_json::{Value, json};

use crate::builtins::delegate::{WorkerRequest, run_worker};
use crate::context::ToolCtx;
use crate::definition::{Tool, ToolDefinition};
use crate::worker::WorkerTarget;

/// The `agent.research` tool.
pub struct AgentResearch {
    definition: ToolDefinition,
}

impl AgentResearch {
    pub const NAME: &'static str = "agent.research";

    pub fn new() -> Self {
        let parameters = json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "A self-contained research question." },
                "max_turns": { "type": "integer", "minimum": 1, "description": "Cap the researcher's model turns." }
            },
            "required": ["task"],
            "additionalProperties": false,
        });
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Ask the researcher specialist to investigate a question with read-only retrieval tools and return cited findings.",
                parameters,
                RiskClass::External,
                vec![Scope::new("agent:research")],
            ),
        }
    }
}

impl Default for AgentResearch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AgentResearch {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(delegation) = ctx.delegation.clone() else {
            return ToolOutcome::error("delegation is not enabled for this run");
        };
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        let Some(resolver) = delegation.resolver.clone() else {
            return ToolOutcome::error("research delegation needs the built-in worker catalog");
        };
        if !delegation.subagents.enabled {
            return ToolOutcome::error("named subagent roles are disabled for this run");
        }
        if let Err(error) = delegation.may_start_worker() {
            return ToolOutcome::error(error);
        }
        if let Err(error) = delegation.reserve_child() {
            return ToolOutcome::error(error);
        }
        let mut spec = match resolver.resolve(WorkerTarget::Role(WorkerRole::Researcher.as_str())) {
            Ok(spec) => spec,
            Err(error) => return ToolOutcome::error(error),
        };
        // Whatever resolved, it runs as a read-only researcher: inside the
        // built-in ceiling (and any narrower one the spec had), never nested.
        let researcher = workers::builtin(WorkerRole::Researcher);
        let ceiling: Vec<String> = researcher
            .toolsets()
            .into_iter()
            .filter(|tool| spec.within_ceiling(tool))
            .collect();
        spec.ceiling = Some(ceiling);
        spec.nested = false;
        spec.typed_result = true;
        let request = WorkerRequest {
            task: args
                .get("task")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            max_turns: args
                .get("max_turns")
                .and_then(Value::as_u64)
                .and_then(|turns| u32::try_from(turns).ok()),
            max_seconds: None,
            tool_scope: None,
        };
        run_worker(&delegation, spec, request, ctx).await
    }
}
