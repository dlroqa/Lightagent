//! The GitHub Actions workflow broker, against a loopback stand-in for the
//! REST API. Real tool implementations; only GitHub is simulated. The live
//! check against github.com is the `subagents` workflow's broker job.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightagent_core::{
    AgentProvider, AllowedWorkflow, ApprovalPolicy, FinishReason, MockProvider, ModelRouting,
    ProfileStore, ProviderError, ProviderEvent, RunId, ToolOutcome,
};
use lightagent_tools::builtins::{ActionsCancel, ActionsDispatch, ActionsLogs, ActionsStatus};
use lightagent_tools::{
    ActionsContext, ActionsPolicy, BuiltinResolver, Delegation, SubagentPolicy, Tool, ToolCtx,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "synthetic-broker-token-not-a-secret";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const BRANCH: &str = "subagents/fixture";

/// One request the stand-in received.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
}

/// The stand-in's state: requests seen and runs created.
#[derive(Default)]
struct Api {
    seen: Vec<Seen>,
    runs: Vec<Value>,
}

struct GitHub {
    port: u16,
    state: Arc<Mutex<Api>>,
}

impl GitHub {
    fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }
}

fn run(id: u64, path: &str, conclusion: &str, title: &str) -> Value {
    json!({
        "id": id,
        "html_url": format!("https://github.test/o/r/actions/runs/{id}"),
        "path": path,
        "head_sha": HEAD,
        "head_branch": BRANCH,
        "event": "workflow_dispatch",
        "status": "completed",
        "conclusion": conclusion,
        "display_title": title,
    })
}

/// Answer one request; `(status, extra headers, body)`.
fn answer(api: &mut Api, port: u16, seen: &Seen) -> (u16, String, String) {
    let path = seen.path.as_str();
    let base = "/repos/o/r";
    let json = |value: Value| (200, String::new(), value.to_string());
    if path == format!("{base}/git/ref/heads/{BRANCH}") {
        return json(json!({ "object": { "sha": HEAD, "type": "commit" } }));
    }
    if path.starts_with(&format!("{base}/actions/workflows/fixture.yml/runs?")) {
        let runs: Vec<Value> = api
            .runs
            .iter()
            .filter(|run| run["path"] == ".github/workflows/fixture.yml")
            .cloned()
            .collect();
        return json(json!({ "workflow_runs": runs }));
    }
    if path == format!("{base}/actions/workflows/fixture.yml/dispatches") && seen.method == "POST" {
        let mode = seen.body["inputs"]["mode"].as_str().unwrap_or("pass");
        let correlation = seen.body["inputs"]["correlation"].as_str().unwrap_or("");
        let id = 101 + api.runs.len() as u64;
        let conclusion = if mode == "pass" { "success" } else { "failure" };
        let title = format!("fixture {mode} {correlation}");
        let path = ".github/workflows/fixture.yml";
        api.runs.push(run(id, path, conclusion, &title));
        return (204, String::new(), String::new());
    }
    if let Some(rest) = path.strip_prefix(&format!("{base}/actions/runs/")) {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let id: u64 = id.parse().unwrap_or(0);
        let found = if id == 999 {
            Some(run(999, ".github/workflows/other.yml", "success", "other"))
        } else {
            api.runs.iter().find(|run| run["id"] == id).cloned()
        };
        let Some(found) = found else {
            return (404, String::new(), json!({ "message": "Not Found" }).to_string());
        };
        if tail.starts_with("jobs") {
            let conclusion = found["conclusion"].clone();
            let job = json!({
                "id": id * 10,
                "name": "verify",
                "status": "completed",
                "conclusion": conclusion,
                "steps": [{ "name": "Check", "conclusion": conclusion }],
            });
            return json(json!({ "jobs": [job] }));
        }
        if tail == "cancel" {
            return (202, String::new(), "{}".to_owned());
        }
        return json(found);
    }
    if let Some(rest) = path.strip_prefix(&format!("{base}/actions/jobs/")) {
        let id = rest.trim_end_matches("/logs");
        let location = format!("location: http://127.0.0.1:{port}/blob/{id}\r\n");
        return (302, location, String::new());
    }
    if path.starts_with("/blob/") {
        return (200, String::new(), "step 1 ok\nFAIL: expected 1, got 2\n".to_owned());
    }
    (404, String::new(), json!({ "message": "Not Found" }).to_string())
}

