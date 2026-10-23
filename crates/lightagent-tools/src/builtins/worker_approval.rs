//! `agent.resume_worker` and `agent.decline_worker` — a person's decision
//! on one action a delegated worker paused for.
//!
//! A worker cannot obtain an approval itself. When it reaches an action that
//! needs one, it is parked under a one-time handle bound to that exact
//! action (worker, tool, arguments) and the lead is told. Approving is a
//! call of `agent.resume_worker`, which is
//! [`RiskClass::Executable`](lightagent_core::RiskClass::Executable): the
//! run pauses and the person sees exactly what the worker will do (see the
//! executor's approval preview), whatever the approval policy, and no
//! remembered grant covers it. Declining needs no approval: the worker
//! continues without the action. Expiry, cancellation, revocation and a
//! withdrawn assignment are all checked again before anything resumes.

use async_trait::async_trait;
use lightagent_core::{RiskClass, Scope, ToolOutcome};
use serde_json::{Value, json};

use crate::builtins::delegate::resume_worker;
use crate::context::ToolCtx;
use crate::definition::{Tool, ToolDefinition};

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "handle": { "type": "string", "description": "The handle a worker result gave for its pending action." }
        },
        "required": ["handle"],
        "additionalProperties": false,
    })
}

async fn decide(args: &Value, ctx: &ToolCtx, granted: bool) -> ToolOutcome {
    let Some(delegation) = ctx.delegation.clone() else {
        return ToolOutcome::error("delegation is not enabled for this run");
    };
    let handle = args
        .get("handle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    resume_worker(&delegation, handle, granted, ctx).await
}

/// `agent.resume_worker`: run the worker's pending action, as approved.
pub struct AgentResumeWorker {
    definition: ToolDefinition,
}

impl AgentResumeWorker {
    pub const NAME: &'static str = "agent.resume_worker";

    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Ask the user to approve the exact action a worker is waiting on; on approval the worker runs it and continues.",
                parameters(),
                RiskClass::Executable,
                vec![Scope::new("agent:approve")],
            ),
        }
    }
}

impl Default for AgentResumeWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AgentResumeWorker {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        decide(args, ctx, true).await
    }
}

/// `agent.decline_worker`: the worker continues without its pending action.
pub struct AgentDeclineWorker {
    definition: ToolDefinition,
}

impl AgentDeclineWorker {
    pub const NAME: &'static str = "agent.decline_worker";

    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Decline the action a worker is waiting on; it continues without it.",
                parameters(),
                RiskClass::Observe,
                vec![Scope::new("agent:approve")],
            ),
        }
    }
}

impl Default for AgentDeclineWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AgentDeclineWorker {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        decide(args, ctx, false).await
    }
}
