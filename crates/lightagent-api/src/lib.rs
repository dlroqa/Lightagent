//! The Lightagent HTTP API.
//!
//! A versioned surface (`/api/lightagent/v1`) over the one runtime: it starts and
//! observes agent runs, streams their canonical events as SSE, lists tools, and
//! reads and deletes saved sessions — all behind scoped bearer authentication.
//! It owns no agent logic; a [`RunManager`] drives the core loop and the handlers
//! only observe it.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod auth;
pub mod manager;
pub mod sse;

use std::collections::HashSet;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use lightagent_core::{
    AgentEvent, ApprovalPolicy, ConfigStore, PlatformEndpointConfig, ProfileId, ProfileStore,
    SkillStore, skill_dirs,
};
use lightagent_store::{Session, SessionId, SessionStore, StoredMessage, model_history};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

pub use auth::{AuthConfig, Scope};
pub use manager::{RunFactory, RunManager, RunState, RunStatus, StartRun};

/// The shared state every handler reads.
#[derive(Clone)]
pub struct AppState {
    /// Drives and tracks runs.
    pub manager: RunManager,
    /// The authentication policy.
    pub auth: AuthConfig,
    /// The session store for the active profile.
    pub sessions: SessionStore,
    /// The profile whose sessions are served by `sessions`.
    pub session_profile: String,
    /// Effective model context when configured; used to bound saved history.
    pub context_limit: usize,
    /// The CLI's own config store. Present in the real server and optional in
    /// embedders/tests that only expose run APIs.
    pub config_store: Option<ConfigStore>,
    /// Prevent overlapping runs from overwriting one session transcript.
    pub busy_sessions: Arc<Mutex<HashSet<SessionId>>>,
    /// When set, the panel is served from this directory (same-origin), so the
    /// WebUI and the API it calls share one origin and need no CORS.
    pub web_root: Option<PathBuf>,
}

/// Build the API router over `state`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/lightagent/v1/tools", get(list_tools))
        .route(
            "/api/lightagent/v1/provider",
            get(get_provider_capabilities),
        )
        .route("/api/lightagent/v1/skills", get(list_skills))
        .route("/api/lightagent/v1/profiles", get(list_profiles))
        .route(
            "/api/lightagent/v1/settings",
            get(get_settings).put(save_settings),
        )
        .route("/api/lightagent/v1/runs", post(create_run))
        .route("/api/lightagent/v1/runs/{id}", get(get_run))
        .route("/api/lightagent/v1/runs/{id}/events", get(run_events))
        .route("/api/lightagent/v1/runs/{id}/cancel", post(cancel_run))
        .route(
            "/api/lightagent/v1/sessions",
            get(list_sessions).post(create_session),
        )
        .route("/api/lightagent/v1/sessions/search", get(search_sessions))
        .route(
            "/api/lightagent/v1/sessions/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route(
            "/api/lightagent/v1/sessions/{id}/attachments",
            post(upload_attachment).layer(DefaultBodyLimit::max(20 * 1024 * 1024)),
        )
        .route("/api/lightagent/v1/approvals", get(list_approvals))
        .route("/api/lightagent/v1/approvals/{run}", post(respond_approval))
        .fallback(serve_static)
        .with_state(Arc::new(state))
}

/// Serve the WebUI from `web_root`, if configured.
///
/// Every API route is matched before this fallback. Path resolution is a
/// whitelist — each component must be an ordinary name, so `..` is refused
/// rather than resolved — and a path with no file extension that does not exist
/// is answered with `index.html`, so a client-side route deep-links, while a
/// missing asset is a 404 rather than the document.
async fn serve_static(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    if uri.path() == "/api/lightagent" || uri.path().starts_with("/api/lightagent/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown Lightagent API endpoint" })),
        )
            .into_response();
    }
    let Some(root) = &state.web_root else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let relative = uri.path().trim_start_matches('/');
    let mut full = root.clone();
    let mut looks_like_asset = false;
    if relative.is_empty() {
        full.push("index.html");
    } else {
        for component in relative.split('/') {
            let ordinary = !component.is_empty()
                && component != "."
                && component != ".."
                && component
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
            if !ordinary {
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
            full.push(component);
        }
        looks_like_asset = std::path::Path::new(relative).extension().is_some();
    }

    match tokio::fs::read(&full).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type(&full))], bytes).into_response(),
        Err(_) if looks_like_asset => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(_) => match tokio::fs::read(root.join("index.html")).await {
            Ok(bytes) => {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        },
    }
}

