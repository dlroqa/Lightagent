//! Seeded regressions for the specialist-subagent baseline.
//!
//! Each test states the behavior a delegated worker must have and is ignored
//! with a `seeded:` reason while the gap it names is open, so the required
//! gate stays green and the `subagents` workflow's seeded job reports which
//! gaps remain. A stage that closes a gap removes its `ignore` in the same
//! change. See `docs/subagents/SUBAGENT_IMPLEMENTATION_PROGRESS.md`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    AgentProfile, AgentProvider, FinishReason, MockProvider, ModelRouting, ProfileId,
    ProfileStore, ProviderError, ProviderEvent, RiskClass, Role, RunId, RunLimits, SkillStore,
    ToolOutcome,
};
use lightagent_tools::{
    Delegation, SkillContext, SubagentPolicy, SubagentRole, Tool, ToolCtx, ToolDefinition,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Hands out clones of one scripted worker provider, so a test can inspect
/// every request a worker run made.
struct MockFactory {
    provider: MockProvider,
}

impl lightagent_core::ProviderFactory for MockFactory {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.provider.clone()))
    }
}

/// A unique scratch home, removed on drop.
struct ScratchHome {
    path: PathBuf,
}

impl ScratchHome {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lightagent-subagent-baseline-{}",
            RunId::new().as_str()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for ScratchHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// An observe-only tool that counts how often a worker ran it.
struct Tick {
    definition: ToolDefinition,
    count: Arc<AtomicUsize>,
}

impl Tick {
    fn new(count: Arc<AtomicUsize>) -> Self {
        Self {
            definition: ToolDefinition::new(
                "count.tick",
                "Count one tick.",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                RiskClass::Observe,
                Vec::new(),
            ),
            count,
        }
    }
}

#[async_trait]
impl Tool for Tick {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        self.count.fetch_add(1, Ordering::SeqCst);
        ToolOutcome::ok("tick")
    }
}

fn tool_call_turn(id: &str, name: &str, arguments: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some(name.into()),
            arguments: Some(arguments.into()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        },
    ]
}

fn final_turn(text: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Content(text.into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]
}

/// A `worker` profile in a scratch home, adjusted by `edit`.
fn worker_store(home: &std::path::Path, edit: impl FnOnce(&mut AgentProfile)) -> ProfileStore {
    let store = ProfileStore::new(home.to_path_buf());
    let mut profile = AgentProfile::new(
        ProfileId::new("worker").unwrap(),
        "Worker",
        "You are a worker.",
        "worker-model",
    );
    edit(&mut profile);
    store.save(&profile).unwrap();
    store
}

fn delegation(store: ProfileStore, worker: MockProvider, registry: ToolRegistry) -> Delegation {
    Delegation::new(
        Arc::new(store),
        Arc::new(MockFactory { provider: worker }),
        registry,
        Duration::from_secs(5),
        262_144,
    )
}

/// Hierarchical delegation on, with the `researcher` role served by `worker`.
fn with_roles(mut delegation: Delegation, max_depth: u8) -> Delegation {
    let mut policy = SubagentPolicy {
        enabled: true,
        max_depth,
        ..SubagentPolicy::default()
    };
    policy.roles.insert(
        "researcher".to_owned(),
        SubagentRole {
            profile: "worker".to_owned(),
        },
    );
    delegation.subagents = policy;
    delegation
}

/// The tool names the worker's first model request offered.
fn offered(worker: &MockProvider) -> Vec<String> {
    let requests = worker.requests();
    let first = requests.first().expect("the worker was called");
    let mut names: Vec<String> = first.tools.iter().map(|tool| tool.name.clone()).collect();
    names.sort();
    names
}

/// Call `agent.delegate` directly, as an approved call would run it.
async fn delegate(ctx: &ToolCtx, args: Value) -> ToolOutcome {
    lightagent_tools::builtins::AgentDelegate::new().call(&args, ctx).await
}

fn skills_with(name: &str, body: &str) -> SkillStore {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-subagent-skill-{}",
        RunId::new().as_str()
    ));
    let skill = dir.join(name);
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A test skill\n---\n{body}"),
    )
    .unwrap();
    let store = SkillStore::load(std::slice::from_ref(&dir));
    std::fs::remove_dir_all(&dir).ok();
    store
}

/// Gap 3: a worker is offered `skill.read` but is not given the run's skill
/// context, so the call it is offered can only fail.
#[tokio::test]
#[ignore = "seeded: gap 3 (child contexts), closed in stage C"]
async fn seeded_a_worker_reads_the_skills_its_run_has() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |_| {});
    let worker = MockProvider::new(vec![
        tool_call_turn("w1", "skill.read", r#"{"name":"method"}"#),
        final_turn("done"),
    ]);
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_delegation(delegation(store, worker.clone(), ToolRegistry::worker_default()))
        .with_skills(SkillContext {
            skills: Arc::new(skills_with("method", "Cite every source.")),
        });
    let outcome = delegate(&ctx, json!({ "profile": "worker", "task": "t" })).await;
    assert!(!outcome.is_error, "{outcome:?}");
    let requests = worker.requests();
    let result = requests
        .last()
        .and_then(|request| request.messages.iter().find(|m| m.role == Role::Tool))
        .expect("the worker saw its skill.read result");
    assert!(
        result.content.contains("Cite every source."),
        "the worker's skill.read failed: {}",
        result.content
    );
}

