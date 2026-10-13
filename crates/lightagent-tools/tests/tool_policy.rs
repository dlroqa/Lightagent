//! The per-profile tool policy at execution: withheld tools cannot be invoked
//! by naming them, approvals are unchanged, and delegation never widens a run.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightagent_core::identity::RunTrust;
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{
    AgentEvent, AgentLoop, AgentProfile, AgentProvider, ApprovalNeed, FinishReason, MockProvider,
    ModelRouting, PolicyEngine, ProfileId, ProfileStore, ProviderError, ProviderEvent, RunConfig,
    RunId, RunLimits, RunOutcome, ToolCall, ToolInvoker, ToolOutcome, ToolPolicyMode,
};
use lightagent_tools::policy::{Withheld, resolve};
use lightagent_tools::{BoundedExecutor, CallGuard, Delegation, ToolRegistry};
use tokio_util::sync::CancellationToken;

struct MockFactory {
    provider: MockProvider,
}

impl lightagent_core::ProviderFactory for MockFactory {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.provider.clone()))
    }
}

struct ScratchHome {
    path: PathBuf,
}

impl ScratchHome {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!("lightagent-policy-{}", RunId::new().as_str())),
        }
    }
}

impl Drop for ScratchHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
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

fn executor(registry: ToolRegistry) -> BoundedExecutor {
    BoundedExecutor::new(
        registry,
        PolicyEngine::new(ApprovalPolicy::permissive()),
        Duration::from_secs(5),
        262_144,
    )
    .with_run(RunId::new())
}

fn completed_outcomes(outcome: RunOutcome) -> Vec<ToolOutcome> {
    outcome
        .into_events()
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect()
}

fn enforce(toolsets: &[&str]) -> lightagent_tools::policy::ToolPolicy {
    let toolsets: Vec<String> = toolsets.iter().map(|entry| (*entry).to_owned()).collect();
    resolve(
        &ToolRegistry::builtin(),
        &toolsets,
        ToolPolicyMode::Enforce,
        RunTrust::LocalOperator,
        &BTreeSet::new(),
    )
}

#[tokio::test]
async fn a_withheld_tool_cannot_be_invoked_by_naming_it() {
    let policy = enforce(&["datetime.now"]);
    let model = MockProvider::new(vec![
        tool_call_turn("call_1", "terminal.run", r#"{"command":"id"}"#),
        tool_call_turn("call_2", "fs.write", r#"{"path":"x","content":"y"}"#),
        final_turn("done"),
    ]);
    let agent = AgentLoop::new(
        model.clone(),
        executor(policy.apply(&ToolRegistry::builtin())),
        RunConfig::new("m"),
    );
    let outcome = agent.run("go", CancellationToken::new()).await.unwrap();
    assert!(
        outcome.is_completed(),
        "never paused for an approval it cannot grant"
    );
    let outcomes = completed_outcomes(outcome);
    assert_eq!(outcomes.len(), 2);
    for (outcome, name) in outcomes.iter().zip(["terminal.run", "fs.write"]) {
        assert!(outcome.is_error, "{name}");
        assert!(
            outcome.content.contains("is not available"),
            "{name}: {}",
            outcome.content
        );
    }
    // And the model was only ever shown the permitted tool.
    for request in model.requests() {
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, ["datetime.now"]);
    }
}

#[tokio::test]
async fn a_call_guard_withdraws_a_tool_mid_run() {
    let permitted = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&permitted);
    let guard: CallGuard = Arc::new(move |_name: &str| flag.load(Ordering::SeqCst));
    let executor = executor(ToolRegistry::builtin()).with_call_guard(guard);
    let call = ToolCall {
        id: "call_1".to_owned(),
        name: "datetime.now".to_owned(),
        arguments: "{}".to_owned(),
    };
    assert!(matches!(
        executor.approval_for(&call),
        ApprovalNeed::AutoApprove
    ));
    assert!(
        !executor
            .invoke(&call, CancellationToken::new())
            .await
            .is_error
    );
    // The policy changes: the next call is refused, before any prompt and at
    // invocation, although the tool is still in the registry.
    permitted.store(false, Ordering::SeqCst);
    match executor.approval_for(&call) {
        ApprovalNeed::Deny(reason) => assert!(reason.contains("no longer permitted"), "{reason}"),
        other => panic!("expected a denial, got {other:?}"),
    }
    let outcome = executor.invoke(&call, CancellationToken::new()).await;
    assert!(outcome.is_error);
    assert!(outcome.content.contains("no longer permitted"));
}