fn content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Reject a request that failed authorization, else `None`.
fn deny(state: &AppState, headers: &HeaderMap, scope: Scope) -> Option<Response> {
    match state.auth.authorize(headers, scope) {
        Ok(()) => None,
        Err((status, message)) => Some((status, Json(json!({ "error": message }))).into_response()),
    }
}

async fn health() -> Response {
    Json(json!({ "status": "ok", "service": "lightagent" })).into_response()
}

async fn list_tools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::ToolsRead) {
        return rejection;
    }
    let definitions = match state.manager.tools().await {
        Ok(tools) => tools,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": error })),
            )
                .into_response();
        }
    };
    let tools: Vec<_> = definitions
        .into_iter()
        .map(|definition| {
            json!({
                "name": definition.name,
                "risk": definition.risk.as_str(),
                "description": definition.description,
            })
        })
        .collect();
    Json(json!({ "tools": tools })).into_response()
}

/// Public metadata for skills installed for the active profile. Bodies stay in
/// the harness and are read by the approval-gated `skill.read` tool only.
async fn list_skills(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::ToolsRead) {
        return rejection;
    }
    let Some(config_store) = &state.config_store else {
        return internal("Lightagent skills are unavailable in this embedding");
    };
    let Some(root) = config_store.path().parent() else {
        return internal("Lightagent config has no parent directory");
    };
    let profile_id = match ProfileId::new(&state.session_profile) {
        Ok(id) => id,
        Err(error) => return internal(&error.to_string()),
    };
    let profiles = ProfileStore::new(root);
    let skills = SkillStore::load(&skill_dirs(root, profiles.handle(&profile_id).dir()));
    let skills: Vec<_> = skills
        .names()
        .into_iter()
        .filter_map(|name| {
            skills.get(&name).map(|skill| {
                json!({
                    "name": skill.name, "description": skill.description,
                })
            })
        })
        .collect();
    Json(json!({ "skills": skills })).into_response()
}
async fn get_provider_capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::ToolsRead) {
        return rejection;
    }
    match state.manager.provider_capabilities().await {
        Ok(capabilities) => Json(capabilities).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))).into_response(),
    }
}

/// A concise, non-sensitive view of a profile suitable for a model/profile picker.
#[derive(Serialize)]
struct ProfileSummary {
    id: String,
    name: String,
    model: String,
    active: bool,
}

async fn list_profiles(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsRead) {
        return rejection;
    }
    let Some(config_store) = &state.config_store else {
        return internal("Lightagent profiles are unavailable in this embedding");
    };
    let Some(root) = config_store.path().parent() else {
        return internal("Lightagent config has no parent directory");
    };
    let profiles = ProfileStore::new(root);
    let ids = match profiles.list() {
        Ok(ids) => ids,
        Err(error) => return internal(&error.to_string()),
    };
    let mut summaries = Vec::with_capacity(ids.len());
    for id in ids {
        let profile = match profiles.load(&id) {
            Ok(profile) => profile,
            Err(error) => return internal(&error.to_string()),
        };
        summaries.push(ProfileSummary {
            active: profile.id.as_str() == state.session_profile,
            id: profile.id.as_str().to_owned(),
            name: profile.name,
            model: profile.routing.model,
        });
    }
    Json(json!({ "active_profile": state.session_profile, "profiles": summaries })).into_response()
}

#[derive(Clone, Deserialize, Serialize)]
struct UiPlatformEndpoint {
    enabled: bool,
    base_url: Option<String>,
    /// Presence only: secret references and values never cross the API boundary.
    api_key_configured: bool,
}

