//! HTTP contract tests over a real loopback listener, plus run-lifecycle tests.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_api::manager::{self, RunFactory, RunManager, RunStatus, StartRun};
use lightagent_api::{AppState, AuthConfig, Scope, router};
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::provider::ProviderMessage;
use lightagent_core::{
    AgentEventSink, AgentLoop, AgentProfile, ApprovalDecision,
    ApprovalPolicy as ConfigApprovalPolicy, ConfigStore, FinishReason, MockProvider, PolicyEngine,
    ProfileId, ProfileStore, ProviderEvent, RunConfig,
};
use lightagent_store::SessionStore;
use lightagent_tools::{BoundedExecutor, ToolRegistry};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;

/// A factory that scripts a datetime.now tool call then a final answer.
struct MockFactory;

/// Reports a deliberately smaller registry than the default factory.
struct LimitedFactory;

#[async_trait]
impl RunFactory for LimitedFactory {
    async fn tools(&self) -> Result<Vec<lightagent_tools::ToolDefinition>, String> {
        let registry = ToolRegistry::builtin();
        Ok(registry
            .get("datetime.now")
            .map(|tool| vec![tool.definition().clone()])
            .unwrap_or_default())
    }

    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        MockFactory.run(request, sink, cancel, decisions).await
    }
}

fn tool_turn() -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("datetime.now".into()),
            arguments: Some("{}".into()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        },
    ]
}

fn final_turn() -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Content("The time is now.".into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]
}

#[async_trait]
impl RunFactory for MockFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        let provider = MockProvider::new(vec![tool_turn(), final_turn()]);
        let executor = BoundedExecutor::new(
            ToolRegistry::builtin(),
            PolicyEngine::new(ApprovalPolicy::permissive()),
            Duration::from_secs(5),
            262_144,
        );
        let agent = AgentLoop::new(provider, executor, RunConfig::new("mock"));
        manager::drive(
            agent,
            request.history,
            request.message,
            sink,
            cancel,
            decisions,
        )
        .await
    }
}

struct HistoryFactory {
    requests: Arc<tokio::sync::Mutex<Vec<Vec<ProviderMessage>>>>,
}

#[async_trait]
impl RunFactory for HistoryFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.requests.lock().await.push(request.history.clone());
        MockFactory.run(request, sink, cancel, decisions).await
    }
}

fn app_state(auth: AuthConfig) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-api-{}",
        lightagent_core::RunId::new().as_str()
    ));
    AppState {
        manager: RunManager::new(Arc::new(MockFactory)),
        auth,
        sessions: SessionStore::new(dir),
        session_profile: "default".into(),
        context_limit: 4_096,
        config_store: None,
        busy_sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
        web_root: None,
    }
}

async fn spawn_server(state: AppState) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(state).into_make_service()).await;
    });
    format!("{addr}")
}

/// Minimal raw HTTP client: returns (status, body). `Connection: close` lets us
/// read a streamed (SSE) response to EOF.
async fn http(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n");
    for (name, value) in headers {
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
        .map(|(headers, rest)| {
            if headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
            {
                decode_chunked(rest.as_bytes())
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_else(|| rest.to_owned())
            } else {
                rest.to_owned()
            }
        })
        .unwrap_or_default();
    (status, body)
}

/// Decode the HTTP/1.1 chunk framing used by axum's SSE response.
///
/// The test client intentionally stays smaller than a general HTTP client, but
/// assertions must see the entity body rather than chunk-size lines inserted
/// between arbitrary bytes of an event name.
fn decode_chunked(mut encoded: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = encoded.windows(2).position(|part| part == b"\r\n")?;
        let size = std::str::from_utf8(&encoded[..line_end])
            .ok()?
            .split(';')
            .next()?
            .trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        encoded = &encoded[line_end + 2..];
        if size == 0 {
            return Some(decoded);
        }
        let end = size.checked_add(2)?;
        if encoded.len() < end || &encoded[size..end] != b"\r\n" {
            return None;
        }
        decoded.extend_from_slice(&encoded[..size]);
        encoded = &encoded[end..];
    }
}

// --- run lifecycle (no HTTP) ------------------------------------------------

#[tokio::test]
async fn a_run_drives_to_completion_and_buffers_its_events() {
    let manager = RunManager::new(Arc::new(MockFactory));
    let run = manager
        .start(StartRun {
            message: "what time is it?".into(),
            history: Vec::new(),
            profile: None,
            cwd: None,
            model: None,
        })
        .await;

    // Drive to a terminal state by streaming from the start.
    let (mut events, mut status) = run.wait_from(0).await;
    while !status.is_terminal() {
        let (more, next) = run.wait_from(events.len()).await;
        events.extend(more);
        status = next;
    }
    assert_eq!(status, RunStatus::Completed);
    let names: Vec<_> = events.iter().map(lightagent_api::sse::name).collect();
    assert!(names.contains(&"run.started"));
    assert!(names.contains(&"tool.requested"));
    assert!(names.contains(&"tool.started"));
    assert!(names.contains(&"tool.output"));
    assert!(names.contains(&"run.completed"));
}

