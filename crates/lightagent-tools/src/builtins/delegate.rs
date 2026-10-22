//! `agent.delegate` — hand a bounded task to a worker.
//!
//! Orchestration without a second agent loop: this tool resolves the target
//! (a role, or the legacy worker profile) to a [`WorkerSpec`], builds a
//! **fresh** [`RunConfig`] (the worker's persona as system, the task as the
//! only user message, none of the orchestrator's history), and drives it
//! through the very same [`AgentLoop`]. [`run_worker`] is the one factory every
//! caller shares: the terminal, the API and ACP all reach it through this
//! tool.
//!
//! A worker's tools are the intersection of the run's worker tools (the
//! parent's effective tools, without delegation), the spec's definitional
//! ceiling, the worker profile's own `toolsets` under the run's profile
//! policy, and the model's `tool_scope`, which can only narrow. It is given
//! only the run contexts its tools need — the requesting profile's web,
//! workspace, remote-execution and skill contexts — never anything wider.
//! The originating run's authorization, cancellation and per-call policy
//! guard are re-checked at every worker model turn and tool call.
//!
//! It is [`RiskClass::Executable`](lightagent_core::RiskClass::Executable), so
//! the default policy pauses the run for approval before a worker ever starts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use lightagent_core::identity::{AuthorizedProvider, Denial, RunAuthorization};
use lightagent_core::{
    AgentEvent, AgentLoop, PolicyEngine, ProfileId, RiskClass, RunConfig, RunLimits, RunOutcome,
    Scope, StopReason, ToolOutcome, ToolProgress,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::{Clock, Delegation, SkillContext, ToolCtx};
use crate::definition::{Tool, ToolDefinition};
use crate::executor::BoundedExecutor;
use crate::registry::ToolRegistry;
use crate::worker::{WorkerSpec, WorkerTarget};

/// The schema a typed worker result declares.
pub const RESULT_SCHEMA: &str = "lightagent.worker_result.v1";

/// The most bytes of a worker's answer a typed result carries, so the result
/// stays a complete JSON document under the caller's output ceiling.
const MAX_ANSWER_BYTES: usize = 32_768;
/// The most sources a typed result lists.
const MAX_SOURCES: usize = 24;
/// Time kept back from the delegate timeout so a worker wraps up with what it
/// has instead of being cut off with nothing.
const WRAP_UP_MARGIN_SECS: u64 = 20;

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
        let target = match (args.profile.as_deref(), args.role.as_deref()) {
            (Some(_), Some(_)) => {
                return ToolOutcome::error("provide either profile or role, not both");
            }
            (Some(profile), None) => WorkerTarget::Profile(profile),
            (None, Some(role)) => {
                if !delegation.subagents.enabled {
                    return ToolOutcome::error("named subagent roles are disabled for this run");
                }
                WorkerTarget::Role(role)
            }
            (None, None) => return ToolOutcome::error("a worker profile or role is required"),
        };
        if let Err(error) = delegation.may_start_worker() {
            return ToolOutcome::error(error);
        }
        // Turning hierarchy on also caps legacy delegates. With the default
        // disabled policy their long-standing one-level behavior is unchanged.
        if delegation.subagents.enabled
            && let Err(error) = delegation.reserve_child()
        {
            return ToolOutcome::error(error);
        }
        let spec = match resolve(&delegation, target) {
            Ok(spec) => spec,
            Err(error) => return ToolOutcome::error(error),
        };
        let request = WorkerRequest {
            task: args.task,
            max_turns: args.max_turns,
            max_seconds: args.max_seconds,
            tool_scope: args.tool_scope,
        };
        run_worker(&delegation, spec, request, ctx).await
    }
}

/// The task a delegation hands a worker, with the caller's optional caps.
#[derive(Clone, Debug)]
pub struct WorkerRequest {
    pub task: String,
    pub max_turns: Option<u32>,
    pub max_seconds: Option<u64>,
    /// Tool names the caller restricts the worker to. Only ever narrows.
    pub tool_scope: Option<Vec<String>>,
}

/// Resolve `target` through the run's resolver, or the legacy way: a
/// profile by id, a role through the configured role-to-profile map.
fn resolve(delegation: &Delegation, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
    if let Some(resolver) = &delegation.resolver {
        return resolver.resolve(target);
    }
    let name = match target {
        WorkerTarget::Profile(profile) => profile,
        WorkerTarget::Role(role) => delegation
            .role_profile(role)
            .ok_or_else(|| format!("worker role '{role}' is not allowed"))?,
    };
    let id = match ProfileId::new(name) {
        Ok(id) => id,
        Err(error) => return Err(format!("invalid worker profile id: {error}")),
    };
    let worker = delegation
        .profiles
        .load(&id)
        .map_err(|error| format!("could not load worker profile '{name}': {error}"))?;
    Ok(WorkerSpec::from_profile(&worker, format!("profile:{name}")))
}

