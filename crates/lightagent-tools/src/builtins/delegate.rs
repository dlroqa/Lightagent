//! `agent.delegate` — hand a bounded task to a worker profile.
//!
//! Orchestration without a second agent loop: this tool loads a worker
//! [`AgentProfile`], builds a **fresh** [`RunConfig`] (the worker's persona as
//! system, the task as the only user message, none of the orchestrator's
//! history), intersects the worker's limits with the caller's caps, scopes the
//! worker's tools (with `agent.delegate` removed, so delegation is one level
//! deep), and drives it through the very same [`AgentLoop`]. The worker's final
//! answer is returned to the orchestrator as the tool result.
//!
//! It is [`RiskClass::Executable`](lightagent_core::RiskClass::Executable), so
//! the default policy pauses the run for approval before a worker ever starts.

use async_trait::async_trait;
use lightagent_core::ProfileId;
use lightagent_core::identity::{AuthorizedProvider, RunAuthorization};
use lightagent_core::{
    AgentEvent, AgentLoop, PolicyEngine, RiskClass, RunConfig, RunLimits, RunOutcome, Scope,
    StopReason, ToolOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::{Clock, ToolCtx};
use crate::definition::{Tool, ToolDefinition};
use crate::executor::BoundedExecutor;

/// The `agent.delegate` tool.
pub struct AgentDelegate {
    definition: ToolDefinition,
}

/// The parsed arguments. The schema has already validated shape and required
/// fields, so this deserialization does not fail in practice.
#[derive(Debug, Deserialize)]
struct DelegateArgs {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    role: Option<String>,
    task: String,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    max_seconds: Option<u64>,
    #[serde(default)]
    tool_scope: Option<Vec<String>>,
}

impl AgentDelegate {
    /// The tool's stable name.
    pub const NAME: &'static str = "agent.delegate";

    /// Build the tool with its declaration.
    pub fn new() -> Self {
        let parameters = json!({
            "type": "object",
            "properties": {
                "profile": { "type": "string", "description": "The worker profile id to run the task (legacy mode)." },
                "role": {
                    "type": "string",
                    "enum": ["researcher", "coder", "reviewer", "tester"],
                    "description": "An enabled server-defined worker role."
                },
                "task": { "type": "string", "description": "The instruction for the worker." },
                "max_turns": { "type": "integer", "minimum": 1, "description": "Cap the worker's model turns." },
                "max_seconds": { "type": "integer", "minimum": 1, "description": "Cap the worker's wall-clock seconds." },
                "tool_scope": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict the worker to these tool names.",
                }
            },
            "required": ["task"],
            "anyOf": [{"required": ["profile"]}, {"required": ["role"]}],
            "additionalProperties": false,
        });
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Delegate a bounded task to a worker profile and return its answer.",
                parameters,
                RiskClass::Executable,
                vec![Scope::new("agent:spawn")],
            ),
        }
    }
}

