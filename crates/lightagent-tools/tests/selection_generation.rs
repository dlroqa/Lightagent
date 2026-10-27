//! Specialist selection together with the generation budget: workers a plan
//! names (or the lead chooses) run through the real `agent.delegate`, and
//! each is budgeted against the model it is actually routed to, inside the
//! lead's run-wide bounds.
//!
//! The models are scripted. These prove the control logic of the combined
//! branch (#61, #62, #65 with #63, #64), not model behavior: which window a
//! worker is budgeted against, what a worker or the lead is sent, when a turn
//! or a continuation is withheld, and that a plan never lifts a bound.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    AgentLoop, AgentProvider, ApprovalDecision, ApprovalPolicy, FinishReason, GenerationConfig,
    MockProvider, ModelRouting, OutputPool, PolicyEngine, ProfileStore, ProviderError,
    ProviderEvent, ProviderRequest, ProviderStream, Role, RunConfig, RunId, RunLimits, RunOutcome,
    ServedModel, ToolOutcome,
};
use lightagent_tools::builtins::{AgentDelegate, PatchContext, PatchPropose};
use lightagent_tools::{
    BoundedExecutor, BuiltinResolver, Delegation, LeadGeneration, SubagentPolicy, Tool, ToolCtx,
    ToolRegistry, TreeBudget, WorkerResolver, WorkerSpec, WorkerTarget, Workspace,
    WorkspaceContext, WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const LEAD: &str = "lead-model";

/// A scripted model that also says which model it serves, and with what
/// window, as Lightweight would.
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
    let mock = MockProvider::new(script);
    (name.to_owned(), Served { mock, served })
}

/// Each routing reaches the scripted model of that name.
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

/// The built-in roles, with some roles routed to other models, as an
/// operator's role routes would.
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

/// A scratch directory, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lightagent-selection-generation-{}",
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

/// The operator's generation settings, with a per-request ceiling.
fn settings(max_output: u32, pool: Option<u32>, reserve: Option<u32>) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: Some(max_output),
        max_run_output_tokens: pool,
        reserve_answer_tokens: reserve,
        ..GenerationConfig::default()
    }
}

/// The lead's generation, resolved as `lightagent::generation::resolve`
/// does: the settings for the lead's model at the window it is served with.
fn lead_generation(settings: GenerationConfig, window: u32) -> LeadGeneration {
    LeadGeneration {
        model: LEAD.to_owned(),
        budget: settings.budget_for(LEAD, Some(window)),
        settings: Some(Arc::new(settings)),
    }
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
        ToolRegistry::builtin().with(Arc::new(PatchPropose::new())),
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

/// A workspace holding `files`, with room for large reads.
fn context(root: &Path, files: &[(&str, &str)]) -> ToolCtx {
    for (path, content) in files {
        std::fs::write(root.join(path), content).unwrap();
    }
    ToolCtx::new(CancellationToken::new())
        .with_workspace(WorkspaceContext {
            workspace: Arc::new(Workspace::new(root).unwrap()),
            policy: Arc::new(WorkspacePolicy {
                max_file_bytes: 262_144,
                allow_terminal: false,
                terminal_timeout: Duration::from_secs(5),
                terminal_allowlist: Vec::new(),
            }),
        })
        .with_patches(PatchContext::default())
}

fn call(id: &str, name: &str, arguments: Value) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some(name.into()),
            arguments: Some(arguments.to_string()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        },
    ]
}

fn answer(text: &str) -> Vec<ProviderEvent> {
    finished(text, FinishReason::Stop)
}