/// The tools a worker is offered: the run's worker tools, inside the spec's
/// ceiling, under the worker profile's own toolsets, narrowed by the
/// caller's scope. Nested delegation is added only where the spec, the depth
/// and the scope all allow it.
pub fn worker_tools(
    delegation: &Delegation,
    spec: &WorkerSpec,
    scope: Option<&[String]>,
) -> ToolRegistry {
    let base = delegation
        .worker_registry
        .clone()
        .without(AgentDelegate::NAME);
    let inside: Vec<String> = base
        .names()
        .into_iter()
        .filter(|name| spec.within_ceiling(name))
        .collect();
    let mut registry = base.scoped(&inside);
    if let Some(toolsets) = &spec.toolsets {
        let policy = crate::policy::resolve(
            &registry,
            toolsets,
            delegation.profile_policy,
            delegation.trust,
            &delegation.network_mcp_servers,
        );
        registry = policy.apply(&registry);
    }
    if let Some(scope) = scope {
        registry = registry.scoped(scope);
    }
    let nesting_in_scope = match scope {
        Some(scope) => scope.iter().any(|name| name == AgentDelegate::NAME),
        None => true,
    };
    if spec.nested && delegation.permits_nested_delegation() && nesting_in_scope {
        registry = registry.with(Arc::new(AgentDelegate::new()));
    }
    registry
}