#[tokio::test]
async fn cancelling_an_unknown_run_is_a_404_and_a_known_one_cancels() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;
    let (status, _) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs/nope/cancel",
        &[],
        Some("{}"),
    )
    .await;
    assert_eq!(status, 404);
}

// --- HTTP contract ----------------------------------------------------------

#[tokio::test]
async fn health_and_tools_are_served() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;

    let (status, body) = http(&addr, "GET", "/health", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"ok\""));

    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/tools", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("datetime.now"));
    assert!(body.contains("agent.delegate"));
}

#[tokio::test]
async fn tools_endpoint_uses_the_factory_registry() {
    let mut state = app_state(AuthConfig::open());
    state.manager = RunManager::new(Arc::new(LimitedFactory));
    let addr = spawn_server(state).await;
    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/tools", &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("datetime.now"));
    assert!(!body.contains("agent.delegate"));
}

#[tokio::test]
async fn a_run_can_be_created_and_streamed_over_sse() {
    let addr = spawn_server(app_state(AuthConfig::keyed("agent-key", [Scope::Admin]))).await;
    let auth = [("Authorization", "Bearer agent-key")];

    let (status, body) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        &auth,
        Some(r#"{"message":"what time is it?"}"#),
    )
    .await;
    assert_eq!(status, 202);
    let id = body
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();
    assert!(id.starts_with("run-"));

    // The event stream replays the run and ends with a terminal event.
    let (status, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}/events"),
        &auth,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("run.started"));
    assert!(body.contains("tool.requested"));
    assert!(body.contains("tool.started"));
    assert!(body.contains("tool.output"));
    assert!(body.contains("model.delta"));
    assert!(body.contains("run.completed"));
}

/// One SSE frame: its id, event name and data.
#[derive(Debug, PartialEq)]
struct Frame {
    id: Option<usize>,
    name: String,
    data: String,
}

fn frames(body: &str) -> Vec<Frame> {
    body.split("\n\n")
        .filter_map(|block| {
            let mut frame = Frame {
                id: None,
                name: String::new(),
                data: String::new(),
            };
            for line in block.lines() {
                if let Some(id) = line.strip_prefix("id: ") {
                    frame.id = id.trim().parse().ok();
                } else if let Some(name) = line.strip_prefix("event: ") {
                    frame.name = name.to_owned();
                } else if let Some(data) = line.strip_prefix("data: ") {
                    frame.data.push_str(data);
                }
            }
            (!frame.name.is_empty()).then_some(frame)
        })
        .collect()
}

/// Start a run and read its whole stream once it has ended.
async fn finished_run(addr: &str, auth: &[(&str, &str)]) -> (String, Vec<Frame>) {
    let (status, body) = http(
        addr,
        "POST",
        "/api/lightagent/v1/runs",
        auth,
        Some(r#"{"message":"what time is it?"}"#),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = http(
        addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}/events"),
        auth,
        None,
    )
    .await;
    assert_eq!(status, 200);
    (id, frames(&body))
}

#[tokio::test]
async fn every_event_is_numbered_in_order_and_the_stream_ends_after_its_terminal_event() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;
    let (_, all) = finished_run(&addr, &[]).await;
    assert!(all.len() > 4, "{all:?}");
    for (position, frame) in all.iter().enumerate() {
        assert_eq!(frame.id, Some(position), "{frame:?}");
    }
    let last = all.last().unwrap();
    assert_eq!(last.name, "run.completed");
    let data: serde_json::Value = serde_json::from_str(&last.data).unwrap();
    assert_eq!(data["reason"], "EndTurn");
    assert_eq!(data["complete"], true);
}

#[tokio::test]
async fn a_reconnecting_stream_resumes_after_the_last_event_it_saw() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;
    let (id, all) = finished_run(&addr, &[]).await;
    let events = format!("/api/lightagent/v1/runs/{id}/events");
    let tail = |from: usize| -> Vec<(Option<usize>, String)> {
        all[from..]
            .iter()
            .map(|frame| (frame.id, frame.name.clone()))
            .collect()
    };
    let resumed = |body: &str| -> Vec<(Option<usize>, String)> {
        frames(body)
            .into_iter()
            .map(|frame| (frame.id, frame.name))
            .collect()
    };

    // `?after=` (what the Web UI sends when it reopens a stream).
    let (status, body) = http(&addr, "GET", &format!("{events}?after=2"), &[], None).await;
    assert_eq!(status, 200);
    assert_eq!(resumed(&body), tail(3), "nothing repeated, nothing skipped");

    // `Last-Event-ID` (what a browser's EventSource sends on its own).
    let (_, body) = http(&addr, "GET", &events, &[("Last-Event-ID", "4")], None).await;
    assert_eq!(resumed(&body), tail(5));

    // Both: the later cursor wins.
    let (_, body) = http(
        &addr,
        "GET",
        &format!("{events}?after=4"),
        &[("Last-Event-ID", "1")],
        None,
    )
    .await;
    assert_eq!(resumed(&body), tail(5));

    // An unreadable header is ignored: the whole run, which a client
    // deduplicates by id.
    let (_, body) = http(&addr, "GET", &events, &[("Last-Event-ID", "x")], None).await;
    assert_eq!(resumed(&body), tail(0));

    // A cursor at or past the end of a finished run: nothing more, and the
    // stream still ends.
    let last = all.len() - 1;
    let (status, body) = http(&addr, "GET", &format!("{events}?after={last}"), &[], None).await;
    assert_eq!(status, 200);
    assert!(frames(&body).is_empty(), "{body}");
    let (status, body) = http(&addr, "GET", &format!("{events}?after=99999"), &[], None).await;
    assert_eq!(status, 200);
    assert!(frames(&body).is_empty(), "{body}");

    // A malformed or negative explicit cursor is a client error.
    for cursor in ["soon", "-1", "1.5"] {
        let path = format!("{events}?after={cursor}");
        let (status, _) = http(&addr, "GET", &path, &[], None).await;
        assert_eq!(status, 400, "{cursor}");
    }
}