/// Non-sensitive settings for TypeSafe Jev's routing adapter.
#[derive(Clone, Deserialize, Serialize)]
struct UiJevSettings {
    #[serde(flatten)]
    endpoint: UiPlatformEndpoint,
    model: String,
    confidence_threshold: f32,
    #[serde(default)]
    allowed_models: Option<Vec<String>>,
    #[serde(default)]
    allowed_profiles: Option<Vec<String>>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// Non-sensitive settings for Qdrant's retrieval adapter.
#[derive(Clone, Deserialize, Serialize)]
struct UiQdrantSettings {
    #[serde(flatten)]
    endpoint: UiPlatformEndpoint,
    collection: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// Non-sensitive settings for Infinity's embedding and reranking adapter.
#[derive(Clone, Deserialize, Serialize)]
struct UiInfinitySettings {
    #[serde(flatten)]
    endpoint: UiPlatformEndpoint,
    embedding_model: String,
    rerank_model: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize)]
struct UiOpenTerminalSettings {
    #[serde(flatten)]
    endpoint: UiPlatformEndpoint,
    #[serde(default)]
    request_timeout_secs: Option<u64>,
    #[serde(default)]
    execution_timeout_secs: Option<u64>,
    #[serde(default)]
    poll_interval_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Clone, Deserialize, Serialize)]
struct UiSettings {
    max_turns: u32,
    max_tool_calls: u32,
    wall_clock_secs: Option<u64>,
    approval_policy: ApprovalPolicy,
    web_enabled: bool,
    filesystem_tools_enabled: bool,
    terminal_enabled: bool,
    memory_enabled: bool,
    show_reasoning_in_tui: bool,
    #[serde(default)]
    subagents_enabled: Option<bool>,
    #[serde(default)]
    delegate_timeout_secs: Option<u64>,
    jev: UiJevSettings,
    qdrant: UiQdrantSettings,
    infinity: UiInfinitySettings,
    open_terminal: UiOpenTerminalSettings,
}

fn ui_settings(config: &lightagent_core::Config) -> UiSettings {
    UiSettings {
        max_turns: config.agent.max_turns,
        max_tool_calls: config.agent.max_tool_calls,
        wall_clock_secs: config.agent.wall_clock_secs,
        approval_policy: config.security.approval_policy,
        web_enabled: config.web.enabled,
        filesystem_tools_enabled: config.tools.enabled,
        terminal_enabled: config.tools.allow_terminal,
        memory_enabled: config.memory.auto_capture,
        show_reasoning_in_tui: config.tui.show_reasoning,
        subagents_enabled: Some(config.subagents.enabled),
        delegate_timeout_secs: Some(config.subagents.delegate_timeout_secs),
        jev: UiJevSettings {
            endpoint: ui_platform_endpoint(&config.platform.jev.endpoint),
            model: config.platform.jev.model.clone(),
            confidence_threshold: config.platform.jev.confidence_threshold,
            allowed_models: Some(config.platform.jev.allowed_models.clone()),
            allowed_profiles: Some(config.platform.jev.allowed_profiles.clone()),
            timeout_secs: Some(config.platform.jev.timeout_secs),
        },
        qdrant: UiQdrantSettings {
            endpoint: ui_platform_endpoint(&config.platform.qdrant.endpoint),
            collection: config.platform.qdrant.collection.clone(),
            timeout_secs: Some(config.platform.qdrant.timeout_secs),
        },
        infinity: UiInfinitySettings {
            endpoint: ui_platform_endpoint(&config.platform.infinity.endpoint),
            embedding_model: config.platform.infinity.embedding_model.clone(),
            rerank_model: config.platform.infinity.rerank_model.clone(),
            timeout_secs: Some(config.platform.infinity.timeout_secs),
        },
        open_terminal: UiOpenTerminalSettings {
            endpoint: ui_platform_endpoint(&config.platform.open_terminal.endpoint),
            request_timeout_secs: Some(config.platform.open_terminal.request_timeout_secs),
            execution_timeout_secs: Some(config.platform.open_terminal.execution_timeout_secs),
            poll_interval_ms: Some(config.platform.open_terminal.poll_interval_ms),
            max_output_bytes: Some(config.platform.open_terminal.max_output_bytes),
        },
    }
}

fn ui_platform_endpoint(endpoint: &PlatformEndpointConfig) -> UiPlatformEndpoint {
    UiPlatformEndpoint {
        enabled: endpoint.enabled,
        base_url: endpoint.base_url.clone(),
        api_key_configured: endpoint.api_key.is_some(),
    }
}