async fn github() -> GitHub {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(Api::default()));
    let shared = Arc::clone(&state);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let state = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 8192];
                let head_end = loop {
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("/").to_owned();
                let headers: BTreeMap<String, String> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .get("content-length")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                while bytes.len() < head_end + length {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                }
                let body = serde_json::from_slice(&bytes[head_end..]).unwrap_or(Value::Null);
                let seen = Seen {
                    method,
                    path,
                    authorization: headers.get("authorization").cloned(),
                    body,
                };
                let (status, extra, body) = {
                    let mut api = state.lock().unwrap();
                    api.seen.push(seen.clone());
                    answer(&mut api, port, &seen)
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    GitHub { port, state }
}

fn broker(github: &GitHub) -> ActionsContext {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let workflow = AllowedWorkflow {
        file: "fixture.yml".to_owned(),
        refs: vec!["subagents/*".to_owned()],
        inputs: BTreeMap::from([(
            "mode".to_owned(),
            vec!["pass".to_owned(), "fail".to_owned()],
        )]),
        correlation_input: Some("correlation".to_owned()),
    };
    ActionsContext::new(
        client,
        ActionsPolicy {
            api_base: format!("http://127.0.0.1:{}", github.port),
            repository: "o/r".to_owned(),
            token: Some(TOKEN.to_owned()),
            workflows: vec![workflow],
            poll_interval: Duration::from_millis(50),
            max_wait: Duration::from_secs(5),
            max_log_bytes: 4096,
        },
    )
}

fn ctx(github: &GitHub) -> ToolCtx {
    ToolCtx::new(CancellationToken::new()).with_actions(broker(github))
}

fn report(outcome: &ToolOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.content);
    serde_json::from_str(&outcome.content).unwrap()
}

async fn dispatch(ctx: &ToolCtx, mode: &str) -> ToolOutcome {
    let args = json!({
        "workflow": "fixture.yml",
        "ref": BRANCH,
        "sha": HEAD,
        "inputs": { "mode": mode },
    });
    ActionsDispatch::new().call(&args, ctx).await
}

#[tokio::test]
async fn a_dispatch_is_correlated_to_its_exact_sha_and_never_repeated() {
    let github = github().await;
    let ctx = ctx(&github);
    let first = report(&dispatch(&ctx, "pass").await);
    assert_eq!(first["status"], "dispatched");
    assert_eq!(first["run_id"], 101);
    assert_eq!(first["sha"], HEAD);
    let correlation = first["correlation"].as_str().unwrap().to_owned();
    assert!(correlation.starts_with("la-"));
    assert!(first["note"].as_str().unwrap().contains("not a pass"));
    let posted: Vec<Seen> = github
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "POST")
        .collect();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].body["ref"], BRANCH);
    assert_eq!(posted[0].body["inputs"]["mode"], "pass");
    assert_eq!(posted[0].body["inputs"]["correlation"], correlation.as_str());
    assert_eq!(
        posted[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // The same dispatch again is the same run, not a second one.
    let again = report(&dispatch(&ctx, "pass").await);
    assert_eq!(again["run_id"], 101);
    assert_eq!(again["repeated"], true);
    let posts = github
        .seen()
        .iter()
        .filter(|seen| seen.method == "POST")
        .count();
    assert_eq!(posts, 1);
}

#[tokio::test]
async fn a_passing_and_a_failing_run_are_reported_as_they_are() {
    let github = github().await;
    let ctx = ctx(&github);
    let pass = report(&dispatch(&ctx, "pass").await);
    let fail = report(&dispatch(&ctx, "fail").await);
    let status = |run: &Value| json!({ "run_id": run["run_id"], "wait_seconds": 1 });
    let passed = report(&ActionsStatus::new().call(&status(&pass), &ctx).await);
    assert_eq!(passed["verdict"], "passed");
    assert_eq!(passed["head_sha"], HEAD);
    assert_eq!(passed["jobs"][0]["conclusion"], "success");
    let failed = report(&ActionsStatus::new().call(&status(&fail), &ctx).await);
    assert_eq!(failed["verdict"], "failed");
    assert_eq!(failed["head_sha"], HEAD);
    assert_eq!(failed["jobs"][0]["failed_steps"], json!(["Check"]));

    let job = failed["jobs"][0]["id"].clone();
    let args = json!({ "run_id": fail["run_id"], "job_id": job });
    let log = ActionsLogs::new().call(&args, &ctx).await;
    assert!(!log.is_error, "{}", log.content);
    assert!(log.content.contains("FAIL: expected 1, got 2"));
    assert!(log.content.contains("untrusted data"));
    // The pre-signed download never receives the token.
    let blob = github
        .seen()
        .into_iter()
        .find(|seen| seen.path.starts_with("/blob/"))
        .unwrap();
    assert_eq!(blob.authorization, None);
    let cancel = ActionsCancel::new()
        .call(&json!({ "run_id": pass["run_id"] }), &ctx)
        .await;
    assert!(!cancel.is_error, "{}", cancel.content);
}

#[tokio::test]
async fn unauthorized_workflows_refs_inputs_and_stale_shas_are_refused() {
    let github = github().await;
    let ctx = ctx(&github);
    let refused = |args: Value| {
        let ctx = ctx.clone();
        async move { ActionsDispatch::new().call(&args, &ctx).await }
    };
    let cases = [
        (
            json!({ "workflow": "other.yml", "ref": BRANCH, "sha": HEAD }),
            "not allowlisted",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": "main", "sha": HEAD }),
            "not allowlisted",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": "subagents/../main", "sha": HEAD }),
            "not allowlisted",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": BRANCH, "sha": HEAD,
                    "inputs": { "mode": "rm -rf /" } }),
            "not allowlisted",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": BRANCH, "sha": HEAD,
                    "inputs": { "correlation": "la-forged" } }),
            "not allowlisted",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": BRANCH, "sha": "abc" }),
            "40-character",
        ),
        (
            json!({ "workflow": "fixture.yml", "ref": BRANCH, "sha": "f".repeat(40) }),
            "stale",
        ),
    ];
    for (args, why) in cases {
        let outcome = refused(args.clone()).await;
        assert!(outcome.is_error, "{args} was accepted");
        assert!(outcome.content.contains(why), "{args}: {}", outcome.content);
    }
    assert!(
        github.seen().iter().all(|seen| seen.method != "POST"),
        "nothing was dispatched"
    );
    // A run of another workflow cannot be read or cancelled through the broker.
    let other = json!({ "run_id": 999 });
    for outcome in [
        ActionsStatus::new().call(&other, &ctx).await,
        ActionsCancel::new().call(&other, &ctx).await,
    ] {
        assert!(outcome.is_error);
        assert!(outcome.content.contains("not a run of an allowlisted workflow"));
    }
    // No result ever carries the token.
    let ok = dispatch(&ctx, "pass").await;
    assert!(!ok.content.contains(TOKEN));
}

