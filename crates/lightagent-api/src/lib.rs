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
#[cfg(feature = "provisioning-api")]
pub mod provisioning;
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
use lightagent_core::identity::{
    AuditAction, Caller, Capability, PrincipalId, PrincipalSource, Role, RunTrust,
    authorize_capability, revoked_message,
};
use lightagent_core::{
    AgentEvent, ApprovalPolicy, ConfigStore, LightagentPaths, PlatformEndpointConfig, ProfileId,
    ProfileStore, SecretRef, SkillStore, skill_dirs,
};
use lightagent_store::{Session, SessionId, SessionStore, StoredMessage, model_history};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

pub use auth::{AuthConfig, IdentityAuthority, IdentityState, Scope};
pub use manager::{RunFactory, RunManager, RunOwner, RunState, RunStatus, StartRun};

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
        .route("/api/lightagent/v1/jev", get(get_jev_status))
        .route("/api/lightagent/v1/jev/verify", post(verify_jev))
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

fn refused(denial: lightagent_core::identity::Denial) -> Response {
    let (status, message) = auth::rejection(denial);
    (status, Json(json!({ "error": message }))).into_response()
}

/// A multi-user caller holding `scope`, checked before any run is looked up
/// so the answer never depends on whether a run exists.
fn multi_user_caller(
    state: &AppState,
    headers: &HeaderMap,
    scope: Scope,
) -> Result<(Arc<auth::IdentityAuthority>, Caller), Box<Response>> {
    let Some(identity) = state.auth.identity() else {
        return Err(Box::new(internal("multi-user identity is not configured")));
    };
    let caller = state
        .auth
        .caller(headers)
        .map_err(|denial| Box::new(refused(denial)))?;
    if let Err(denial) = authorize_capability(&caller, scope.into()) {
        identity.record(
            Some(&caller),
            AuditAction::Request(scope.into()),
            None,
            Err(denial),
        );
        return Err(Box::new(refused(denial)));
    }
    Ok((Arc::clone(identity), caller))
}

/// Re-checks, during an event stream, that the subscriber may still see it.
type StreamGuard = Arc<dyn Fn() -> bool + Send + Sync>;

/// The run `id`, if the caller may take `scope` on it. In multi-user mode a
/// run the caller may not act on is answered exactly like a missing one, and
/// the returned guard re-checks the credential and ownership while a stream
/// stays open. The single-user path is unchanged.
async fn authorized_run(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    scope: Scope,
) -> Result<(Arc<RunState>, Option<StreamGuard>), Box<Response>> {
    if state.auth.identity().is_none() {
        if let Some(rejection) = deny(state, headers, scope) {
            return Err(Box::new(rejection));
        }
        return state
            .manager
            .get(id)
            .await
            .map(|run| (run, None))
            .ok_or_else(|| Box::new(not_found(id)));
    }
    let (identity, caller) = multi_user_caller(state, headers, scope)?;
    let Some(run) = state.manager.get(id).await else {
        return Err(Box::new(not_found(id)));
    };
    let capability: Capability = scope.into();
    let owner = run.owner().clone();
    let decision = identity.authorize_run(&caller, &owner, capability);
    identity.record(
        Some(&caller),
        AuditAction::Request(capability),
        owner.profile(),
        decision,
    );
    if decision.is_err() {
        return Err(Box::new(not_found(id)));
    }
    let guard: StreamGuard = Arc::new(move || {
        identity
            .refresh(&caller)
            .and_then(|current| identity.authorize_run(&current, &owner, capability))
            .is_ok()
    });
    Ok((run, Some(guard)))
}

/// How a request reaches profile-scoped data (sessions, the tool catalog,
/// skills, Jev status, the provider view).
enum Access {
    /// Single-user mode, or an operator in multi-user mode: the behavior
    /// these routes have always had.
    Unscoped,
    /// An ordinary user in multi-user mode, authorized for one profile.
    User(UserAccess),
}

/// A user authorized, from server-side ownership, for one profile.
struct UserAccess {
    principal: PrincipalId,
    profile: ProfileId,
    root: PathBuf,
}

impl UserAccess {
    /// The authorized profile's own session store.
    fn sessions(&self, state: &AppState) -> SessionStore {
        SessionStore::at_profile(&ProfileStore::new(&self.root).handle(&self.profile))
            .keep_history(state.sessions.is_history_kept())
    }