impl Default for AgentDelegate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AgentDelegate {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(delegation) = ctx.delegation.clone() else {
            return ToolOutcome::error("delegation is not enabled for this run");
        };
        // A revoked run starts no worker.
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        let args: DelegateArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutcome::error(format!("could not read delegate arguments: {error}"));
            }
        };
        let profile_name = match (args.profile.as_deref(), args.role.as_deref()) {
            (Some(_), Some(_)) => {
                return ToolOutcome::error("provide either profile or role, not both");
            }
            (Some(profile), None) => profile,
            (None, Some(role)) => {
                if !delegation.subagents.enabled {
                    return ToolOutcome::error("named subagent roles are disabled for this run");
                }
                match delegation.role_profile(role) {
                    Some(profile) => profile,
                    None => {
                        return ToolOutcome::error(format!("worker role '{role}' is not allowed"));
                    }
                }
            }
            (None, None) => return ToolOutcome::error("a worker profile or role is required"),
        };
        // Turning hierarchy on also caps legacy delegates. With the default
        // disabled policy their long-standing one-level behavior is unchanged.
        if delegation.subagents.enabled
            && let Err(error) = delegation.reserve_child()
        {
            return ToolOutcome::error(error);
        }
        let profile_id = match ProfileId::new(profile_name) {
            Ok(id) => id,
            Err(error) => return ToolOutcome::error(format!("invalid worker profile id: {error}")),
        };
        let worker = match delegation.profiles.load(&profile_id) {
            Ok(worker) => worker,
            Err(error) => {
                return ToolOutcome::error(format!(
                    "could not load worker profile '{}': {error}",
                    profile_name
                ));
            }
        };

        let provider = match delegation.factory.provider(&worker.routing) {
            Ok(provider) => provider,
            Err(error) => {
                return ToolOutcome::error(format!(
                    "no provider for worker '{}': {error}",
                    profile_name
                ));
            }
        };

        // A fresh, scoped tool set. Legacy delegates remain one level deep;
        // opted-in role delegates may expose one further bounded level.
        let mut registry = match &args.tool_scope {
            Some(scope) => delegation.worker_registry.scoped(scope),
            None => delegation.worker_registry.clone(),
        };
        if !delegation.permits_nested_delegation() {
            registry = registry.without(Self::NAME);
        } else if !registry.contains(Self::NAME) {
            registry = registry.with(std::sync::Arc::new(AgentDelegate::new()));
        }

        let mut limits = intersect_caps(&worker.limits, args.max_turns, args.max_seconds);
        if let Some(ceiling) = &delegation.limit_ceiling {
            limits = within_ceiling(limits, ceiling);
        }
        let approval = match delegation.approval_floor {
            Some(floor) => worker.approval_policy.stricter(floor),
            None => worker.approval_policy,
        };
        let mut executor = BoundedExecutor::new(
            registry,
            PolicyEngine::new(approval.into()),
            delegation.worker_per_call,
            delegation.worker_max_output_bytes,
        )
        .with_clock(Clock::System);
        if delegation.permits_nested_delegation() {
            executor = executor.with_delegation(delegation.child_context());
        }
        // The worker acts for the same run: its tool calls, remote requests
        // and model turns check the run's authorization, and it shares the
        // run's cancellation token, so a revocation stops it too.
        let authorization = match &ctx.authorization {
            Some(authorization) => {
                executor = executor.with_authorization(authorization.clone());
                authorization.clone()
            }
            None => RunAuthorization::unconditional(ctx.cancel.clone()),
        };
        let provider = AuthorizedProvider::new(provider, authorization);

        let mut config = RunConfig::new(worker.routing.model.clone());
        config.system = Some(worker.persona.clone());
        config.limits = limits;
        config.parent = ctx.run.clone();

        let child = AgentLoop::new(provider, executor, config);
        let result = child.run(args.task, ctx.cancel.clone()).await;
        // A worker whose run was revoked while it worked hands nothing back,
        // not even a partial answer.
        if let Err(reason) = ctx.authorized() {
            return ToolOutcome::error(reason);
        }
        match result {
            Ok(RunOutcome::Completed { events }) => ToolOutcome::ok(worker_answer(&events)),
            Ok(RunOutcome::AwaitingApproval { .. }) => ToolOutcome::error(format!(
                "worker '{}' paused awaiting approval, which delegation cannot grant",
                profile_name
            )),
            // Unreachable under the default wrap-up policy a worker runs with;
            // handled so a pause could never be mistaken for an answer.
            Ok(RunOutcome::OutOfTime { .. }) => ToolOutcome::error(format!(
                "worker '{}' ran out of time before answering",
                profile_name
            )),
            Err(error) => ToolOutcome::error(format!("worker '{profile_name}' failed: {error}")),
        }
    }
}

