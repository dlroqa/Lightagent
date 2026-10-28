//! Adaptive output allocation through the real `agent.delegate`: a planned
//! role's worker holds its share of the run's output pool, bounded by the
//! model it is actually routed to, and releases it when it ends.
//!
//! The models are scripted. These prove control logic only: what each worker
//! request is sent, what the pool's accounts hold afterwards, and that an
//! allocation grants no permission and lifts no bound.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    AgentProvider, ApprovalPolicy, Demand, FinishReason, GenerationConfig, MockProvider,
    ModelRouting, OutputPlan, OutputPool, PlannedShare, ProfileStore, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream, RunId, ServedModel, ShareReport, ToolOutcome,
};
use lightagent_tools::builtins::AgentDelegate;
use lightagent_tools::plan::RolePlan;
use lightagent_tools::{
    BuiltinResolver, Delegation, LeadGeneration, SubagentPolicy, Tool, ToolCtx, ToolRegistry,
    WorkerResolver, WorkerSpec, WorkerTarget, Workspace, WorkspaceContext, WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const LEAD: &str = "lead-model";

/// A scripted model that says which model it serves and with what window.
#[derive(Clone)]
struct Served {
    mock: MockProvider,
    served: ServedModel,
}

#[async_trait]
impl AgentProvider for Served {
    async fn served_model(&self) -> ServedModel {
        self.served.clone()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.mock.stream(request, cancel).await
    }
}

fn model(name: &str, window: Option<u32>, script: Vec<Vec<ProviderEvent>>) -> (String, Served) {
    let served = ServedModel {
        model: window.map(|_| name.to_owned()),
        context_window: window,
    };
    (
        name.to_owned(),
        Served {
            mock: MockProvider::new(script),
            served,
        },
    )
}

struct Models(HashMap<String, Served>);

impl lightagent_core::ProviderFactory for Models {
    fn provider(&self, routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        let served = self
            .0
            .get(routing.model.as_str())
            .cloned()
            .ok_or_else(|| ProviderError::Upstream(format!("no model {}", routing.model)))?;
        Ok(Arc::new(served))
    }
}

/// The built-in roles, each routed to the model the test names.
struct Routed {
    inner: BuiltinResolver,
    routes: HashMap<String, String>,
}

impl WorkerResolver for Routed {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        let mut spec = self.inner.resolve(target)?;
        if let Some(model) = spec.role.and_then(|role| self.routes.get(role.as_str())) {
            spec.routing.model = model.clone();
        }
        Ok(spec)
    }
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lightagent-adaptive-allocation-{}",
            RunId::new().as_str()
        ));
        std::fs::create_dir_all(path.join("src")).unwrap();
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A 32,000-token run pool, no per-request ceiling: what bounds each turn is
/// the worker's own window, or the 2,048 default when it has none.
fn lead() -> LeadGeneration {
    let settings = GenerationConfig {
        max_run_output_tokens: Some(32_000),
        ..GenerationConfig::default()
    };
    LeadGeneration {
        model: LEAD.to_owned(),
        budget: settings.budget_for(LEAD, Some(8_192)),
        settings: Some(Arc::new(settings)),
    }
}

/// Lead 8,000, follow-up 3,200, reviewer 15,600, researcher 5,200.
fn install(pool: &OutputPool) {
    pool.install(OutputPlan {
        source: "role_defaults".to_owned(),
        fallback_reason: None,
        shares: vec![
            PlannedShare {
                role: "reviewer".to_owned(),
                demand: Demand::Heavy,
            },
            PlannedShare {
                role: "researcher".to_owned(),
                demand: Demand::Light,
            },
        ],
        lead_floor: 2_048,
        follow_up: true,
    })
    .expect("installed");
}

fn delegation(
    home: &Path,
    models: Vec<(String, Served)>,
    routes: &[(&str, &str)],
    lead: LeadGeneration,
) -> Delegation {
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(home.join("home"))),
        Arc::new(Models(models.into_iter().collect())),
        ToolRegistry::builtin(),
        Duration::from_secs(5),
        262_144,
    );
    delegation.subagents = SubagentPolicy {
        enabled: true,
        ..SubagentPolicy::default()
    };
    let routing = ModelRouting {
        model: LEAD.to_owned(),
        ..ModelRouting::default()
    };
    delegation.resolver = Some(Arc::new(Routed {
        inner: BuiltinResolver::new(routing, ApprovalPolicy::Balanced),
        routes: routes
            .iter()
            .map(|(role, model)| ((*role).to_owned(), (*model).to_owned()))
            .collect(),
    }));
    delegation.generation.set(Some(lead));
    delegation
}

fn context(root: &Path) -> ToolCtx {
    std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
    ToolCtx::new(CancellationToken::new()).with_workspace(WorkspaceContext {
        workspace: Arc::new(Workspace::new(root).unwrap()),
        policy: Arc::new(WorkspacePolicy {
            max_file_bytes: 262_144,
            allow_terminal: false,
            terminal_timeout: Duration::from_secs(5),
            terminal_allowlist: Vec::new(),
        }),
    })
}

fn read() -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some("r".into()),
            name: Some("fs.read".into()),
            arguments: Some(json!({ "path": "src/a.rs" }).to_string()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        },
    ]
}

fn answer(text: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Content(text.into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]
}

async fn delegate(ctx: &ToolCtx, role: &str, extra: Value) -> (ToolOutcome, Value) {
    let mut args = json!({ "role": role, "task": format!("Do the {role}'s part.") });
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    let outcome = AgentDelegate::new().call(&args, ctx).await;
    let report = serde_json::from_str(&outcome.content).unwrap_or(Value::Null);
    (outcome, report)
}