fn apply_platform_endpoint(endpoint: &mut PlatformEndpointConfig, settings: &UiPlatformEndpoint) {
    endpoint.enabled = settings.enabled;
    endpoint.base_url = settings
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
}

fn active_profile(
    store: &ConfigStore,
    name: &str,
) -> Result<Option<(ProfileStore, lightagent_core::AgentProfile)>, String> {
    let root = store
        .path()
        .parent()
        .ok_or("Lightagent config has no parent directory")?;
    let profiles = ProfileStore::new(root);
    let id = ProfileId::new(name).map_err(|error| error.to_string())?;
    match profiles.load(&id) {
        Ok(profile) => Ok(Some((profiles, profile))),
        Err(lightagent_core::ProfileError::NotFound { .. }) if name == "default" => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

async fn get_settings(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let config = match store.load() {
        Ok(config) => config,
        Err(error) => return internal(&error.to_string()),
    };
    let mut settings = ui_settings(&config);
    match active_profile(store, &state.session_profile) {
        Ok(Some((_, profile))) => {
            settings.approval_policy = profile.approval_policy;
            let limits = config.agent.apply_to(profile.limits);
            settings.max_turns = limits.max_turns;
            settings.max_tool_calls = limits.max_tool_calls;
            settings.wall_clock_secs = limits.wall_clock_secs;
        }
        Ok(None) => {}
        Err(error) => return internal(&error),
    }
    Json(settings).into_response()
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(settings): Json<UiSettings>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let mut config = match store.load() {
        Ok(config) => config,
        Err(error) => return internal(&error.to_string()),
    };
    config.agent.max_turns = settings.max_turns;
    config.agent.max_tool_calls = settings.max_tool_calls;
    config.agent.wall_clock_secs = settings.wall_clock_secs;
    let profile = match active_profile(store, &state.session_profile) {
        Ok(profile) => profile,
        Err(error) => return internal(&error),
    };
    config.security.approval_policy = settings.approval_policy;
    config.web.enabled = settings.web_enabled;
    config.tools.enabled = settings.filesystem_tools_enabled;
    config.tools.allow_terminal = settings.terminal_enabled;
    config.memory.auto_capture = settings.memory_enabled;
    config.tui.show_reasoning = settings.show_reasoning_in_tui;
    if let Some(enabled) = settings.subagents_enabled {
        config.subagents.enabled = enabled;
    }
    if let Some(timeout) = settings.delegate_timeout_secs {
        config.subagents.delegate_timeout_secs = timeout;
    }
    apply_platform_endpoint(&mut config.platform.jev.endpoint, &settings.jev.endpoint);
    config.platform.jev.model = settings.jev.model.trim().to_owned();
    config.platform.jev.confidence_threshold = settings.jev.confidence_threshold;
    if let Some(value) = settings.jev.allowed_models {
        config.platform.jev.allowed_models = value;
    }
    if let Some(value) = settings.jev.allowed_profiles {
        config.platform.jev.allowed_profiles = value;
    }
    if let Some(value) = settings.jev.timeout_secs {
        config.platform.jev.timeout_secs = value;
    }
    apply_platform_endpoint(
        &mut config.platform.qdrant.endpoint,
        &settings.qdrant.endpoint,
    );
    config.platform.qdrant.collection = settings.qdrant.collection.trim().to_owned();
    if let Some(value) = settings.qdrant.timeout_secs {
        config.platform.qdrant.timeout_secs = value;
    }
    apply_platform_endpoint(
        &mut config.platform.infinity.endpoint,
        &settings.infinity.endpoint,
    );
    config.platform.infinity.embedding_model = settings.infinity.embedding_model.trim().to_owned();
    config.platform.infinity.rerank_model = settings.infinity.rerank_model.trim().to_owned();
    if let Some(value) = settings.infinity.timeout_secs {
        config.platform.infinity.timeout_secs = value;
    }
    apply_platform_endpoint(
        &mut config.platform.open_terminal.endpoint,
        &settings.open_terminal.endpoint,
    );
    if let Some(value) = settings.open_terminal.request_timeout_secs {
        config.platform.open_terminal.request_timeout_secs = value;
    }
    if let Some(value) = settings.open_terminal.execution_timeout_secs {
        config.platform.open_terminal.execution_timeout_secs = value;
    }
    if let Some(value) = settings.open_terminal.poll_interval_ms {
        config.platform.open_terminal.poll_interval_ms = value;
    }
    if let Some(value) = settings.open_terminal.max_output_bytes {
        config.platform.open_terminal.max_output_bytes = value;
    }
    if let Err(error) = config.validate() {
        return bad_request(&error.to_string());
    }
    if let Some((profiles, mut profile)) = profile {
        profile.approval_policy = settings.approval_policy;
        profile.limits.max_turns = settings.max_turns;
        profile.limits.max_tool_calls = settings.max_tool_calls;
        profile.limits.wall_clock_secs = settings.wall_clock_secs;
        if let Err(error) = profiles.save(&profile) {
            return internal(&error.to_string());
        }
    }
    match store.save(&config) {
        Ok(()) => Json(ui_settings(&config)).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

#[derive(Deserialize)]
struct CreateRunBody {
    message: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateRunBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::RunsWrite) {
        return rejection;
    }
    let mut history = Vec::new();
    let mut profile = body.profile;
    let mut session_id = None;
    if let Some(raw) = body.session_id {
        let id = match SessionId::parse(&raw) {
            Ok(id) => id,
            Err(error) => return bad_request(&error.to_string()),
        };
        let mut busy = state.busy_sessions.lock().await;
        if !busy.insert(id.clone()) {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "session already has an active run"})),
            )
                .into_response();
        }
        let mut session = match state.sessions.load(&id) {
            Ok(session) => session,
            Err(error) => {
                busy.remove(&id);
                return not_found(&error.to_string());
            }
        };
        if session.profile != state.session_profile
            || profile
                .as_ref()
                .is_some_and(|value| value != &session.profile)
        {
            busy.remove(&id);
            return bad_request("session profile does not match");
        }
        history = model_history(&session, &body.message, state.context_limit);
        profile = Some(session.profile.clone());
        if session.messages.is_empty() && session.title == "agent session" {
            session.title = session_title(&body.message);
        }
        session.push_message(StoredMessage::new("user", &body.message));
        if let Err(error) = state.sessions.save(&session) {
            busy.remove(&id);
            return internal(&error.to_string());
        }
        session_id = Some(id);
    }
    let run = state
        .manager
        .start(StartRun {
            message: body.message,
            history,
            profile,
            cwd: None,
            model: body.model,
        })
        .await;
    if let Some(id) = &session_id {
        let state = Arc::clone(&state);
        let run = Arc::clone(&run);
        let id = id.clone();
        tokio::spawn(async move {
            let mut seen = 0;
            loop {
                let (events, status) = run.wait_from(seen).await;
                seen += events.len();
                if status.is_terminal() {
                    match state.sessions.load(&id) {
                        Ok(mut session) => {
                            let events = run.events().await;
                            session.record_run_events(&events, &format!("{status:?}"));
                            if let Err(error) = state.sessions.save(&session) {
                                eprintln!("could not save agent session {id:?}: {error}");
                            }
                        }
                        Err(error) => eprintln!("could not read agent session {id:?}: {error}"),
                    }
                    state.busy_sessions.lock().await.remove(&id);
                    break;
                }
            }
        });
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": run.id(), "status": run.status().await, "session_id": session_id })),
    )
        .into_response()
}

