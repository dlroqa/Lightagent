//! Read-after-write reconciliation in the workflow broker, against a scripted
//! loopback stand-in for GitHub. GitHub can answer a read of a branch with its
//! previous commit for a moment after an update. A read is repeated, within
//! bounds, only when this run confirmed the update and the read still shows
//! the commit it moved from; anything else fails closed, and no update or
//! dispatch is ever sent twice. Real tool implementations; only GitHub is
//! simulated, with each test scripting the answers it needs.

use std::collections::{BTreeMap, VecDeque};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightagent_core::{AllowedWorkflow, RunId, ToolOutcome};
use lightagent_tools::builtins::{ActionsDispatch, PatchContext, PatchPropose, RepoCommit};
use lightagent_tools::{
    ActionsContext, ActionsPolicy, RefReconcile, Tool, ToolCtx, Workspace, WorkspaceContext,
    WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const BASE: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const OTHER: &str = "3333333333333333333333333333333333333333";
const BRANCH: &str = "subagents/live-coder-fixture";
const FILE: &str = "src/lib.rs";
const REF: &str = "/repos/o/r/git/ref/heads/subagents/live-coder-fixture";
const UPDATE: &str = "/repos/o/r/git/refs/heads/subagents/live-coder-fixture";
const DISPATCHES: &str = "/repos/o/r/actions/workflows/fixture.yml/dispatches";

/// How the stand-in answers one request.
#[derive(Clone, Debug)]
enum Answer {
    /// This status, extra header lines (each ending in `\r\n`), and body.
    Reply(u16, &'static str, Value),
    /// Close the connection without answering.
    Drop,
}

fn ok(body: Value) -> Answer {
    Answer::Reply(200, "", body)
}

/// A read of the branch answering `sha`.
fn at(sha: &str) -> Answer {
    ok(json!({ "object": { "sha": sha, "type": "commit" } }))
}

/// What the branch update does: whether it moves the branch, and its answer.
#[derive(Clone, Debug)]
struct Update {
    applies: bool,
    answer: Answer,
}

#[derive(Default)]
struct State {
    /// Where the branch really is.
    branch: String,
    /// Answers to the next branch reads, in order; then the real branch.
    reads: VecDeque<Answer>,
    /// The branch update's behaviour; by default it applies and names `NEW`.
    update: Option<Update>,
    /// The dispatch's answer; by default 204. A run is created either way.
    dispatch: Option<Answer>,
    runs: Vec<Value>,
    seen: Vec<(String, String)>,
}

fn respond(state: &mut State, method: &str, path: &str, body: &Value) -> Answer {
    let path = path.split('?').next().unwrap_or(path);
    match (method, path) {
        ("GET", REF) => match state.reads.pop_front() {
            Some(answer) => answer,
            None => at(&state.branch.clone()),
        },
        ("GET", p) if p.starts_with("/repos/o/r/git/commits/") => {
            ok(json!({ "tree": { "sha": "tree-base" } }))
        }
        ("POST", "/repos/o/r/git/blobs") => Answer::Reply(201, "", json!({ "sha": "blob-1" })),
        ("POST", "/repos/o/r/git/trees") => Answer::Reply(201, "", json!({ "sha": "tree-new" })),
        ("POST", "/repos/o/r/git/commits") => Answer::Reply(201, "", json!({ "sha": NEW })),
        ("PATCH", UPDATE) => {
            let update = state.update.clone().unwrap_or(Update {
                applies: true,
                answer: at(NEW),
            });
            if update.applies {
                state.branch = body["sha"].as_str().unwrap_or_default().to_owned();
            }
            update.answer
        }
        // Every run of the workflow, whatever its SHA: the broker itself
        // must keep to runs at the SHA it verifies.
        ("GET", "/repos/o/r/actions/workflows/fixture.yml/runs") => {
            ok(json!({ "workflow_runs": state.runs }))
        }
        ("POST", DISPATCHES) => {
            let correlation = body["inputs"]["correlation"].as_str().unwrap_or_default();
            let id = 501 + state.runs.len() as u64;
            state.runs.push(json!({
                "id": id,
                "html_url": format!("https://github.test/o/r/actions/runs/{id}"),
                "path": ".github/workflows/fixture.yml",
                "head_sha": state.branch,
                "head_branch": BRANCH,
                "event": "workflow_dispatch",
                "status": "queued",
                "conclusion": null,
                "display_title": format!("fixture {correlation}"),
            }));
            state
                .dispatch
                .clone()
                .unwrap_or(Answer::Reply(204, "", Value::Null))
        }
        _ => Answer::Reply(404, "", json!({ "message": "Not Found" })),
    }
}

struct GitHub {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl GitHub {
    fn reads(&self, answers: impl IntoIterator<Item = Answer>) {
        self.state.lock().unwrap().reads.extend(answers);
    }

    fn mark(&self) -> usize {
        self.state.lock().unwrap().seen.len()
    }

    /// Requests of `method` to `path` (without its query) since `mark`.
    fn count_since(&self, mark: usize, method: &str, path: &str) -> usize {
        self.state.lock().unwrap().seen[mark..]
            .iter()
            .filter(|(m, p)| m == method && p.split('?').next() == Some(path))
            .count()
    }

    fn count(&self, method: &str, path: &str) -> usize {
        self.count_since(0, method, path)
    }

    fn run(&self, id: &Value) -> Value {
        let state = self.state.lock().unwrap();
        state.runs.iter().find(|run| &run["id"] == id).cloned().unwrap()
    }
}

async fn github() -> GitHub {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(State {
        branch: BASE.to_owned(),
        ..State::default()
    }));
    let shared = Arc::clone(&state);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let state = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 8192];
                let end = loop {
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
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
                let body: Value = serde_json::from_slice(&bytes[end..]).unwrap_or(Value::Null);
                let answer = {
                    let mut state = state.lock().unwrap();
                    state.seen.push((method.clone(), path.clone()));
                    respond(&mut state, &method, &path, &body)
                };
                if let Answer::Reply(status, headers, body) = answer {
                    let text = if body.is_null() {
                        String::new()
                    } else {
                        body.to_string()
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    GitHub { port, state }
}

/// A checkout at `BASE` holding one file, a broker for the stand-in with
/// short read-after-write bounds, and the run's cancellation.
struct Fixture {
    github: GitHub,
    ctx: ToolCtx,
    cancel: CancellationToken,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn fixture(deadline: Duration) -> Fixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let github = github().await;
    let root = std::env::temp_dir().join(format!(
        "lightagent-ref-consistency-{}",
        RunId::new().as_str()
    ));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(".git/HEAD"), format!("{BASE}\n")).unwrap();
    std::fs::write(root.join(FILE), "pub fn one() -> u32 {\n    2\n}\n").unwrap();
    let workspace = WorkspaceContext {
        workspace: Arc::new(Workspace::new(&root).unwrap()),
        policy: Arc::new(WorkspacePolicy {
            max_file_bytes: 65_536,
            allow_terminal: false,
            terminal_timeout: Duration::from_secs(5),
            terminal_allowlist: Vec::new(),
        }),
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let workflow = AllowedWorkflow {
        file: "fixture.yml".to_owned(),
        refs: vec!["subagents/*".to_owned()],
        inputs: BTreeMap::new(),
        correlation_input: Some("correlation".to_owned()),
    };
    let actions = ActionsContext::new(
        client,
        ActionsPolicy {
            api_base: format!("http://127.0.0.1:{}", github.port),
            repository: "o/r".to_owned(),
            token: Some("synthetic-token-not-a-secret".to_owned()),
            workflows: vec![workflow],
            commit_refs: vec!["subagents/*".to_owned()],
            poll_interval: Duration::from_millis(20),
            max_wait: Duration::from_secs(2),
            max_log_bytes: 4096,
        },
    )
    .with_reconcile(RefReconcile {
        first_delay: Duration::from_millis(20),
        max_delay: Duration::from_millis(80),
        deadline,
    });
    let patches = PatchContext {
        scope: Some(vec![FILE.to_owned()]),
        ..PatchContext::default()
    };
    let cancel = CancellationToken::new();
    let ctx = ToolCtx::new(cancel.clone())
        .with_workspace(workspace)
        .with_patches(patches)
        .with_actions(actions);
    Fixture {
        github,
        ctx,
        cancel,
        root,
    }
}

impl Fixture {
    fn update(&self, applies: bool, answer: Answer) {
        self.github.state.lock().unwrap().update = Some(Update { applies, answer });
    }

    /// Propose a change at `BASE` and commit it to the branch.
    async fn commit(&self) -> ToolOutcome {
        let change = json!({ "path": FILE, "content": "pub fn one() -> u32 {\n    1\n}\n" });
        let args = json!({ "summary": "one() returns 1", "changes": [change] });
        let proposed = report(&PatchPropose::new().call(&args, &self.ctx).await);
        assert_eq!(proposed["base_commit"], BASE);
        let args = json!({
            "patch_id": proposed["patch_id"],
            "branch": BRANCH,
            "message": "one() returns 1",
        });
        RepoCommit::new().call(&args, &self.ctx).await
    }

    async fn dispatch(&self, sha: &str) -> ToolOutcome {
        let args = json!({ "workflow": "fixture.yml", "ref": BRANCH, "sha": sha });
        ActionsDispatch::new().call(&args, &self.ctx).await
    }

    fn branch(&self) -> String {
        self.github.state.lock().unwrap().branch.clone()
    }
}

fn report(outcome: &ToolOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.content);
    serde_json::from_str(&outcome.content).unwrap()
}

fn refusal(outcome: &ToolOutcome) -> String {
    assert!(outcome.is_error, "expected a refusal: {}", outcome.content);
    outcome.content.clone()
}

/// The SHAs of the reads in `reads`.
fn shas(reads: &Value) -> Vec<String> {
    reads
        .as_array()
        .unwrap()
        .iter()
        .map(|read| read["sha"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_consistent_branch_is_verified_after_one_read() {
    let f = fixture(Duration::from_millis(600)).await;
    let committed = report(&f.commit().await);
    assert_eq!(committed["commit"], NEW);
    assert_eq!(committed["write"]["confirmed_by"], "response");
    assert_eq!(committed["write"]["response_sha"], NEW);
    assert_eq!(committed["write"]["status"], 200);

    let dispatched = report(&f.dispatch(NEW).await);
    assert_eq!(dispatched["status"], "dispatched");
    assert_eq!(shas(&dispatched["branch_reads"]), [NEW]);
    assert_eq!(f.github.run(&dispatched["run_id"])["head_sha"], NEW);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
    assert_eq!(f.github.count("POST", DISPATCHES), 1);
}

#[tokio::test]
async fn a_read_lagging_a_confirmed_update_is_repeated_until_it_shows_the_commit() {
    let f = fixture(Duration::from_millis(2_000)).await;
    report(&f.commit().await);
    f.github.reads([at(BASE), at(BASE)]);

    let dispatched = report(&f.dispatch(NEW).await);
    assert_eq!(dispatched["status"], "dispatched");
    assert_eq!(shas(&dispatched["branch_reads"]), [BASE, BASE, NEW]);
    let times: Vec<u64> = dispatched["branch_reads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|read| read["ms"].as_u64().unwrap())
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
    assert_eq!(f.github.run(&dispatched["run_id"])["head_sha"], NEW);

    // Only the read was repeated: one commit, one update, one dispatch. A
    // repeated dispatch returns the first and sends nothing.
    let again = report(&f.dispatch(NEW).await);
    assert_eq!(again["repeated"], true);
    assert_eq!(again["run_id"], dispatched["run_id"]);
    assert_eq!(f.github.count("POST", "/repos/o/r/git/commits"), 1);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
    assert_eq!(f.github.count("POST", DISPATCHES), 1);
}

#[tokio::test]
async fn a_branch_that_stays_stale_is_refused_at_the_deadline() {
    let f = fixture(Duration::from_millis(300)).await;
    report(&f.commit().await);
    f.github.reads(vec![at(BASE); 500]);
    let mark = f.github.mark();
    let started = Instant::now();

    let refused = refusal(&f.dispatch(NEW).await);
    let elapsed = started.elapsed();
    assert!(refused.contains("refusing to verify a stale ref"), "{refused}");
    assert!(refused.contains(&format!("still read at {BASE}")), "{refused}");
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    let reads = f.github.count_since(mark, "GET", REF);
    assert!((3..=12).contains(&reads), "{reads} reads");
    assert_eq!(f.github.count("POST", DISPATCHES), 0);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
}

#[tokio::test]
async fn without_a_confirmed_update_a_stale_branch_is_refused_at_once() {
    let f = fixture(Duration::from_secs(5)).await;
    // Nothing was committed: the branch is at BASE.
    let refused = refusal(&f.dispatch(NEW).await);
    assert!(
        refused.contains(&format!("is at {BASE}, not {NEW}: refusing to verify a stale ref")),
        "{refused}"
    );
    assert_eq!(f.github.count("GET", REF), 1);

    // After an update to NEW, verifying the commit it moved from is refused
    // at once too: only the commit the update moved to is waited for.
    report(&f.commit().await);
    let mark = f.github.mark();
    let refused = refusal(&f.dispatch(BASE).await);
    assert!(refused.contains("refusing to verify a stale ref"), "{refused}");
    assert_eq!(f.github.count_since(mark, "GET", REF), 1);
    assert_eq!(f.github.count("POST", DISPATCHES), 0);
}

#[tokio::test]
async fn a_branch_moved_by_someone_else_is_refused_not_waited_out() {
    let f = fixture(Duration::from_secs(5)).await;
    report(&f.commit().await);
    f.github.reads([at(BASE), at(OTHER)]);
    let mark = f.github.mark();

    let refused = refusal(&f.dispatch(NEW).await);
    assert!(refused.contains("moved by someone else"), "{refused}");
    assert!(refused.contains(OTHER), "{refused}");
    assert_eq!(f.github.count_since(mark, "GET", REF), 2);
    assert_eq!(f.github.count("POST", DISPATCHES), 0);
}

#[tokio::test]
async fn an_update_answer_naming_another_commit_is_not_taken_for_the_update() {
    let f = fixture(Duration::from_secs(5)).await;
    f.update(true, at(OTHER));
    let refused = refusal(&f.commit().await);
    assert!(refused.contains("not treated as done"), "{refused}");
    assert!(refused.contains(OTHER), "{refused}");
    // No reconciliation reads: only the read before the update.
    assert_eq!(f.github.count("GET", REF), 1);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);

    // Nothing was recorded, so a stale read is not waited for.
    f.github.reads([at(BASE)]);
    let mark = f.github.mark();
    let refused = refusal(&f.dispatch(NEW).await);
    assert!(refused.contains("refusing to verify a stale ref"), "{refused}");
    assert_eq!(f.github.count_since(mark, "GET", REF), 1);
}

#[tokio::test]
async fn an_update_answer_without_a_commit_is_confirmed_only_by_reading() {
    let f = fixture(Duration::from_secs(2)).await;
    f.update(true, ok(json!({})));
    // The first read is the one before the update; the second lags it.
    f.github.reads([at(BASE), at(BASE)]);
    let committed = report(&f.commit().await);
    assert_eq!(committed["write"]["confirmed_by"], "read");
    assert_eq!(committed["write"]["response_sha"], Value::Null);
    assert_eq!(shas(&committed["write"]["reads"]), [BASE, NEW]);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);

    // Confirmed by reading, the update is recorded like any other.
    f.github.reads([at(BASE)]);
    let dispatched = report(&f.dispatch(NEW).await);
    assert_eq!(shas(&dispatched["branch_reads"]), [BASE, NEW]);
}

#[tokio::test]
async fn an_update_answer_without_a_commit_that_never_shows_is_unconfirmed() {
    let f = fixture(Duration::from_millis(200)).await;
    f.update(false, ok(json!({ "unexpected": true })));
    let refused = refusal(&f.commit().await);
    assert!(refused.contains("is unconfirmed"), "{refused}");
    assert!(refused.contains("not sent again"), "{refused}");
    assert_eq!(f.branch(), BASE);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
    assert_eq!(f.github.count("POST", "/repos/o/r/git/commits"), 1);
}

#[tokio::test]
async fn an_update_without_an_answer_is_reconciled_by_reading_never_resent() {
    // It happened: reading finds the new commit.
    let f = fixture(Duration::from_secs(2)).await;
    f.update(true, Answer::Drop);
    let committed = report(&f.commit().await);
    assert_eq!(committed["write"]["status"], "no answer");
    assert_eq!(committed["write"]["confirmed_by"], "read");
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
    let dispatched = report(&f.dispatch(NEW).await);
    assert_eq!(dispatched["status"], "dispatched");

    // It did not happen: never assumed, never sent again.
    let f = fixture(Duration::from_millis(200)).await;
    f.update(false, Answer::Drop);
    let refused = refusal(&f.commit().await);
    assert!(refused.contains("is unconfirmed"), "{refused}");
    assert_eq!(f.branch(), BASE);
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
    let refused = refusal(&f.dispatch(NEW).await);
    assert!(refused.contains("refusing to verify a stale ref"), "{refused}");
}

#[tokio::test]
async fn an_update_answered_with_a_server_error_is_reconciled_by_reading() {
    let f = fixture(Duration::from_secs(2)).await;
    f.update(true, Answer::Reply(502, "", json!({ "message": "Bad Gateway" })));
    let committed = report(&f.commit().await);
    assert_eq!(committed["write"]["status"], 502);
    assert_eq!(committed["write"]["confirmed_by"], "read");
    assert_eq!(f.github.count("PATCH", UPDATE), 1);
}

#[tokio::test]
async fn a_refused_update_is_named_and_never_waited_out() {
    let cases = [
        (401, "", "authentication failed"),
        (403, "x-ratelimit-remaining: 0\r\n", "rate limited"),
        (403, "", "permission denied"),
        (422, "", "rejected"),
        (429, "", "rate limited"),
    ];
    for (status, headers, kind) in cases {
        let f = fixture(Duration::from_secs(5)).await;
        let answer = Answer::Reply(status, headers, json!({ "message": "refused" }));
        f.update(false, answer);
        let refused = refusal(&f.commit().await);
        assert!(refused.contains(&format!("HTTP {status}")), "{refused}");
        assert!(refused.contains(kind), "{status}: {refused}");
        assert!(!refused.contains("unconfirmed"), "{refused}");
        assert_eq!(f.github.count("GET", REF), 1, "{status}");
        assert_eq!(f.github.count("PATCH", UPDATE), 1, "{status}");
    }
}

#[tokio::test]
async fn a_failed_read_is_named_never_taken_for_a_lagging_one() {
    let cases = [
        (
            Answer::Reply(401, "", json!({ "message": "Bad credentials" })),
            "authentication failed",
        ),
        (
            Answer::Reply(403, "retry-after: 60\r\n", json!({ "message": "slow down" })),
            "rate limited",
        ),
        (
            Answer::Reply(403, "", json!({ "message": "Resource not accessible" })),
            "permission denied",
        ),
        (
            Answer::Reply(500, "", json!({ "message": "Server Error" })),
            "GitHub server error",
        ),
        (ok(json!({ "object": {} })), "malformed"),
        (Answer::Drop, "could not be reached"),
    ];
    for (answer, kind) in cases {
        let f = fixture(Duration::from_secs(5)).await;
        report(&f.commit().await);
        f.github.reads([at(BASE), answer]);
        let refused = refusal(&f.dispatch(NEW).await);
        assert!(refused.contains(kind), "{kind}: {refused}");
        assert!(!refused.contains("stale"), "{refused}");
        assert_eq!(f.github.count("POST", DISPATCHES), 0);
    }
}

#[tokio::test]
async fn cancellation_stops_the_wait_at_once() {
    let f = fixture(Duration::from_secs(30)).await;
    report(&f.commit().await);
    f.github.reads(vec![at(BASE); 5_000]);
    let cancel = f.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let refused = refusal(&f.dispatch(NEW).await);
    assert!(refused.contains("stopped while waiting"), "{refused}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(f.github.count("POST", DISPATCHES), 0);
}

#[tokio::test]
async fn a_dispatch_without_a_clear_answer_is_found_not_sent_again() {
    let f = fixture(Duration::from_secs(5)).await;
    let gateway = Answer::Reply(502, "", json!({ "message": "Bad Gateway" }));
    f.github.state.lock().unwrap().dispatch = Some(gateway);
    let dispatched = report(&f.dispatch(BASE).await);
    assert_eq!(dispatched["status"], "dispatched");
    assert_eq!(dispatched["dispatch_confirmed"], false);
    assert_eq!(f.github.run(&dispatched["run_id"])["head_sha"], BASE);

    let again = report(&f.dispatch(BASE).await);
    assert_eq!(again["repeated"], true);
    assert_eq!(f.github.count("POST", DISPATCHES), 1);
}

#[tokio::test]
async fn a_run_at_another_sha_is_never_reported_as_the_verification() {
    let f = fixture(Duration::from_secs(5)).await;
    // The branch reads BASE, but is at OTHER when the run starts.
    f.github.state.lock().unwrap().branch = OTHER.to_owned();
    f.github.reads([at(BASE)]);
    let dispatched = report(&f.dispatch(BASE).await);
    assert_eq!(dispatched["status"], "dispatched_unlocated");
    assert!(dispatched.get("run_id").is_none(), "{dispatched}");
    assert_eq!(f.github.count("POST", DISPATCHES), 1);
}