#[tokio::test]
async fn without_a_broker_the_tools_refuse() {
    let ctx = ToolCtx::new(CancellationToken::new());
    let outcome = ActionsStatus::new().call(&json!({ "run_id": 1 }), &ctx).await;
    assert!(outcome.is_error);
    assert!(outcome.content.contains("no workflow broker"));
}

/// Hands out clones of one scripted worker provider.
struct MockFactory {
    provider: MockProvider,
}

impl lightagent_core::ProviderFactory for MockFactory {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.provider.clone()))
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

#[tokio::test]
async fn the_tester_verifies_through_the_broker_and_reports_the_run() {
    let github = github().await;
    let home = std::env::temp_dir().join(format!("lightagent-tester-{}", RunId::new().as_str()));
    let dispatch_args = json!({
        "workflow": "fixture.yml",
        "ref": BRANCH,
        "sha": HEAD,
        "inputs": { "mode": "fail" },
    });
    let worker = MockProvider::new(vec![
        call("w1", "actions.dispatch", dispatch_args),
        call("w2", "actions.status", json!({ "run_id": 101 })),
        vec![
            ProviderEvent::Content(format!(
                "Run 101 https://github.test/o/r/actions/runs/101 at {HEAD}: failed (verify)."
            )),
            ProviderEvent::Finished {
                reason: FinishReason::Stop,
                usage: None,
            },
        ],
    ]);
    let registry = ToolRegistry::builtin()
        .with(Arc::new(ActionsDispatch::new()))
        .with(Arc::new(ActionsStatus::new()))
        .with(Arc::new(ActionsLogs::new()))
        .with(Arc::new(ActionsCancel::new()));
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(home.clone())),
        Arc::new(MockFactory { provider: worker }),
        registry,
        Duration::from_secs(10),
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
    delegation.resolver = Some(Arc::new(BuiltinResolver::new(
        routing,
        ApprovalPolicy::Balanced,
    )));
    let ctx = ctx(&github).with_delegation(delegation);
    let outcome = lightagent_tools::builtins::AgentDelegate::new()
        .call(&json!({ "role": "tester", "task": "Verify the fixture." }), &ctx)
        .await;
    let _ = std::fs::remove_dir_all(&home);
    let result = report(&outcome);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["role"], "tester");
    let calls = result["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call["ok"] == true), "{calls:?}");
    let offered: Vec<&str> = result["tools_offered"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(offered.contains(&"actions.dispatch"));
    assert!(!offered.contains(&"terminal.run"));
    assert!(!offered.contains(&"fs.write"));
    // The run URL the tester cites came back from the broker.
    let sources: Vec<&str> = result["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(sources.contains(&"https://github.test/o/r/actions/runs/101"));
    assert_eq!(result["unverified_citations"], json!([]));
}

/// The live check, run by the `subagents` workflow's broker job against
/// github.com: the fixture workflow dispatched on this very commit, once to
/// pass and once to fail, each reported as it is, at the exact SHA.
#[tokio::test]
#[ignore = "live: needs the LIGHTAGENT_BROKER_LIVE_* variables the subagents workflow sets"]
async fn live_the_broker_reports_a_passing_and_a_failing_hosted_run() {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let token = env("LIGHTAGENT_BROKER_LIVE_TOKEN");
    let (repository, branch, sha) = (
        env("LIGHTAGENT_BROKER_LIVE_REPOSITORY"),
        env("LIGHTAGENT_BROKER_LIVE_REF"),
        env("LIGHTAGENT_BROKER_LIVE_SHA"),
    );
    if token.is_empty() || repository.is_empty() || branch.is_empty() || sha.is_empty() {
        println!("BLOCKED live broker check: its variables are not set");
        panic!("the live broker check needs its variables");
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let workflow = AllowedWorkflow {
        file: "subagent-fixture.yml".to_owned(),
        refs: vec![branch.clone()],
        inputs: BTreeMap::from([(
            "mode".to_owned(),
            vec!["pass".to_owned(), "fail".to_owned()],
        )]),
        correlation_input: Some("correlation".to_owned()),
    };
    let actions = ActionsContext::new(
        client,
        ActionsPolicy {
            api_base: "https://api.github.com".to_owned(),
            repository,
            token: Some(token.clone()),
            workflows: vec![workflow],
            poll_interval: Duration::from_secs(10),
            max_wait: Duration::from_secs(600),
            max_log_bytes: 8192,
        },
    );
    let ctx = ToolCtx::new(CancellationToken::new()).with_actions(actions);
    let mut runs = Vec::new();
    for mode in ["pass", "fail"] {
        let args = json!({
            "workflow": "subagent-fixture.yml",
            "ref": branch,
            "sha": sha,
            "inputs": { "mode": mode },
        });
        let outcome = ActionsDispatch::new().call(&args, &ctx).await;
        assert!(!outcome.content.contains(&token));
        println!("EVIDENCE dispatch {mode}: {}", outcome.content);
        let dispatched = report(&outcome);
        assert_eq!(dispatched["status"], "dispatched", "{dispatched}");
        runs.push((mode, dispatched["run_id"].clone()));
    }
    for (mode, run) in runs {
        let args = json!({ "run_id": run, "wait_seconds": 600 });
        let outcome = ActionsStatus::new().call(&args, &ctx).await;
        println!("EVIDENCE status {mode}: {}", outcome.content);
        let status = report(&outcome);
        assert_eq!(status["head_sha"], sha.as_str());
        let expected = if mode == "pass" { "passed" } else { "failed" };
        assert_eq!(status["verdict"], expected, "{status}");
        if mode == "fail" {
            let job = status["jobs"][0]["id"].clone();
            let args = json!({ "run_id": run, "job_id": job });
            let log = ActionsLogs::new().call(&args, &ctx).await;
            assert!(!log.is_error, "{}", log.content);
            assert!(log.content.contains("FAIL: deliberate fixture failure"));
            println!("EVIDENCE log {mode}: the failing step's line was read");
        }
    }
    // An input outside the allowlist is refused without dispatching.
    let forged = json!({
        "workflow": "subagent-fixture.yml",
        "ref": branch,
        "sha": sha,
        "inputs": { "mode": "deploy" },
    });
    let refused = ActionsDispatch::new().call(&forged, &ctx).await;
    assert!(refused.is_error && refused.content.contains("not allowlisted"));
    println!("EVIDENCE refusal: {}", refused.content);
}