fn session_title(message: &str) -> String {
    const LIMIT: usize = 48;
    const MAX_WORDS: usize = 7;
    const LEADING_PHRASES: &[&str] = &[
        "could you please ",
        "can you please ",
        "would you please ",
        "please can you ",
        "please could you ",
        "can you ",
        "could you ",
        "would you ",
        "what is ",
        "what are ",
        "what's ",
        "tell me about ",
        "tell me ",
        "help me ",
        "i need help ",
        "i want to ",
        "how do i ",
        "how can i ",
        "please ",
    ];

    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut subject = compact.trim();
    loop {
        let lower = subject.to_lowercase();
        let Some(prefix) = LEADING_PHRASES
            .iter()
            .find(|prefix| lower.starts_with(**prefix))
        else {
            break;
        };
        subject = subject[prefix.len()..].trim_start();
    }
    subject = subject.trim_start_matches(|character: char| matches!(character, '"' | '\''));
    for article in ["the ", "a ", "an "] {
        if subject.to_lowercase().starts_with(article) {
            subject = subject[article.len()..].trim_start();
            break;
        }
    }
    let words = subject
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>();
    let joined = words.join(" ");
    let candidate = joined
        .trim_end_matches(|character: char| matches!(character, '.' | '?' | '!' | ':' | ';' | ','));
    let mut title = candidate.chars().take(LIMIT).collect::<String>();
    title = title
        .trim_end_matches(|character: char| matches!(character, '.' | '?' | '!' | ':' | ';' | ','))
        .to_owned();
    if title.is_empty() {
        return "agent session".to_owned();
    }
    title
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::RunsRead) {
        return rejection;
    }
    match state.manager.get(&id).await {
        Some(run) => Json(json!({
            "id": run.id(),
            "status": run.status().await,
            "events": run.events().await.len(),
            "pending_approval": run.pending().await,
        }))
        .into_response(),
        None => not_found(&id),
    }
}