fn sent(served: &Served) -> Vec<Option<u32>> {
    served
        .mock
        .requests()
        .iter()
        .map(|request| request.max_tokens)
        .collect()
}

fn share(pool: &OutputPool, role: &str) -> ShareReport {
    pool.allocation_report(None)
        .expect("a plan")
        .shares
        .into_iter()
        .find(|share| share.role == role)
        .expect("a planned role")
}

#[tokio::test]
async fn a_known_window_worker_is_bounded_by_its_share_not_the_unknown_default() {
    let scratch = Scratch::new();
    let lead = lead();
    let pool = lead.budget.output_pool.clone().unwrap();
    install(&pool);
    let (id, wide) = model("wide@16k", Some(16_384), vec![answer("reviewed")]);
    let worker = wide.clone();
    let ctx = context(&scratch.path).with_delegation(delegation(
        &scratch.path,
        vec![(id, wide)],
        &[("reviewer", "wide@16k")],
        lead,
    ));
    let (outcome, report) = delegate(&ctx, "reviewer", json!({})).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(report["model"], "wide@16k");
    // Its own window bounds the turn (the room it leaves), then its share:
    // never the 2,048 default meant for unknown capacity.
    let first = sent(&worker)[0].expect("a ceiling is sent");
    assert!(first > 2_048 && first <= 15_600, "{first}");
    let reviewer = share(&pool, "reviewer");
    assert_eq!(reviewer.state, "released", "its worker ended");
    assert_eq!(reviewer.left, 0);
    let after = pool.allocation_report(None).unwrap();
    assert_eq!(
        after.lead_reserve_left, 8_000,
        "the lead's reserve is whole"
    );
    assert_eq!(
        share(&pool, "researcher").left,
        5_200,
        "and the researcher's share"
    );
    // Charged in full (no usage was reported); what it did not reserve is
    // free for later work.
    assert_eq!(pool.spent(), first);
    assert_eq!(after.unallocated_available, 15_600 - first);
}

#[tokio::test]
async fn an_unknown_capacity_worker_keeps_the_default_and_passes_on_its_excess() {
    let scratch = Scratch::new();
    let lead = lead();
    let pool = lead.budget.output_pool.clone().unwrap();
    install(&pool);
    let (id, mystery) = model(
        "mystery",
        None,
        vec![answer("first review"), answer("second review")],
    );
    let worker = mystery.clone();
    let ctx = context(&scratch.path).with_delegation(delegation(
        &scratch.path,
        vec![(id, mystery)],
        &[("reviewer", "mystery")],
        lead,
    ));
    // Three turns at the 2,048 default can use at most 6,144: the rest of
    // the reviewer's 15,600 goes to the researcher, which has not started.
    let (outcome, _) = delegate(&ctx, "reviewer", json!({ "max_turns": 3 })).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(
        sent(&worker),
        [Some(2_048)],
        "never raised to fill the share"
    );
    let reviewer = share(&pool, "reviewer");
    assert_eq!(
        (reviewer.planned, reviewer.capacity_cap),
        (6_144, Some(6_144))
    );
    assert_eq!(share(&pool, "researcher").planned, 5_200 + 9_456);

    // The reviewer again (a re-check): the follow-up reserve, not a second
    // share.
    let (outcome, _) = delegate(&ctx, "reviewer", json!({ "max_turns": 3 })).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(sent(&worker), [Some(2_048), Some(2_048)]);
    let after = pool.allocation_report(None).unwrap();
    assert_eq!(after.follow_up_left, 3_200 - 2_048);
    assert_eq!(after.lead_reserve_left, 8_000);
    assert_eq!(share(&pool, "researcher").left, 14_656);
}

#[tokio::test]
async fn an_allocation_grants_no_permission() {
    let scratch = Scratch::new();
    let lead = lead();
    let pool = lead.budget.output_pool.clone().unwrap();
    install(&pool);
    let (id, reviewer) = model("checker", None, vec![answer("reviewed")]);
    let delegation = delegation(
        &scratch.path,
        vec![(id, reviewer.clone())],
        &[("reviewer", "checker")],
        lead,
    );
    delegation.plan.set(Some(RolePlan::new([
        ("researcher".to_owned(), Vec::new()),
        ("reviewer".to_owned(), vec!["researcher".to_owned()]),
    ])));
    let ctx = context(&scratch.path).with_delegation(delegation);
    // The plan still orders the workers: a planned share is not a
    // permission to start.
    let (outcome, _) = delegate(&ctx, "reviewer", json!({})).await;
    assert!(outcome.is_error, "{}", outcome.content);
    assert!(reviewer.mock.requests().is_empty());
    let planned = share(&pool, "reviewer");
    assert_eq!((planned.state.as_str(), planned.left), ("planned", 15_600));
}

#[tokio::test]
async fn a_worker_that_ends_without_an_answer_releases_its_share() {
    let scratch = Scratch::new();
    let lead = lead();
    let pool = lead.budget.output_pool.clone().unwrap();
    install(&pool);
    let (id, quiet) = model("quiet", None, Vec::new());
    let ctx = context(&scratch.path).with_delegation(delegation(
        &scratch.path,
        vec![(id, quiet)],
        &[("reviewer", "quiet")],
        lead,
    ));
    let _ = delegate(&ctx, "reviewer", json!({})).await;
    let reviewer = share(&pool, "reviewer");
    assert_eq!((reviewer.state.as_str(), reviewer.left), ("released", 0));
    // Charged its whole reservation (no usage), the rest is free again.
    assert_eq!(pool.spent(), 2_048);
    assert!(pool.estimated());
    let after = pool.allocation_report(None).unwrap();
    assert_eq!(after.unallocated_available, 15_600 - 2_048);
    assert_eq!(after.lead_reserve_left, 8_000);
}