    /// Whether `session` is this user's, on this profile. Both facts come
    /// from the session record the server wrote; a session without an owner
    /// (created before multi-user mode, or by an operator) is never a user's.
    fn owns(&self, session: &Session) -> bool {
        session.owner.as_deref() == Some(self.principal.as_str())
            && session.profile == self.profile.as_str()
    }

    fn owns_summary(&self, summary: &lightagent_store::SessionSummary) -> bool {
        summary.owner.as_deref() == Some(self.principal.as_str())
            && summary.profile == self.profile.as_str()
    }
}

/// The Lightagent home, from the config store, when the embedding has one.
fn home(state: &AppState) -> Option<PathBuf> {
    state
        .config_store
        .as_ref()
        .and_then(|store| store.path().parent().map(std::path::Path::to_path_buf))
}

/// Authorize a request for `scope` on profile-scoped data.
///
/// Single-user mode and operators take the route's existing check. A user
/// must hold `scope` and be authorized for the profile (`requested`, else the
/// server's default) by the current ownership state: the name selects, the
/// ownership registry decides. A refusal is the same whether the profile is
/// someone else's, unowned or missing.
fn access(
    state: &AppState,
    headers: &HeaderMap,
    scope: Scope,
    requested: Option<&str>,
) -> Result<Access, Box<Response>> {
    let Some(identity) = state.auth.identity() else {
        return match deny(state, headers, scope) {
            Some(rejection) => Err(Box::new(rejection)),
            None => Ok(Access::Unscoped),
        };
    };
    let caller = state
        .auth
        .caller(headers)
        .map_err(|denial| Box::new(refused(denial)))?;
    if caller.principal.role == Role::Operator {
        return match deny(state, headers, scope) {
            Some(rejection) => Err(Box::new(rejection)),
            None => Ok(Access::Unscoped),
        };
    }
    if let Err(denial) = authorize_capability(&caller, scope.into()) {
        identity.record(
            Some(&caller),
            AuditAction::Request(scope.into()),
            None,
            Err(denial),
        );
        return Err(Box::new(refused(denial)));
    }
    let raw = requested.unwrap_or(state.session_profile.as_str());
    let Ok(profile) = ProfileId::new(raw) else {
        return Err(Box::new(bad_request("invalid profile id")));
    };
    let authorized = identity
        .authorize_profile(&caller, scope.into(), &profile)
        .map_err(|denial| Box::new(refused(denial)))?;
    // Without the home, no restricted view can be built: operator-only.
    let Some(root) = home(state) else {
        return Err(Box::new(operator_only()));
    };
    Ok(Access::User(UserAccess {
        principal: authorized.principal().clone(),
        profile: authorized.profile().clone(),
        root,
    }))
}

/// The answer for a session a user may not see, and for one that does not
/// exist: identical, so an id reveals nothing.
fn no_session() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "no such session" })),
    )
        .into_response()
}

/// The answer when a restricted representation cannot be produced: the
/// endpoint stays operator-only.
fn operator_only() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "this endpoint is operator-only in multi-user mode" })),
    )
        .into_response()
}

/// The optional `?profile=` of a profile-scoped request.
#[derive(Default, Deserialize)]
struct ProfileQuery {
    #[serde(default)]
    profile: Option<String>,
}

async fn health() -> Response {
    Json(json!({ "status": "ok", "service": "lightagent" })).into_response()
}

