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
        let busy = Arc::clone(&state.busy_sessions);
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
            // The next prompt is admitted once the session is released.
            released(&busy, &id_parsed).await;
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
        // The next prompt is admitted once the session is released.
        released(&busy, &parsed).await;
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

/// Each run's prompt and packed history, in the order the runs started.
type Prompts = Arc<tokio::sync::Mutex<Vec<(String, Vec<ProviderMessage>)>>>;

/// Records each run's prompt and packed history.
struct PromptFactory {
    requests: Prompts,
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

        let after = settled(&store, &busy, &parsed, 2).await;
        // The prompt is kept once; only its answer and run record change.
        let roles: Vec<&str> = after.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        assert_eq!(after.messages[2].content, "follow up");
        assert_eq!(after.messages[3].content, "The time is now.");
        // A run record holds the agent loop's own run id, not the API's.
        assert_eq!(after.runs[0].run_id, before.runs[0].run_id);
        assert_ne!(after.runs[1].run_id, replaced);
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
            &format!(
                "/api/lightagent/v1/sessions/{}/regenerate",
                missing.as_str()
            ),
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

/// How a [`GateFactory`] run ends once it is released.
#[derive(Clone, Copy)]
enum GateMode {
    /// The mock answer: a `datetime.now` call, then text.
    Answer,
    /// A provider failure, after part of an answer has streamed.
    Fail,
}

/// Holds each run until [`GateFactory::release`], or until it is cancelled,
/// and counts the runs it was asked for.
#[derive(Clone)]
struct GateFactory {
    gate: Arc<tokio::sync::Semaphore>,
    mode: Arc<std::sync::Mutex<GateMode>>,
    started: Arc<std::sync::atomic::AtomicUsize>,
}

impl GateFactory {
    fn new(mode: GateMode) -> Self {
        Self {
            gate: Arc::new(tokio::sync::Semaphore::new(0)),
            mode: Arc::new(std::sync::Mutex::new(mode)),
            started: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn release(&self) {
        self.gate.add_permits(1);
    }

    fn set(&self, mode: GateMode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn started(&self) -> usize {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RunFactory for GateFactory {
    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: tokio_util::sync::CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        if matches!(mode, GateMode::Fail) {
            let _ = sink.send(lightagent_core::AgentEvent::Content {
                text: "partial words".into(),
            });
        }
        tokio::select! {
            permit = self.gate.acquire() => {
                if let Ok(permit) = permit {
                    permit.forget();
                }
            }
            () = cancel.cancelled() => return RunStatus::Cancelled,
        }
        match mode {
            GateMode::Answer => MockFactory.run(request, sink, cancel, decisions).await,
            GateMode::Fail => RunStatus::Failed,
        }
    }
}

/// A session holding one answered prompt, saved as a finished run with one
/// tool call, as a real run would have left it.
fn answered_session(store: &SessionStore) -> lightagent_store::Session {
    let mut session = lightagent_store::Session::new("default", "Answered");
    session.push_message(lightagent_store::StoredMessage::new(
        "user",
        "what time is it",
    ));
    session.push_message(lightagent_store::StoredMessage::new(
        "assistant",
        "the old answer",
    ));
    let now = std::time::SystemTime::now();
    session.push_run(lightagent_store::RunRecord {
        run_id: "run-old".into(),
        started_at: now,
        ended_at: Some(now),
        stop_reason: Some("EndTurn".into()),
        tools: vec![lightagent_store::ToolHistoryEntry {
            id: "call_old".into(),
            tool: "datetime.now".into(),
            arguments_preview: "{}".into(),
            result_excerpt: "earlier".into(),
            source: None,
            truncated: false,
            outcome: "ok".into(),
            duration_ms: None,
        }],
    });
    store.save(&session).unwrap();
    session
}

async fn regenerate(addr: &str, id: &str, run_id: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let payload = serde_json::json!({ "run_id": run_id }).to_string();
    http(
        addr,
        "POST",
        &format!("/api/lightagent/v1/sessions/{id}/regenerate"),
        headers,
        Some(&payload),
    )
    .await
}

/// Wait until session `id` is free for another run.
async fn released(
    busy: &tokio::sync::Mutex<std::collections::HashSet<lightagent_store::SessionId>>,
    id: &lightagent_store::SessionId,
) {
    while busy.lock().await.contains(id) {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_failed_or_cancelled_regeneration_keeps_the_saved_answer() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut state = app_state(AuthConfig::open());
        let factory = GateFactory::new(GateMode::Fail);
        state.manager = RunManager::new(Arc::new(factory.clone()));
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let saved = answered_session(&store);
        let id = saved.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        // A provider failure after part of a new answer streamed.
        let (status, body) = regenerate(&addr, &id, "run-old", &[]).await;
        assert_eq!(status, 202, "{body}");
        while factory.started() < 1 {
            tokio::task::yield_now().await;
        }
        // While the new run works, the saved answer is untouched on disk.
        assert_eq!(store.load(&saved.id).unwrap(), saved);
        factory.release();
        released(&busy, &saved.id).await;
        assert_eq!(store.load(&saved.id).unwrap(), saved);

        // A cancelled regeneration, likewise.
        let (status, body) = regenerate(&addr, &id, "run-old", &[]).await;
        assert_eq!(status, 202, "{body}");
        let run = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, _) = http(
            &addr,
            "POST",
            &format!("/api/lightagent/v1/runs/{run}/cancel"),
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(status, 200);
        released(&busy, &saved.id).await;
        assert_eq!(store.load(&saved.id).unwrap(), saved);

        // The kept answer can still be regenerated; a finished run replaces it.
        factory.set(GateMode::Answer);
        let (status, body) = regenerate(&addr, &id, "run-old", &[]).await;
        assert_eq!(status, 202, "{body}");
        factory.release();
        released(&busy, &saved.id).await;
        let after = store.load(&saved.id).unwrap();
        let roles: Vec<&str> = after.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant"]);
        assert_eq!(after.messages[0].content, "what time is it");
        assert_eq!(after.messages[1].content, "The time is now.");
        assert_eq!(after.runs.len(), 1);
        assert_ne!(after.runs[0].run_id, "run-old");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_regenerations_admit_one_and_a_replaced_run_id_is_stale() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut state = app_state(AuthConfig::open());
        let factory = GateFactory::new(GateMode::Answer);
        state.manager = RunManager::new(Arc::new(factory.clone()));
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let saved = answered_session(&store);
        let id = saved.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        let (first, second) = tokio::join!(
            regenerate(&addr, &id, "run-old", &[]),
            regenerate(&addr, &id, "run-old", &[]),
        );
        let mut statuses = [first.0, second.0];
        statuses.sort_unstable();
        assert_eq!(statuses, [202, 409], "{first:?} {second:?}");
        factory.release();
        released(&busy, &saved.id).await;
        assert_eq!(factory.started(), 1);

        // The prompt is kept once and the answer replaced; the tool history
        // is the new run's alone.
        let after = store.load(&saved.id).unwrap();
        let roles: Vec<&str> = after.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant"]);
        assert_eq!(after.messages[0].content, "what time is it");
        assert_eq!(after.messages[1].content, "The time is now.");
        assert_eq!(after.runs.len(), 1);
        let tools: Vec<(&str, &str, &str)> = after.runs[0]
            .tools
            .iter()
            .map(|tool| (tool.id.as_str(), tool.tool.as_str(), tool.outcome.as_str()))
            .collect();
        assert_eq!(tools, [("call_1", "datetime.now", "ok")]);

        // The replaced run's id no longer names the latest answer.
        let (status, _) = regenerate(&addr, &id, "run-old", &[]).await;
        assert_eq!(status, 409);
        assert_eq!(store.load(&saved.id).unwrap(), after);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn regenerating_needs_session_write_access_as_well_as_run_access() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for (scopes, expected) in [
            (vec![Scope::RunsWrite], 403),
            (vec![Scope::SessionsWrite], 403),
            (vec![Scope::RunsWrite, Scope::SessionsWrite], 202),
        ] {
            let label = format!("{scopes:?}");
            let state = app_state(AuthConfig::keyed("secret", scopes));
            let store = state.sessions.clone();
            let busy = Arc::clone(&state.busy_sessions);
            let saved = answered_session(&store);
            let addr = spawn_server(state).await;
            let (status, body) = regenerate(
                &addr,
                saved.id.as_str(),
                "run-old",
                &[("Authorization", "Bearer secret")],
            )
            .await;
            assert_eq!(status, expected, "{label}: {body}");
            released(&busy, &saved.id).await;
            if expected == 403 {
                assert_eq!(store.load(&saved.id).unwrap(), saved, "{label}");
            }
        }
    })
    .await
    .unwrap();
}

// --- session writes: one locked writer for every process (#100) ------------

/// A server state whose session store is in a directory the test knows, so
/// the test can hold the store's lock as another process's writer would.
fn state_with_store(factory: GateFactory) -> (AppState, std::path::PathBuf) {
    let mut state = app_state(AuthConfig::open());
    let dir = std::env::temp_dir().join(format!(
        "lightagent-api-writes-{}",
        lightagent_core::RunId::new().as_str()
    ));
    state.sessions = SessionStore::new(&dir);
    state.manager = RunManager::new(Arc::new(factory));
    (state, dir)
}

/// Hold the session store's write lock, as another process's writer would.
fn hold_store_lock(dir: &std::path::Path) -> std::fs::File {
    std::fs::create_dir_all(dir).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))
        .unwrap();
    lock.lock().unwrap();
    lock
}

/// The writer process of the session-write tests: this test binary, run
/// again with `LIGHTAGENT_TEST_RENAME_*` naming a store, a session and a
/// title. It marks that it started, then renames the session through the
/// store, waiting for the store's lock as every writer does. Run any other
/// way it does nothing.
#[test]
#[ignore = "run only as the writer process of the session-write tests"]
fn rename_writer_process() {
    let (Ok(dir), Ok(id), Ok(title)) = (
        std::env::var("LIGHTAGENT_TEST_RENAME_DIR"),
        std::env::var("LIGHTAGENT_TEST_RENAME_ID"),
        std::env::var("LIGHTAGENT_TEST_RENAME_TITLE"),
    ) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let id = lightagent_store::SessionId::parse(&id).unwrap();
    std::fs::write(dir.join("writer.started"), b"").unwrap();
    SessionStore::new(&dir)
        .update(&id, |session| {
            session.title = title;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
}

/// Start the writer process renaming session `id` to `title`, while the test
/// holds the store's lock, and return once it is waiting for that lock.
fn rename_in_another_process(dir: &std::path::Path, id: &str, title: &str) -> std::process::Child {
    let started = dir.join("writer.started");
    let _ = std::fs::remove_file(&started);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "rename_writer_process", "--ignored"])
        .env("LIGHTAGENT_TEST_RENAME_DIR", dir)
        .env("LIGHTAGENT_TEST_RENAME_ID", id)
        .env("LIGHTAGENT_TEST_RENAME_TITLE", title)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the writer process ended before writing"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the writer process never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the writer wrote while the store's lock was held"
    );
    child
}

/// Wait for the writer process to finish its write.
async fn finish(writer: std::process::Child) {
    let status = tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        writer.wait().unwrap()
    })
    .await
    .unwrap();
    assert!(status.success(), "the writer process failed: {status}");
}