/// Run one worker to a result: the shared worker-runtime factory.
pub async fn run_worker(
    delegation: &Delegation,
    spec: WorkerSpec,
    request: WorkerRequest,
    ctx: &ToolCtx,
) -> ToolOutcome {
    if let Some(validity) = &spec.validity
        && let Err(reason) = validity()
    {
        return ToolOutcome::error(reason);
    }
    let provider = match delegation.factory.provider(&spec.routing) {
        Ok(provider) => provider,
        Err(error) => {
            return ToolOutcome::error(format!("no provider for worker '{}': {error}", spec.label));
        }
    };
    let registry = worker_tools(delegation, &spec, request.tool_scope.as_deref());
    let offered = registry.names();

    let mut limits = intersect_caps(&spec.limits, request.max_turns, request.max_seconds);
    if let Some(ceiling) = &delegation.limit_ceiling {
        limits = within_ceiling(limits, ceiling);
    }
    // Leave room under the delegate timeout for the worker's own wrap-up turn,
    // so a slow worker returns what it found rather than nothing.
    let wrap_up = delegation
        .delegate_timeout
        .as_secs()
        .saturating_sub(WRAP_UP_MARGIN_SECS)
        .max(1);
    let own = limits.wall_clock_secs.unwrap_or(wrap_up);
    limits.wall_clock_secs = Some(own.min(wrap_up));
    let approval = match delegation.approval_floor {
        Some(floor) => spec.approval.stricter(floor),
        None => spec.approval,
    };

    let mut executor = BoundedExecutor::new(
        registry,
        PolicyEngine::new(approval.into()),
        delegation.worker_per_call,
        delegation.worker_max_output_bytes,
    )
    .with_clock(Clock::System);
    if delegation.permits_nested_delegation() && offered.iter().any(|n| n == AgentDelegate::NAME) {
        executor = executor.with_delegation(delegation.child_context());
    }
    // Only the contexts the worker's tools need: the requesting run's own.
    let needs = |prefixes: &[&str]| {
        offered
            .iter()
            .any(|name| prefixes.iter().any(|prefix| name.starts_with(prefix)))
    };
    if needs(&["web."])
        && let Some(web) = &ctx.web
    {
        executor = executor.with_web(web.clone());
    }
    if needs(&["fs.", "terminal."])
        && let Some(workspace) = &ctx.workspace
    {
        executor = executor.with_workspace(workspace.clone());
    }
    if needs(&["open_terminal."])
        && let Some(open_terminal) = &ctx.open_terminal
    {
        executor = executor.with_open_terminal(open_terminal.clone());
    }
    if needs(&["actions."])
        && let Some(actions) = &ctx.actions
    {
        executor = executor.with_actions(actions.clone());
    }
    let mut persona = spec.persona.clone();
    if needs(&["skill."]) {
        let mut skills = ctx
            .skills
            .as_ref()
            .map(|context| context.skills.as_ref().clone())
            .unwrap_or_default();
        for skill in &spec.skills {
            skills = skills.with_default(skill.clone());
        }
        if !skills.is_empty() {
            persona.push_str(&format!("\n\n{}", skills.catalog()));
            executor = executor.with_skills(SkillContext {
                skills: Arc::new(skills),
            });
        }
    }
    let parent_guard = delegation.call_guard.clone();
    let validity = spec.validity.clone();
    if parent_guard.is_some() || validity.is_some() {
        executor = executor.with_call_guard(Arc::new(move |name: &str| {
            parent_guard.as_ref().is_none_or(|guard| guard(name))
                && validity.as_ref().is_none_or(|valid| valid().is_ok())
        }));
    }

    // The worker acts for the same run: its tool calls, remote requests and
    // model turns check the run's authorization, and its cancellation is a
    // child of the run's, so a revocation or cancel stops it too. A worker
    // whose own resolution can be withdrawn (an assignment) is checked as well.
    let cancel = ctx.cancel.child_token();
    let parent = ctx
        .authorization
        .clone()
        .unwrap_or_else(|| RunAuthorization::unconditional(ctx.cancel.clone()));
    let authorization = match spec.validity.clone() {
        Some(validity) => {
            let run = parent.clone();
            RunAuthorization::revocable(
                Arc::new(move || {
                    run.verify()?;
                    validity().map_err(|_| Denial::ProfileNotGranted)
                }),
                cancel.clone(),
                parent.generation(),
            )
        }
        None => parent,
    };
    executor = executor.with_authorization(authorization.clone());
    let provider = AuthorizedProvider::new(provider, authorization);

    let mut config = RunConfig::new(spec.routing.model.clone());
    config.system = Some(persona);
    config.limits = limits;
    config.parent = ctx.run.clone();

    ctx.report(progress("worker.started", None));
    let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
    let child = AgentLoop::new(provider, executor, config);
    let run = child.run_streaming(request.task, cancel, sink);
    tokio::pin!(run);
    let mut tracker = Tracker::new(limits.max_tool_calls);
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            Some(event) = events.recv() => tracker.observe(&event, ctx),
        }
    };
    while let Ok(event) = events.try_recv() {
        tracker.observe(&event, ctx);
    }

    // A worker whose run was revoked, or whose assignment was withdrawn,
    // while it worked hands nothing back, not even a partial answer.
    if let Err(reason) = ctx.authorized() {
        ctx.report(progress("worker.cancelled", Some("revoked")));
        return ToolOutcome::error(reason);
    }
    if let Some(validity) = &spec.validity
        && let Err(reason) = validity()
    {
        ctx.report(progress("worker.cancelled", Some("revoked")));
        return ToolOutcome::error(reason);
    }

    let (outcome, status) = if spec.typed_result {
        typed_outcome(&spec, &offered, result, &tracker)
    } else {
        let outcome = legacy_outcome(&spec, result);
        let status = if outcome.is_error {
            "failed"
        } else {
            "completed"
        };
        (outcome, status)
    };
    ctx.report(progress(&format!("worker.{status}"), Some(status)));
    outcome
}

/// A progress report of `stage`, with the outcome on the last one.
fn progress(stage: &str, outcome: Option<&str>) -> ToolProgress {
    let report = ToolProgress::stage(stage);
    match outcome {
        Some(outcome) => report.with_outcome(outcome),
        None => report,
    }
}

/// The legacy result: the worker's bare answer, or a plain error.
fn legacy_outcome(
    spec: &WorkerSpec,
    result: Result<RunOutcome, lightagent_core::AgentError>,
) -> ToolOutcome {
    match result {
        Ok(RunOutcome::Completed { events }) => ToolOutcome::ok(worker_answer(&events)),
        Ok(RunOutcome::AwaitingApproval { .. }) => ToolOutcome::error(format!(
            "worker '{}' paused awaiting approval, which delegation cannot grant",
            legacy_name(spec)
        )),
        // Unreachable under the default wrap-up policy a worker runs with;
        // handled so a pause could never be mistaken for an answer.
        Ok(RunOutcome::OutOfTime { .. }) => ToolOutcome::error(format!(
            "worker '{}' ran out of time before answering",
            legacy_name(spec)
        )),
        Err(error) => ToolOutcome::error(format!("worker '{}' failed: {error}", legacy_name(spec))),
    }
}