/// Gap 5: the worker profile's own `toolsets` do not bound the tools it is
/// offered.
#[tokio::test]
#[ignore = "seeded: gap 5 (worker toolsets), closed in stage B"]
async fn seeded_a_worker_profiles_toolsets_bound_its_tools() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |profile| {
        profile.toolsets = vec!["datetime.now".to_owned()];
    });
    let worker = MockProvider::new(vec![final_turn("done")]);
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation(
        store,
        worker.clone(),
        ToolRegistry::worker_default(),
    ));
    let outcome = delegate(&ctx, json!({ "profile": "worker", "task": "t" })).await;
    assert!(!outcome.is_error, "{outcome:?}");
    assert_eq!(offered(&worker), ["datetime.now"]);
}

/// A model-supplied `tool_scope` may only narrow; nested delegation adds
/// `agent.delegate` back after the scope was applied.
#[tokio::test]
#[ignore = "seeded: tool_scope widening, closed in stage B"]
async fn seeded_a_tool_scope_is_never_widened() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |_| {});
    let worker = MockProvider::new(vec![final_turn("done")]);
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(with_roles(
        delegation(store, worker.clone(), ToolRegistry::worker_default()),
        2,
    ));
    let outcome = delegate(
        &ctx,
        json!({ "role": "researcher", "task": "t", "tool_scope": ["datetime.now"] }),
    )
    .await;
    assert!(!outcome.is_error, "{outcome:?}");
    assert_eq!(offered(&worker), ["datetime.now"]);
}

/// The depth bound: with `max_depth = 1` only the lead (depth 0) may start a
/// worker. A depth-1 execution starting one would create depth 2.
#[tokio::test]
#[ignore = "seeded: depth off-by-one, closed in stage B"]
async fn seeded_no_worker_starts_below_the_maximum_depth() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |_| {});
    let worker = MockProvider::new(vec![final_turn("too deep")]);
    let lead = with_roles(
        delegation(store, worker.clone(), ToolRegistry::worker_default()),
        1,
    );
    // What the depth-1 worker's executor is given.
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(lead.child_context());
    let outcome = delegate(&ctx, json!({ "role": "researcher", "task": "t" })).await;
    assert!(outcome.is_error, "a depth-2 worker started: {outcome:?}");
    assert!(worker.requests().is_empty());
}

/// Gap 6: every worker gets the whole remaining parent allowance, so two
/// workers together exceed the run's tool-call ceiling.
#[tokio::test]
#[ignore = "seeded: gap 6 (tree budget), closed in stage E"]
async fn seeded_workers_share_one_tool_call_budget() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |_| {});
    let ticks = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new().with(Arc::new(Tick::new(Arc::clone(&ticks))));
    let tick = || tool_call_turn("t", "count.tick", "{}");
    let worker = MockProvider::new(vec![
        tick(),
        tick(),
        final_turn("first"),
        tick(),
        tick(),
        final_turn("second"),
    ]);
    let mut delegation = delegation(store, worker, registry);
    delegation.limit_ceiling = Some(RunLimits {
        max_tool_calls: 2,
        ..RunLimits::default()
    });
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    let task = json!({ "profile": "worker", "task": "tick twice" });
    let _ = delegate(&ctx, task.clone()).await;
    let _ = delegate(&ctx, task).await;
    assert!(
        ticks.load(Ordering::SeqCst) <= 2,
        "workers made {} tool calls under a ceiling of 2",
        ticks.load(Ordering::SeqCst)
    );
}

/// Gap 7: a worker's progress never reaches the parent while it works.
#[tokio::test]
#[ignore = "seeded: gap 7 (worker progress), closed in stage E"]
async fn seeded_a_worker_reports_its_lifecycle_to_the_parent() {
    let home = ScratchHome::new();
    let store = worker_store(&home.path, |_| {});
    let worker = MockProvider::new(vec![final_turn("done")]);
    let (sink, mut reports) = tokio::sync::mpsc::unbounded_channel();
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_delegation(delegation(store, worker, ToolRegistry::worker_default()))
        .with_progress(sink);
    let outcome = delegate(&ctx, json!({ "profile": "worker", "task": "t" })).await;
    assert!(!outcome.is_error, "{outcome:?}");
    let mut stages = Vec::new();
    while let Ok(report) = reports.try_recv() {
        stages.push(report.stage);
    }
    assert!(
        stages.iter().any(|stage| stage.starts_with("worker.")),
        "no worker lifecycle reported: {stages:?}"
    );
}
