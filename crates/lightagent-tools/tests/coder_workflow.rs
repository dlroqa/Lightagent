//! The coder's change workflow: a scoped patch tied to its base, applied or
//! committed only while that base still holds. Scripted model, real tools;
//! GitHub is a loopback stand-in. The live counterpart commits to a scratch
//! branch and verifies the commit on GitHub Actions (`actions_broker.rs`).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightagent_core::{
    AgentProvider, ApprovalPolicy, FinishReason, MockProvider, ModelRouting, ProfileStore,
    ProviderError, ProviderEvent, RunId, ToolOutcome,
};
use lightagent_tools::builtins::{
    AgentDelegate, PatchApply, PatchContext, PatchPropose, RepoCommit,
};
use lightagent_tools::{
    ActionsContext, ActionsPolicy, BuiltinResolver, Delegation, SubagentPolicy, Tool, ToolCtx,
    ToolRegistry, Workspace, WorkspaceContext, WorkspacePolicy,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";
const NEW: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
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

/// A checkout at `BASE` with `src/lib.rs`, removed on drop.
struct Checkout {
    path: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lightagent-coder-{}", RunId::new().as_str()));
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::create_dir_all(path.join(".git/refs/heads")).unwrap();
        std::fs::write(path.join(".git/HEAD"), "ref: refs/heads/work\n").unwrap();
        std::fs::write(path.join(".git/refs/heads/work"), format!("{BASE}\n")).unwrap();
        std::fs::write(path.join("src/lib.rs"), BEFORE).unwrap();
        Self { path }
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.path.join("src/lib.rs")).unwrap()
    }

    fn context(&self) -> ToolCtx {
        let workspace = WorkspaceContext {
            workspace: Arc::new(Workspace::new(&self.path).unwrap()),
            policy: Arc::new(WorkspacePolicy {
                max_file_bytes: 65_536,
                allow_terminal: false,
                terminal_timeout: Duration::from_secs(5),
                terminal_allowlist: Vec::new(),
            }),
        };
        ToolCtx::new(CancellationToken::new())
            .with_workspace(workspace)
            .with_patches(PatchContext::default())
    }
}

impl Drop for Checkout {
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

fn json_of(outcome: &ToolOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.content);
    serde_json::from_str(&outcome.content).unwrap()
}

/// Propose `content` for `path` directly, as a worker with `scope` would.
async fn propose(ctx: &ToolCtx, scope: &[&str], path: &str, content: &str) -> ToolOutcome {
    let mut ctx = ctx.clone();
    if let Some(patches) = ctx.patches.as_mut() {
        patches.scope = Some(scope.iter().map(|path| (*path).to_owned()).collect());
    }
    let args = json!({ "summary": "s", "changes": [{ "path": path, "content": content }] });
    PatchPropose::new().call(&args, &ctx).await
}