async fn run_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::RunsRead) {
        return rejection;
    }
    match state.manager.get(&id).await {
        Some(run) => Sse::new(event_stream(run))
            .keep_alive(KeepAlive::default())
            .into_response(),
        None => not_found(&id),
    }
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::RunsWrite) {
        return rejection;
    }
    match state.manager.get(&id).await {
        Some(run) => {
            run.cancel();
            Json(json!({ "id": run.id(), "cancelled": true })).into_response()
        }
        None => not_found(&id),
    }
}

async fn list_sessions(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsRead) {
        return rejection;
    }
    match state.sessions.list() {
        Ok(list) => Json(json!({ "sessions": list })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

#[derive(Deserialize)]
struct SearchSessionsQuery {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn search_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<SearchSessionsQuery>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsRead) {
        return rejection;
    }
    let term = query.q.trim();
    if term.is_empty() {
        return Json(json!({ "sessions": [] })).into_response();
    }
    if term.chars().count() > 200 {
        return bad_request("search query must be 200 characters or fewer");
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    match state.sessions.search(term, limit) {
        Ok(list) => Json(json!({ "sessions": list })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn create_session(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsWrite) {
        return rejection;
    }
    if !state.sessions.is_history_kept() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session history is disabled" })),
        )
            .into_response();
    }
    let session = Session::new(&state.session_profile, "agent session");
    match state.sessions.save(&session) {
        Ok(()) => (StatusCode::CREATED, Json(json!({ "id": session.id }))).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

/// Save one browser-selected attachment for a session. The filename is accepted
/// only as a simple basename, preventing a client from choosing its destination.
async fn upload_attachment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsWrite) {
        return rejection;
    }
    const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
    if body.len() > MAX_ATTACHMENT_BYTES {
        return bad_request("attachments must be 20 MB or smaller");
    }
    let id = match SessionId::parse(&id) {
        Ok(id) => id,
        Err(error) => return bad_request(&error.to_string()),
    };
    let filename = headers
        .get("x-lightagent-filename")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 180
                && name.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ' ')
                })
        });
    let Some(filename) = filename else {
        return bad_request("attachment filename is invalid");
    };
    let session = match state.sessions.load(&id) {
        Ok(session) => session,
        Err(error) => return not_found(&error.to_string()),
    };
    if session.profile != state.session_profile {
        return bad_request("session profile does not match");
    }
    match state.sessions.save_attachment(&id, filename, &body) {
        Ok(path) => Json(json!({ "name": filename, "path": path })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsRead) {
        return rejection;
    }
    let id = match SessionId::parse(&id) {
        Ok(id) => id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    match state.sessions.load(&id) {
        Ok(session) => Json(session).into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

/// Fields that can be changed from the session sidebar. `project: null` clears
/// an existing project; omitted fields are left unchanged.
#[derive(Deserialize)]
struct UpdateSessionBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    pinned: Option<bool>,
    #[serde(default)]
    archived: Option<bool>,
    #[serde(default)]
    project: Patch<Option<String>>,
}

/// Distinguishes an omitted PATCH field from an explicitly supplied JSON null.
enum Patch<T> {
    Missing,
    Value(T),
}

impl<T> Default for Patch<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Value)
    }
}

