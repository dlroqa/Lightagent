//! Operational checks of the built-in specialist workers.
//!
//! The model is scripted, so these prove mechanics, not model competence:
//! the role a worker runs as, the real tool implementations and run contexts
//! it is given, what it may not do, and the typed result the lead receives.
//! The tools are real: `web.search` and `web.fetch` reach a loopback fixture
//! server through the egress guard, and `fs.*` act on a confined workspace.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    AgentProvider, ApprovalPolicy, FinishReason, MockProvider, ModelRouting, ProfileStore,
    ProviderError, ProviderEvent, Role, RunId, ToolOutcome,
};
use lightagent_tools::egress::Resolver;
use lightagent_tools::{
    BuiltinResolver, Delegation, SubagentPolicy, Tool, ToolCtx, ToolRegistry, WebContext,
    WebPolicy, WorkerResolver, WorkerSpec, WorkerTarget, Workspace, WorkspaceContext,
    WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Hands out clones of one scripted worker provider.
struct MockFactory {
    provider: MockProvider,
}

impl lightagent_core::ProviderFactory for MockFactory {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.provider.clone()))
    }
}

/// A scratch directory, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("lightagent-specialists-{}", RunId::new().as_str()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A loopback HTTP fixture: a body per path, recording each path.
struct Fixture {
    port: u16,
    paths: Arc<Mutex<Vec<String>>>,
}

/// Serve `pages(port, path)`, which knows the fixture's own port so a page
/// can link back to it.
async fn fixture(pages: impl Fn(u16, &str) -> Option<String> + Send + Sync + 'static) -> Fixture {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&paths);
    let pages = Arc::new(pages);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (log, pages) = (Arc::clone(&log), Arc::clone(&pages));
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&bytes).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                let response = match pages(port, &path) {
                    Some(body) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_owned()
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Fixture { port, paths }
}

/// A resolver answering from a fixed table.
struct Table {
    answers: HashMap<String, IpAddr>,
}

#[async_trait]
impl Resolver for Table {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        self.answers
            .get(host)
            .map(|ip| vec![SocketAddr::new(*ip, port)])
            .ok_or_else(|| std::io::Error::other("no such host"))
    }
}