#[tokio::test]
async fn the_coder_returns_a_scoped_patch_tied_to_its_base() {
    let checkout = Checkout::new();
    let worker = MockProvider::new(vec![
        call(
            "w1",
            "patch.propose",
            json!({ "summary": "one() returns 1", "changes": [{ "path": "src/lib.rs", "content": AFTER }] }),
        ),
        call(
            "w2",
            "patch.propose",
            json!({ "summary": "also main", "changes": [{ "path": "src/main.rs", "content": "fn main() {}\n" }] }),
        ),
        answer("Proposed the change to src/lib.rs."),
    ]);
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(checkout.path.join("home"))),
        Arc::new(MockFactory { provider: worker }),
        ToolRegistry::builtin().with(Arc::new(PatchPropose::new())),
        Duration::from_secs(5),
        262_144,
    );
    delegation.subagents = SubagentPolicy {
        enabled: true,
        ..SubagentPolicy::default()
    };
    let routing = ModelRouting {
        model: "m".to_owned(),
        ..ModelRouting::default()
    };
    delegation.resolver = Some(Arc::new(BuiltinResolver::new(
        routing,
        ApprovalPolicy::Balanced,
    )));
    let ctx = checkout.context().with_delegation(delegation);
    let args = json!({ "role": "coder", "task": "Make one() return 1.", "write_scope": ["src/lib.rs"] });
    let report = json_of(&AgentDelegate::new().call(&args, &ctx).await);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["tool_calls"][0]["ok"], true, "in scope");
    assert_eq!(report["tool_calls"][1]["ok"], false, "src/main.rs is out of scope");
    assert_eq!(checkout.file(), BEFORE, "proposing writes nothing");
    assert_eq!(report["worker"], "builtin:coder@2");

    // The patch is the lead's to apply, against the same base.
    let patch_id = {
        let patches = ctx.patches.as_ref().unwrap();
        let again = propose(&ctx, &["src/lib.rs"], "src/lib.rs", AFTER).await;
        let proposed = json_of(&again);
        assert_eq!(proposed["base_commit"], BASE);
        assert_eq!(proposed["files"][0]["change"], "modify");
        let diff = proposed["files"][0]["diff"].as_str().unwrap();
        assert!(diff.contains("-    2\n+    1\n"), "{diff}");
        let id = proposed["patch_id"].as_str().unwrap().to_owned();
        assert!(patches.store.get(&id).is_some());
        id
    };
    let applied = PatchApply::new()
        .call(&json!({ "patch_id": patch_id }), &ctx)
        .await;
    assert!(!applied.is_error, "{}", applied.content);
    assert_eq!(checkout.file(), AFTER);
}

#[tokio::test]
async fn a_patch_without_a_scope_or_outside_it_is_refused() {
    let checkout = Checkout::new();
    let ctx = checkout.context();
    let none = propose(&ctx, &[], "src/lib.rs", AFTER).await;
    assert!(none.is_error && none.content.contains("no write scope"));
    let outside = propose(&ctx, &["src/other.rs", "tests/**"], "src/lib.rs", AFTER).await;
    assert!(outside.is_error && outside.content.contains("outside"));
    let escape = propose(&ctx, &["../x"], "../x", "x").await;
    assert!(escape.is_error);
}

#[tokio::test]
async fn a_patch_on_a_moved_file_or_checkout_is_never_applied() {
    let checkout = Checkout::new();
    let ctx = checkout.context();
    let id = |outcome: &ToolOutcome| json_of(outcome)["patch_id"].as_str().unwrap().to_owned();
    let apply = |id: String| {
        let ctx = ctx.clone();
        async move {
            PatchApply::new()
                .call(&json!({ "patch_id": id }), &ctx)
                .await
        }
    };

    // Someone else edited the file after the patch was proposed.
    let first = id(&propose(&ctx, &["src/**"], "src/lib.rs", AFTER).await);
    std::fs::write(checkout.path.join("src/lib.rs"), "concurrent edit\n").unwrap();
    let conflict = apply(first).await;
    assert!(conflict.is_error && conflict.content.contains("changed since"));
    assert_eq!(checkout.file(), "concurrent edit\n", "the other edit survives");

    // The checkout moved to another commit.
    std::fs::write(checkout.path.join("src/lib.rs"), BEFORE).unwrap();
    let second = id(&propose(&ctx, &["src/**"], "src/lib.rs", AFTER).await);
    std::fs::write(checkout.path.join(".git/refs/heads/work"), format!("{OTHER}\n")).unwrap();
    let stale = apply(second).await;
    assert!(stale.is_error && stale.content.contains("stale"), "{}", stale.content);
    assert_eq!(checkout.file(), BEFORE);
}

/// The GitHub git-data stand-in: the branch is at `head`; records requests.
struct GitHub {
    port: u16,
    head: Arc<Mutex<String>>,
    seen: Arc<Mutex<Vec<(String, String, Value)>>>,
}