#[test]
fn approval_requirements_are_unchanged_for_permitted_tools() {
    let policy = enforce(&["fs.*", "terminal.run", "web.fetch", "datetime.now"]);
    for approval in [
        ApprovalPolicy::permissive(),
        ApprovalPolicy::balanced(),
        ApprovalPolicy::strict(),
    ] {
        let unrestricted = BoundedExecutor::new(
            ToolRegistry::builtin(),
            PolicyEngine::new(approval.clone()),
            Duration::from_secs(5),
            1024,
        );
        let restricted = BoundedExecutor::new(
            policy.apply(&ToolRegistry::builtin()),
            PolicyEngine::new(approval.clone()),
            Duration::from_secs(5),
            1024,
        );
        for (name, arguments) in [
            ("fs.write", r#"{"path":"a","content":"b"}"#),
            ("terminal.run", r#"{"command":"ls"}"#),
            ("web.fetch", r#"{"url":"https://example.com"}"#),
            ("datetime.now", "{}"),
        ] {
            let call = ToolCall {
                id: "c".to_owned(),
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            };
            let kind = |need: ApprovalNeed| match need {
                ApprovalNeed::AutoApprove => "auto",
                ApprovalNeed::Require(_) => "ask",
                ApprovalNeed::Deny(_) => "deny",
            };
            assert_eq!(
                kind(unrestricted.approval_for(&call)),
                kind(restricted.approval_for(&call)),
                "{name}: the policy narrows tools, never approvals"
            );
        }
    }
}

/// A `worker` profile with a permissive approval policy and generous limits.
fn worker_store(home: &std::path::Path) -> ProfileStore {
    let store = ProfileStore::new(home.to_path_buf());
    let mut profile = AgentProfile::new(
        ProfileId::new("worker").unwrap(),
        "Worker",
        "You fetch pages.",
        "worker-model",
    );
    profile.approval_policy = lightagent_core::ApprovalPolicy::Permissive;
    profile.limits.max_turns = 50;
    store.save(&profile).unwrap();
    store
}

async fn delegate_once(delegation: Delegation, worker: &MockProvider) -> ToolOutcome {
    let lead = MockProvider::new(vec![
        tool_call_turn(
            "call_1",
            "agent.delegate",
            r#"{"profile":"worker","task":"fetch it"}"#,
        ),
        final_turn("ok"),
    ]);
    let executor = BoundedExecutor::new(
        ToolRegistry::builtin(),
        PolicyEngine::new(ApprovalPolicy::permissive()),
        Duration::from_secs(5),
        262_144,
    )
    .with_run(RunId::new())
    .with_delegation(delegation);
    let agent = AgentLoop::new(lead, executor, RunConfig::new("lead"));
    let paused = agent.run("go", CancellationToken::new()).await.unwrap();
    let RunOutcome::AwaitingApproval {
        request, suspended, ..
    } = paused
    else {
        panic!("delegation asks first");
    };
    let outcome = agent
        .resume(
            suspended,
            lightagent_core::ApprovalDecision::grant(request.id),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let _ = worker;
    completed_outcomes(outcome)
        .into_iter()
        .next()
        .expect("the delegate call completed")
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

#[tokio::test]
async fn a_worker_is_offered_no_tool_its_run_was_denied() {
    let home = ScratchHome::new();
    let policy = enforce(&["datetime.now", "agent.delegate"]);
    let worker = MockProvider::new(vec![final_turn("done")]);
    let delegation = delegation(
        worker_store(&home.path),
        worker.clone(),
        policy.worker_registry(&ToolRegistry::worker_default()),
    );
    let outcome = delegate_once(delegation, &worker).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    for request in worker.requests() {
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["datetime.now"],
            "not the installation's worker defaults"
        );
    }
}

#[tokio::test]
async fn a_worker_runs_under_the_stricter_of_its_and_its_runs_approval_policy() {
    // The worker profile is permissive, so web.fetch would auto-run. With the
    // originating run's strict policy as the floor it needs an approval,
    // which delegation cannot grant.
    let home = ScratchHome::new();
    let script = || {
        MockProvider::new(vec![
            tool_call_turn("w1", "web.fetch", r#"{"url":"https://example.com"}"#),
            final_turn("fetched"),
        ])
    };
    let worker = script();
    let mut floored = delegation(
        worker_store(&home.path),
        worker.clone(),
        ToolRegistry::worker_default(),
    );
    floored.approval_floor = Some(lightagent_core::ApprovalPolicy::Strict);
    let outcome = delegate_once(floored, &worker).await;
    assert!(outcome.is_error);
    assert!(
        outcome.content.contains("awaiting approval"),
        "{}",
        outcome.content
    );

    // Without a floor, the worker's own permissive policy applies, as before.
    let worker = script();
    let legacy = delegation(
        worker_store(&home.path),
        worker.clone(),
        ToolRegistry::worker_default(),
    );
    let outcome = delegate_once(legacy, &worker).await;
    assert!(
        !outcome.content.contains("awaiting approval"),
        "{}",
        outcome.content
    );
}

#[tokio::test]
async fn a_worker_never_exceeds_its_runs_limits() {
    // The worker profile allows 50 turns; the originating run allows one.
    let home = ScratchHome::new();
    let worker = MockProvider::new(vec![
        tool_call_turn("w1", "datetime.now", "{}"),
        tool_call_turn("w2", "datetime.now", "{}"),
        final_turn("never reached"),
    ]);
    let mut bounded = delegation(
        worker_store(&home.path),
        worker.clone(),
        ToolRegistry::worker_default(),
    );
    bounded.limit_ceiling = Some(RunLimits {
        max_turns: 1,
        ..RunLimits::default()
    });
    let outcome = delegate_once(bounded, &worker).await;
    assert!(
        worker.requests().len() <= 2,
        "a one-turn ceiling stops the worker early: {} requests",
        worker.requests().len()
    );
    assert!(
        !outcome.content.contains("never reached"),
        "{}",
        outcome.content
    );
}

#[test]
fn nested_delegation_inherits_every_restriction() {
    let home = ScratchHome::new();
    let store = ProfileStore::new(home.path.clone());
    let narrowed = enforce(&["datetime.now"]).worker_registry(&ToolRegistry::worker_default());
    let mut parent = delegation(store, MockProvider::new(vec![]), narrowed);
    parent.limit_ceiling = Some(RunLimits {
        max_turns: 2,
        ..RunLimits::default()
    });
    parent.approval_floor = Some(lightagent_core::ApprovalPolicy::Strict);
    let child = parent.child_context();
    let grandchild = child.child_context();
    for level in [&child, &grandchild] {
        assert_eq!(level.worker_registry.names(), ["datetime.now"]);
        assert_eq!(level.limit_ceiling.map(|limits| limits.max_turns), Some(2));
        assert_eq!(
            level.approval_floor,
            Some(lightagent_core::ApprovalPolicy::Strict)
        );
    }
    assert_eq!(grandchild.depth, 2);
}

#[test]
fn a_withheld_reason_is_reported_for_every_removed_tool() {
    let policy = enforce(&["datetime.now"]);
    let removed: BTreeSet<_> = policy
        .withheld
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let expected: BTreeSet<_> = ToolRegistry::builtin()
        .names()
        .into_iter()
        .filter(|name| name != "datetime.now")
        .collect();
    assert_eq!(removed, expected);
    assert!(
        policy
            .withheld
            .iter()
            .all(|(_, why)| *why == Withheld::NotInProfile)
    );
}