/// An operator web context: `docs.test` is the loopback fixture (trusted),
/// `internal.test` a private address nobody trusted.
fn web(search: &Fixture) -> WebContext {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let policy = WebPolicy {
        trusted_networks: vec!["127.0.0.1/32".parse().unwrap()],
        timeout: Duration::from_secs(5),
        search_endpoint: Some(format!("http://127.0.0.1:{}/search", search.port)),
        ..WebPolicy::default()
    };
    let mut context = WebContext::new(client, Arc::new(policy));
    let answers = HashMap::from([
        ("docs.test".to_owned(), IpAddr::V4(Ipv4Addr::LOCALHOST)),
        (
            "internal.test".to_owned(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
        ),
    ]);
    context.resolver = Arc::new(Table { answers });
    context
}

/// A confined workspace holding `files`.
fn workspace(root: &std::path::Path, files: &[(&str, &str)]) -> WorkspaceContext {
    for (path, content) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    WorkspaceContext {
        workspace: Arc::new(Workspace::new(root).unwrap()),
        policy: Arc::new(WorkspacePolicy {
            max_file_bytes: 65_536,
            allow_terminal: false,
            terminal_timeout: Duration::from_secs(5),
            terminal_allowlist: Vec::new(),
        }),
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

/// A lead's delegation: every built-in tool the lead has, roles enabled,
/// resolved by the built-in catalog under a balanced lead.
fn lead(home: &std::path::Path, worker: MockProvider) -> Delegation {
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(home.to_path_buf())),
        Arc::new(MockFactory { provider: worker }),
        ToolRegistry::builtin(),
        Duration::from_secs(5),
        262_144,
    );
    delegation.subagents = SubagentPolicy {
        enabled: true,
        ..SubagentPolicy::default()
    };
    let routing = ModelRouting {
        model: "lead-model".to_owned(),
        ..ModelRouting::default()
    };
    let resolver = BuiltinResolver::new(routing, ApprovalPolicy::Balanced);
    delegation.resolver = Some(Arc::new(resolver));
    delegation
}

async fn delegate(ctx: &ToolCtx, role: &str, task: &str) -> (ToolOutcome, Value) {
    let outcome = lightagent_tools::builtins::AgentDelegate::new()
        .call(&json!({ "role": role, "task": task }), ctx)
        .await;
    let report = serde_json::from_str(&outcome.content).unwrap_or(Value::Null);
    (outcome, report)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_researcher_searches_and_fetches_through_the_runs_web_context() {
    let home = Scratch::new();
    let server = fixture(|port, path| {
        if path.starts_with("/search?") {
            let page = format!("http://docs.test:{port}/spec");
            let hit = json!({ "title": "Spec", "url": page, "content": "Specified." });
            Some(json!({ "results": [hit] }).to_string())
        } else if path == "/spec" {
            Some("The answer is 42, per section 3.".to_owned())
        } else {
            None
        }
    })
    .await;
    let page = format!("http://docs.test:{}/spec", server.port);
    let worker = MockProvider::new(vec![
        call("w1", "web.search", json!({ "query": "the answer" })),
        call("w2", "web.fetch", json!({ "url": page })),
        answer(&format!(
            "1. The answer is 42 ({page}). Also https://invented.test/claim (inference)."
        )),
    ]);
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_delegation(lead(&home.path, worker.clone()))
        .with_web(web(&server));
    let (outcome, report) = delegate(&ctx, "researcher", "What is the answer?").await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(report["schema"], "lightagent.worker_result.v1");
    assert_eq!(report["status"], "completed");
    assert_eq!(report["role"], "researcher");
    assert_eq!(report["worker"], "builtin:researcher@1");
    assert_eq!(report["model"], "lead-model");
    assert_eq!(strings(&report["skills"]), ["worker-researcher@1"]);
    // The tool calls really ran, against the fixture.
    let paths = server.paths.lock().unwrap().clone();
    assert!(
        paths.iter().any(|path| path.starts_with("/search?")),
        "{paths:?}"
    );
    assert!(paths.iter().any(|path| path == "/spec"), "{paths:?}");
    let calls = report["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["ok"] == true), "{calls:?}");
    // The fetched page is a source; the invented citation is flagged.
    assert_eq!(strings(&report["sources"]), std::slice::from_ref(&page));
    assert_eq!(
        strings(&report["unverified_citations"]),
        ["https://invented.test/claim"]
    );
    // A read-only role is never offered writes, commands or delegation, even
    // though the lead has them.
    let offered = strings(&report["tools_offered"]);
    for tool in [
        "fs.write",
        "terminal.run",
        "open_terminal.run",
        "agent.delegate",
    ] {
        assert!(!offered.contains(&tool.to_owned()), "{tool} in {offered:?}");
    }
    // The role skill reached the worker as its instructions.
    let requests = worker.requests();
    let system = &requests[0].messages[0];
    assert_eq!(system.role, Role::System);
    assert!(system.content.contains("# Role skill: worker-researcher@1"));
    assert!(
        system
            .content
            .contains("Never invent, complete or guess a citation")
    );
}

#[tokio::test]
async fn the_researchers_private_egress_is_refused_and_reported() {
    let home = Scratch::new();
    let server = fixture(|_, _| None).await;
    let worker = MockProvider::new(vec![
        call(
            "w1",
            "web.fetch",
            json!({ "url": "http://internal.test/admin" }),
        ),
        answer("No evidence was retrieved."),
    ]);
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_delegation(lead(&home.path, worker))
        .with_web(web(&server));
    let (outcome, report) = delegate(&ctx, "researcher", "Read the admin page.").await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(report["tool_calls"][0]["ok"], false);
    assert!(strings(&report["sources"]).is_empty());
    let limitations = strings(&report["limitations"]);
    assert!(
        limitations
            .iter()
            .any(|line| line.contains("'web.fetch' call failed")),
        "{limitations:?}"
    );
}

#[tokio::test]
async fn the_reviewer_reads_the_workspace_and_cannot_write_it() {
    let home = Scratch::new();
    let root = Scratch::new();
    let code = "pub fn ratio(a: u32, b: u32) -> u32 {\n    a / b\n}\n";
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_workspace(workspace(&root.path, &[("src/lib.rs", code)]));
    let worker = MockProvider::new(vec![
        call("w1", "fs.read", json!({ "path": "src/lib.rs" })),
        call(
            "w2",
            "fs.write",
            json!({ "path": "src/lib.rs", "content": "" }),
        ),
        answer("1. High: src/lib.rs:2 divides by `b` unchecked; `ratio(1, 0)` panics."),
    ]);
    let ctx = ctx.with_delegation(lead(&home.path, worker.clone()));
    let (outcome, report) = delegate(&ctx, "reviewer", "Review src/lib.rs.").await;
    assert!(!outcome.is_error, "{}", outcome.content);
    let calls = report["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["name"], "fs.read");
    assert_eq!(calls[0]["ok"], true);
    assert_eq!(calls[1]["name"], "fs.write");
    assert_eq!(calls[1]["ok"], false, "the reviewer has no fs.write");
    assert!(!strings(&report["tools_offered"]).contains(&"fs.write".to_owned()));
    // The file is untouched, and the worker saw its real content.
    assert_eq!(
        std::fs::read_to_string(root.path.join("src/lib.rs")).unwrap(),
        code
    );
    let requests = worker.requests();
    let read = requests[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .unwrap();
    assert!(read.content.contains("a / b"), "{}", read.content);
}

#[tokio::test]
async fn the_tester_is_offered_no_workflow_tools_without_a_broker() {
    let home = Scratch::new();
    let worker = MockProvider::new(vec![answer("No workflow tools; nothing verified.")]);
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(lead(&home.path, worker));
    let (_, report) = delegate(&ctx, "tester", "Verify the change.").await;
    let offered = strings(&report["tools_offered"]);
    assert!(
        offered.iter().all(|name| !name.starts_with("actions.")),
        "{offered:?}"
    );
    assert!(!offered.contains(&"terminal.run".to_owned()));
}

/// A resolver whose single worker loses its validity once `flag` is set.
struct Revocable {
    inner: BuiltinResolver,
    revoked: Arc<AtomicBool>,
}

impl WorkerResolver for Revocable {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        let mut spec = self.inner.resolve(target)?;
        let revoked = Arc::clone(&self.revoked);
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
async fn a_withdrawn_assignment_stops_the_worker_and_withholds_its_results() {
    let home = Scratch::new();
    let revoked = Arc::new(AtomicBool::new(false));
    // A tool the worker runs withdraws the assignment, as an owner would.
    struct Withdraw(Arc<AtomicBool>, lightagent_tools::ToolDefinition);
    #[async_trait]
    impl Tool for Withdraw {
        fn definition(&self) -> &lightagent_tools::ToolDefinition {
            &self.1
        }
        async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
            self.0.store(true, Ordering::SeqCst);
            ToolOutcome::ok("secret evidence")
        }
    }
    let definition = lightagent_tools::ToolDefinition::new(
        "datetime.now",
        "Stands in for any read tool.",
        json!({ "type": "object", "properties": {} }),
        lightagent_core::RiskClass::Observe,
        Vec::new(),
    );
    let worker = MockProvider::new(vec![
        call("w1", "datetime.now", json!({})),
        call("w2", "datetime.now", json!({})),
        answer("secret evidence"),
    ]);
    let mut delegation = lead(&home.path, worker.clone());
    delegation.worker_registry =
        ToolRegistry::new().with(Arc::new(Withdraw(Arc::clone(&revoked), definition)));
    let routing = ModelRouting {
        model: "lead-model".to_owned(),
        ..ModelRouting::default()
    };
    delegation.resolver = Some(Arc::new(Revocable {
        inner: BuiltinResolver::new(routing, ApprovalPolicy::Balanced),
        revoked: Arc::clone(&revoked),
    }));
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    let (outcome, _) = delegate(&ctx, "researcher", "Find it.").await;
    assert!(outcome.is_error);
    assert!(outcome.content.contains("withdrawn"), "{}", outcome.content);
    assert!(!outcome.content.contains("secret evidence"));
    // The worker made no model turn after the withdrawal.
    assert_eq!(worker.requests().len(), 1);
}

#[tokio::test]
async fn a_worker_is_refused_before_it_starts_when_its_policy_forbids_the_role() {
    let home = Scratch::new();
    let worker = MockProvider::new(vec![answer("x")]);
    let mut delegation = lead(&home.path, worker.clone());
    let mut resolver = BuiltinResolver::new(ModelRouting::default(), ApprovalPolicy::Balanced);
    resolver.roles.clear();
    delegation.resolver = Some(Arc::new(resolver));
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    let (outcome, _) = delegate(&ctx, "researcher", "x").await;
    assert!(outcome.is_error);
    assert!(outcome.content.contains("not allowed"));
    assert!(worker.requests().is_empty());
}

/// Resolves every target to a worker profile with no ceiling at all, as a
/// private assignment might.
struct Unbounded;

impl WorkerResolver for Unbounded {
    fn resolve(&self, _target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        let profile = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("wide").unwrap(),
            "Wide",
            "You may do anything.",
            "wide-model",
        );
        Ok(WorkerSpec::from_profile(
            &profile,
            "profile:wide".to_owned(),
        ))
    }
}

#[tokio::test]
async fn research_delegation_stays_a_read_only_researcher_whatever_resolves() {
    let home = Scratch::new();
    let worker = MockProvider::new(vec![
        call("w1", "fs.write", json!({ "path": "x", "content": "y" })),
        answer("Nothing written."),
    ]);
    let mut delegation = lead(&home.path, worker.clone());
    delegation.resolver = Some(Arc::new(Unbounded));
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    let outcome = lightagent_tools::builtins::AgentResearch::new()
        .call(&json!({ "task": "Look it up." }), &ctx)
        .await;
    let report: Value = serde_json::from_str(&outcome.content).unwrap();
    let offered = strings(&report["tools_offered"]);
    for tool in &offered {
        assert!(
            [
                "datetime.now",
                "web.search",
                "web.fetch",
                "rag.search",
                "rag.realtime",
                "skill.read"
            ]
            .contains(&tool.as_str()),
            "{tool} offered to a research worker"
        );
    }
    assert_eq!(
        report["tool_calls"][0]["ok"], false,
        "fs.write is not offered"
    );
    // Without the catalog, research delegation refuses rather than guess.
    let mut legacy = lead(&home.path, MockProvider::new(vec![answer("x")]));
    legacy.resolver = None;
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(legacy);
    let refused = lightagent_tools::builtins::AgentResearch::new()
        .call(&json!({ "task": "x" }), &ctx)
        .await;
    assert!(refused.is_error);
}

#[tokio::test]
async fn legacy_resolution_is_refused_to_an_untrusted_run() {
    let home = Scratch::new();
    let store = ProfileStore::new(home.path.clone());
    store
        .save(&lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("worker").unwrap(),
            "Worker",
            "You work.",
            "m",
        ))
        .unwrap();
    let worker = MockProvider::new(vec![answer("should not run")]);
    let mut delegation = lead(&home.path, worker.clone());
    delegation.resolver = None;
    delegation.trust = lightagent_core::identity::RunTrust::RemoteUser;
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    for args in [
        json!({ "profile": "worker", "task": "x" }),
        json!({ "role": "researcher", "task": "x" }),
    ] {
        let outcome = lightagent_tools::builtins::AgentDelegate::new()
            .call(&args, &ctx)
            .await;
        assert!(outcome.is_error, "{args}");
    }
    assert!(worker.requests().is_empty());
}

#[tokio::test]
async fn a_planned_tester_waits_for_the_coder_whose_change_it_verifies() {
    let home = Scratch::new();
    let worker = MockProvider::new(vec![answer("Patch proposed.")]);
    let delegation = lead(&home.path, worker);
    delegation
        .plan
        .set(Some(lightagent_tools::plan::RolePlan::new([
            ("coder".to_owned(), Vec::new()),
            ("tester".to_owned(), vec!["coder".to_owned()]),
        ])));
    let ctx = ToolCtx::new(CancellationToken::new()).with_delegation(delegation);
    // Refused before any worker starts: the scripted turns are untouched.
    let (early, _) = delegate(&ctx, "tester", "Run the tests on the patch.").await;
    assert!(early.is_error);
    assert!(
        early.content.contains("after the coder"),
        "{}",
        early.content
    );
    let (coded, report) = delegate(&ctx, "coder", "Fix the parser bug.").await;
    assert!(!coded.is_error, "{}", coded.content);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["answer"], "Patch proposed.");
    // The coder only said it made a change: there is no proposed patch to
    // bind a test to, so the tester is refused rather than told to verify
    // nothing. (Binding to a real patch is `coder_workflow.rs`.)
    let (tested, _) = delegate(&ctx, "tester", "Run the tests on the patch.").await;
    assert!(tested.is_error);
    assert!(
        tested.content.contains("proposed no change"),
        "{}",
        tested.content
    );
}
