//! Multi-user run ownership over HTTP. Synthetic identities and credentials
//! only; no provider, no Jev, no network beyond loopback.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_api::manager::{RunFactory, RunManager, RunStatus, StartRun};
use lightagent_api::{AppState, AuthConfig, IdentityAuthority, IdentityState, router};
use lightagent_core::identity::{
    AuditAction, AuditOutcome, Capability, Denial, PrincipalId, PrincipalSource, Restriction, Role,
};
use lightagent_core::{AgentEvent, AgentEventSink, ApprovalDecision, ApprovalId, ProfileId};
use lightagent_store::SessionStore;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

const LEGACY_KEY: &str = "synthetic-legacy-operator-key";
const MARKER: &str = "private-marker-7f3a";

/// Records every request it is asked to run. A message containing
/// `needs-approval` waits for one decision; anything else emits a `tick`
/// every 100 ms until cancelled.
#[derive(Default)]
struct ScriptFactory {
    seen: Arc<Mutex<Vec<StartRun>>>,
}

#[async_trait]
impl RunFactory for ScriptFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: CancellationToken,
        mut decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        let approval = request.message.contains("needs-approval");
        self.seen.lock().unwrap().push(request);
        if approval {
            let _ = sink.send(AgentEvent::AwaitingApproval {
                approval_id: ApprovalId::new(),
                tool_call_id: "call_1".to_owned(),
                name: "fs.write".to_owned(),
            });
            return tokio::select! {
                _ = decisions.recv() => {
                    let _ = sink.send(AgentEvent::Content { text: "decided".to_owned() });
                    RunStatus::Completed
                }
                () = cancel.cancelled() => RunStatus::Cancelled,
            };
        }
        for _ in 0..600 {
            let _ = sink.send(AgentEvent::Content {
                text: "tick".to_owned(),
            });
            tokio::select! {
                () = cancel.cancelled() => return RunStatus::Cancelled,
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        RunStatus::Completed
    }
}

/// A multi-user server: `alice` owns `alice-bot`, `bob` owns `bob-bot`,
/// `orphan` and `default` have no owner, `ops` holds an issued operator
/// credential, and the legacy key is configured.
struct World {
    addr: String,
    identity: Arc<IdentityAuthority>,
    manager: RunManager,
    seen: Arc<Mutex<Vec<StartRun>>>,
    alice: String,
    alice_key: String,
    bob: String,
    ops: String,
    alice_readonly: String,
}

fn principal(id: &str) -> PrincipalId {
    PrincipalId::new(id).unwrap()
}

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn key_id(token: &str) -> String {
    token[3..19].to_owned()
}

async fn world() -> World {
    let mut state = IdentityState::default();
    let mut issue = |id: &str, role: Role, restriction: Restriction| {
        state
            .credentials
            .issue(principal(id), role, restriction, 1)
            .unwrap()
            .expose()
            .to_owned()
    };
    let alice = issue("alice", Role::User, Restriction::default());
    let bob = issue("bob", Role::User, Restriction::default());
    let ops = issue("ops", Role::Operator, Restriction::default());
    let alice_readonly = issue(
        "alice",
        Role::User,
        Restriction {
            capabilities: Some([Capability::RunsRead].into()),
            profiles: None,
        },
    );
    state
        .ownership
        .assign(profile("alice-bot"), principal("alice"))
        .unwrap();
    state
        .ownership
        .assign(profile("bob-bot"), principal("bob"))
        .unwrap();
    let identity = Arc::new(IdentityAuthority::new(state));
    let factory = ScriptFactory::default();
    let seen = Arc::clone(&factory.seen);
    let manager = RunManager::new(Arc::new(factory));
    let addr = spawn(
        manager.clone(),
        AuthConfig::multi_user(Arc::clone(&identity), Some(LEGACY_KEY.to_owned())),
    )
    .await;
    World {
        addr,
        identity,
        manager,
        seen,
        alice_key: key_id(&alice),
        alice,
        bob,
        ops,
        alice_readonly,
    }
}

async fn spawn(manager: RunManager, auth: AuthConfig) -> String {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-ownership-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let state = AppState {
        manager,
        auth,
        sessions: SessionStore::new(dir),
        session_profile: "default".into(),
        context_limit: 4_096,
        config_store: None,
        busy_sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
        web_root: None,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(state).into_make_service()).await;
    });
    format!("{addr}")
}

/// (status, body) for one request with an optional bearer token.
async fn call(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    call_with(addr, method, path, token, &[], body).await
}