async fn github(head: &str) -> GitHub {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let head = Arc::new(Mutex::new(head.to_owned()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (branch_head, log) = (Arc::clone(&head), Arc::clone(&seen));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (branch_head, log) = (Arc::clone(&branch_head), Arc::clone(&log));
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
                let head_text = String::from_utf8_lossy(&bytes[..end]).into_owned();
                let mut first = head_text.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("/").to_owned();
                let length = head_text
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
                log.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                let base = "/repos/o/r/git";
                let reply = match (method.as_str(), path.as_str()) {
                    ("GET", p) if p == format!("{base}/ref/heads/subagents/coder") => {
                        let sha = branch_head.lock().unwrap().clone();
                        (200, json!({ "object": { "sha": sha } }))
                    }
                    ("GET", p) if p.starts_with(&format!("{base}/commits/")) => {
                        (200, json!({ "tree": { "sha": "tree-base" } }))
                    }
                    ("POST", p) if p == format!("{base}/blobs") => (201, json!({ "sha": "blob-1" })),
                    ("POST", p) if p == format!("{base}/trees") => (201, json!({ "sha": "tree-new" })),
                    ("POST", p) if p == format!("{base}/commits") => (201, json!({ "sha": NEW })),
                    ("PATCH", p) if p == format!("{base}/refs/heads/subagents/coder") => {
                        *branch_head.lock().unwrap() = NEW.to_owned();
                        (200, json!({}))
                    }
                    _ => (404, json!({ "message": "Not Found" })),
                };
                let text = reply.1.to_string();
                let response = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    reply.0,
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    GitHub { port, head, seen }
}

fn with_broker(ctx: ToolCtx, github: &GitHub) -> ToolCtx {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    ctx.with_actions(ActionsContext::new(
        client,
        ActionsPolicy {
            api_base: format!("http://127.0.0.1:{}", github.port),
            repository: "o/r".to_owned(),
            token: None,
            workflows: Vec::new(),
            commit_refs: vec!["subagents/coder".to_owned()],
            poll_interval: Duration::from_millis(50),
            max_wait: Duration::from_secs(5),
            max_log_bytes: 4096,
        },
    ))
}

#[tokio::test]
async fn a_commit_is_a_fast_forward_of_exactly_the_patch_base() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let checkout = Checkout::new();
    let github = github(BASE).await;
    let ctx = with_broker(checkout.context(), &github);
    let proposed = json_of(&propose(&ctx, &["src/lib.rs"], "src/lib.rs", AFTER).await);
    let patch_id = proposed["patch_id"].as_str().unwrap().to_owned();
    let commit = |branch: &str| {
        let ctx = ctx.clone();
        let args = json!({ "patch_id": patch_id, "branch": branch, "message": "one() returns 1" });
        async move { RepoCommit::new().call(&args, &ctx).await }
    };

    let refused = commit("main").await;
    assert!(refused.is_error && refused.content.contains("not allowlisted"));

    let done = json_of(&commit("subagents/coder").await);
    assert_eq!(done["commit"], NEW);
    assert_eq!(done["base"], BASE);
    assert!(done["note"].as_str().unwrap().contains("Not verified yet"));
    let seen = github.seen.lock().unwrap().clone();
    let tree = seen.iter().find(|(_, path, _)| path.ends_with("/git/trees")).unwrap();
    assert_eq!(tree.2["base_tree"], "tree-base");
    assert_eq!(tree.2["tree"][0]["path"], "src/lib.rs");
    let created = seen.iter().find(|(m, path, _)| m == "POST" && path.ends_with("/git/commits")).unwrap();
    assert_eq!(created.2["parents"], json!([BASE]));
    let moved = seen.iter().find(|(m, _, _)| m == "PATCH").unwrap();
    assert_eq!(moved.2["force"], false);

    // The branch is now at NEW: the same patch is stale for it.
    let stale = commit("subagents/coder").await;
    assert!(stale.is_error && stale.content.contains("not the patch's base"));
    assert_eq!(github.head.lock().unwrap().as_str(), NEW);
    let writes = github
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _, _)| method != "GET")
        .count();
    assert_eq!(writes, 4, "one blob, tree, commit and ref update; nothing for the refusals");
}