/// The profile id a legacy label names.
fn legacy_name(spec: &WorkerSpec) -> &str {
    spec.label.strip_prefix("profile:").unwrap_or(&spec.label)
}

/// What a worker did, collected from its events as they stream.
struct Tracker {
    max_tool_calls: usize,
    /// Tool calls by id: name and arguments.
    calls: BTreeMap<String, (String, String)>,
    /// Finished calls, in order.
    finished: Vec<Finished>,
    turns: u32,
}

/// One finished worker tool call.
struct Finished {
    name: String,
    arguments: String,
    ok: bool,
    output: String,
}

impl Tracker {
    fn new(max_tool_calls: u32) -> Self {
        Self {
            max_tool_calls: max_tool_calls as usize,
            calls: BTreeMap::new(),
            finished: Vec::new(),
            turns: 0,
        }
    }

    /// Record `event` and report the worker's progress to the parent.
    fn observe(&mut self, event: &AgentEvent, ctx: &ToolCtx) {
        match event {
            AgentEvent::ToolCallRequested { call } => {
                let entry = (call.name.clone(), call.arguments.clone());
                self.calls.insert(call.id.clone(), entry);
            }
            AgentEvent::ToolCallStarted { .. } => {
                let report = progress("worker.tool", None);
                let started = self.finished.len() + 1;
                ctx.report(report.counted(started, self.max_tool_calls));
            }
            AgentEvent::ToolCallCompleted { id, outcome } => {
                let (name, arguments) = self.calls.get(id).cloned().unwrap_or_default();
                self.finished.push(Finished {
                    name,
                    arguments,
                    ok: !outcome.is_error,
                    output: outcome.content.clone(),
                });
            }
            AgentEvent::AwaitingApproval { .. } => {
                ctx.report(progress("worker.awaiting_approval", None));
            }
            AgentEvent::TurnCompleted { .. } => self.turns += 1,
            _ => {}
        }
    }
}

/// One tool the worker used, as reported in a typed result.
#[derive(Debug, Serialize)]
struct ToolUse {
    name: String,
    ok: bool,
}

/// The typed, bounded result of a role worker.
#[derive(Debug, Serialize)]
struct WorkerReport {
    schema: &'static str,
    /// `completed`, `partial`, `timed_out`, `blocked`, `failed` or `cancelled`.
    status: &'static str,
    worker: String,
    role: Option<&'static str>,
    model: String,
    model_source: &'static str,
    skills: Vec<String>,
    tools_offered: Vec<String>,
    stop_reason: Option<&'static str>,
    answer: String,
    /// Fetched pages and URLs that retrieval tools returned in this run.
    sources: Vec<String>,
    /// URLs the answer cites that no tool returned in this run.
    unverified_citations: Vec<String>,
    tool_calls: Vec<ToolUse>,
    /// The tool the worker was waiting to have approved, when blocked.
    blocked_on: Option<String>,
    model_turns: u32,
    limitations: Vec<String>,
    notes: Vec<&'static str>,
}

