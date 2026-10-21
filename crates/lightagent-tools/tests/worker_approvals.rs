//! A worker's paused action, decided by a person through the lead.
//!
//! Scripted model, real tools: the coder's `fs.write` really writes to a
//! confined workspace once approved, and never otherwise.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lightagent_core::{
    AgentProvider, ApprovalNeed, ApprovalPolicy, FinishReason, MockProvider, ModelRouting,
    PolicyEngine, ProfileStore, ProviderError, ProviderEvent, RunId, ToolCall, ToolInvoker,
    ToolOutcome,
};
use lightagent_tools::builtins::{AgentDeclineWorker, AgentDelegate, AgentResumeWorker};
use lightagent_tools::{
    BoundedExecutor, BuiltinResolver, Delegation, SubagentPolicy, Tool, ToolCtx, ToolRegistry,
    WorkerResolver, WorkerSpec, WorkerTarget, Workspace, WorkspaceContext, WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const BEFORE: &str = "pub fn one() -> u32 {\n    2\n}\n";
const AFTER: &str = "pub fn one() -> u32 {\n    1\n}\n";

struct MockFactory {
    provider: MockProvider,
}

impl lightagent_core::ProviderFactory for MockFactory {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.provider.clone()))
    }
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("lightagent-approvals-{}", RunId::new().as_str()));
        std::fs::create_dir_all(path.join("ws/src")).unwrap();
        std::fs::write(path.join("ws/src/lib.rs"), BEFORE).unwrap();
        Self { path }
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.path.join("ws/src/lib.rs")).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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
    vec![
        ProviderEvent::Content(text.into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]
}

/// The coder writes `AFTER`, then answers.
fn coder() -> MockProvider {
    MockProvider::new(vec![
        call(
            "w1",
            "fs.write",
            json!({ "path": "src/lib.rs", "content": AFTER }),
        ),
        answer("Changed one() to return 1."),
    ])
}

fn resolver() -> BuiltinResolver {
    let routing = ModelRouting {
        model: "lead-model".to_owned(),
        ..ModelRouting::default()
    };
    BuiltinResolver::new(routing, ApprovalPolicy::Balanced)
}

fn context(scratch: &Scratch, worker: MockProvider) -> ToolCtx {
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(scratch.path.join("home"))),
        Arc::new(MockFactory { provider: worker }),
        ToolRegistry::builtin(),
        Duration::from_secs(5),
        262_144,
    );
    delegation.subagents = SubagentPolicy {
        enabled: true,
        ..SubagentPolicy::default()
    };
    delegation.resolver = Some(Arc::new(resolver()));
    let workspace = WorkspaceContext {
        workspace: Arc::new(Workspace::new(scratch.path.join("ws")).unwrap()),
        policy: Arc::new(WorkspacePolicy {
            max_file_bytes: 65_536,
            allow_terminal: false,
            terminal_timeout: Duration::from_secs(5),
            terminal_allowlist: Vec::new(),
        }),
    };
    ToolCtx::new(CancellationToken::new())
        .with_workspace(workspace)
        .with_delegation(delegation)
}

fn report(outcome: &ToolOutcome) -> Value {
    serde_json::from_str(&outcome.content)
        .unwrap_or_else(|_| panic!("not a typed result: {}", outcome.content))
}

/// Delegate the change and return the parked worker's handle.
async fn park(ctx: &ToolCtx) -> String {
    let outcome = AgentDelegate::new()
        .call(
            &json!({ "role": "coder", "task": "Make one() return 1." }),
            ctx,
        )
        .await;
    let parked = report(&outcome);
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(parked["status"], "awaiting_approval");
    assert_eq!(parked["approval"]["tool"], "fs.write");
    parked["approval"]["handle"].as_str().unwrap().to_owned()
}

async fn resume(ctx: &ToolCtx, handle: &str) -> ToolOutcome {
    AgentResumeWorker::new()
        .call(&json!({ "handle": handle }), ctx)
        .await
}