fn run_id(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Wait until run `run` reports `status`.
async fn ended(addr: &str, run: &str, status: &str) {
    loop {
        let (_, body) = http(
            addr,
            "GET",
            &format!("/api/lightagent/v1/runs/{run}"),
            &[],
            None,
        )
        .await;
        let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if value["status"] == status {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn start_session_run(addr: &str, id: &str, message: &str) -> (u16, String) {
    let payload = serde_json::json!({ "message": message, "session_id": id }).to_string();
    http(addr, "POST", "/api/lightagent/v1/runs", &[], Some(&payload)).await
}

fn transcript(session: &lightagent_store::Session) -> Vec<(String, String)> {
    session
        .messages
        .iter()
        .map(|message| (message.role.clone(), message.content.clone()))
        .collect()
}

fn pair(role: &str, content: &str) -> (String, String) {
    (role.to_owned(), content.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_run_and_a_rename_from_another_process_are_both_kept() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let factory = GateFactory::new(GateMode::Answer);
        let (state, dir) = state_with_store(factory.clone());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let session = lightagent_store::Session::new("default", "Draft");
        store.save(&session).unwrap();
        let id = session.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        let (status, body) = start_session_run(&addr, &id, "what time is it").await;
        assert_eq!(status, 202, "{body}");
        let run = run_id(&body);
        while factory.started() < 1 {
            tokio::task::yield_now().await;
        }
        // The run finishes while another process holds the store's lock and
        // waits to rename the session: the recorder waits too.
        let lock = hold_store_lock(&dir);
        let writer = rename_in_another_process(&dir, &id, "Renamed elsewhere");
        factory.release();
        ended(&addr, &run, "completed").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            transcript(&store.load(&session.id).unwrap()),
            [pair("user", "what time is it")],
            "the run was recorded while another process held the lock"
        );
        drop(lock);
        finish(writer).await;
        released(&busy, &session.id).await;

        let saved = store.load(&session.id).unwrap();
        assert_eq!(saved.title, "Renamed elsewhere");
        assert_eq!(
            transcript(&saved),
            [
                pair("user", "what time is it"),
                pair("assistant", "The time is now.")
            ]
        );
        assert_eq!(saved.runs.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_regenerated_answer_and_a_rename_from_another_process_are_both_kept() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let factory = GateFactory::new(GateMode::Answer);
        let (state, dir) = state_with_store(factory.clone());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let saved = answered_session(&store);
        let id = saved.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        let (status, body) = regenerate(&addr, &id, "run-old", &[]).await;
        assert_eq!(status, 202, "{body}");
        let run = run_id(&body);
        while factory.started() < 1 {
            tokio::task::yield_now().await;
        }
        let lock = hold_store_lock(&dir);
        let writer = rename_in_another_process(&dir, &id, "Renamed elsewhere");
        factory.release();
        ended(&addr, &run, "completed").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            store.load(&saved.id).unwrap(),
            saved,
            "the answer was replaced while another process held the lock"
        );
        drop(lock);
        finish(writer).await;
        released(&busy, &saved.id).await;

        let after = store.load(&saved.id).unwrap();
        assert_eq!(after.title, "Renamed elsewhere");
        assert_eq!(
            transcript(&after),
            [
                pair("user", "what time is it"),
                pair("assistant", "The time is now.")
            ]
        );
        assert_eq!(after.runs.len(), 1);
        assert_ne!(after.runs[0].run_id, "run-old");
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_or_cancelled_regeneration_keeps_the_answer_and_another_processs_rename() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let factory = GateFactory::new(GateMode::Fail);
        let (state, dir) = state_with_store(factory.clone());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let saved = answered_session(&store);
        let id = saved.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        for (round, ending) in [(1, "failed"), (2, "cancelled")] {
            let (status, body) = regenerate(&addr, &id, "run-old", &[]).await;
            assert_eq!(status, 202, "{body}");
            let run = run_id(&body);
            while factory.started() < round {
                tokio::task::yield_now().await;
            }
            let lock = hold_store_lock(&dir);
            let title = format!("Renamed while the run {ending}");
            let writer = rename_in_another_process(&dir, &id, &title);
            if ending == "failed" {
                // A provider failure after part of a new answer streamed.
                factory.release();
            } else {
                let (status, _) = http(
                    &addr,
                    "POST",
                    &format!("/api/lightagent/v1/runs/{run}/cancel"),
                    &[],
                    Some("{}"),
                )
                .await;
                assert_eq!(status, 200);
            }
            ended(&addr, &run, ending).await;
            drop(lock);
            finish(writer).await;
            released(&busy, &saved.id).await;
            let after = store.load(&saved.id).unwrap();
            assert_eq!(after.title, title);
            assert_eq!(after.messages, saved.messages, "{ending}");
            assert_eq!(after.runs, saved.runs, "{ending}");
        }
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_whose_record_cannot_be_written_leaves_the_saved_session_whole() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let factory = GateFactory::new(GateMode::Answer);
        let (state, dir) = state_with_store(factory.clone());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let session = lightagent_store::Session::new("default", "Draft");
        store.save(&session).unwrap();
        let id = session.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        let (status, body) = start_session_run(&addr, &id, "what time is it").await;
        assert_eq!(status, 202, "{body}");
        while factory.started() < 1 {
            tokio::task::yield_now().await;
        }
        // The atomic write's temporary file cannot be created.
        let path = dir.join(format!("{id}.json"));
        let temporary = dir.join(format!("{id}.json.tmp"));
        std::fs::create_dir(&temporary).unwrap();
        let before = std::fs::read(&path).unwrap();
        factory.release();
        released(&busy, &session.id).await;
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            transcript(&store.load(&session.id).unwrap()),
            [pair("user", "what time is it")]
        );

        // Once writes work again, the next run is admitted and recorded.
        std::fs::remove_dir(&temporary).unwrap();
        let (status, body) = start_session_run(&addr, &id, "and now?").await;
        assert_eq!(status, 202, "{body}");
        factory.release();
        released(&busy, &session.id).await;
        assert_eq!(
            transcript(&store.load(&session.id).unwrap()),
            [
                pair("user", "what time is it"),
                pair("user", "and now?"),
                pair("assistant", "The time is now.")
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_prompts_to_one_session_admit_one_and_save_its_prompt_once() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let factory = GateFactory::new(GateMode::Answer);
        let (state, dir) = state_with_store(factory.clone());
        let store = state.sessions.clone();
        let busy = Arc::clone(&state.busy_sessions);
        let session = lightagent_store::Session::new("default", "Draft");
        store.save(&session).unwrap();
        let id = session.id.as_str().to_owned();
        let addr = spawn_server(state).await;

        let (first, second) = tokio::join!(
            start_session_run(&addr, &id, "first"),
            start_session_run(&addr, &id, "second"),
        );
        let mut statuses = [first.0, second.0];
        statuses.sort_unstable();
        assert_eq!(statuses, [202, 409], "{first:?} {second:?}");
        // A rename in this server is still refused while the run is active.
        let (status, _) = http(
            &addr,
            "PATCH",
            &format!("/api/lightagent/v1/sessions/{id}"),
            &[],
            Some(r#"{"title":"Renamed"}"#),
        )
        .await;
        assert_eq!(status, 409);
        factory.release();
        released(&busy, &session.id).await;
        assert_eq!(factory.started(), 1);

        let saved = store.load(&session.id).unwrap();
        assert_eq!(saved.title, "Draft");
        let roles: Vec<&str> = saved.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant"]);
        assert!(["first", "second"].contains(&saved.messages[0].content.as_str()));
        assert_eq!(saved.runs.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    })
    .await
    .unwrap();
}