/// The typed result of a role worker. A failure is still a typed result, so
/// the lead never mistakes a failed or partial run for a verified answer.
fn typed_outcome(
    spec: &WorkerSpec,
    offered: &[String],
    result: Result<RunOutcome, lightagent_core::AgentError>,
    tracker: &Tracker,
) -> (ToolOutcome, &'static str) {
    let mut limitations = Vec::new();
    let mut blocked_on = None;
    let (status, stop, events) = match result {
        Ok(RunOutcome::Completed { events }) => {
            let stop = stop_reason(&events);
            let status = match stop {
                Some(StopReason::EndTurn) => "completed",
                Some(StopReason::Cancelled) => "cancelled",
                Some(StopReason::Error) => "failed",
                Some(StopReason::WallClockExceeded) => "timed_out",
                _ => "partial",
            };
            (status, stop, events)
        }
        Ok(RunOutcome::AwaitingApproval {
            events, request, ..
        }) => {
            limitations.push(format!(
                "the worker stopped before running '{}', which needs an approval a worker cannot obtain; it did not run",
                request.tool
            ));
            blocked_on = Some(request.tool);
            ("blocked", None, events)
        }
        Ok(RunOutcome::OutOfTime { events, .. }) => ("timed_out", None, events),
        Err(error) => {
            limitations.push(format!("the worker failed: {error}"));
            ("failed", None, Vec::new())
        }
    };
    if let Some(why) = stop.and_then(StopReason::explanation) {
        limitations.push(format!("the worker did not finish: {why}"));
    }
    let failed_tools: BTreeSet<&str> = tracker
        .finished
        .iter()
        .filter(|call| !call.ok)
        .map(|call| call.name.as_str())
        .collect();
    for name in failed_tools {
        limitations.push(format!("at least one '{name}' call failed"));
    }

    let answer = bounded(final_content(&events), MAX_ANSWER_BYTES);
    let mut sources = Vec::new();
    let mut returned = BTreeSet::new();
    for call in tracker.finished.iter().filter(|call| call.ok) {
        let fetch = call.name == "web.fetch";
        if fetch && let Some(url) = argument(&call.arguments, "url") {
            returned.insert(trim_url(&url).to_owned());
            if !sources.contains(&url) {
                sources.push(url);
            }
        }
        for url in urls_in(&call.output) {
            returned.insert(url.clone());
            if !fetch && !sources.contains(&url) {
                sources.push(url);
            }
        }
    }
    sources.truncate(MAX_SOURCES);
    let unverified_citations: Vec<String> = urls_in(&answer)
        .into_iter()
        .filter(|url| !returned.contains(url))
        .collect();
    if !unverified_citations.is_empty() {
        limitations.push(
            "the answer cites URLs no tool returned in this run; treat them as unverified"
                .to_owned(),
        );
    }
    let report = WorkerReport {
        schema: RESULT_SCHEMA,
        status,
        worker: spec.label.clone(),
        role: spec.role.map(|role| role.as_str()),
        model: spec.routing.model.clone(),
        model_source: spec.model_source,
        skills: spec.skill_labels(),
        tools_offered: offered.to_vec(),
        stop_reason: stop.map(stop_code),
        answer,
        sources,
        unverified_citations,
        tool_calls: tracker
            .finished
            .iter()
            .map(|call| ToolUse {
                name: call.name.clone(),
                ok: call.ok,
            })
            .collect(),
        blocked_on,
        model_turns: tracker.turns,
        limitations,
        notes: spec.notes.clone(),
    };
    let content = serde_json::to_string(&report)
        .unwrap_or_else(|_| format!("{{\"schema\":\"{RESULT_SCHEMA}\",\"status\":\"failed\"}}"));
    let is_error = matches!(status, "blocked" | "failed" | "cancelled");
    (ToolOutcome { content, is_error }, status)
}

/// One string argument of a call, if its arguments carry it.
fn argument(arguments: &str, key: &str) -> Option<String> {
    let arguments: Value = serde_json::from_str(arguments).ok()?;
    arguments.get(key)?.as_str().map(str::to_owned)
}

/// Why the run ended, from its last [`AgentEvent::RunCompleted`].
fn stop_reason(events: &[AgentEvent]) -> Option<StopReason> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::RunCompleted { reason } => Some(*reason),
        _ => None,
    })
}

/// A stable code for a stop reason.
fn stop_code(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTurns => "max_turns",
        StopReason::MaxToolCalls => "max_tool_calls",
        StopReason::RepeatedToolCalls => "repeated_tool_calls",
        StopReason::WallClockExceeded => "wall_clock_exceeded",
        StopReason::MaxTokens => "max_tokens",
        StopReason::Incomplete => "incomplete",
        StopReason::Cancelled => "cancelled",
        StopReason::Error => "error",
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
fn bounded(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The `http(s)://` URLs in `text`, in order, without duplicates.
fn urls_in(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for token in text.split(separates) {
        let Some(start) = token.find("http://").or_else(|| token.find("https://")) else {
            continue;
        };
        let url = trim_url(&token[start..]);
        if url.len() > "https://".len() && !urls.iter().any(|seen| seen == url) {
            urls.push(url.to_owned());
        }
    }
    urls
}

/// Whether `c` ends a URL written in prose or markup.
fn separates(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '`')
}

/// A URL without the punctuation that ends the sentence around it.
fn trim_url(url: &str) -> &str {
    url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*'])
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
    let early = stop_reason(events).filter(|reason| !reason.is_complete());
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

    #[test]
    fn urls_are_found_once_without_trailing_punctuation() {
        let text = "See https://a.test/x. Also (https://b.test/y), and https://a.test/x again; \
                    not ftp://c.test or https:// alone.";
        assert_eq!(urls_in(text), ["https://a.test/x", "https://b.test/y"]);
    }

    #[test]
    fn an_answer_is_bounded_on_a_character_boundary() {
        assert_eq!(bounded("héllo".to_owned(), 2), "h");
        assert_eq!(bounded("hello".to_owned(), 10), "hello");
    }
}