#[tokio::test]
async fn an_approved_action_runs_exactly_once_and_only_after_approval() {
    let scratch = Scratch::new();
    let ctx = context(&scratch, coder());
    let handle = park(&ctx).await;
    assert_eq!(scratch.file(), BEFORE, "nothing ran before the approval");

    // The lead's approval prompt names the exact worker action, and asks
    // even in a session allowed to run without restrictions.
    let mut registry = ToolRegistry::new();
    registry.insert(Arc::new(AgentResumeWorker::new()));
    let executor = BoundedExecutor::new(
        registry,
        PolicyEngine::new(lightagent_core::permissions::ApprovalPolicy::permissive()),
        Duration::from_secs(5),
        262_144,
    )
    .with_delegation(ctx.delegation.clone().unwrap());
    executor.allow_without_restrictions();
    let resume_call = ToolCall {
        id: "c1".to_owned(),
        name: AgentResumeWorker::NAME.to_owned(),
        arguments: json!({ "handle": handle }).to_string(),
    };
    let ApprovalNeed::Require(request) = executor.approval_for(&resume_call) else {
        panic!("resuming a worker's action must ask a person");
    };
    let preview = request.arguments_preview;
    assert!(preview.contains("worker=builtin:coder@2"), "{preview}");
    assert!(preview.contains("tool=fs.write"), "{preview}");
    assert!(preview.contains("risk=mutating"), "{preview}");

    let finished = resume(&ctx, &handle).await;
    let result = report(&finished);
    assert!(!finished.is_error, "{}", finished.content);
    assert_eq!(result["status"], "completed");
    assert_eq!(scratch.file(), AFTER);
    // The handle was one-time: replaying it resumes nothing.
    let replay = resume(&ctx, &handle).await;
    assert!(replay.is_error);
    assert!(replay.content.contains("no worker is waiting"));
}

#[tokio::test]
async fn a_declined_action_never_runs_and_the_worker_continues() {
    let scratch = Scratch::new();
    let ctx = context(&scratch, coder());
    let handle = park(&ctx).await;
    let declined = AgentDeclineWorker::new()
        .call(&json!({ "handle": handle }), &ctx)
        .await;
    let result = report(&declined);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["tool_calls"][0]["ok"], false);
    assert_eq!(scratch.file(), BEFORE);
}

#[tokio::test]
async fn an_expired_or_cancelled_approval_resumes_nothing() {
    let scratch = Scratch::new();
    let mut ctx = context(&scratch, coder());
    if let Some(delegation) = ctx.delegation.as_mut() {
        delegation.approval_ttl = Duration::ZERO;
    }
    let handle = park(&ctx).await;
    let expired = resume(&ctx, &handle).await;
    assert!(expired.is_error);
    assert!(expired.content.contains("expired"), "{}", expired.content);
    assert_eq!(scratch.file(), BEFORE);

    let ctx = context(&scratch, coder());
    let handle = park(&ctx).await;
    ctx.cancel.cancel();
    let cancelled = resume(&ctx, &handle).await;
    assert!(cancelled.is_error);
    assert!(
        cancelled.content.contains("cancelled"),
        "{}",
        cancelled.content
    );
    assert_eq!(scratch.file(), BEFORE);
}

/// The built-in coder, whose authority is withdrawn once `revoked` is set.
struct Revocable(Arc<AtomicBool>);

impl WorkerResolver for Revocable {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        let mut spec = resolver().resolve(target)?;
        let revoked = Arc::clone(&self.0);
        spec.validity = Some(Arc::new(move || {
            if revoked.load(Ordering::SeqCst) {
                Err("the worker assignment was withdrawn".to_owned())
            } else {
                Ok(())
            }
        }));
        Ok(spec)
    }
}

#[tokio::test]
async fn a_withdrawn_authority_resumes_nothing() {
    let scratch = Scratch::new();
    let revoked = Arc::new(AtomicBool::new(false));
    let mut ctx = context(&scratch, coder());
    if let Some(delegation) = ctx.delegation.as_mut() {
        delegation.resolver = Some(Arc::new(Revocable(Arc::clone(&revoked))));
    }
    let handle = park(&ctx).await;
    revoked.store(true, Ordering::SeqCst);
    let refused = resume(&ctx, &handle).await;
    assert!(refused.is_error);
    assert!(refused.content.contains("withdrawn"), "{}", refused.content);
    assert_eq!(scratch.file(), BEFORE);
}