const SESSION_TITLE_LIMIT: usize = 200;
const SESSION_PROJECT_LIMIT: usize = 120;

fn normalized_metadata(value: String, field: &str, limit: usize) -> Result<String, Response> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(bad_request(&format!("session {field} cannot be empty")));
    }
    if value.chars().count() > limit {
        return Err(bad_request(&format!(
            "session {field} must be at most {limit} characters"
        )));
    }
    Ok(value)
}

async fn update_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateSessionBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsWrite) {
        return rejection;
    }
    let id = match SessionId::parse(&id) {
        Ok(id) => id,
        Err(error) => return bad_request(&error.to_string()),
    };
    let title = match body.title {
        Some(value) => match normalized_metadata(value, "title", SESSION_TITLE_LIMIT) {
            Ok(value) => Some(value),
            Err(response) => return response,
        },
        None => None,
    };
    let project = match body.project {
        Patch::Value(Some(value)) => {
            match normalized_metadata(value, "project", SESSION_PROJECT_LIMIT) {
                Ok(value) => Some(Some(value)),
                Err(response) => return response,
            }
        }
        Patch::Value(None) => Some(None),
        Patch::Missing => None,
    };

    let busy = state.busy_sessions.lock().await;
    if busy.contains(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session has an active run" })),
        )
            .into_response();
    }
    let mut session = match state.sessions.load(&id) {
        Ok(session) => session,
        Err(error) => return not_found(&error.to_string()),
    };
    if let Some(value) = title {
        session.title = value;
    }
    if let Some(value) = body.pinned {
        session.pinned = value;
    }
    if let Some(value) = body.archived {
        session.archived = value;
    }
    if let Some(value) = project {
        session.project = value;
    }
    session.touch();
    let summary = lightagent_store::SessionSummary::of(&session);
    let result = state.sessions.save(&session);
    drop(busy);
    match result {
        Ok(()) => Json(summary).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn delete_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::SessionsWrite) {
        return rejection;
    }
    let id = match SessionId::parse(&id) {
        Ok(id) => id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let busy = state.busy_sessions.lock().await;
    if busy.contains(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session has an active run" })),
        )
            .into_response();
    }
    let result = state.sessions.delete(&id);
    drop(busy);
    match result {
        Ok(removed) => Json(json!({ "deleted": removed })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn list_approvals(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::ApprovalsWrite) {
        return rejection;
    }
    let mut waiting = Vec::new();
    for run in state.manager.awaiting_approval().await {
        waiting.push(json!({ "run": run.id(), "pending": run.pending().await }));
    }
    Json(json!({ "approvals": waiting })).into_response()
}

#[derive(Deserialize)]
struct RespondBody {
    #[serde(default)]
    approve: bool,
    #[serde(default)]
    remember_secs: Option<u64>,
}

async fn respond_approval(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    Json(body): Json<RespondBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::ApprovalsWrite) {
        return rejection;
    }
    use lightagent_core::ApprovalDecision;
    let Some(state_run) = state.manager.get(&run).await else {
        return not_found(&run);
    };
    let Some(pending) = state_run.pending().await else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "run is not awaiting approval" })),
        )
            .into_response();
    };
    let decision = if body.approve {
        match body.remember_secs {
            Some(secs) => ApprovalDecision::grant_for(
                pending.approval_id,
                std::time::Duration::from_secs(secs),
            ),
            None => ApprovalDecision::grant(pending.approval_id),
        }
    } else {
        ApprovalDecision::deny(pending.approval_id)
    };
    let delivered = state_run.decide(decision);
    Json(json!({ "run": state_run.id(), "delivered": delivered })).into_response()
}

fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("no run '{id}'") })),
    )
        .into_response()
}

fn internal(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message })),
    )
        .into_response()
}