#[tokio::test]
async fn a_resumed_stream_keeps_the_same_authentication() {
    let addr = spawn_server(app_state(AuthConfig::keyed("agent-key", [Scope::Admin]))).await;
    let auth = [("Authorization", "Bearer agent-key")];
    let (id, _) = finished_run(&addr, &auth).await;
    let events = format!("/api/lightagent/v1/runs/{id}/events?after=0");
    let (status, _) = http(&addr, "GET", &events, &[("Last-Event-ID", "0")], None).await;
    assert_eq!(status, 401, "a cursor never stands in for a credential");
    let (status, _) = http(&addr, "GET", &events, &auth, None).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_finished_run_reports_why_it_stopped() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;
    let (id, _) = finished_run(&addr, &[]).await;
    let (status, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(run["status"], "completed");
    assert_eq!(run["stop_reason"], "EndTurn");
}

#[tokio::test]
async fn a_saved_session_is_reused_by_follow_up_runs_and_new_sessions_are_empty() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut state = app_state(AuthConfig::open());
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        state.manager = RunManager::new(Arc::new(HistoryFactory {
            requests: Arc::clone(&requests),
        }));
        let store = state.sessions.clone();
        let addr = spawn_server(state).await;
        let (status, body) = http(
            &addr,
            "POST",
            "/api/lightagent/v1/sessions",
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(status, 201);
        let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();

        for (index, message) in ["hello", "follow up"].into_iter().enumerate() {
            let payload = serde_json::json!({ "message": message, "session_id": id }).to_string();
            let (status, body) = http(
                &addr,
                "POST",
                "/api/lightagent/v1/runs",
                &[],
                Some(&payload),
            )
            .await;
            assert_eq!(status, 202, "{body}");
            let run = serde_json::from_str::<serde_json::Value>(&body).unwrap();
            assert_eq!(run["session_id"], id);
            let id_parsed = lightagent_store::SessionId::parse(&id).unwrap();
            loop {
                let saved = store.load(&id_parsed).unwrap();
                if saved.runs.len() == index + 1 {
                    assert_eq!(saved.messages.len(), (index + 1) * 2);
                    assert_eq!(saved.messages[index * 2].content, message);
                    assert_eq!(saved.title, "hello");
                    // The run's stop reason is saved, not just its status.
                    assert_eq!(saved.runs[index].stop_reason.as_deref(), Some("EndTurn"));
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        assert!(requests.lock().await[0].is_empty());
        assert_eq!(
            requests.lock().await[1],
            vec![
                ProviderMessage::user("hello"),
                ProviderMessage::assistant("The time is now."),
            ]
        );

        let (status, body) = http(
            &addr,
            "POST",
            "/api/lightagent/v1/sessions",
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(status, 201);
        let new_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(id, new_id);
        assert!(
            store
                .load(&lightagent_store::SessionId::parse(&new_id).unwrap())
                .unwrap()
                .messages
                .is_empty()
        );

        let (status, body) = http(
            &addr,
            "GET",
            &format!("/api/lightagent/v1/sessions/{id}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 200);
        let saved: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(saved["title"], "hello");
        assert_eq!(saved["messages"].as_array().unwrap().len(), 4);
        assert_eq!(saved["runs"].as_array().unwrap().len(), 2);

        let (status, body) = http(&addr, "GET", "/api/lightagent/v1/sessions", &[], None).await;
        assert_eq!(status, 200);
        let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            listed["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == id)
        );

        let (status, body) = http(
            &addr,
            "DELETE",
            &format!("/api/lightagent/v1/sessions/{id}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["deleted"],
            true
        );
        let (status, _) = http(
            &addr,
            "GET",
            &format!("/api/lightagent/v1/sessions/{id}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 404);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn session_metadata_can_be_updated_and_is_returned_in_listings() {
    let state = app_state(AuthConfig::open());
    let store = state.sessions.clone();
    let mut session = lightagent_store::Session::new("default", "Initial title");
    session.archived = true;
    store.save(&session).unwrap();
    let id = session.id.as_str().to_owned();
    let addr = spawn_server(state).await;

    let (status, body) = http(
        &addr,
        "PATCH",
        &format!("/api/lightagent/v1/sessions/{id}"),
        &[],
        Some(r#"{"title":"Renamed chat","pinned":true,"archived":false,"project":"Launch"}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let updated: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(updated["title"], "Renamed chat");
    assert_eq!(updated["pinned"], true);
    assert_eq!(updated["archived"], false);
    assert_eq!(updated["project"], "Launch");
    assert!(updated.get("message_count").is_some());

    let saved = store
        .load(&lightagent_store::SessionId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(saved.title, "Renamed chat");
    assert!(saved.pinned);
    assert!(!saved.archived);
    assert_eq!(saved.project.as_deref(), Some("Launch"));

    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/sessions", &[], None).await;
    assert_eq!(status, 200, "{body}");
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let summary = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert_eq!(summary["pinned"], true);
    assert_eq!(summary["archived"], false);
    assert_eq!(summary["project"], "Launch");

    let (status, body) = http(
        &addr,
        "PATCH",
        &format!("/api/lightagent/v1/sessions/{id}"),
        &[],
        Some(r#"{"project":null}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .get("project")
            .is_none()
    );
}

#[tokio::test]
async fn ui_settings_update_the_cli_config_and_active_profile() {
    let root = std::env::temp_dir().join(format!(
        "lightagent-ui-settings-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let config_store = ConfigStore::new(root.join("config.json"));
    let profiles = ProfileStore::new(&root);
    let profile_id = ProfileId::new("default").unwrap();
    let profile = AgentProfile::new(profile_id.clone(), "Default", "Be helpful.", "default");
    profiles.save(&profile).unwrap();
    let mut initial = lightagent_core::Config::default();
    initial.platform.jev.endpoint.api_key =
        Some(lightagent_core::SecretRef::env("JEV_TEST_REFERENCE"));
    initial.platform.open_terminal.endpoint.api_key =
        Some(lightagent_core::SecretRef::env("TERMINAL_TEST_REFERENCE"));
    config_store.save(&initial).unwrap();

    let mut state = app_state(AuthConfig::open());
    state.config_store = Some(config_store.clone());
    let addr = spawn_server(state).await;
    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/settings", &[], None).await;
    assert_eq!(status, 200, "{body}");
    let mut settings: serde_json::Value = serde_json::from_str(&body).unwrap();
    settings["approval_policy"] = "strict".into();
    settings["max_turns"] = 12.into();
    settings["max_tool_calls"] = 7.into();
    settings["web_enabled"] = true.into();
    settings["filesystem_tools_enabled"] = true.into();
    settings["terminal_enabled"] = true.into();
    settings["memory_enabled"] = false.into();
    settings["subagents_enabled"] = false.into();
    settings["delegate_timeout_secs"] = 240.into();
    settings["jev"]["allowed_models"] = serde_json::json!(["fast"]);
    settings["jev"]["allowed_profiles"] = serde_json::json!(["careful"]);
    settings["jev"]["timeout_secs"] = 2.into();
    settings["jev"]["api_key"] = "jev-ui-test-secret".into();
    settings["qdrant"]["timeout_secs"] = 9.into();
    settings["infinity"]["timeout_secs"] = 12.into();
    settings["open_terminal"]["execution_timeout_secs"] = 90.into();
    settings["open_terminal"]["max_output_bytes"] = 4096.into();
    let payload = settings.to_string();
    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&payload),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let loaded = config_store.load().unwrap();
    assert_eq!(
        loaded.security.approval_policy,
        ConfigApprovalPolicy::Strict
    );
    assert_eq!(loaded.agent.max_turns, 12);
    assert!(loaded.web.enabled);
    assert!(loaded.tools.enabled && loaded.tools.allow_terminal);
    assert!(!loaded.memory.auto_capture);
    assert!(!loaded.subagents.enabled);
    assert_eq!(loaded.subagents.delegate_timeout_secs, 240);
    assert_eq!(loaded.platform.jev.allowed_models, ["fast"]);
    assert_eq!(loaded.platform.jev.allowed_profiles, ["careful"]);
    assert_eq!(loaded.platform.jev.timeout_secs, 2);
    assert_eq!(loaded.platform.qdrant.timeout_secs, 9);
    assert_eq!(loaded.platform.infinity.timeout_secs, 12);
    assert_eq!(loaded.platform.open_terminal.execution_timeout_secs, 90);
    assert_eq!(loaded.platform.open_terminal.max_output_bytes, 4096);
    assert_eq!(
        loaded
            .platform
            .jev
            .endpoint
            .api_key
            .as_ref()
            .and_then(lightagent_core::SecretRef::resolve)
            .as_deref(),
        Some("jev-ui-test-secret")
    );
    assert_eq!(
        loaded.platform.open_terminal.endpoint.api_key,
        initial.platform.open_terminal.endpoint.api_key
    );
    assert!(!body.contains("JEV_TEST_REFERENCE"));
    assert!(!body.contains("jev-ui-test-secret"));
    assert!(!body.contains("TERMINAL_TEST_REFERENCE"));
    let profile = profiles.load(&profile_id).unwrap();
    assert_eq!(profile.approval_policy, ConfigApprovalPolicy::Strict);
    assert_eq!(profile.limits.max_turns, 12);
    assert_eq!(profile.limits.max_tool_calls, 7);

    // Older clients omit newly introduced platform fields. Preserve their
    // configured routing, limits and secrets when saving that payload.
    let mut legacy_settings = settings.clone();
    for (section, fields) in [
        (
            "jev",
            vec!["allowed_models", "allowed_profiles", "timeout_secs"],
        ),
        ("qdrant", vec!["timeout_secs"]),
        ("infinity", vec!["timeout_secs"]),
        (
            "open_terminal",
            vec![
                "request_timeout_secs",
                "execution_timeout_secs",
                "poll_interval_ms",
                "max_output_bytes",
            ],
        ),
    ] {
        let endpoint = legacy_settings[section].as_object_mut().unwrap();
        for field in fields {
            endpoint.remove(field);
        }
    }
    legacy_settings["show_reasoning_in_tui"] = true.into();
    legacy_settings
        .as_object_mut()
        .unwrap()
        .remove("subagents_enabled");
    legacy_settings
        .as_object_mut()
        .unwrap()
        .remove("delegate_timeout_secs");
    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&legacy_settings.to_string()),
    )
    .await;
    assert_eq!(status, 200, "legacy settings payload: {body}");
    let legacy_loaded = config_store.load().unwrap();
    assert_eq!(legacy_loaded.platform, loaded.platform);
    assert!(legacy_loaded.tui.show_reasoning);
    assert!(!legacy_loaded.subagents.enabled);
    assert_eq!(legacy_loaded.subagents.delegate_timeout_secs, 240);
    assert!(!body.contains("JEV_TEST_REFERENCE"));
    assert!(!body.contains("TERMINAL_TEST_REFERENCE"));

    settings["open_terminal"]["execution_timeout_secs"] = 0.into();
    let (status, _) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        config_store
            .load()
            .unwrap()
            .platform
            .open_terminal
            .execution_timeout_secs,
        90
    );
    settings["open_terminal"]["execution_timeout_secs"] = 90.into();
    settings["max_turns"] = 0.into();
    let (status, _) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(config_store.load().unwrap().agent.max_turns, 12);
    assert_eq!(profiles.load(&profile_id).unwrap().limits.max_turns, 12);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn deleting_a_busy_session_is_refused_until_its_run_finishes() {
    let state = app_state(AuthConfig::open());
    let session = lightagent_store::Session::new("default", "Busy");
    state.sessions.save(&session).unwrap();
    state.busy_sessions.lock().await.insert(session.id.clone());
    let addr = spawn_server(state).await;
    let (status, body) = http(
        &addr,
        "DELETE",
        &format!("/api/lightagent/v1/sessions/{}", session.id.as_str()),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 409);
    assert!(body.contains("active run"));
}

#[tokio::test]
async fn unknown_agent_api_paths_return_json_not_the_panel_document() {
    let addr = spawn_server(app_state(AuthConfig::open())).await;
    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/not-a-route", &[], None).await;
    assert_eq!(status, 404);
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["error"], "unknown Lightagent API endpoint");
}

#[tokio::test]
async fn scoped_auth_refuses_cross_capability_access() {
    // A key that may read runs but not start them.
    let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
    let addr = spawn_server(app_state(auth)).await;

    // No key at all: 401.
    let (status, _) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        &[],
        Some(r#"{"message":"x"}"#),
    )
    .await;
    assert_eq!(status, 401);

    // Right key, wrong scope (RunsWrite): 403.
    let (status, _) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        &[("Authorization", "Bearer secret")],
        Some(r#"{"message":"x"}"#),
    )
    .await;
    assert_eq!(status, 403);

    // Right key, right scope (RunsRead) on a read route: allowed (404 for an
    // unknown run, i.e. past the auth gate).
    let (status, _) = http(
        &addr,
        "GET",
        "/api/lightagent/v1/runs/whatever",
        &[("Authorization", "Bearer secret")],
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn jev_status_and_verification_use_the_existing_scopes() {
    // A tool-catalog reader may see the Jev status but may not spend a Jev
    // request on a connection check, and nothing reaches either without a key.
    let auth = AuthConfig::keyed("reader", [Scope::ToolsRead]);
    let addr = spawn_server(app_state(auth)).await;
    for (method, path) in [
        ("GET", "/api/lightagent/v1/jev"),
        ("POST", "/api/lightagent/v1/jev/verify"),
    ] {
        let (status, _) = http(&addr, method, path, &[], None).await;
        assert_eq!(status, 401, "{method} {path} without a key");
    }
    let reader = [("Authorization", "Bearer reader")];
    // Past the auth gate: this embedding has no Jev status, so 503 JSON.
    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/jev", &reader, None).await;
    assert_eq!(status, 503);
    assert!(body.contains("not available"));
    let (status, _) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/jev/verify",
        &reader,
        None,
    )
    .await;
    assert_eq!(status, 403, "verification needs the administrator scope");

    let admin = AuthConfig::keyed("admin", [Scope::Admin]);
    let addr = spawn_server(app_state(admin)).await;
    let (status, _) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/jev/verify",
        &[("Authorization", "Bearer admin")],
        None,
    )
    .await;
    assert_eq!(status, 503, "an administrator passes the gate");
}

#[tokio::test]
async fn classification_settings_round_trip_without_touching_routing() {
    let root = std::env::temp_dir().join(format!(
        "lightagent-jev-settings-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let config_store = ConfigStore::new(root.join("config.json"));
    let profiles = ProfileStore::new(&root);
    let profile = AgentProfile::new(
        ProfileId::new("default").unwrap(),
        "Default",
        "Be helpful.",
        "default",
    );
    profiles.save(&profile).unwrap();
    let mut initial = lightagent_core::Config::default();
    initial.platform.jev.allowed_models = vec!["fast".to_owned()];
    config_store.save(&initial).unwrap();
    let mut state = app_state(AuthConfig::open());
    state.config_store = Some(config_store.clone());
    let addr = spawn_server(state).await;

    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/settings", &[], None).await;
    assert_eq!(status, 200, "{body}");
    let mut settings: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(settings["jev"]["classification"]["mode"], "off");
    assert_eq!(settings["jev"]["classification"]["explicit_tool"], false);

    // An older client that does not know classification leaves it unchanged.
    let mut older = settings.clone();
    older["jev"]
        .as_object_mut()
        .unwrap()
        .remove("classification");
    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&older.to_string()),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    settings["jev"]["classification"] = serde_json::json!({
        "mode": "shadow",
        "allowed_profiles": ["default"],
        "max_input_chars": 300,
        "default_threshold": 0.92,
        "version_thresholds": {"jev-1.13.0": 0.8},
        "explicit_tool": true,
        "max_calls_per_run": 5
    });
    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let loaded = config_store.load().unwrap();
    let classification = &loaded.platform.jev.classification;
    assert_eq!(
        classification.mode,
        lightagent_core::JevClassificationMode::Shadow
    );
    assert_eq!(classification.allowed_profiles, ["default"]);
    assert_eq!(classification.max_input_chars, 300);
    assert_eq!(classification.version_thresholds["jev-1.13.0"], 0.8);
    assert!(classification.explicit_tool);
    assert_eq!(classification.max_calls_per_run, 5);
    assert_eq!(
        loaded.platform.jev.allowed_models,
        ["fast"],
        "routing untouched"
    );

    // Invalid values are refused and nothing is saved.
    settings["jev"]["classification"]["max_calls_per_run"] = 1.into();
    let (status, _) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &[],
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        config_store
            .load()
            .unwrap()
            .platform
            .jev
            .classification
            .max_calls_per_run,
        5
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn routing_permission_settings_round_trip_and_need_the_administrator() {
    let root = std::env::temp_dir().join(format!(
        "lightagent-routing-settings-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let config_store = ConfigStore::new(root.join("config.json"));
    let profiles = ProfileStore::new(&root);
    let profile = AgentProfile::new(
        ProfileId::new("default").unwrap(),
        "Default",
        "Be helpful.",
        "default",
    );
    profiles.save(&profile).unwrap();
    let mut initial = lightagent_core::Config::default();
    initial.platform.jev.allowed_models = vec!["fast".to_owned()];
    config_store.save(&initial).unwrap();

    let mut state = app_state(AuthConfig::keyed("admin", [Scope::Admin]));
    state.config_store = Some(config_store.clone());
    let addr = spawn_server(state).await;
    let admin = [("Authorization", "Bearer admin")];
    let (status, body) = http(&addr, "GET", "/api/lightagent/v1/settings", &admin, None).await;
    assert_eq!(status, 200, "{body}");
    let mut settings: serde_json::Value = serde_json::from_str(&body).unwrap();
    // A fresh configuration: strict, nobody permitted.
    assert_eq!(settings["jev"]["routing_permission"], "strict");
    assert_eq!(
        settings["jev"]["routing_allowed_profiles"],
        serde_json::json!([])
    );
    settings["jev"]["routing_allowed_profiles"] = serde_json::json!(["default"]);

    // Without the administrator scope a well-formed grant is refused and
    // nothing is read or saved.
    let mut state = app_state(AuthConfig::keyed("reader", [Scope::ToolsRead]));
    state.config_store = Some(config_store.clone());
    let reader_addr = spawn_server(state).await;
    let grant = settings.to_string();
    for headers in [&[][..], &[("Authorization", "Bearer reader")][..]] {
        let (status, _) = http(
            &reader_addr,
            "GET",
            "/api/lightagent/v1/settings",
            headers,
            None,
        )
        .await;
        assert!(status == 401 || status == 403, "{status}");
        let (status, _) = http(
            &reader_addr,
            "PUT",
            "/api/lightagent/v1/settings",
            headers,
            Some(&grant),
        )
        .await;
        assert!(status == 401 || status == 403, "{status}");
    }
    let unchanged = config_store.load().unwrap();
    assert!(
        unchanged.platform.jev.routing_allowed_profiles.is_empty(),
        "nothing was saved"
    );
    assert_eq!(
        unchanged.platform.jev.routing_grant("default"),
        lightagent_core::JevRoutingGrant::Denied
    );

    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &admin,
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let loaded = config_store.load().unwrap();
    assert_eq!(loaded.platform.jev.routing_allowed_profiles, ["default"]);
    assert_eq!(
        loaded.platform.jev.routing_grant("default"),
        lightagent_core::JevRoutingGrant::Allowed
    );
    // Separate from both other profile lists.
    assert!(loaded.platform.jev.allowed_profiles.is_empty());
    assert!(
        loaded
            .platform
            .jev
            .classification
            .allowed_profiles
            .is_empty()
    );

    // An older client omits both fields: the saved permission is unchanged.
    let mut older = settings.clone();
    let jev = older["jev"].as_object_mut().unwrap();
    jev.remove("routing_permission");
    jev.remove("routing_allowed_profiles");
    let (status, body) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &admin,
        Some(&older.to_string()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        config_store
            .load()
            .unwrap()
            .platform
            .jev
            .routing_allowed_profiles,
        ["default"]
    );

    // Invalid values are refused and never widen the permission.
    for (field, value) in [
        ("routing_permission", serde_json::json!("allow_all")),
        ("routing_permission", serde_json::json!(true)),
        ("routing_allowed_profiles", serde_json::json!(["Bad Id!"])),
        ("routing_allowed_profiles", serde_json::json!("default")),
    ] {
        let mut invalid = settings.clone();
        invalid["jev"]["routing_allowed_profiles"] = serde_json::json!([]);
        invalid["jev"][field] = value.clone();
        let (status, _) = http(
            &addr,
            "PUT",
            "/api/lightagent/v1/settings",
            &admin,
            Some(&invalid.to_string()),
        )
        .await;
        assert!((400..500).contains(&status), "{field}={value}: {status}");
        let loaded = config_store.load().unwrap();
        assert_eq!(
            loaded.platform.jev.routing_permission,
            lightagent_core::JevRoutingPermission::Strict,
            "{field}={value}"
        );
        assert_eq!(
            loaded.platform.jev.routing_allowed_profiles,
            ["default"],
            "{field}={value}"
        );
    }

    // Revoking is saved like any other change.
    settings["jev"]["routing_allowed_profiles"] = serde_json::json!([]);
    let (status, _) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/settings",
        &admin,
        Some(&settings.to_string()),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        config_store
            .load()
            .unwrap()
            .platform
            .jev
            .routing_grant("default"),
        lightagent_core::JevRoutingGrant::Denied
    );
    std::fs::remove_dir_all(root).ok();
}

/// Records each run's packed history, and reports `window` as the window the
/// run's model is served with, as the real factory resolves it.
struct WindowedFactory {
    window: Option<u32>,
    requests: Arc<tokio::sync::Mutex<Vec<Vec<ProviderMessage>>>>,
}

#[async_trait]
impl RunFactory for WindowedFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.requests.lock().await.push(request.history.clone());
        MockFactory.run(request, sink, cancel, decisions).await
    }

    async fn history_window(
        &self,
        _profile: Option<&str>,
        _model: Option<&str>,
    ) -> Option<lightagent_core::HistoryWindow> {
        self.window.map(|tokens| lightagent_core::HistoryWindow {
            tokens,
            source: lightagent_core::HistorySource::Provider,
        })
    }
}

/// The history the API packs for a follow-up to a long first message, when
/// the factory resolves `window` and the server's configured limit is 4096.
async fn follow_up_history(window: Option<u32>) -> Vec<ProviderMessage> {
    let mut state = app_state(AuthConfig::open());
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    state.manager = RunManager::new(Arc::new(WindowedFactory {
        window,
        requests: Arc::clone(&requests),
    }));
    let store = state.sessions.clone();
    let addr = spawn_server(state).await;
    let (_, body) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/sessions",
        &[],
        Some("{}"),
    )
    .await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let parsed = lightagent_store::SessionId::parse(&id).unwrap();
    for (index, message) in ["L".repeat(2_000), "next".to_owned()].iter().enumerate() {
        let payload = serde_json::json!({ "message": message, "session_id": id }).to_string();
        let (status, body) = http(
            &addr,
            "POST",
            "/api/lightagent/v1/runs",
            &[],
            Some(&payload),
        )
        .await;
        assert_eq!(status, 202, "{body}");
        while store.load(&parsed).unwrap().runs.len() <= index {
            tokio::task::yield_now().await;
        }
    }
    let requests = requests.lock().await;
    requests[1].clone()
}

#[tokio::test]
async fn session_history_is_packed_against_the_resolved_window() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let long = "L".repeat(2_000);
        let carries = |history: &[ProviderMessage]| history.iter().any(|m| m.content == long);
        // The configured 4096 applies only when nothing was resolved.
        assert!(carries(&follow_up_history(None).await));
        // A larger served window keeps it; a smaller one is respected.
        assert!(carries(&follow_up_history(Some(131_072)).await));
        assert!(!carries(&follow_up_history(Some(200)).await));
    })
    .await
    .unwrap();
}

/// Records each run's prompt and packed history.
struct PromptFactory {
    requests: Arc<tokio::sync::Mutex<Vec<(String, Vec<ProviderMessage>)>>>,
}

#[async_trait]
impl RunFactory for PromptFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.requests
            .lock()
            .await
            .push((request.message.clone(), request.history.clone()));
        MockFactory.run(request, sink, cancel, decisions).await
    }
}

/// Wait until session `id` has `runs` saved runs and is free for another.
async fn settled(
    store: &SessionStore,
    busy: &tokio::sync::Mutex<std::collections::HashSet<lightagent_store::SessionId>>,
    id: &lightagent_store::SessionId,
    runs: usize,
) -> lightagent_store::Session {
    loop {
        let saved = store.load(id).unwrap();
        if saved.runs.len() == runs && !busy.lock().await.contains(id) {
            return saved;
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn regenerating_replaces_the_latest_answer_in_place() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut state = app_state(AuthConfig::open());
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        state.manager = RunManager::new(Arc::new(PromptFactory {
            requests: Arc::clone(&requests),
        }));
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let addr = spawn_server(state).await;
        let (_, body) = http(
            &addr,
            "POST",
            "/api/lightagent/v1/sessions",
            &[],
            Some("{}"),
        )
        .await;
        let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let parsed = lightagent_store::SessionId::parse(&id).unwrap();
        for (index, message) in ["hello", "follow up"].into_iter().enumerate() {
            let payload = serde_json::json!({ "message": message, "session_id": id }).to_string();
            let (status, body) = http(
                &addr,
                "POST",
                "/api/lightagent/v1/runs",
                &[],
                Some(&payload),
            )
            .await;
            assert_eq!(status, 202, "{body}");
            settled(&store, &busy, &parsed, index + 1).await;
        }
        let before = store.load(&parsed).unwrap();
        let replaced = before.runs[1].run_id.clone();

        let payload = serde_json::json!({ "run_id": replaced }).to_string();
        let (status, body) = http(
            &addr,
            "POST",
            &format!("/api/lightagent/v1/sessions/{id}/regenerate"),
            &[],
            Some(&payload),
        )
        .await;
        assert_eq!(status, 202, "{body}");
        let run = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        assert_eq!(run["session_id"], id);
        let created = run["id"].as_str().unwrap().to_owned();
        assert_ne!(created, replaced);

        let after = settled(&store, &busy, &parsed, 2).await;
        // The prompt is kept once; only its answer and run record change.
        let roles: Vec<&str> = after.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        assert_eq!(after.messages[2].content, "follow up");
        assert_eq!(after.messages[3].content, "The time is now.");
        assert_eq!(after.runs[0].run_id, before.runs[0].run_id);
        assert_eq!(after.runs[1].run_id, created);
        assert_eq!(after.runs[1].stop_reason.as_deref(), Some("EndTurn"));

        // The prompt is run again with the history its first run had.
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2], requests[1]);
        assert_eq!(requests[2].0, "follow up");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn regenerating_anything_but_the_latest_answer_is_refused() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = app_state(AuthConfig::open());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let addr = spawn_server(state).await;
        let (_, body) = http(
            &addr,
            "POST",
            "/api/lightagent/v1/sessions",
            &[],
            Some("{}"),
        )
        .await;
        let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let parsed = lightagent_store::SessionId::parse(&id).unwrap();
        let regenerate = |run_id: String| {
            let addr = addr.clone();
            let path = format!("/api/lightagent/v1/sessions/{id}/regenerate");
            async move {
                let payload = serde_json::json!({ "run_id": run_id }).to_string();
                http(&addr, "POST", &path, &[], Some(&payload)).await
            }
        };

        // Nothing has been answered yet.
        let (status, _) = regenerate("run-none".into()).await;
        assert_eq!(status, 409);

        for (index, message) in ["first", "second"].into_iter().enumerate() {
            let payload = serde_json::json!({ "message": message, "session_id": id }).to_string();
            let (status, body) = http(
                &addr,
                "POST",
                "/api/lightagent/v1/runs",
                &[],
                Some(&payload),
            )
            .await;
            assert_eq!(status, 202, "{body}");
            settled(&store, &busy, &parsed, index + 1).await;
        }
        let before = store.load(&parsed).unwrap();

        // An earlier answer, or a run id from a transcript that moved on.
        let (status, body) = regenerate(before.runs[0].run_id.clone()).await;
        assert_eq!(status, 409, "{body}");
        assert!(body.contains("latest answer"));
        assert_eq!(store.load(&parsed).unwrap(), before);

        // A transcript that ends with a prompt, not that run's answer.
        let mut unanswered = before.clone();
        unanswered.push_message(lightagent_store::StoredMessage::new("user", "third"));
        store.save(&unanswered).unwrap();
        let (status, _) = regenerate(before.runs[1].run_id.clone()).await;
        assert_eq!(status, 409);
        assert_eq!(store.load(&parsed).unwrap(), unanswered);

        // An unknown session is answered like any missing one.
        let missing = lightagent_store::SessionId::generate();
        let payload = serde_json::json!({ "run_id": before.runs[1].run_id }).to_string();
        let (status, _) = http(
            &addr,
            "POST",
            &format!("/api/lightagent/v1/sessions/{}/regenerate", missing.as_str()),
            &[],
            Some(&payload),
        )
        .await;
        assert_eq!(status, 404);
        // A refused regeneration leaves the session free for the next run.
        assert!(!busy.lock().await.contains(&parsed));
    })
    .await
    .unwrap();
}