/// Bound a worker's limits by the originating run's: the tighter of each.
fn within_ceiling(limits: RunLimits, ceiling: &RunLimits) -> RunLimits {
    let mut bounded = limits;
    bounded.max_turns = bounded.max_turns.min(ceiling.max_turns);
    bounded.max_tool_calls = bounded.max_tool_calls.min(ceiling.max_tool_calls);
    bounded.wall_clock_secs = match (bounded.wall_clock_secs, ceiling.wall_clock_secs) {
        (Some(own), Some(cap)) => Some(own.min(cap)),
        (None, cap) => cap,
        (own, None) => own,
    };
    bounded
}

/// Intersect the worker's own limits with the caller's optional caps, taking the
/// tighter of each — a delegate can only ever narrow what a worker allows.
fn intersect_caps(base: &RunLimits, max_turns: Option<u32>, max_seconds: Option<u64>) -> RunLimits {
    let mut limits = *base;
    if let Some(turns) = max_turns {
        limits.max_turns = limits.max_turns.min(turns);
    }
    if let Some(seconds) = max_seconds {
        limits.wall_clock_secs = Some(match limits.wall_clock_secs {
            Some(existing) => existing.min(seconds),
            None => seconds,
        });
    }
    limits
}

/// The worker's answer, followed by why it stopped when it did not finish the
/// answer itself, so the delegating model never takes a cut-short or
/// budget-limited answer for a whole one.
fn worker_answer(events: &[AgentEvent]) -> String {
    let answer = final_content(events);
    let stopped = events.iter().rev().find_map(|event| match event {
        AgentEvent::RunCompleted { reason } => Some(*reason),
        _ => None,
    });
    let early = stopped.filter(|reason| !reason.is_complete());
    match early.and_then(StopReason::explanation) {
        Some(why) if answer.is_empty() => format!("(The worker did not finish: {why}.)"),
        Some(why) => format!("{answer}\n\n(The worker did not finish its answer: {why}.)"),
        None => answer,
    }
}

/// The worker's visible answer: every [`AgentEvent::Content`] fragment joined.
fn final_content(events: &[AgentEvent]) -> String {
    let mut answer = String::new();
    for event in events {
        if let AgentEvent::Content { text } = event {
            answer.push_str(text);
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_caps_takes_the_tighter_bound() {
        let base = RunLimits {
            max_turns: 8,
            wall_clock_secs: Some(600),
            ..RunLimits::default()
        };
        let tightened = intersect_caps(&base, Some(2), Some(60));
        assert_eq!(tightened.max_turns, 2);
        assert_eq!(tightened.wall_clock_secs, Some(60));

        let loosened = intersect_caps(&base, Some(20), None);
        assert_eq!(
            loosened.max_turns, 8,
            "a cap cannot loosen the worker's own limit"
        );
    }

    #[test]
    fn final_content_joins_content_events() {
        let events = vec![
            AgentEvent::Content { text: "4".into() },
            AgentEvent::Content { text: "2".into() },
        ];
        assert_eq!(final_content(&events), "42");
    }

    #[test]
    fn a_worker_answer_says_when_the_worker_stopped_early() {
        let finished = vec![
            AgentEvent::Content { text: "42".into() },
            AgentEvent::RunCompleted {
                reason: StopReason::EndTurn,
            },
        ];
        assert_eq!(worker_answer(&finished), "42");
        let cut = vec![
            AgentEvent::Content {
                text: "The first half".into(),
            },
            AgentEvent::RunCompleted {
                reason: StopReason::Incomplete,
            },
        ];
        assert_eq!(
            worker_answer(&cut),
            "The first half\n\n(The worker did not finish its answer: the model stream ended early; the answer may be incomplete.)"
        );
        let nothing = vec![AgentEvent::RunCompleted {
            reason: StopReason::MaxTurns,
        }];
        assert_eq!(
            worker_answer(&nothing),
            "(The worker did not finish: the run reached its turn limit.)"
        );
    }
}
