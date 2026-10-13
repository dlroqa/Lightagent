//! Profile-scoped API access in multi-user mode, through the real HTTP
//! handlers: sessions, profiles, the tool catalog, skills, Jev status and the
//! provider view. Synthetic identities, profiles and sessions only; the
//! factory reports a fixed, profile-dependent catalog and Jev record.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lightagent_api::manager::{ProviderCapabilities, RunFactory, RunManager, RunStatus, StartRun};
use lightagent_api::{AppState, AuthConfig, IdentityAuthority, IdentityState, router};
use lightagent_core::identity::{Capability, PrincipalId, Restriction, Role, RunTrust};
use lightagent_core::{
    AgentEvent, AgentEventSink, AgentProfile, ApprovalDecision, ConfigStore, ProfileId,
    ProfileStore, RunId,
};
use lightagent_store::{Session, SessionStore};
use lightagent_tools::{ToolDefinition, ToolRegistry};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

/// Reports a profile-dependent catalog and Jev record, and records what it
/// was asked. Runs answer at once.
#[derive(Default)]
struct Scoped {
    catalogs: Arc<Mutex<Vec<(String, RunTrust)>>>,
    runs: Arc<Mutex<Vec<StartRun>>>,
}

fn definitions(names: &[&str]) -> Vec<ToolDefinition> {
    let registry = ToolRegistry::builtin();
    names
        .iter()
        .filter_map(|name| registry.get(name).map(|tool| tool.definition().clone()))
        .collect()
}

#[async_trait]
impl RunFactory for Scoped {
    async fn tools(&self) -> Result<Vec<ToolDefinition>, String> {
        // The installation-wide (operator) catalog.
        Ok(definitions(&[
            "datetime.now",
            "fs.write",
            "terminal.run",
            "skill.read",
            "web.fetch",
        ]))
    }

    async fn tools_for(
        &self,
        profile: &ProfileId,
        trust: RunTrust,
    ) -> Result<Vec<ToolDefinition>, String> {
        self.catalogs
            .lock()
            .unwrap()
            .push((profile.as_str().to_owned(), trust));
        Ok(match profile.as_str() {
            "alice-bot" => definitions(&["datetime.now", "skill.read"]),
            "bob-bot" => definitions(&["datetime.now", "web.fetch"]),
            _ => definitions(&["datetime.now"]),
        })
    }

    async fn jev_status(&self) -> Result<Value, String> {
        Ok(json!({
            "allowed_profiles": ["alice-bot", "bob-bot"],
            "endpoint_configured": true,
            "last_classification": {"category": "operator-view"},
        }))
    }

    async fn jev_status_for(&self, profile: &ProfileId) -> Result<Value, String> {
        Ok(json!({
            "scope": "profile",
            "profile": profile.as_str(),
            "last_classification": {"category": format!("{}-category", profile.as_str())},
        }))
    }

    async fn provider_capabilities(&self) -> Result<ProviderCapabilities, String> {
        Ok(ProviderCapabilities {
            provider: "lightweight".into(),
            base_url: "http://provider.test:9999".into(),
            configured_model: Some("installation-model".into()),
            models: vec!["installation-model".into(), "secret-model".into()],
            model_aliases: Default::default(),
            model_catalog: Default::default(),
            runtime_models: Vec::new(),
            streaming: true,
            tool_calls: true,
            reasoning_content: false,
        })
    }

    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        _cancel: CancellationToken,
        _decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.runs.lock().unwrap().push(request);
        let _ = sink.send(AgentEvent::Content { text: "ok".into() });
        RunStatus::Completed
    }

    async fn run_as(
        &self,
        request: StartRun,
        _trust: RunTrust,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.run(request, sink, cancel, decisions).await
    }
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