async fn call_with(
    addr: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n");
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    request.push_str("\r\n");
    request.push_str(payload);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).await.unwrap();
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default();
    (status, body)
}

async fn start(addr: &str, token: &str, body: &str) -> (u16, String) {
    call(
        addr,
        "POST",
        "/api/lightagent/v1/runs",
        Some(token),
        Some(body),
    )
    .await
}

fn run_id(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The first `count` requests the factory recorded, once it has them: a run
/// is accepted before its background task reaches the factory.
async fn recorded(seen: &Arc<Mutex<Vec<StartRun>>>, count: usize) -> Vec<StartRun> {
    for _ in 0..200 {
        let requests = seen.lock().unwrap().clone();
        if requests.len() >= count {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the factory never received {count} requests");
}

async fn status_of(world: &World, id: &str) -> RunStatus {
    world.manager.get(id).await.unwrap().status().await
}

/// Wait until the run reports `status`.
async fn until_status(world: &World, id: &str, status: RunStatus) {
    for _ in 0..100 {
        if status_of(world, id).await == status {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("run {id} never reached {status:?}");
}

// --- single-user compatibility -------------------------------------------

#[tokio::test]
async fn single_user_loopback_compatibility_is_unchanged() {
    let factory = ScriptFactory::default();
    let seen = Arc::clone(&factory.seen);
    let manager = RunManager::new(Arc::new(factory));
    let addr = spawn(manager.clone(), AuthConfig::open()).await;
    // No credential, any profile name, as today.
    let (status, body) = call(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        None,
        Some(r#"{"message":"hello","profile":"bob-bot"}"#),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = run_id(&body);
    let (status, _) = call(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &addr,
        "POST",
        &format!("/api/lightagent/v1/runs/{id}/cancel"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let request = recorded(&seen, 1).await[0].clone();
    assert_eq!(
        request.profile.as_deref(),
        Some("bob-bot"),
        "passed through as before"
    );
    // Runs the single-user API starts are operator-only if multi-user mode
    // is ever enabled.
    let run = manager.get(&id).await.unwrap();
    assert_eq!(run.owner().principal(), None);
    // And a request without a profile still leaves the choice to the runtime.
    let (status, _) = call(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        None,
        Some(r#"{"message":"hello"}"#),
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(recorded(&seen, 2).await[1].profile, None);
}

#[test]
fn the_policy_never_prints_a_key() {
    let keyed = format!("{:?}", AuthConfig::keyed(LEGACY_KEY, []));
    assert!(!keyed.contains(LEGACY_KEY), "{keyed}");
    let identity = Arc::new(IdentityAuthority::new(IdentityState::default()));
    let multi = AuthConfig::multi_user(identity, Some(LEGACY_KEY.to_owned()));
    let printed = format!("{multi:?}");
    assert!(!printed.contains(LEGACY_KEY), "{printed}");
    assert!(printed.contains("multi_user: true"), "{printed}");
    let notices = multi.startup_notices().join("\n");
    assert!(notices.contains("legacy OPERATOR credential"), "{notices}");
    assert!(!notices.contains(LEGACY_KEY));
    assert!(AuthConfig::open().startup_notices().is_empty());
    assert!(multi.requires_key());
}

// --- authentication -------------------------------------------------------

#[tokio::test]
async fn multi_user_mode_requires_a_credential_on_loopback() {
    let world = world().await;
    let id = run_id(
        &start(
            &world.addr,
            &world.alice,
            r#"{"message":"x","profile":"alice-bot"}"#,
        )
        .await
        .1,
    );
    let routes = [
        (
            "POST",
            "/api/lightagent/v1/runs".to_owned(),
            Some(r#"{"message":"x","profile":"alice-bot"}"#),
        ),
        ("GET", format!("/api/lightagent/v1/runs/{id}"), None),
        ("GET", format!("/api/lightagent/v1/runs/{id}/events"), None),
        ("POST", format!("/api/lightagent/v1/runs/{id}/cancel"), None),
        ("GET", "/api/lightagent/v1/approvals".to_owned(), None),
        (
            "POST",
            format!("/api/lightagent/v1/approvals/{id}"),
            Some("{}"),
        ),
        ("GET", "/api/lightagent/v1/sessions".to_owned(), None),
        ("GET", "/api/lightagent/v1/tools".to_owned(), None),
        ("GET", "/api/lightagent/v1/settings".to_owned(), None),
        ("GET", "/api/lightagent/v1/jev".to_owned(), None),
        ("GET", "/api/lightagent/v1/profiles".to_owned(), None),
    ];
    // Nothing about the connection stands in for a credential.
    let spoofing: [(&str, &str); 3] = [
        ("X-Forwarded-For", "127.0.0.1"),
        ("Forwarded", "for=127.0.0.1;proto=https"),
        ("X-Real-IP", "127.0.0.1"),
    ];
    for (method, path, body) in &routes {
        for token in [None, Some("wrong"), Some("la_0000000000000000_0000")] {
            let (status, _) = call_with(&world.addr, method, path, token, &spoofing, *body).await;
            assert_eq!(status, 401, "{method} {path} with {token:?}");
        }
    }
    assert_eq!(
        status_of(&world, &id).await,
        RunStatus::Running,
        "nothing was cancelled"
    );
    // Failed authentications are audited without the presented token.
    let audit = serde_json::to_string(&world.identity.audit_log()).unwrap();
    assert!(audit.contains("authenticate"));
    assert!(!audit.contains("wrong\""));
}

#[tokio::test]
async fn a_revoked_credential_is_refused_at_the_next_request_and_stream() {
    let world = world().await;
    let (status, body) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = run_id(&body);
    // An open event stream as alice.
    let mut stream = TcpStream::connect(&world.addr).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/lightagent/v1/runs/{id}/events HTTP/1.1\r\nHost: local\r\nConnection: close\r\nAuthorization: Bearer {}\r\n\r\n",
                world.alice
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains("tick") {
        let count = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "the stream closed before any event");
        seen.extend_from_slice(&buf[..count]);
    }
    assert!(String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 200"));
    world
        .identity
        .update(|state| state.credentials.revoke(&world.alice_key, 2))
        .unwrap()
        .unwrap();
    // The open stream ends although the run keeps ticking.
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut rest))
        .await
        .expect("a revoked subscriber's stream must end")
        .unwrap();
    assert_eq!(status_of(&world, &id).await, RunStatus::Running);
    // Every later request is refused.
    for (method, path) in [
        ("GET", format!("/api/lightagent/v1/runs/{id}")),
        ("POST", format!("/api/lightagent/v1/runs/{id}/cancel")),
    ] {
        let (status, _) = call(&world.addr, method, &path, Some(world.alice.as_str()), None).await;
        assert_eq!(status, 401, "{method} {path}");
    }
    let (status, _) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    assert_eq!(status, 401);
    // Bob and the operator are unaffected.
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.ops.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200);
}

// --- profile ownership ----------------------------------------------------

#[tokio::test]
async fn a_user_runs_its_own_profile_and_the_run_records_who_and_which() {
    let world = world().await;
    let (status, body) = start(
        &world.addr,
        &world.alice,
        &format!(r#"{{"message":"{MARKER}","profile":"alice-bot"}}"#),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = run_id(&body);
    let run = world.manager.get(&id).await.unwrap();
    assert_eq!(run.owner().principal(), Some(&principal("alice")));
    assert_eq!(run.owner().profile(), Some(&profile("alice-bot")));
    // The runtime receives the authorized profile and nothing else from the
    // identity: no tool, approval or model is added.
    let request = recorded(&world.seen, 1).await[0].clone();
    assert_eq!(request.profile.as_deref(), Some("alice-bot"));
    assert_eq!(request.model, None);
    assert_eq!(request.cwd, None);
    assert!(request.history.is_empty());
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.alice.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &world.addr,
        "POST",
        &format!("/api/lightagent/v1/runs/{id}/cancel"),
        Some(world.alice.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    until_status(&world, &id, RunStatus::Cancelled).await;
    // The audit trail has the decision and none of the message.
    let audit = world.identity.audit_log();
    assert!(audit.iter().any(|record| record.action
        == AuditAction::Request(Capability::RunsWrite)
        && record.profile == Some(profile("alice-bot"))
        && record.outcome == AuditOutcome::Allowed));
    assert!(!serde_json::to_string(&audit).unwrap().contains(MARKER));
}

#[tokio::test]
async fn a_user_cannot_run_another_users_or_an_unowned_profile() {
    let world = world().await;
    let mut refusals = Vec::new();
    for body in [
        r#"{"message":"x","profile":"bob-bot"}"#,
        r#"{"message":"x","profile":"orphan"}"#,
        r#"{"message":"x","profile":"does-not-exist"}"#,
        // No profile: the server's default profile, which alice does not own.
        r#"{"message":"x"}"#,
    ] {
        let (status, response) = start(&world.addr, &world.alice, body).await;
        assert_eq!(status, 403, "{body}: {response}");
        refusals.push(response);
    }
    // Other, unowned and missing profiles are refused identically.
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "{refusals:?}"
    );
    // Malformed ids are refused before anything else.
    for body in [
        r#"{"message":"x","profile":"Bob-bot"}"#,
        r#"{"message":"x","profile":" bob-bot"}"#,
        r#"{"message":"x","profile":"../bob-bot"}"#,
    ] {
        let (status, _) = start(&world.addr, &world.alice, body).await;
        assert_eq!(status, 400, "{body}");
    }
    // Sessions stay operator-only in this slice, whatever the profile.
    let (status, _) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot","session_id":"0000000000000000"}"#,
    )
    .await;
    assert_eq!(status, 403);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(world.seen.lock().unwrap().is_empty(), "no run was started");
    let audit = world.identity.audit_log();
    assert!(
        audit
            .iter()
            .any(|record| record.outcome == AuditOutcome::Denied(Denial::ProfileNotGranted))
    );
    assert!(
        audit
            .iter()
            .any(|record| record.outcome == AuditOutcome::Denied(Denial::ProfileUnowned))
    );
}

#[tokio::test]
async fn knowing_a_run_id_never_authorizes_access_to_it() {
    let world = world().await;
    let (_, body) = start(
        &world.addr,
        &world.bob,
        r#"{"message":"x","profile":"bob-bot"}"#,
    )
    .await;
    let bobs = run_id(&body);
    let (_, unknown) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/runs/run-00000000000000000000000000000000",
        Some(world.alice.as_str()),
        None,
    )
    .await;
    for (method, path) in [
        ("GET", format!("/api/lightagent/v1/runs/{bobs}")),
        ("GET", format!("/api/lightagent/v1/runs/{bobs}/events")),
        ("POST", format!("/api/lightagent/v1/runs/{bobs}/cancel")),
        ("POST", format!("/api/lightagent/v1/approvals/{bobs}")),
    ] {
        let body = (method == "POST").then_some("{}");
        let (status, response) =
            call(&world.addr, method, &path, Some(world.alice.as_str()), body).await;
        assert_eq!(status, 404, "{method} {path}");
        // Indistinguishable from a run that does not exist, apart from the id.
        assert_eq!(
            response.replace(&bobs, "ID"),
            unknown.replace("run-00000000000000000000000000000000", "ID"),
            "{method} {path}"
        );
    }
    assert_eq!(
        status_of(&world, &bobs).await,
        RunStatus::Running,
        "not cancelled"
    );
    // Its owner and the operators still reach it.
    for token in [world.bob.as_str(), world.ops.as_str(), LEGACY_KEY] {
        let (status, _) = call(
            &world.addr,
            "GET",
            &format!("/api/lightagent/v1/runs/{bobs}"),
            Some(token),
            None,
        )
        .await;
        assert_eq!(status, 200);
    }
}

#[tokio::test]
async fn runs_without_an_owner_are_operator_only() {
    let world = world().await;
    // As ACP, the single-user API or anything before multi-user mode starts runs.
    let historical = world
        .manager
        .start(StartRun {
            message: "x".to_owned(),
            history: vec![],
            profile: Some("alice-bot".to_owned()),
            model: None,
            cwd: None,
        })
        .await;
    let id = historical.id().to_owned();
    for token in [world.alice.as_str(), world.bob.as_str()] {
        for (method, path) in [
            ("GET", format!("/api/lightagent/v1/runs/{id}")),
            ("GET", format!("/api/lightagent/v1/runs/{id}/events")),
            ("POST", format!("/api/lightagent/v1/runs/{id}/cancel")),
        ] {
            let (status, _) = call(&world.addr, method, &path, Some(token), None).await;
            assert_eq!(
                status, 404,
                "a user naming the run's profile is not its owner: {method} {path}"
            );
        }
    }
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.ops.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &world.addr,
        "POST",
        &format!("/api/lightagent/v1/runs/{id}/cancel"),
        Some(LEGACY_KEY),
        None,
    )
    .await;
    assert_eq!(status, 200);
    // An operator's own new run records the operator, so users cannot reach it.
    let (status, body) = start(
        &world.addr,
        &world.ops,
        r#"{"message":"x","profile":"bob-bot"}"#,
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = run_id(&body);
    let run = world.manager.get(&id).await.unwrap();
    assert_eq!(run.owner().principal(), Some(&principal("ops")));
    assert_eq!(run.owner().profile(), Some(&profile("bob-bot")));
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.bob.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 404, "bob owns the profile, not the operator's run");
}

#[tokio::test]
async fn losing_ownership_ends_access_to_earlier_runs() {
    let world = world().await;
    let (_, body) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    let id = run_id(&body);
    world
        .identity
        .update(|state| {
            state
                .ownership
                .reassign(profile("alice-bot"), principal("bob"))
        })
        .unwrap();
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.alice.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 404);
    // Reassignment does not hand alice's run to bob either.
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.bob.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = start(
        &world.addr,
        &world.bob,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    assert_eq!(status, 202);
}

#[tokio::test]
async fn approvals_are_listed_and_answered_only_for_authorized_runs() {
    let world = world().await;
    let (_, body) = start(
        &world.addr,
        &world.bob,
        r#"{"message":"needs-approval","profile":"bob-bot"}"#,
    )
    .await;
    let bobs = run_id(&body);
    let (_, body) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"needs-approval","profile":"alice-bot"}"#,
    )
    .await;
    let alices = run_id(&body);
    until_status(&world, &bobs, RunStatus::AwaitingApproval).await;
    until_status(&world, &alices, RunStatus::AwaitingApproval).await;
    let listed = |body: &str| -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(body).unwrap()["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["run"].as_str().unwrap().to_owned())
            .collect()
    };
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/approvals",
        Some(world.alice.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        listed(&body),
        vec![alices.clone()],
        "alice sees only her run"
    );
    let (_, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/approvals",
        Some(world.ops.as_str()),
        None,
    )
    .await;
    let mut all = listed(&body);
    all.sort();
    let mut expected = vec![alices.clone(), bobs.clone()];
    expected.sort();
    assert_eq!(all, expected, "an operator sees every run");
    // Alice cannot answer bob's approval.
    let (status, _) = call(
        &world.addr,
        "POST",
        &format!("/api/lightagent/v1/approvals/{bobs}"),
        Some(world.alice.as_str()),
        Some(r#"{"approve":true}"#),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(status_of(&world, &bobs).await, RunStatus::AwaitingApproval);
    // Bob answers his own.
    let (status, _) = call(
        &world.addr,
        "POST",
        &format!("/api/lightagent/v1/approvals/{bobs}"),
        Some(world.bob.as_str()),
        Some(r#"{"approve":false}"#),
    )
    .await;
    assert_eq!(status, 200);
    until_status(&world, &bobs, RunStatus::Completed).await;
}

// --- capability and role --------------------------------------------------

#[tokio::test]
async fn users_never_reach_operator_or_unscoped_endpoints() {
    let world = world().await;
    for (method, path, body) in [
        ("GET", "/api/lightagent/v1/settings", None),
        ("PUT", "/api/lightagent/v1/settings", Some("{}")),
        ("POST", "/api/lightagent/v1/jev/verify", None),
        ("GET", "/api/lightagent/v1/jev", None),
        ("GET", "/api/lightagent/v1/tools", None),
        ("GET", "/api/lightagent/v1/profiles", None),
        ("GET", "/api/lightagent/v1/sessions", None),
        ("POST", "/api/lightagent/v1/sessions", None),
    ] {
        let (status, _) = call(&world.addr, method, path, Some(world.alice.as_str()), body).await;
        // A body-less PUT is a 4xx from the extractor before any handler runs.
        assert!(
            status == 403 || (method == "PUT" && (400..500).contains(&status)),
            "{method} {path}: {status}"
        );
        let (status, _) = call(&world.addr, method, path, Some(world.ops.as_str()), body).await;
        assert!(
            status != 401 && status != 403,
            "operator {method} {path}: {status}"
        );
    }
}

#[tokio::test]
async fn a_credential_restriction_limits_what_its_holder_may_do() {
    let world = world().await;
    let (status, _) = start(
        &world.addr,
        &world.alice_readonly,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    assert_eq!(status, 403, "a read-only credential cannot start runs");
    let (_, body) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    let id = run_id(&body);
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        Some(world.alice_readonly.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 200, "it may read alice's run");
    let (status, _) = call(
        &world.addr,
        "POST",
        &format!("/api/lightagent/v1/runs/{id}/cancel"),
        Some(world.alice_readonly.as_str()),
        None,
    )
    .await;
    assert_eq!(status, 403, "but not cancel it");
    assert_eq!(status_of(&world, &id).await, RunStatus::Running);
}

#[tokio::test]
async fn the_legacy_key_is_an_operator_credential_and_never_a_user() {
    let world = world().await;
    let (status, body) = start(
        &world.addr,
        LEGACY_KEY,
        r#"{"message":"x","profile":"orphan"}"#,
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let run = world.manager.get(&run_id(&body)).await.unwrap();
    assert_eq!(
        run.owner().principal().map(PrincipalId::as_str),
        Some(lightagent_core::identity::LOCAL_OPERATOR)
    );
    let caller = AuthConfig::multi_user(Arc::clone(&world.identity), Some(LEGACY_KEY.to_owned()))
        .caller(&{
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "authorization",
                format!("Bearer {LEGACY_KEY}").parse().unwrap(),
            );
            headers
        })
        .unwrap();
    assert_eq!(caller.principal.role, Role::Operator);
    assert_eq!(caller.principal.source, PrincipalSource::LegacyOperatorKey);
}

// --- adversarial ----------------------------------------------------------

#[tokio::test]
async fn concurrent_requests_never_cross_profiles() {
    let world = Arc::new(world().await);
    let mut tasks = Vec::new();
    for index in 0..24 {
        let world = Arc::clone(&world);
        tasks.push(tokio::spawn(async move {
            // Alternate callers, each also trying the other's profile.
            let (own_token, own, other) = if index % 2 == 0 {
                (world.alice.clone(), "alice-bot", "bob-bot")
            } else {
                (world.bob.clone(), "bob-bot", "alice-bot")
            };
            let (own_status, body) = start(
                &world.addr,
                &own_token,
                &format!(r#"{{"message":"x","profile":"{own}"}}"#),
            )
            .await;
            let (swap_status, _) = start(
                &world.addr,
                &own_token,
                &format!(r#"{{"message":"x","profile":"{other}"}}"#),
            )
            .await;
            (own_status, swap_status, own_token, run_id(&body))
        }));
    }
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    assert!(
        results
            .iter()
            .all(|(own, swap, _, _)| *own == 202 && *swap == 403)
    );
    // Every run is visible to its starter and to nobody else.
    for (_, _, token, id) in &results {
        let token = token.as_str();
        let other = if token == world.alice {
            world.bob.as_str()
        } else {
            world.alice.as_str()
        };
        let reads = tokio::join!(
            call(
                &world.addr,
                "GET",
                &format!("/api/lightagent/v1/runs/{id}"),
                Some(token),
                None
            ),
            call(
                &world.addr,
                "GET",
                &format!("/api/lightagent/v1/runs/{id}"),
                Some(other),
                None
            ),
        );
        assert_eq!((reads.0.0, reads.1.0), (200, 404), "{id}");
    }
    let requests = recorded(&world.seen, 24).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        world.seen.lock().unwrap().len(),
        24,
        "only the 24 authorized runs started"
    );
    assert!(requests.iter().all(|request| matches!(
        request.profile.as_deref(),
        Some("alice-bot") | Some("bob-bot")
    )));
}

#[tokio::test]
async fn revocation_racing_requests_never_admits_afterwards() {
    let world = Arc::new(world().await);
    let (_, body) = start(
        &world.addr,
        &world.alice,
        r#"{"message":"x","profile":"alice-bot"}"#,
    )
    .await;
    let id = run_id(&body);
    let mut readers = Vec::new();
    for _ in 0..16 {
        let world = Arc::clone(&world);
        let id = id.clone();
        readers.push(tokio::spawn(async move {
            call(
                &world.addr,
                "GET",
                &format!("/api/lightagent/v1/runs/{id}"),
                Some(world.alice.as_str()),
                None,
            )
            .await
            .0
        }));
    }
    world
        .identity
        .update(|state| state.credentials.revoke(&world.alice_key, 3))
        .unwrap()
        .unwrap();
    for reader in readers {
        let status = reader.await.unwrap();
        assert!(status == 200 || status == 401, "{status}");
    }
    for _ in 0..8 {
        let (status, _) = call(
            &world.addr,
            "GET",
            &format!("/api/lightagent/v1/runs/{id}"),
            Some(world.alice.as_str()),
            None,
        )
        .await;
        assert_eq!(status, 401, "after revocation completes, never admitted");
    }
}