async fn list_tools(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    let scoped = match access(&state, &headers, Scope::ToolsRead, query.profile.as_deref()) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
    let listed = match &scoped {
        // What a run of this profile, started by this user, would receive:
        // a configured tool is not available to every profile.
        Some(user) => match state
            .manager
            .tools_for(&user.profile, RunTrust::RemoteUser)
            .await
        {
            Ok(tools) => Ok(tools),
            Err(_) => return operator_only(),
        },
        None => state.manager.tools().await,
    };
    let definitions = match listed {
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
    match scoped {
        Some(user) => {
            Json(json!({ "tools": tools, "profile": user.profile.as_str() })).into_response()
        }
        None => Json(json!({ "tools": tools })).into_response(),
    }
}

/// Public metadata for skills installed for the active profile. Bodies stay in
/// the harness and are read by the approval-gated `skill.read` tool only.
async fn list_skills(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    match access(&state, &headers, Scope::ToolsRead, query.profile.as_deref()) {
        Ok(Access::Unscoped) => {}
        Ok(Access::User(user)) => return user_skills(&state, &user).await,
        Err(response) => return *response,
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
/// A user's skill list: the authorized profile's skills, and only when a run
/// of that profile would receive `skill.read`.
async fn user_skills(state: &AppState, user: &UserAccess) -> Response {
    let Ok(tools) = state
        .manager
        .tools_for(&user.profile, RunTrust::RemoteUser)
        .await
    else {
        return operator_only();
    };
    let readable = tools.iter().any(|tool| tool.name == "skill.read");
    let mut listed = Vec::new();
    if readable {
        let profiles = ProfileStore::new(&user.root);
        let skills = SkillStore::load(&skill_dirs(
            &user.root,
            profiles.handle(&user.profile).dir(),
        ));
        for name in skills.names() {
            if let Some(skill) = skills.get(&name) {
                listed.push(json!({"name": skill.name, "description": skill.description}));
            }
        }
    }
    Json(json!({ "skills": listed, "profile": user.profile.as_str() })).into_response()
}

/// The Jev status the Tools and Settings screens show. Readable with the
/// tool-catalog scope; it holds no credential, secret reference or message.
async fn get_jev_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    match access(&state, &headers, Scope::ToolsRead, query.profile.as_deref()) {
        Ok(Access::Unscoped) => {}
        // Only the authorized profile's own permissions and latest record.
        Ok(Access::User(user)) => {
            return match state.manager.jev_status_for(&user.profile).await {
                Ok(status) => Json(status).into_response(),
                Err(_) => operator_only(),
            };
        }
        Err(response) => return *response,
    }
    match state.manager.jev_status().await {
        Ok(status) => Json(status).into_response(),
        Err(error) => unavailable(&error),
    }
}

/// The operator's connection check. It spends one Jev request, so it needs the
/// same administrator scope as changing settings.
async fn verify_jev(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    match state.manager.verify_jev().await {
        Ok(verification) => Json(verification).into_response(),
        Err(error) => unavailable(&error),
    }
}

fn unavailable(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": message })),
    )
        .into_response()
}

async fn get_provider_capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    match access(&state, &headers, Scope::ToolsRead, query.profile.as_deref()) {
        Ok(Access::Unscoped) => {}
        Ok(Access::User(user)) => return user_provider(&user),
        Err(response) => return *response,
    }
    match state.manager.provider_capabilities().await {
        Ok(capabilities) => Json(capabilities).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))).into_response(),
    }
}

