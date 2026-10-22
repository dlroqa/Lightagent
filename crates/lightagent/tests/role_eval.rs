//! The specialist role evaluation: `evaluation/subagents/roles.v1.json`.
//!
//! The set, its checks and its pass thresholds were recorded before any run.
//! The scoring rules are tested here deterministically. The live evaluation
//! runs real models through the real worker runtime and tools against local
//! fixtures; it needs an operator-approved endpoint
//! (`LIGHTAGENT_EVAL_BASE_URL`, `LIGHTAGENT_EVAL_MODEL`, and optionally
//! `LIGHTAGENT_EVAL_API_KEY`) and reports BLOCKED without one. An endpoint
//! that refuses dotted function names is evaluated with
//! `LIGHTAGENT_EVAL_TOOL_NAMES=portable`, the same mapping as
//! `inference.tool_names`. Its results measure the named model on these
//! tasks only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    AgentProvider, AllowedWorkflow, ApprovalPolicy, ModelRouting, ProfileStore, ProviderError,
    RunId,
};
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};
use lightagent_tools::builtins::{
    ActionsCancel, ActionsDispatch, ActionsLogs, ActionsStatus, AgentDelegate,
};
use lightagent_tools::egress::Resolver;
use lightagent_tools::{
    ActionsContext, ActionsPolicy, BuiltinResolver, Delegation, SubagentPolicy, Tool, ToolCtx,
    ToolRegistry, WebContext, WebPolicy, Workspace, WorkspaceContext, WorkspacePolicy,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Debug, Deserialize)]