struct Home(std::path::PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Profiles on disk: `default`, `alice-bot` (alice), `bob-bot` (bob) and
/// `shared-bot` (bob's, alice may read its sessions). Alice and bob hold
/// `sessions_write` and `tools_read` grants on their own profiles.
struct World {
    home: Home,
    addr: String,
    identity: Arc<IdentityAuthority>,
    catalogs: Arc<Mutex<Vec<(String, RunTrust)>>>,
    runs: Arc<Mutex<Vec<StartRun>>>,
    alice: String,
    alice_ro: String,
    bob: String,
    ops: String,
}

async fn world() -> World {
    let root = std::env::temp_dir().join(format!("lightagent-scoped-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    let profiles = ProfileStore::new(&root);
    for id in ["default", "alice-bot", "bob-bot", "shared-bot"] {
        profiles
            .create(&AgentProfile::new(
                profile(id),
                format!("{id} name"),
                "Persona",
                format!("{id}-model"),
            ))
            .unwrap();
    }
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
    let alice_ro = issue(
        "alice",
        Role::User,
        Restriction {
            capabilities: Some([Capability::SessionsRead, Capability::ToolsRead].into()),
            profiles: None,
        },
    );
    let bob = issue("bob", Role::User, Restriction::default());
    let ops = issue("ops", Role::Operator, Restriction::default());
    let ownership = &mut state.ownership;
    ownership
        .assign(profile("alice-bot"), principal("alice"))
        .unwrap();
    ownership
        .assign(profile("bob-bot"), principal("bob"))
        .unwrap();
    ownership
        .assign(profile("shared-bot"), principal("bob"))
        .unwrap();
    for (bot, who) in [("alice-bot", "alice"), ("bob-bot", "bob")] {
        for capability in [Capability::SessionsWrite, Capability::ToolsRead] {
            ownership
                .grant(&profile(bot), principal(who), capability)
                .unwrap();
        }
    }
    ownership
        .grant(
            &profile("shared-bot"),
            principal("alice"),
            Capability::SessionsRead,
        )
        .unwrap();
    let identity = Arc::new(IdentityAuthority::new(state));
    let factory = Scoped::default();
    let (catalogs, runs) = (factory.catalogs.clone(), factory.runs.clone());
    let addr = serve(
        &root,
        RunManager::new(Arc::new(factory)),
        AuthConfig::multi_user(identity.clone(), None),
    )
    .await;
    World {
        home: Home(root),
        addr,
        identity,
        catalogs,
        runs,
        alice,
        alice_ro,
        bob,
        ops,
    }
}

async fn serve(root: &std::path::Path, manager: RunManager, auth: AuthConfig) -> String {
    let state = AppState {
        manager,
        auth,
        sessions: SessionStore::at_profile(&ProfileStore::new(root).handle(&profile("default"))),
        session_profile: "default".into(),
        context_limit: 4_096,
        config_store: Some(ConfigStore::new(root.join("config.json"))),
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

/// (status, body) for one request; an empty token sends none.
async fn call(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n");
    if !token.is_empty() {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
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

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or(Value::Null)
}

const SESSIONS: &str = "/api/lightagent/v1/sessions";

/// Create a session as `token` on `bot`; returns its id.
async fn create(world: &World, token: &str, bot: &str) -> String {
    let (status, body) = call(
        &world.addr,
        "POST",
        &format!("{SESSIONS}?profile={bot}"),
        token,
        None,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    json_of(&body)["id"].as_str().unwrap().to_owned()
}

fn ids(body: &str) -> Vec<String> {
    json_of(body)["sessions"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|session| session["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A session written directly, as one created before multi-user mode (no
/// owner).
fn historic(world: &World, bot: &str) -> String {
    let store = SessionStore::at_profile(&ProfileStore::new(&world.home.0).handle(&profile(bot)));
    let session = Session::new(bot, "historic");
    store.save(&session).unwrap();
    session.id.as_str().to_owned()
}

// 1, 2, 16. Users list and read only their own sessions; another user's
// session, an unowned historic one and a missing one look the same.
#[tokio::test]
async fn users_see_only_their_own_sessions() {
    let world = world().await;
    let mine = create(&world, &world.alice, "alice-bot").await;
    let bobs = create(&world, &world.bob, "bob-bot").await;
    let bobs_shared = {
        // Bob cannot write shared-bot's sessions through the API without a
        // grant; write one as the server would for him.
        let store = SessionStore::at_profile(
            &ProfileStore::new(&world.home.0).handle(&profile("shared-bot")),
        );
        let mut session = Session::new("shared-bot", "bob's");
        session.owner = Some("bob".into());
        store.save(&session).unwrap();
        session.id.as_str().to_owned()
    };
    let old = historic(&world, "alice-bot");

    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body), vec![mine.clone()], "only alice's own session");
    // A read grant on bob's profile does not expose bob's sessions.
    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=shared-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(ids(&body).is_empty(), "{body}");
    assert!(!body.contains(&bobs_shared));
    // Another profile, the server default, a missing one: refused alike.
    let mut refusals = Vec::new();
    for bot in ["bob-bot", "default", "missing-bot"] {
        let (status, body) = call(
            &world.addr,
            "GET",
            &format!("{SESSIONS}?profile={bot}"),
            &world.alice,
            None,
        )
        .await;
        assert_eq!(status, 403, "{bot}: {body}");
        refusals.push(body);
    }
    let (status, body) = call(&world.addr, "GET", SESSIONS, &world.alice, None).await;
    assert_eq!(status, 403, "the default profile is not alice's: {body}");
    refusals.push(body);
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "{refusals:?}"
    );

    // Reading by id: bob's, historic and missing ids are indistinguishable.
    let mut answers = Vec::new();
    for id in [
        bobs.as_str(),
        old.as_str(),
        "0000000000000000000000000000dead",
    ] {
        let (status, body) = call(
            &world.addr,
            "GET",
            &format!("{SESSIONS}/{id}?profile=alice-bot"),
            &world.alice,
            None,
        )
        .await;
        answers.push((status, body));
    }
    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(json_of(&body)["owner"], json!("alice"));
    let first = answers[0].clone();
    assert_eq!(first.0, 404);
    assert!(
        answers.iter().all(|answer| answer.1 == first.1),
        "{answers:?}"
    );
    // Search never surfaces another user's sessions.
    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}/search?q=session&profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body), vec![mine]);
    // The operator still sees the default store unchanged.
    let (status, _) = call(&world.addr, "GET", SESSIONS, &world.ops, None).await;
    assert_eq!(status, 200);
}

// 3. A user cannot attach another user's session to a run.
#[tokio::test]
async fn a_run_cannot_attach_another_users_session() {
    let world = world().await;
    let mine = create(&world, &world.alice, "alice-bot").await;
    let bobs = create(&world, &world.bob, "bob-bot").await;
    let old = historic(&world, "alice-bot");
    let mut answers = Vec::new();
    for id in [
        bobs.as_str(),
        old.as_str(),
        "0000000000000000000000000000dead",
    ] {
        let body = json!({"message": "hi", "profile": "alice-bot", "session_id": id}).to_string();
        answers.push(
            call(
                &world.addr,
                "POST",
                "/api/lightagent/v1/runs",
                &world.alice,
                Some(&body),
            )
            .await,
        );
    }
    assert!(
        answers
            .iter()
            .all(|answer| answer.0 == 404 && answer.1 == answers[0].1),
        "{answers:?}"
    );
    // Bob's session named with bob's profile: alice is not authorized there.
    let body = json!({"message": "hi", "profile": "bob-bot", "session_id": bobs}).to_string();
    let (status, _) = call(
        &world.addr,
        "POST",
        "/api/lightagent/v1/runs",
        &world.alice,
        Some(&body),
    )
    .await;
    assert_eq!(status, 403);
    assert!(world.runs.lock().unwrap().is_empty(), "no run started");
    // Her own session works.
    let body = json!({"message": "hi", "profile": "alice-bot", "session_id": mine}).to_string();
    let (status, body) = call(
        &world.addr,
        "POST",
        "/api/lightagent/v1/runs",
        &world.alice,
        Some(&body),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(json_of(&body)["session_id"], json!(mine));
}

// 4, 14. Updates and deletion check ownership; a read-only credential
// changes nothing.
#[tokio::test]
async fn mutations_need_ownership_and_write_permission() {
    let world = world().await;
    let mine = create(&world, &world.alice, "alice-bot").await;
    let bobs = create(&world, &world.bob, "bob-bot").await;
    for (method, body) in [("PATCH", Some(r#"{"title":"taken"}"#)), ("DELETE", None)] {
        let (status, _) = call(
            &world.addr,
            method,
            &format!("{SESSIONS}/{bobs}?profile=alice-bot"),
            &world.alice,
            body,
        )
        .await;
        assert_eq!(status, 404, "{method}");
    }
    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}/{bobs}?profile=bob-bot"),
        &world.bob,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json_of(&body)["title"], json!("agent session"), "untouched");

    for (method, path, body) in [
        ("POST", format!("{SESSIONS}?profile=alice-bot"), None),
        (
            "PATCH",
            format!("{SESSIONS}/{mine}?profile=alice-bot"),
            Some(r#"{"title":"x"}"#),
        ),
        (
            "DELETE",
            format!("{SESSIONS}/{mine}?profile=alice-bot"),
            None,
        ),
    ] {
        let (status, _) = call(&world.addr, method, &path, &world.alice_ro, body).await;
        assert_eq!(status, 403, "read-only {method}");
    }
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.alice_ro,
        None,
    )
    .await;
    assert_eq!(status, 200, "reading is allowed");

    let (status, body) = call(
        &world.addr,
        "PATCH",
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.alice,
        Some(r#"{"title":"renamed"}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = call(
        &world.addr,
        "DELETE",
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(
        (status, json_of(&body)["deleted"].clone()),
        (200, json!(true))
    );
}

// 5. Profile enumeration lists only accessible profiles.
#[tokio::test]
async fn users_enumerate_only_accessible_profiles() {
    let world = world().await;
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/profiles",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let parsed = json_of(&body);
    let listed: Vec<&str> = parsed["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|summary| summary["id"].as_str())
        .collect();
    assert_eq!(listed, vec!["alice-bot", "shared-bot"]);
    assert_eq!(json_of(&body)["active_profile"], Value::Null);
    for leaked in ["bob-bot\"", "default", "bob-bot name"] {
        assert!(!body.contains(leaked), "{leaked}: {body}");
    }
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/profiles",
        &world.ops,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        json_of(&body)["profiles"].as_array().unwrap().len(),
        4,
        "operators see all"
    );
}

// 6, 7, 21. The tool catalog is the authorized profile's effective catalog
// for an untrusted run, never another profile's or the installation's.
#[tokio::test]
async fn the_tool_catalog_is_profile_scoped() {
    let world = world().await;
    let (status, _) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/tools?profile=bob-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 403);
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/tools?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let parsed = json_of(&body);
    let names: Vec<&str> = parsed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["datetime.now", "skill.read"]);
    for restricted in ["terminal.run", "fs.write", "web.fetch"] {
        assert!(!body.contains(restricted), "{restricted}");
    }
    assert_eq!(
        world.catalogs.lock().unwrap().clone(),
        vec![("alice-bot".to_owned(), RunTrust::RemoteUser)],
        "asked only for alice's own profile, as an untrusted run"
    );
    // Reading Jev status in between changes no tool eligibility.
    call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/jev?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    let (_, again) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/tools?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(json_of(&again)["tools"], json_of(&body)["tools"]);
    // Operators keep the installation catalog.
    let (_, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/tools",
        &world.ops,
        None,
    )
    .await;
    assert!(body.contains("terminal.run"));
}

// Skills follow the effective catalog: none for a profile whose runs get no
// `skill.read`.
#[tokio::test]
async fn skills_are_listed_only_where_skill_read_is_available() {
    let world = world().await;
    for bot in ["alice-bot", "bob-bot"] {
        let dir = ProfileStore::new(&world.home.0)
            .handle(&profile(bot))
            .dir()
            .join("skills")
            .join(format!("{bot}-skill"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {bot}-skill\ndescription: For {bot}\n---\n\nBody.\n"),
        )
        .unwrap();
    }
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/skills?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("alice-bot-skill") && !body.contains("bob-bot-skill"),
        "{body}"
    );
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/skills?profile=bob-bot",
        &world.bob,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        json_of(&body)["skills"],
        json!([]),
        "bob-bot runs get no skill.read"
    );
    let (status, _) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/skills?profile=bob-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 403);
}

// 8, 9. Jev diagnostics are the authorized profile's own; global Jev
// settings and verification stay operator-only.
#[tokio::test]
async fn jev_status_is_profile_scoped_and_settings_operator_only() {
    let world = world().await;
    let (status, _) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/jev?profile=bob-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 403);
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/jev?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("alice-bot-category"));
    for leaked in [
        "bob-bot",
        "operator-view",
        "allowed_profiles",
        "endpoint_configured",
    ] {
        assert!(!body.contains(leaked), "{leaked}: {body}");
    }
    for (method, path, payload) in [
        ("PUT", "/api/lightagent/v1/settings", Some("{}")),
        ("GET", "/api/lightagent/v1/settings", None),
        ("POST", "/api/lightagent/v1/jev/verify", None),
    ] {
        let (status, _) = call(&world.addr, method, path, &world.alice, payload).await;
        assert!(
            status == 403 || (method == "PUT" && (400..500).contains(&status)),
            "{method} {path}: {status}"
        );
    }
}

// 10. The provider view never exposes the endpoint, catalogs or another
// profile's model.
#[tokio::test]
async fn the_provider_view_withholds_installation_details() {
    let world = world().await;
    let (status, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/provider?profile=alice-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let view = json_of(&body);
    assert_eq!(view["base_url"], Value::Null);
    assert_eq!(view["configured_model"], json!("alice-bot-model"));
    for leaked in [
        "provider.test",
        "secret-model",
        "installation-model",
        "bob-bot",
    ] {
        assert!(!body.contains(leaked), "{leaked}: {body}");
    }
    let (status, _) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/provider?profile=bob-bot",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 403);
    let (_, body) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/provider",
        &world.ops,
        None,
    )
    .await;
    assert!(
        body.contains("provider.test"),
        "the operator view is unchanged"
    );
}

// 11, 12, 13. Reassignment, revocation and suspension apply at the next
// request; sessions are never reassigned with the profile.
#[tokio::test]
async fn ownership_and_credential_changes_apply_at_the_next_request() {
    let world = world().await;
    let mine = create(&world, &world.alice, "alice-bot").await;
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
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 403, "alice lost the profile");
    // The new owner does not inherit alice's session.
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}/{mine}?profile=alice-bot"),
        &world.bob,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=alice-bot"),
        &world.bob,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(ids(&body).is_empty());

    world
        .identity
        .update(|state| state.credentials.revoke(&key_id(&world.bob), 2))
        .unwrap()
        .unwrap();
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=bob-bot"),
        &world.bob,
        None,
    )
    .await;
    assert_eq!(status, 401, "revoked");
    world
        .identity
        .update(|state| state.credentials.revoke_principal(&principal("alice"), 3))
        .unwrap();
    for token in [&world.alice, &world.alice_ro] {
        let (status, _) = call(
            &world.addr,
            "GET",
            "/api/lightagent/v1/profiles",
            token,
            None,
        )
        .await;
        assert_eq!(status, 401, "suspended");
    }
}