fn finished(text: &str, reason: FinishReason) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Content(text.into()),
        ProviderEvent::Finished {
            reason,
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

/// The tool result a model was shown in its `turn`-th request.
fn tool_result(mock: &MockProvider, turn: usize) -> String {
    mock.requests()[turn]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

/// About 60 KB of source: some 22,000 estimated tokens.
fn large_file() -> String {
    let mut text = String::new();
    let mut line = 0;
    while text.len() < 60_000 {
        text.push_str(&format!(
            "// line {line}: a long comment that makes the file large\n"
        ));
        line += 1;
    }
    text.push_str("// END-OF-FILE\n");
    text
}

#[tokio::test]
async fn each_worker_is_budgeted_for_the_window_of_the_model_it_runs_on() {
    let big = large_file();
    let read = || call("r", "fs.read", json!({ "path": "src/big.rs" }));
    for (name, window, shortened) in [("small@16k", 16_384, true), ("large@128k", 131_072, false)] {
        let scratch = Scratch::new();
        let (id, served) = model(name, Some(window), vec![read(), answer("reviewed")]);
        let worker = served.mock.clone();
        let lead = lead_generation(settings(1_024, None, None), 8_192);
        let ctx = context(&scratch.path, &[("src/big.rs", &big)]).with_delegation(delegation(
            &scratch.path,
            vec![(id, served)],
            &[("reviewer", name)],
            lead,
        ));
        let (outcome, report) = delegate(&ctx, "reviewer", json!({})).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(report["model"], name);
        // The same 60 KB read: shortened in place for the 16k model, whole for
        // the 128k one, though the lead itself has only 8k.
        let seen = tool_result(&worker, 1);
        if shortened {
            assert!(seen.len() < 45_000, "{name}: {} bytes", seen.len());
        } else {
            assert!(seen.contains("END-OF-FILE"), "{name}: {} bytes", seen.len());
        }
        // Never a larger allowance than the lead's ceiling.
        for request in worker.requests() {
            assert_eq!(request.max_tokens, Some(1_024), "{name}");
        }
    }
}

#[tokio::test]
async fn a_worker_whose_model_reports_no_window_is_neither_guarded_nor_continued() {
    let scratch = Scratch::new();
    let big = large_file();
    let (id, served) = model(
        "mystery",
        None,
        vec![
            call("r", "fs.read", json!({ "path": "src/big.rs" })),
            finished("cut off here", FinishReason::Length),
            answer("a continuation that must never be asked for"),
        ],
    );
    let worker = served.mock.clone();
    let lead = lead_generation(settings(1_024, None, None), 8_192);
    let ctx = context(&scratch.path, &[("src/big.rs", &big)]).with_delegation(delegation(
        &scratch.path,
        vec![(id, served)],
        &[("reviewer", "mystery")],
        lead,
    ));
    let (_, report) = delegate(&ctx, "reviewer", json!({})).await;
    // Unknown capacity: nothing is cut to fit a guessed window, the cut-off
    // answer is reported as such, and the ceiling still applies.
    assert!(tool_result(&worker, 1).contains("END-OF-FILE"));
    assert_eq!(report["stop_reason"], "max_tokens", "{report}");
    assert_eq!(worker.requests().len(), 2, "no continuation");
    assert!(
        worker
            .requests()
            .iter()
            .all(|r| r.max_tokens == Some(1_024))
    );
}

#[tokio::test]
async fn workers_draw_on_one_run_wide_pool_and_stop_when_it_is_spent() {
    let scratch = Scratch::new();
    // 600 tokens for the whole run, 256 per request, every turn charged its
    // whole reservation (no usage is reported).
    let lead = lead_generation(settings(256, Some(600), Some(64)), 8_192);
    let pool: OutputPool = lead.budget.output_pool.clone().unwrap();
    let (id, served) = model(
        LEAD,
        Some(8_192),
        vec![
            answer("one"),
            answer("two"),
            answer("three"),
            answer("never"),
        ],
    );
    let worker = served.mock.clone();
    let models = vec![(id, served)];
    let ctx = context(&scratch.path, &[]).with_delegation(delegation(
        &scratch.path,
        models.clone(),
        &[],
        lead.clone(),
    ));
    for role in ["researcher", "reviewer", "reviewer"] {
        let (outcome, _) = delegate(&ctx, role, json!({})).await;
        assert!(!outcome.is_error, "{}", outcome.content);
    }
    // The third worker is granted only what is left, never its full ceiling.
    let sent: Vec<Option<u32>> = worker.requests().iter().map(|r| r.max_tokens).collect();
    assert_eq!(sent, [Some(256), Some(256), Some(88)]);
    assert_eq!(pool.available(), 0);
    // A worker started once the pool is spent takes no turn at all.
    let ctx =
        context(&scratch.path, &[]).with_delegation(delegation(&scratch.path, models, &[], lead));
    let (_, report) = delegate(&ctx, "reviewer", json!({})).await;
    assert_eq!(report["stop_reason"], "output_budget", "{report}");
    assert_eq!(worker.requests().len(), 3);
}

#[tokio::test]
async fn a_workers_continuation_spends_its_turns_and_the_pool() {
    let script = || {
        vec![
            finished("part one, ", FinishReason::Length),
            answer("part two."),
        ]
    };
    // Room for it: the cut-off answer is continued once, answer-only.
    let scratch = Scratch::new();
    let (id, served) = model(LEAD, Some(8_192), script());
    let worker = served.mock.clone();
    let lead = lead_generation(settings(256, Some(2_000), Some(64)), 8_192);
    let pool = lead.budget.output_pool.clone().unwrap();
    let ctx = context(&scratch.path, &[]).with_delegation(delegation(
        &scratch.path,
        vec![(id, served)],
        &[],
        lead,
    ));
    let (_, report) = delegate(&ctx, "reviewer", json!({})).await;
    assert_eq!(report["stop_reason"], "end_turn", "{report}");
    let answer_text = report["answer"].as_str().unwrap();
    assert!(answer_text.contains("part one") && answer_text.contains("part two"));
    assert_eq!(worker.requests().len(), 2);
    assert!(
        worker.requests()[1].tools.is_empty(),
        "a continuation is answer-only"
    );
    assert_eq!(
        pool.spent(),
        512,
        "both turns are charged to the run's pool"
    );

    // A pool that cannot grant a continuation's minimum: none is taken.
    let scratch = Scratch::new();
    let (id, served) = model(LEAD, Some(8_192), script());
    let worker = served.mock.clone();
    let lead = lead_generation(settings(256, Some(300), Some(64)), 8_192);
    let ctx = context(&scratch.path, &[]).with_delegation(delegation(
        &scratch.path,
        vec![(id, served)],
        &[],
        lead,
    ));
    let (_, report) = delegate(&ctx, "reviewer", json!({})).await;
    assert_eq!(report["stop_reason"], "max_tokens", "{report}");
    assert_eq!(worker.requests().len(), 1);

    // A tree with one turn left for its workers: none for a continuation.
    let scratch = Scratch::new();
    let (id, served) = model(LEAD, Some(8_192), script());
    let worker = served.mock.clone();
    let lead = lead_generation(settings(256, None, Some(64)), 8_192);
    let mut tree = delegation(&scratch.path, vec![(id, served)], &[], lead);
    let limits = RunLimits {
        max_turns: 1,
        ..RunLimits::default()
    };
    tree.budget = Some(TreeBudget::new(&limits, 4));
    let ctx = context(&scratch.path, &[]).with_delegation(tree);
    let (_, report) = delegate(&ctx, "reviewer", json!({})).await;
    assert_ne!(report["stop_reason"], "end_turn", "{report}");
    assert_eq!(worker.requests().len(), 1, "the tree's one turn, no more");
}

const BEFORE: &str = "pub fn one() -> u32 {\n    2\n}\n";
const AFTER: &str = "pub fn one() -> u32 {\n    1\n}\n";

#[tokio::test]
async fn a_plan_admits_a_repair_round_but_never_lifts_the_run_wide_pool() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.path.join(".git/refs/heads")).unwrap();
    std::fs::write(scratch.path.join(".git/HEAD"), "ref: refs/heads/work\n").unwrap();
    std::fs::write(
        scratch.path.join(".git/refs/heads/work"),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .unwrap();
    let propose = |content: &str| {
        call(
            "c",
            "patch.propose",
            json!({ "summary": "s", "changes": [{ "path": "src/lib.rs", "content": content }] }),
        )
    };
    let (id, served) = model(
        LEAD,
        Some(8_192),
        vec![
            propose(AFTER),
            answer("coded"),
            answer("one test fails"),
            propose("pub fn one() -> u32 {\n    1 // repaired\n}\n"),
            answer("repaired"),
        ],
    );
    let worker = served.mock.clone();
    // Three turns of 256 before the repair: 768 of 780, leaving 12, which is
    // less than any turn may take.
    let lead = lead_generation(settings(256, Some(780), Some(64)), 8_192);
    let mut planned = delegation(&scratch.path, vec![(id, served)], &[], lead);
    planned.subagents.max_children_per_agent = 2;
    planned.subagents.max_repair_rounds = 1;
    planned
        .plan
        .set(Some(lightagent_tools::plan::RolePlan::new([
            ("coder".to_owned(), Vec::new()),
            ("tester".to_owned(), vec!["coder".to_owned()]),
        ])));
    let ctx = context(&scratch.path, &[("src/lib.rs", BEFORE)]).with_delegation(planned);
    let scope = json!({ "write_scope": ["src/lib.rs"] });
    let store = ctx.patches.as_ref().unwrap().store.clone();
    let (coded, _) = delegate(&ctx, "coder", scope.clone()).await;
    assert!(!coded.is_error, "{}", coded.content);
    let first = store.latest().unwrap().id;
    let (tested, _) = delegate(&ctx, "tester", scope.clone()).await;
    assert!(!tested.is_error, "{}", tested.content);
    // The plan admits the coder's repair round...
    let (repair, report) = delegate(&ctx, "coder", scope).await;
    assert!(
        !repair.content.contains("repair rounds"),
        "the plan admitted it: {}",
        repair.content
    );
    // ...but the pool has less than a turn left: no model request, no patch.
    assert_eq!(report["stop_reason"], "output_budget", "{report}");
    assert_eq!(worker.requests().len(), 3);
    assert_eq!(store.latest().unwrap().id, first);
}

#[tokio::test]
async fn a_plan_or_none_the_same_bounds_apply() {
    // With selection off, or Jev's classification failing, there is no plan;
    // with one, the plan orders roles only. The worker is sent the same.
    let mut sent = Vec::new();
    for plan in [false, true] {
        let scratch = Scratch::new();
        let (id, served) = model("large@128k", Some(131_072), vec![answer("done")]);
        let worker = served.mock.clone();
        let lead = lead_generation(settings(512, Some(10_000), None), 8_192);
        let routed = delegation(
            &scratch.path,
            vec![(id, served)],
            &[("reviewer", "large@128k")],
            lead,
        );
        if plan {
            routed
                .plan
                .set(Some(lightagent_tools::plan::RolePlan::new([(
                    "reviewer".to_owned(),
                    Vec::new(),
                )])));
        } else {
            routed.plan.set(None);
        }
        let ctx = context(&scratch.path, &[]).with_delegation(routed);
        let (outcome, _) = delegate(&ctx, "reviewer", json!({})).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        sent.push(worker.requests()[0].max_tokens);
    }
    assert_eq!(sent, [Some(512), Some(512)]);
}

#[tokio::test]
async fn the_leads_synthesis_fits_the_leads_window() {
    let scratch = Scratch::new();
    // A worker that answers at length: its answer reaches the lead bounded,
    // and the lead's 8k window shortens it in place for the synthesis turn.
    let long = "evidence ".repeat(6_000);
    let (id, served) = model(LEAD, Some(8_192), vec![answer(&long)]);
    let generation = lead_generation(settings(1_024, None, None), 8_192);
    let mut config = RunConfig::new(LEAD);
    config.generation = generation.budget.clone();
    let lead = MockProvider::new(vec![
        call(
            "d",
            "agent.delegate",
            json!({ "role": "reviewer", "task": "Review it." }),
        ),
        answer("Synthesized."),
    ]);
    let executor = BoundedExecutor::new(
        ToolRegistry::new().with(Arc::new(AgentDelegate::new())),
        PolicyEngine::new(lightagent_core::permissions::ApprovalPolicy::permissive()),
        Duration::from_secs(5),
        262_144,
    )
    .with_run(RunId::new())
    .with_delegation(delegation(
        &scratch.path,
        vec![(id, served)],
        &[],
        generation,
    ));
    let agent = AgentLoop::new(lead.clone(), executor, config);
    let outcome = agent
        .run("Review it.", CancellationToken::new())
        .await
        .unwrap();
    let outcome = match outcome {
        RunOutcome::AwaitingApproval {
            request, suspended, ..
        } => agent
            .resume(
                suspended,
                ApprovalDecision::grant(request.id),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        other => other,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let seen = tool_result(&lead, 1);
    assert!(!seen.is_empty());
    assert!(
        seen.len() < 30_000,
        "the worker's result was shortened for the lead: {} bytes",
        seen.len()
    );
}