/// A user's provider view: the same fields as the operator's, with the
/// provider, endpoint, catalogs and runtime models withheld. Only the
/// authorized profile's own configured model is shown.
fn user_provider(user: &UserAccess) -> Response {
    let Ok(profile) = ProfileStore::new(&user.root).load(&user.profile) else {
        return internal("the profile could not be read");
    };
    Json(json!({
        "provider": null,
        "base_url": null,
        "configured_model": profile.routing.model,
        "models": [],
        "model_aliases": {},
        "model_catalog": {},
        "runtime_models": [],
        "streaming": true,
        "tool_calls": true,
        "reasoning_content": false,
        "scope": "profile",
        "profile": user.profile.as_str(),
    }))
    .into_response()
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
    if let Some(identity) = state.auth.identity() {
        match state.auth.caller(&headers) {
            Ok(caller) if caller.principal.role == Role::User => {
                return user_profiles(&state, identity, &caller);
            }
            Ok(_) => {}
            Err(denial) => return refused(denial),
        }
    }
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

/// A user's profile list: only the profiles it owns or was granted, from the
/// current ownership state. Another profile's id, name or model never
/// appears, and the server's default profile is named only if accessible.
fn user_profiles(
    state: &AppState,
    identity: &auth::IdentityAuthority,
    caller: &Caller,
) -> Response {
    let accessible = match identity.accessible_profiles(caller, Capability::SessionsRead) {
        Ok(accessible) => accessible,
        Err(denial) => {
            identity.record(
                Some(caller),
                AuditAction::Request(Capability::SessionsRead),
                None,
                Err(denial),
            );
            return refused(denial);
        }
    };
    identity.record(
        Some(caller),
        AuditAction::Request(Capability::SessionsRead),
        None,
        Ok(()),
    );
    let Some(root) = home(state) else {
        return operator_only();
    };
    let profiles = ProfileStore::new(&root);
    let mut summaries = Vec::new();
    for id in &accessible {
        // Ownership of a profile that no longer exists lists nothing.
        if let Ok(profile) = profiles.load(id) {
            summaries.push(ProfileSummary {
                active: profile.id.as_str() == state.session_profile,
                id: profile.id.as_str().to_owned(),
                name: profile.name,
                model: profile.routing.model,
            });
        }
    }
    let active = summaries
        .iter()
        .any(|summary| summary.active)
        .then(|| state.session_profile.clone());
    Json(json!({ "active_profile": active, "profiles": summaries })).into_response()
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
    /// Task classification. Absent from an older client's request, which then
    /// leaves the saved classification settings unchanged.
    #[serde(default)]
    classification: Option<UiJevClassification>,
    /// What model routing sends: `legacy_raw` or `sanitized`. Absent from an
    /// older client's request, which leaves the saved policy unchanged.
    #[serde(default)]
    routing_input: Option<lightagent_core::JevRoutingInput>,
    /// Which profiles may send task content to Jev for model routing:
    /// `strict` or the `legacy_all_profiles` compatibility policy. Absent
    /// from an older client's request, which leaves it unchanged.
    #[serde(default)]
    routing_permission: Option<lightagent_core::JevRoutingPermission>,
    /// Profiles permitted to send task content for model routing under
    /// `strict`. Absent from an older client's request, which leaves it
    /// unchanged.
    #[serde(default)]
    routing_allowed_profiles: Option<Vec<String>>,
    /// Write-only credential submitted from the Settings screen. It is never
    /// serialized in a settings response.
    #[serde(default, skip_serializing)]
    api_key: Option<String>,
}

/// Task-classification settings, separate from model routing.
#[derive(Clone, Deserialize, Serialize)]
struct UiJevClassification {
    mode: lightagent_core::JevClassificationMode,
    allowed_profiles: Vec<String>,
    max_input_chars: usize,
    default_threshold: f32,
    #[serde(default)]
    version_thresholds: std::collections::BTreeMap<String, f32>,
    explicit_tool: bool,
    max_calls_per_run: u32,
    /// Absent from an older client's request, which leaves it unchanged.
    #[serde(default)]
    workflow_guidance: Option<bool>,
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
            classification: Some(UiJevClassification {
                mode: config.platform.jev.classification.mode,
                allowed_profiles: config.platform.jev.classification.allowed_profiles.clone(),
                max_input_chars: config.platform.jev.classification.max_input_chars,
                default_threshold: config.platform.jev.classification.default_threshold,
                version_thresholds: config
                    .platform
                    .jev
                    .classification
                    .version_thresholds
                    .clone(),
                explicit_tool: config.platform.jev.classification.explicit_tool,
                max_calls_per_run: config.platform.jev.classification.max_calls_per_run,
                workflow_guidance: Some(config.platform.jev.classification.workflow_guidance),
            }),
            routing_input: Some(config.platform.jev.routing_input),
            routing_permission: Some(config.platform.jev.routing_permission),
            routing_allowed_profiles: Some(config.platform.jev.routing_allowed_profiles.clone()),
            api_key: None,
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

fn apply_jev_api_key(
    config: &mut lightagent_core::Config,
    store: &ConfigStore,
    api_key: Option<String>,
) -> Result<(), String> {
    let Some(api_key) = api_key
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let root = store
        .path()
        .parent()
        .ok_or("Lightagent config has no parent directory")?;
    let path = LightagentPaths::rooted_at(root).provider_key_file("jev");
    lightagent_core::paths::write_private(&path, api_key.as_bytes())
        .map_err(|error| format!("could not save the Jev API key: {error}"))?;
    config.platform.jev.endpoint.api_key = Some(SecretRef::file(path));
    Ok(())
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
    if let Some(value) = settings.jev.classification {
        let classification = &mut config.platform.jev.classification;
        classification.mode = value.mode;
        classification.allowed_profiles = value.allowed_profiles;
        classification.max_input_chars = value.max_input_chars;
        classification.default_threshold = value.default_threshold;
        classification.version_thresholds = value.version_thresholds;
        classification.explicit_tool = value.explicit_tool;
        classification.max_calls_per_run = value.max_calls_per_run;
        if let Some(workflow_guidance) = value.workflow_guidance {
            classification.workflow_guidance = workflow_guidance;
        }
    }
    if let Some(routing_input) = settings.jev.routing_input {
        config.platform.jev.routing_input = routing_input;
    }
    if let Some(routing_permission) = settings.jev.routing_permission {
        config.platform.jev.routing_permission = routing_permission;
    }
    if let Some(profiles) = settings.jev.routing_allowed_profiles {
        config.platform.jev.routing_allowed_profiles = profiles;
    }
    if let Err(error) = apply_jev_api_key(&mut config, store, settings.jev.api_key) {
        return internal(&error);
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
    Json(mut body): Json<CreateRunBody>,
) -> Response {
    // Multi-user mode authorizes the caller for the profile, resolved on the
    // server, before anything profile-specific is built. `owner` records who
    // started the run and as which profile; it never changes afterwards.
    let mut owner: Option<(
        lightagent_core::identity::PrincipalId,
        Option<ProfileId>,
        lightagent_core::identity::RunTrust,
    )> = None;
    // In multi-user mode the run stays revocable: who started it is
    // re-authorized at every enforcement point while it runs.
    let mut supervision: Option<(Arc<auth::IdentityAuthority>, Caller)> = None;
    // A user's session: in the authorized profile's store, and its own.
    let mut session_scope: Option<UserAccess> = None;
    if state.auth.identity().is_some() {
        let (identity, caller) = match multi_user_caller(&state, &headers, Scope::RunsWrite) {
            Ok(found) => found,
            Err(response) => return *response,
        };
        if caller.principal.role == Role::User {
            let requested = body
                .profile
                .clone()
                .unwrap_or_else(|| state.session_profile.clone());
            let Ok(requested) = ProfileId::new(requested) else {
                return bad_request("invalid profile id");
            };
            match identity.authorize_profile(&caller, Capability::RunsWrite, &requested) {
                Ok(authorized) => {
                    // Continuing a session appends to it: `sessions_write` on
                    // the same profile, which the owner bundle does not hold
                    // without a grant.
                    if body.session_id.is_some() {
                        if let Err(denial) = identity.authorize_profile(
                            &caller,
                            Capability::SessionsWrite,
                            authorized.profile(),
                        ) {
                            return refused(denial);
                        }
                        let Some(root) = home(&state) else {
                            return operator_only();
                        };
                        session_scope = Some(UserAccess {
                            principal: authorized.principal().clone(),
                            profile: authorized.profile().clone(),
                            root,
                        });
                    }
                    body.profile = Some(authorized.profile().as_str().to_owned());
                    owner = Some((
                        authorized.principal().clone(),
                        Some(authorized.profile().clone()),
                        caller.principal.role.run_trust(),
                    ));
                }
                Err(denial) => return refused(denial),
            }
        } else {
            owner = Some((
                caller.principal.id.clone(),
                None,
                caller.principal.role.run_trust(),
            ));
        }
        supervision = Some((identity, caller));
    } else if let Some(rejection) = deny(&state, &headers, Scope::RunsWrite) {
        return rejection;
    }
    let mut history = Vec::new();
    let mut profile = body.profile;
    let mut session_id = None;
    let session_store = match &session_scope {
        Some(user) => user.sessions(&state),
        None => state.sessions.clone(),
    };
    let session_profile = match &session_scope {
        Some(user) => user.profile.as_str().to_owned(),
        None => state.session_profile.clone(),
    };
    if let Some(raw) = body.session_id {
        let id = match SessionId::parse(&raw) {
            Ok(id) => id,
            Err(error) => return bad_request(&error.to_string()),
        };
        // Another user's session (or an unowned one) is answered exactly
        // like a missing one, before anything else is revealed about it.
        if let Some(user) = &session_scope
            && !session_store
                .load(&id)
                .is_ok_and(|session| user.owns(&session))
        {
            return no_session();
        }
        let mut busy = state.busy_sessions.lock().await;
        if !busy.insert(id.clone()) {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "session already has an active run"})),
            )
                .into_response();
        }
        let mut session = match session_store.load(&id) {
            Ok(session) => session,
            Err(error) => {
                busy.remove(&id);
                return not_found(&error.to_string());
            }
        };
        if session.profile != session_profile
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
        if let Err(error) = session_store.save(&session) {
            busy.remove(&id);
            return internal(&error.to_string());
        }
        session_id = Some(id);
    }
    let request = StartRun {
        message: body.message,
        history,
        profile,
        cwd: None,
        model: body.model,
    };
    let run = match owner {
        Some((principal, authorized, trust)) => {
            // An operator's run records the profile it named, if valid.
            let profile = authorized.or_else(|| {
                request
                    .profile
                    .as_deref()
                    .and_then(|id| ProfileId::new(id).ok())
            });
            let mut owner = RunOwner::new(principal, profile, trust);
            match supervision {
                Some((identity, caller)) => {
                    if let PrincipalSource::Credential { key_id } = &caller.principal.source {
                        owner = owner.with_credential(key_id.clone());
                    }
                    let check = identity.run_check(caller, owner.clone());
                    let started = state
                        .manager
                        .start_supervised(request, owner, check, identity.generation())
                        .await;
                    match started {
                        Ok(run) => run,
                        Err(denial) => {
                            if let Some(id) = &session_id {
                                state.busy_sessions.lock().await.remove(id);
                            }
                            return refused(denial);
                        }
                    }
                }
                None => state.manager.start_as(request, owner).await,
            }
        }
        None => state.manager.start(request).await,
    };
    if let Some(id) = &session_id {
        let state = Arc::clone(&state);
        let run = Arc::clone(&run);
        let id = id.clone();
        let sessions = session_store.clone();
        tokio::spawn(async move {
            let mut seen = 0;
            loop {
                let (events, status) = run.wait_from(seen).await;
                seen += events.len();
                if status.is_terminal() {
                    match sessions.load(&id) {
                        Ok(mut session) => {
                            let events = run.events().await;
                            // The stop reason, as the terminal saves it, so a
                            // reloaded session still says why a run ended.
                            let stop =
                                stop_reason_of(&events).unwrap_or_else(|| format!("{status:?}"));
                            session.record_run_events(&events, &stop);
                            if let Err(error) = sessions.save(&session) {
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
    subject = subject.trim_start_matches(['"', '\'']);
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
    let candidate = joined.trim_end_matches(['.', '?', '!', ':', ';', ',']);
    let mut title = candidate.chars().take(LIMIT).collect::<String>();
    title = title
        .trim_end_matches(['.', '?', '!', ':', ';', ','])
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
    match authorized_run(&state, &headers, &id, Scope::RunsRead).await {
        Ok((run, _)) => {
            let events = run.events().await;
            let mut body = json!({
                "id": run.id(),
                "status": run.status().await,
                "events": events.len(),
                "pending_approval": run.pending().await,
                // Additive: why a terminal run stopped (`EndTurn`,
                // `MaxTokens`, `Incomplete`, …); null while it runs.
                "stop_reason": stop_reason_of(&events),
            });
            // Only a revoked (multi-user) run carries this, as a fixed code.
            if let Some(denial) = run.revocation() {
                body["revocation"] = json!(denial.code());
            }
            Json(body).into_response()
        }
        Err(response) => *response,
    }
}

/// Where a client asks an event stream to resume.
#[derive(Deserialize)]
struct EventsQuery {
    /// The id of the last event the client already has.
    #[serde(default)]
    after: Option<usize>,
}

/// The first buffer position a stream sends: one past the last event id the
/// client reports, from `?after=` or the `Last-Event-ID` a reconnecting
/// `EventSource` sends, whichever is later. An unreadable header is ignored
/// (the client deduplicates by id); none means the whole run.
fn resume_position(headers: &HeaderMap, query: &EventsQuery) -> usize {
    let header = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok());
    [header, query.after]
        .into_iter()
        .flatten()
        .map(|last| last.saturating_add(1))
        .max()
        .unwrap_or(0)
}

async fn run_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Response {
    match authorized_run(&state, &headers, &id, Scope::RunsRead).await {
        Ok((run, guard)) => {
            // A cursor past the buffer resumes at its end rather than waiting
            // for events that may never exist.
            let start = resume_position(&headers, &query).min(run.events().await.len());
            Sse::new(event_stream(run, guard, start))
                .keep_alive(KeepAlive::default())
                .into_response()
        }
        Err(response) => *response,
    }
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match authorized_run(&state, &headers, &id, Scope::RunsWrite).await {
        Ok((run, _)) => {
            run.cancel();
            Json(json!({ "id": run.id(), "cancelled": true })).into_response()
        }
        Err(response) => *response,
    }
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    match access(
        &state,
        &headers,
        Scope::SessionsRead,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => {}
        // Only this user's own sessions of the authorized profile.
        Ok(Access::User(user)) => {
            return match user.sessions(&state).list() {
                Ok(list) => {
                    let list: Vec<_> = list
                        .into_iter()
                        .filter(|summary| user.owns_summary(summary))
                        .collect();
                    Json(json!({ "sessions": list })).into_response()
                }
                Err(_) => internal("sessions could not be listed"),
            };
        }
        Err(response) => return *response,
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
    #[serde(default)]
    profile: Option<String>,
}

async fn search_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<SearchSessionsQuery>,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsRead,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
    let term = query.q.trim();
    if term.is_empty() {
        return Json(json!({ "sessions": [] })).into_response();
    }
    if term.chars().count() > 200 {
        return bad_request("search query must be 200 characters or fewer");
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    if let Some(user) = user {
        // Filtered before the limit, so another user's sessions neither
        // appear nor crowd this user's out.
        return match user.sessions(&state).search(term, usize::MAX) {
            Ok(found) => {
                let found: Vec<_> = found
                    .into_iter()
                    .filter(|result| user.owns_summary(&result.session))
                    .take(limit)
                    .collect();
                Json(json!({ "sessions": found })).into_response()
            }
            Err(_) => internal("sessions could not be searched"),
        };
    }
    match state.sessions.search(term, limit) {
        Ok(list) => Json(json!({ "sessions": list })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsWrite,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
    if !state.sessions.is_history_kept() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session history is disabled" })),
        )
            .into_response();
    }
    if let Some(user) = user {
        // Recorded with its owner and the authorized profile, by the server.
        let mut session = Session::new(user.profile.as_str(), "agent session");
        session.owner = Some(user.principal.as_str().to_owned());
        return match user.sessions(&state).save(&session) {
            Ok(()) => (StatusCode::CREATED, Json(json!({ "id": session.id }))).into_response(),
            Err(_) => internal("the session could not be saved"),
        };
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
    Query(query): Query<ProfileQuery>,
    body: Bytes,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsWrite,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
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
    if let Some(user) = user {
        let store = user.sessions(&state);
        return match store.load(&id) {
            Ok(session) if user.owns(&session) => {
                match store.save_attachment(&id, filename, &body) {
                    Ok(path) => Json(json!({ "name": filename, "path": path })).into_response(),
                    Err(_) => internal("the attachment could not be saved"),
                }
            }
            _ => no_session(),
        };
    }
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
    Query(query): Query<ProfileQuery>,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsRead,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
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
    if let Some(user) = user {
        return match user.sessions(&state).load(&id) {
            Ok(session) if user.owns(&session) => Json(session).into_response(),
            _ => no_session(),
        };
    }
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
#[derive(Default)]
enum Patch<T> {
    #[default]
    Missing,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Value)
    }
}

const SESSION_TITLE_LIMIT: usize = 200;
const SESSION_PROJECT_LIMIT: usize = 120;

fn normalized_metadata(value: String, field: &str, limit: usize) -> Result<String, Box<Response>> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(Box::new(bad_request(&format!(
            "session {field} cannot be empty"
        ))));
    }
    if value.chars().count() > limit {
        return Err(Box::new(bad_request(&format!(
            "session {field} must be at most {limit} characters"
        ))));
    }
    Ok(value)
}

async fn update_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ProfileQuery>,
    Json(body): Json<UpdateSessionBody>,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsWrite,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
    let store = match &user {
        Some(user) => user.sessions(&state),
        None => state.sessions.clone(),
    };
    let id = match SessionId::parse(&id) {
        Ok(id) => id,
        Err(error) => return bad_request(&error.to_string()),
    };
    let title = match body.title {
        Some(value) => match normalized_metadata(value, "title", SESSION_TITLE_LIMIT) {
            Ok(value) => Some(value),
            Err(response) => return *response,
        },
        None => None,
    };
    let project = match body.project {
        Patch::Value(Some(value)) => {
            match normalized_metadata(value, "project", SESSION_PROJECT_LIMIT) {
                Ok(value) => Some(Some(value)),
                Err(response) => return *response,
            }
        }
        Patch::Value(None) => Some(None),
        Patch::Missing => None,
    };

    // A user learns nothing about a session that is not its own, not even
    // whether it has an active run.
    if let Some(user) = &user
        && !user
            .sessions(&state)
            .load(&id)
            .is_ok_and(|session| user.owns(&session))
    {
        return no_session();
    }
    let busy = state.busy_sessions.lock().await;
    if busy.contains(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session has an active run" })),
        )
            .into_response();
    }
    let mut session = match (store.load(&id), &user) {
        (Ok(session), None) => session,
        (Ok(session), Some(user)) if user.owns(&session) => session,
        (_, Some(_)) => return no_session(),
        (Err(error), None) => return not_found(&error.to_string()),
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
    let result = store.save(&session);
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
    Query(query): Query<ProfileQuery>,
) -> Response {
    let user = match access(
        &state,
        &headers,
        Scope::SessionsWrite,
        query.profile.as_deref(),
    ) {
        Ok(Access::Unscoped) => None,
        Ok(Access::User(user)) => Some(user),
        Err(response) => return *response,
    };
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
    // A user learns nothing about a session that is not its own, not even
    // whether it has an active run.
    if let Some(user) = &user
        && !user
            .sessions(&state)
            .load(&id)
            .is_ok_and(|session| user.owns(&session))
    {
        return no_session();
    }
    let busy = state.busy_sessions.lock().await;
    if busy.contains(&id) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "session has an active run" })),
        )
            .into_response();
    }
    if let Some(user) = user {
        let store = user.sessions(&state);
        let result = match store.load(&id) {
            Ok(session) if user.owns(&session) => store.delete(&id),
            _ => return no_session(),
        };
        drop(busy);
        return match result {
            Ok(removed) => Json(json!({ "deleted": removed })).into_response(),
            Err(_) => internal("the session could not be deleted"),
        };
    }
    let result = state.sessions.delete(&id);
    drop(busy);
    match result {
        Ok(removed) => Json(json!({ "deleted": removed })).into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn list_approvals(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    // Multi-user mode lists only the runs the caller may answer for.
    let multi_user = if state.auth.identity().is_some() {
        match multi_user_caller(&state, &headers, Scope::ApprovalsWrite) {
            Ok(found) => Some(found),
            Err(response) => return *response,
        }
    } else if let Some(rejection) = deny(&state, &headers, Scope::ApprovalsWrite) {
        return rejection;
    } else {
        None
    };
    let mut waiting = Vec::new();
    for run in state.manager.awaiting_approval().await {
        if let Some((identity, caller)) = &multi_user
            && identity
                .authorize_run(caller, run.owner(), Capability::ApprovalsWrite)
                .is_err()
        {
            continue;
        }
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
    use lightagent_core::ApprovalDecision;
    let state_run = match authorized_run(&state, &headers, &run, Scope::ApprovalsWrite).await {
        Ok((found, _)) => found,
        Err(response) => return *response,
    };
    // An approval for a run that has lost its authorization is stale: it is
    // refused here, and the run checks again before acting on any decision.
    if let Err(denial) = state_run.authorization().verify() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": revoked_message(denial) })),
        )
            .into_response();
    }
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

/// The stop reason of a run's terminal event, spelled as the `run.completed`
/// payload spells it.
fn stop_reason_of(events: &[AgentEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event {
        AgentEvent::RunCompleted { reason } => Some(format!("{reason:?}")),
        _ => None,
    })
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

/// How often a guarded event stream re-checks authorization while no event
/// arrives.
const STREAM_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// A run's events as an SSE stream from buffer position `start`: buffered
/// history first, then the live tail, ending after the run's terminal event.
/// Every frame's `id` is its buffer position, so a reconnect resumes in order
/// with nothing repeated or skipped.
fn event_stream(
    run: Arc<RunState>,
    guard: Option<StreamGuard>,
    start: usize,
) -> impl Stream<Item = Result<Event, Infallible>> {
    struct Cursor {
        run: Arc<RunState>,
        seen: usize,
        queue: VecDeque<(usize, AgentEvent)>,
        finished: bool,
        guard: Option<StreamGuard>,
        rechecks: tokio::sync::watch::Receiver<u64>,
    }
    let rechecks = run.rechecks();
    stream::unfold(
        Cursor {
            run,
            seen: start,
            queue: VecDeque::new(),
            finished: false,
            guard,
            rechecks,
        },
        |mut cursor| async move {
            loop {
                // A guarded (multi-user) stream ends as soon as the
                // subscriber's credential or ownership no longer allows it.
                if cursor.guard.as_ref().is_some_and(|allowed| !allowed()) {
                    return None;
                }
                if let Some((position, event)) = cursor.queue.pop_front() {
                    return Some((Ok(sse::to_sse_with_id(&event, position)), cursor));
                }
                if cursor.finished {
                    return None;
                }
                let (new_events, status) = if cursor.guard.is_some() {
                    // Wake periodically to re-check while the run is quiet,
                    // and at once when the supervisor asks.
                    let run = Arc::clone(&cursor.run);
                    tokio::select! {
                        next = run.wait_from(cursor.seen) => next,
                        () = tokio::time::sleep(STREAM_RECHECK) => continue,
                        changed = cursor.rechecks.changed() => {
                            if changed.is_err() {
                                return None;
                            }
                            continue;
                        }
                    }
                } else {
                    cursor.run.wait_from(cursor.seen).await
                };
                let first = cursor.seen;
                cursor.seen += new_events.len();
                cursor.queue.extend((first..).zip(new_events));
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