struct EvaluationSet {
    schema: String,
    recorded: String,
    thresholds: Thresholds,
    tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
struct Thresholds {
    runs_per_task: u32,
    per_role_pass_rate: f64,
}

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    role: String,
    fixture: String,
    task: String,
    expect: Expect,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Expect {
    status_in: Vec<String>,
    answer_contains_all: Vec<String>,
    answer_contains_any: Vec<String>,
    answer_contains_none: Vec<String>,
    sources_include: Vec<String>,
    no_unverified_citations: bool,
    tool_called: Vec<String>,
}

fn evaluation_set() -> EvaluationSet {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evaluation/subagents/roles.v1.json");
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Every check of `expect` that `report` fails, in words.
fn score(expect: &Expect, report: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    let status = report["status"].as_str().unwrap_or("");
    if !expect.status_in.is_empty() && !expect.status_in.iter().any(|s| s == status) {
        failures.push(format!("status {status:?} not in {:?}", expect.status_in));
    }
    let answer = report["answer"].as_str().unwrap_or("").to_lowercase();
    let has = |needle: &String| answer.contains(&needle.to_lowercase());
    for needle in expect
        .answer_contains_all
        .iter()
        .filter(|needle| !has(needle))
    {
        failures.push(format!("answer lacks {needle:?}"));
    }
    if !expect.answer_contains_any.is_empty() && !expect.answer_contains_any.iter().any(&has) {
        failures.push(format!(
            "answer has none of {:?}",
            expect.answer_contains_any
        ));
    }
    for needle in expect
        .answer_contains_none
        .iter()
        .filter(|needle| has(needle))
    {
        failures.push(format!("answer has {needle:?}"));
    }
    let strings = |key: &str| -> Vec<String> {
        report[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let sources = strings("sources");
    for suffix in &expect.sources_include {
        if !sources
            .iter()
            .any(|source| source.ends_with(suffix.as_str()))
        {
            failures.push(format!("no source ending {suffix:?}"));
        }
    }
    if expect.no_unverified_citations && !strings("unverified_citations").is_empty() {
        failures.push("cites URLs no tool returned".to_owned());
    }
    let called: Vec<&str> = report["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for tool in &expect.tool_called {
        if !called.contains(&tool.as_str()) {
            failures.push(format!("never called {tool}"));
        }
    }
    failures
}

#[test]
fn the_evaluation_set_covers_every_role_with_recorded_thresholds() {
    let set = evaluation_set();
    assert_eq!(set.schema, "lightagent.role_eval.v1");
    assert_eq!(set.recorded, "2026-10-21");
    assert!(set.thresholds.runs_per_task >= 3);
    assert!(set.thresholds.per_role_pass_rate > 0.5);
    for role in ["researcher", "coder", "reviewer", "tester"] {
        assert!(set.tasks.iter().any(|task| task.role == role), "{role}");
    }
    let mut ids: Vec<&str> = set.tasks.iter().map(|task| task.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), set.tasks.len(), "task ids are unique");
}

#[test]
fn scoring_follows_the_recorded_rules() {
    let set = evaluation_set();
    let research = &set
        .tasks
        .iter()
        .find(|task| task.id == "res-1")
        .unwrap()
        .expect;
    let good = json!({
        "status": "completed",
        "answer": "Zephyr shipped on September 10, 2026 (http://docs.test/spec).",
        "sources": ["http://docs.test/spec"],
        "unverified_citations": [],
        "tool_calls": [{ "name": "web.fetch", "ok": true }],
    });
    assert!(
        score(research, &good).is_empty(),
        "{:?}",
        score(research, &good)
    );
    let invented = json!({
        "status": "completed",
        "answer": "September 10, 2026 per https://made.up/x",
        "sources": [],
        "unverified_citations": ["https://made.up/x"],
    });
    let failures = score(research, &invented);
    assert!(failures.iter().any(|f| f.contains("no source")));
    assert!(failures.iter().any(|f| f.contains("no tool returned")));
    let tester = &set
        .tasks
        .iter()
        .find(|task| task.id == "tst-1")
        .unwrap()
        .expect;
    let dispatched_only = json!({
        "status": "completed",
        "answer": format!("Run failed at {SHA}"),
        "tool_calls": [{ "name": "actions.dispatch", "ok": true }],
    });
    assert_eq!(
        score(tester, &dispatched_only),
        ["never called actions.status"]
    );
}

// --- live --------------------------------------------------------------------

struct Factory {
    base_url: String,
    api_key: Option<String>,
    tool_names: lightagent_core::ToolNameMode,
    /// Model requests made so far, and the most the run may make.
    used: Arc<AtomicU32>,
    max_requests: u32,
}

/// A provider that refuses every request past the run's ceiling, so a
/// pilot can never spend more than it was authorized to.
struct Counted {
    inner: Arc<dyn AgentProvider>,
    used: Arc<AtomicU32>,
    max_requests: u32,
}

#[async_trait]
impl AgentProvider for Counted {
    async fn stream(
        &self,
        request: lightagent_core::provider::ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<lightagent_core::provider::ProviderStream, ProviderError> {
        if self.used.fetch_add(1, Ordering::SeqCst) >= self.max_requests {
            return Err(ProviderError::Upstream(
                "the evaluation's request ceiling is reached".to_owned(),
            ));
        }
        self.inner.stream(request, cancel).await
    }
}

impl lightagent_core::ProviderFactory for Factory {
    fn provider(&self, routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        let mut config = ProviderConfig::new(self.base_url.clone(), routing.model.clone())
            .with_tool_names(self.tool_names);
        if let Some(key) = &self.api_key {
            config = config.with_api_key(key.clone());
        }
        Ok(Arc::new(Counted {
            inner: Arc::new(LightweightProvider::new(config)?),
            used: Arc::clone(&self.used),
            max_requests: self.max_requests,
        }))
    }
}

/// A loopback server answering `pages(port, method, path, body)`.
async fn serve(
    pages: impl Fn(u16, &str, &str, &Value) -> (u16, String, String) + Send + Sync + 'static,
) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let pages = Arc::new(pages);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let pages = Arc::clone(&pages);
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 8192];
                let end = loop {
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("/").to_owned();
                let length = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while bytes.len() < end + length {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                }
                let body = serde_json::from_slice(&bytes[end..]).unwrap_or(Value::Null);
                let (status, extra, text) = pages(port, &method, &path, &body);
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: text/plain\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

/// Web pages for a research fixture.
fn page(fixture: &str, port: u16, path: &str) -> Option<String> {
    let url = |page: &str| format!("http://docs.test:{port}/{page}");
    let results = |pages: &[(&str, &str)]| {
        let hits: Vec<Value> = pages
            .iter()
            .map(|(page, title)| json!({ "title": title, "url": url(page), "content": title }))
            .collect();
        json!({ "results": hits }).to_string()
    };
    match (fixture, path) {
        ("zephyr", search) if search.starts_with("/search") => {
            Some(results(&[("spec", "Zephyr release notes")]))
        }
        ("zephyr", "/spec") => Some(
            "Zephyr release notes. Zephyr shipped on September 10, 2026, after a beta in July."
                .to_owned(),
        ),
        ("quill-conflict", search) if search.starts_with("/search") => Some(results(&[
            ("a", "Quill documentation"),
            ("b", "Quill FAQ (older)"),
        ])),
        ("quill-conflict", "/a") => {
            Some("Quill service documentation: the default port is 8080.".to_owned())
        }
        ("quill-conflict", "/b") => {
            Some("Quill FAQ, older revision: Quill listens on port 9090 by default.".to_owned())
        }
        ("empty", search) if search.starts_with("/search") => {
            Some(json!({ "results": [] }).to_string())
        }
        _ => None,
    }
}

/// The workspace files of a fixture.
fn files(fixture: &str) -> Vec<(&'static str, &'static str)> {
    match fixture {
        "ratio" => vec![(
            "src/lib.rs",
            "/// The ratio of a to b.\npub fn ratio(a: u32, b: u32) -> u32 {\n    a / b\n}\n",
        )],
        "clean" => vec![(
            "src/clean.rs",
            "/// The sum, saturating at u32::MAX.\npub fn add(a: u32, b: u32) -> u32 {\n    a.saturating_add(b)\n}\n",
        )],
        "one" => vec![("src/lib.rs", "pub fn one() -> u32 {\n    2\n}\n")],
        _ => Vec::new(),
    }
}

/// A minimal GitHub stand-in: `fixture.yml` on `subagents/fixture` at SHA,
/// whose runs pass in mode pass and fail otherwise.
fn github(
    runs: &Mutex<Vec<Value>>,
    method: &str,
    path: &str,
    body: &Value,
) -> (u16, String, String) {
    let base = "/repos/o/r";
    let ok = |value: Value| (200, String::new(), value.to_string());
    let mut runs = runs.lock().unwrap();
    if path == format!("{base}/git/ref/heads/subagents/fixture") {
        return ok(json!({ "object": { "sha": SHA } }));
    }
    if path.starts_with(&format!("{base}/actions/workflows/fixture.yml/runs")) {
        return ok(json!({ "workflow_runs": *runs }));
    }
    if method == "POST" && path.ends_with("/dispatches") {
        let mode = body["inputs"]["mode"].as_str().unwrap_or("pass");
        let correlation = body["inputs"]["correlation"].as_str().unwrap_or("");
        let id = 101 + runs.len() as u64;
        let conclusion = if mode == "pass" { "success" } else { "failure" };
        runs.push(json!({
            "id": id,
            "html_url": format!("https://github.test/o/r/actions/runs/{id}"),
            "path": ".github/workflows/fixture.yml",
            "head_sha": SHA,
            "head_branch": "subagents/fixture",
            "event": "workflow_dispatch",
            "status": "completed",
            "conclusion": conclusion,
            "display_title": format!("fixture {mode} {correlation}"),
        }));
        return (204, String::new(), String::new());
    }
    if let Some(rest) = path.strip_prefix(&format!("{base}/actions/runs/")) {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let id: u64 = id.parse().unwrap_or(0);
        if let Some(run) = runs.iter().find(|run| run["id"] == id).cloned() {
            if tail.starts_with("jobs") {
                let job = json!({ "id": id * 10, "name": "verify", "status": "completed",
                    "conclusion": run["conclusion"], "steps": [] });
                return ok(json!({ "jobs": [job] }));
            }
            return ok(run);
        }
    }
    (
        404,
        String::new(),
        json!({ "message": "Not Found" }).to_string(),
    )
}

struct Table(HashMap<String, IpAddr>);

#[async_trait]
impl Resolver for Table {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        self.0
            .get(host)
            .map(|ip| vec![SocketAddr::new(*ip, port)])
            .ok_or_else(|| std::io::Error::other("no such host"))
    }
}

/// One run of `task` with `model`: the worker's typed result.
async fn run_task(task: &Task, model: &str, factory: Arc<Factory>, root: &Path) -> Value {
    lightagent_provider_lightweight::ensure_provider();
    let fixture = task.fixture.clone();
    let web_port = serve(move |port, _, path, _| match page(&fixture, port, path) {
        Some(text) => (200, String::new(), text),
        None => (404, String::new(), String::new()),
    })
    .await;
    let runs = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&runs);
    let github_port = serve(move |_, method, path, body| github(&shared, method, path, body)).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let policy = WebPolicy {
        trusted_networks: vec!["127.0.0.1/32".parse().unwrap()],
        search_endpoint: Some(format!("http://127.0.0.1:{web_port}/search")),
        ..WebPolicy::default()
    };
    let mut web = WebContext::new(client.clone(), Arc::new(policy));
    let hosts = HashMap::from([("docs.test".to_owned(), IpAddr::V4(Ipv4Addr::LOCALHOST))]);
    web.resolver = Arc::new(Table(hosts));

    let workspace_root = root.join(format!("ws-{}", RunId::new().as_str()));
    for (path, content) in files(&task.fixture) {
        let path = workspace_root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    std::fs::create_dir_all(&workspace_root).unwrap();
    let workspace = WorkspaceContext {
        workspace: Arc::new(Workspace::new(&workspace_root).unwrap()),
        policy: Arc::new(WorkspacePolicy {
            max_file_bytes: 65_536,
            allow_terminal: false,
            terminal_timeout: Duration::from_secs(5),
            terminal_allowlist: Vec::new(),
        }),
    };
    let actions = ActionsContext::new(
        client,
        ActionsPolicy {
            api_base: format!("http://127.0.0.1:{github_port}"),
            repository: "o/r".to_owned(),
            token: None,
            workflows: vec![AllowedWorkflow {
                file: "fixture.yml".to_owned(),
                refs: vec!["subagents/*".to_owned()],
                inputs: BTreeMap::from([(
                    "mode".to_owned(),
                    vec!["pass".to_owned(), "fail".to_owned()],
                )]),
                correlation_input: Some("correlation".to_owned()),
            }],
            commit_refs: Vec::new(),
            poll_interval: Duration::from_millis(200),
            max_wait: Duration::from_secs(10),
            max_log_bytes: 4096,
        },
    );
    let registry = ToolRegistry::builtin()
        .with(Arc::new(ActionsDispatch::new()))
        .with(Arc::new(ActionsStatus::new()))
        .with(Arc::new(ActionsLogs::new()))
        .with(Arc::new(ActionsCancel::new()));
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(root.to_path_buf())),
        factory,
        registry,
        Duration::from_secs(60),
        262_144,
    );
    delegation.delegate_timeout = Duration::from_secs(240);
    delegation.subagents = SubagentPolicy {
        enabled: true,
        ..SubagentPolicy::default()
    };
    let routing = ModelRouting {
        model: model.to_owned(),
        ..ModelRouting::default()
    };
    delegation.resolver = Some(Arc::new(BuiltinResolver::new(
        routing,
        ApprovalPolicy::Balanced,
    )));
    let ctx = ToolCtx::new(CancellationToken::new())
        .with_web(web)
        .with_workspace(workspace)
        .with_actions(actions)
        .with_delegation(delegation);
    let outcome = AgentDelegate::new()
        .call(&json!({ "role": task.role, "task": task.task }), &ctx)
        .await;
    serde_json::from_str(&outcome.content)
        .unwrap_or_else(|_| json!({ "status": "failed", "answer": outcome.content }))
}

#[tokio::test]
#[ignore = "live: needs an operator-approved model endpoint (LIGHTAGENT_EVAL_*)"]
async fn live_role_evaluation() {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let (Some(base_url), Some(models)) = (
        env("LIGHTAGENT_EVAL_BASE_URL"),
        env("LIGHTAGENT_EVAL_MODEL"),
    ) else {
        println!("BLOCKED live role evaluation: no approved model endpoint is configured");
        panic!("BLOCKED: LIGHTAGENT_EVAL_BASE_URL and LIGHTAGENT_EVAL_MODEL are required");
    };
    let set = evaluation_set();
    let tool_names = match env("LIGHTAGENT_EVAL_TOOL_NAMES") {
        Some(value) => value
            .parse()
            .expect("LIGHTAGENT_EVAL_TOOL_NAMES: native or portable"),
        None => lightagent_core::ToolNameMode::Native,
    };
    // A staged pilot runs fewer tasks, fewer runs and a hard request
    // ceiling; by default the recorded set in full, at most 8 requests a run.
    let only: Option<BTreeSet<String>> = env("LIGHTAGENT_EVAL_TASKS").map(|tasks| {
        tasks
            .split(',')
            .map(|task| task.trim().to_owned())
            .filter(|task| !task.is_empty())
            .collect()
    });
    let runs = env("LIGHTAGENT_EVAL_RUNS")
        .and_then(|runs| runs.parse::<u32>().ok())
        .map_or(set.thresholds.runs_per_task, |runs| {
            runs.clamp(1, set.thresholds.runs_per_task)
        });
    let tasks: Vec<&Task> = set
        .tasks
        .iter()
        .filter(|task| only.as_ref().is_none_or(|only| only.contains(&task.id)))
        .collect();
    assert!(
        !tasks.is_empty(),
        "LIGHTAGENT_EVAL_TASKS names no recorded task"
    );
    let model_count = models.split(',').filter(|m| !m.trim().is_empty()).count();
    let natural = u32::try_from(tasks.len() * model_count).unwrap_or(u32::MAX) * runs * 8;
    let max_requests = env("LIGHTAGENT_EVAL_MAX_REQUESTS")
        .and_then(|max| max.parse::<u32>().ok())
        .map_or(natural, |max| max.min(natural));
    let used = Arc::new(AtomicU32::new(0));
    let factory = Arc::new(Factory {
        base_url,
        api_key: env("LIGHTAGENT_EVAL_API_KEY"),
        tool_names,
        used: Arc::clone(&used),
        max_requests,
    });
    let mut stopped = false;
    let root: PathBuf =
        std::env::temp_dir().join(format!("lightagent-role-eval-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    let mut results = Vec::new();
    let mut below = Vec::new();
    for model in models
        .split(',')
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        let mut by_role: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for task in &tasks {
            for attempt in 0..runs {
                if used.load(Ordering::SeqCst) >= max_requests {
                    stopped = true;
                    break;
                }
                let report = run_task(task, model, Arc::clone(&factory), &root).await;
                let failures = score(&task.expect, &report);
                let entry = by_role.entry(task.role.clone()).or_default();
                entry.1 += 1;
                if failures.is_empty() {
                    entry.0 += 1;
                }
                println!(
                    "EVIDENCE {model} {} #{attempt}: {} {:?}",
                    task.id,
                    if failures.is_empty() { "pass" } else { "fail" },
                    failures
                );
                results.push(json!({
                    "model": model, "task": task.id, "attempt": attempt,
                    "passed": failures.is_empty(), "failures": failures, "report": report,
                }));
            }
        }
        for (role, (passed, total)) in by_role {
            let rate = f64::from(passed) / f64::from(total);
            println!("EVIDENCE {model} {role}: {passed}/{total} = {rate:.2}");
            if rate < set.thresholds.per_role_pass_rate {
                below.push(format!("{model} {role} {rate:.2}"));
            }
        }
    }
    if let Some(out) = env("LIGHTAGENT_EVAL_OUT") {
        std::fs::create_dir_all(&out).unwrap();
        // A pilot (fewer tasks or runs) or a run stopped at its ceiling is
        // evaluated, but it is not the recorded evaluation and cannot pass it.
        let complete = !stopped
            && tasks.len() == set.tasks.len()
            && runs == set.thresholds.runs_per_task;
        let status = match (stopped, below.is_empty(), complete) {
            (true, _, _) => "INCOMPLETE",
            (false, false, _) => "BELOW_THRESHOLD",
            (false, true, true) => "PASSED",
            (false, true, false) => "PILOT_ONLY",
        };
        let report = json!({
            "evaluation": "role_competence", "status": status, "evaluated": true,
            "counts_as_competence_evidence": status == "PASSED",
            "set": set.schema, "recorded": set.recorded, "results": results,
            "requests": used.load(Ordering::SeqCst), "request_ceiling": max_requests,
            "runs_per_task": runs, "stopped_at_ceiling": stopped,
        });
        std::fs::write(Path::new(&out).join("role-eval.json"), report.to_string()).unwrap();
    }
    let _ = std::fs::remove_dir_all(&root);
    println!(
        "EVIDENCE requests {} of {max_requests}",
        used.load(Ordering::SeqCst)
    );
    assert!(
        !stopped,
        "stopped at the request ceiling: the evaluation is incomplete, not a pass"
    );
    assert!(below.is_empty(), "below the recorded threshold: {below:?}");
}