// 15. Forged or malformed profile identifiers.
#[tokio::test]
async fn forged_profile_identifiers_are_refused() {
    let world = world().await;
    for forged in ["..%2Fbob-bot", "Bob-bot", "%20bob-bot", "bob-bot%00"] {
        let (status, _) = call(
            &world.addr,
            "GET",
            &format!("{SESSIONS}?profile={forged}"),
            &world.alice,
            None,
        )
        .await;
        assert!(status == 400 || status == 403, "{forged}: {status}");
    }
    let (status, _) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=bob-bot&profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert!(
        status == 400 || status == 403,
        "an ambiguous profile is not resolved in the caller's favor: {status}"
    );
}

// 17. Concurrent requests by two users never cross.
#[tokio::test]
async fn concurrent_requests_never_cross_profiles() {
    let world = Arc::new(world().await);
    let alices = create(&world, &world.alice, "alice-bot").await;
    let bobs = create(&world, &world.bob, "bob-bot").await;
    let mut tasks = Vec::new();
    for round in 0..24 {
        let world = Arc::clone(&world);
        let (alices, bobs) = (alices.clone(), bobs.clone());
        tasks.push(tokio::spawn(async move {
            let (token, bot, own, other) = if round % 2 == 0 {
                (world.alice.clone(), "alice-bot", alices, bobs)
            } else {
                (world.bob.clone(), "bob-bot", bobs, alices)
            };
            let (status, body) = call(
                &world.addr,
                "GET",
                &format!("{SESSIONS}?profile={bot}"),
                &token,
                None,
            )
            .await;
            assert_eq!(status, 200);
            assert_eq!(ids(&body), vec![own.clone()]);
            assert!(!body.contains(&other));
            let (status, _) = call(
                &world.addr,
                "GET",
                &format!("{SESSIONS}/{other}?profile={bot}"),
                &token,
                None,
            )
            .await;
            assert_eq!(status, 404);
            let (status, _) = call(
                &world.addr,
                "POST",
                &format!("{SESSIONS}?profile={bot}"),
                &token,
                None,
            )
            .await;
            assert_eq!(status, 201);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let (_, body) = call(
        &world.addr,
        "GET",
        &format!("{SESSIONS}?profile=alice-bot"),
        &world.alice,
        None,
    )
    .await;
    assert_eq!(
        ids(&body).len(),
        13,
        "alice's 1 + 12 created, no one else's"
    );
}

// 18. Single-user mode is unchanged: no profile scoping, no owner field on
// sessions created there.
#[tokio::test]
async fn single_user_mode_is_unchanged() {
    let root = std::env::temp_dir().join(format!("lightagent-scoped-{}", RunId::new().as_str()));
    let _home = Home(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    ProfileStore::new(&root)
        .create(&AgentProfile::new(
            profile("default"),
            "Default",
            "Persona",
            "m",
        ))
        .unwrap();
    let factory = Scoped::default();
    let catalogs = factory.catalogs.clone();
    let addr = serve(
        &root,
        RunManager::new(Arc::new(factory)),
        AuthConfig::open(),
    )
    .await;
    let (status, body) = call(&addr, "POST", SESSIONS, "", None).await;
    assert_eq!(status, 201, "{body}");
    let id = json_of(&body)["id"].as_str().unwrap().to_owned();
    let (_, body) = call(&addr, "GET", SESSIONS, "", None).await;
    assert_eq!(ids(&body), vec![id.clone()]);
    assert!(!body.contains("owner"));
    let (_, body) = call(&addr, "GET", &format!("{SESSIONS}/{id}"), "", None).await;
    assert!(!body.contains("owner"));
    let (_, body) = call(&addr, "GET", "/api/lightagent/v1/tools", "", None).await;
    assert!(body.contains("terminal.run") && !body.contains("\"profile\""));
    let (_, body) = call(&addr, "GET", "/api/lightagent/v1/provider", "", None).await;
    assert!(body.contains("provider.test"));
    let (_, body) = call(&addr, "GET", "/api/lightagent/v1/profiles", "", None).await;
    assert_eq!(json_of(&body)["active_profile"], json!("default"));
    assert!(
        catalogs.lock().unwrap().is_empty(),
        "no scoped catalog in single-user mode"
    );
}

// 22. No new access through other routes: unscoped and operator routes stay
// operator-only for users, and a profile query does not change that.
#[tokio::test]
async fn other_routes_grant_users_nothing_new() {
    let world = world().await;
    for (method, path, payload) in [
        ("GET", "/api/lightagent/v1/settings?profile=alice-bot", None),
        (
            "POST",
            "/api/lightagent/v1/jev/verify?profile=alice-bot",
            None,
        ),
        ("GET", "/api/lightagent/v1/approvals?profile=bob-bot", None),
    ] {
        let (status, body) = call(&world.addr, method, path, &world.alice, payload).await;
        if path.contains("approvals") {
            assert_eq!(status, 200);
            assert_eq!(json_of(&body)["approvals"], json!([]));
        } else {
            assert_eq!(status, 403, "{method} {path}");
        }
    }
    let (status, _) = call(
        &world.addr,
        "GET",
        "/api/lightagent/v1/unknown",
        &world.alice,
        None,
    )
    .await;
    assert_eq!(status, 404);
}