/// A run's events as an SSE stream: buffered history first, then the live tail,
/// ending when the run reaches a terminal state.
fn event_stream(run: Arc<RunState>) -> impl Stream<Item = Result<Event, Infallible>> {
    struct Cursor {
        run: Arc<RunState>,
        seen: usize,
        queue: VecDeque<AgentEvent>,
        finished: bool,
    }
    stream::unfold(
        Cursor {
            run,
            seen: 0,
            queue: VecDeque::new(),
            finished: false,
        },
        |mut cursor| async move {
            loop {
                if let Some(event) = cursor.queue.pop_front() {
                    return Some((Ok(sse::to_sse(&event)), cursor));
                }
                if cursor.finished {
                    return None;
                }
                let (new_events, status) = cursor.run.wait_from(cursor.seen).await;
                cursor.seen += new_events.len();
                cursor.queue.extend(new_events);
                if status.is_terminal() {
                    cursor.finished = true;
                }
                if cursor.queue.is_empty() && cursor.finished {
                    return None;
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::SecretRef;

    #[test]
    fn session_titles_extract_a_short_prompt_subject() {
        assert_eq!(
            session_title(
                "What is the current news about a possible trucking strike in California?"
            ),
            "current news about a possible trucking strike"
        );
        assert_eq!(
            session_title("Can you explain how OAuth token refresh works in browser applications?"),
            "explain how OAuth token refresh works in"
        );
        assert_eq!(session_title("hello"), "hello");
    }

    #[test]
    fn typed_platform_settings_are_exposed_without_secret_references() {
        let mut config = lightagent_core::Config::default();
        config.platform.jev.endpoint.enabled = true;
        config.platform.jev.endpoint.api_key = Some(SecretRef::env("JEV_TEST_TOKEN"));
        config.platform.jev.model = "jev-router-v2".to_owned();
        config.platform.jev.confidence_threshold = 0.9;
        config.platform.jev.allowed_models = vec!["permitted-model".to_owned()];
        config.platform.jev.allowed_profiles = vec!["permitted-profile".to_owned()];
        config.platform.jev.timeout_secs = 7;
        config.platform.qdrant.collection = "workspace-documents".to_owned();
        config.platform.qdrant.timeout_secs = 11;
        config.platform.infinity.timeout_secs = 13;
        config.platform.open_terminal.endpoint.api_key =
            Some(SecretRef::env("TERMINAL_TEST_TOKEN"));
        config.platform.open_terminal.execution_timeout_secs = 120;
        config.platform.infinity.embedding_model = "embedding-v2".to_owned();
        config.platform.infinity.rerank_model = "reranker-v2".to_owned();

        let value = serde_json::to_value(ui_settings(&config)).expect("settings serialize");

        assert_eq!(value["jev"]["model"], "jev-router-v2");
        assert_eq!(value["jev"]["allowed_models"], json!(["permitted-model"]));
        assert_eq!(
            value["jev"]["allowed_profiles"],
            json!(["permitted-profile"])
        );
        assert_eq!(value["jev"]["timeout_secs"], 7);
        assert_eq!(value["qdrant"]["timeout_secs"], 11);
        assert_eq!(value["infinity"]["timeout_secs"], 13);
        assert_eq!(value["open_terminal"]["execution_timeout_secs"], 120);
        assert_eq!(value["open_terminal"]["request_timeout_secs"], 30);
        assert_eq!(value["open_terminal"]["poll_interval_ms"], 250);
        assert_eq!(value["open_terminal"]["max_output_bytes"], 32768);
        let confidence = value["jev"]["confidence_threshold"]
            .as_f64()
            .expect("confidence is numeric");
        assert!((confidence - 0.9).abs() < 1e-6);
        assert_eq!(value["qdrant"]["collection"], "workspace-documents");
        assert_eq!(value["infinity"]["embedding_model"], "embedding-v2");
        assert_eq!(value["infinity"]["rerank_model"], "reranker-v2");
        assert_eq!(value["jev"]["api_key_configured"], true);
        assert!(!value.to_string().contains("JEV_TEST_TOKEN"));
        assert!(!value.to_string().contains("TERMINAL_TEST_TOKEN"));
        let decoded: UiSettings = serde_json::from_value(value).expect("settings roundtrip");
        assert_eq!(
            decoded.jev.allowed_profiles,
            Some(config.platform.jev.allowed_profiles.clone())
        );
        let mut endpoint = config.platform.open_terminal.endpoint.clone();
        apply_platform_endpoint(&mut endpoint, &decoded.open_terminal.endpoint);
        assert_eq!(
            endpoint.api_key,
            config.platform.open_terminal.endpoint.api_key
        );
    }
}
