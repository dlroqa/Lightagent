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
pub mod desktop;
pub mod manager;
pub mod ops;
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
use lightagent_core::saved_providers;
use lightagent_core::{
    AgentEvent, ApprovalPolicy, ConfigError, ConfigRevision, ConfigStore, LightagentPaths,
    PlatformEndpointConfig, ProfileId, ProfileStore, SecretRef, SkillStore, UpdateError,
    skill_dirs,
};
use lightagent_extensions::manage as extension_ops;
use lightagent_store::{
    Session, SessionId, SessionStore, SessionUpdateError, StoreError, StoredMessage, model_history,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

pub use auth::{AuthConfig, IdentityAuthority, IdentityState, Scope};
pub use manager::{RunFactory, RunManager, RunOptions, RunOwner, RunState, RunStatus, StartRun};

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
        .route("/api/lightagent/v1/workers", get(list_workers))
        .route(
            "/api/lightagent/v1/workers/{role}",
            axum::routing::put(assign_worker).delete(revoke_worker),
        )
        .route(
            "/api/lightagent/v1/provider",
            get(get_provider_capabilities),
        )
        .route("/api/lightagent/v1/jev", get(get_jev_status))
        .route("/api/lightagent/v1/jev/verify", post(verify_jev))
        .route(
            "/api/lightagent/v1/provider/check",
            post(post_provider_check),
        )
        .route(
            "/api/lightagent/v1/provider/credential",
            get(get_provider_credential)
                .put(put_provider_credential)
                .delete(delete_provider_credential),
        )
        .route(
            "/api/lightagent/v1/provider/active",
            axum::routing::put(select_provider),
        )
        .route(
            "/api/lightagent/v1/provider/model",
            axum::routing::put(select_provider_model),
        )
        .route(
            "/api/lightagent/v1/providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/api/lightagent/v1/providers/{id}",
            get(show_provider)
                .patch(update_provider)
                .delete(remove_provider),
        )
        .route(
            "/api/lightagent/v1/providers/{id}/check",
            post(check_saved_provider),
        )
        .route(
            "/api/lightagent/v1/workspace",
            get(get_workspace).put(put_workspace),
        )
        .route(
            "/api/lightagent/v1/jev/routing-report",
            get(get_jev_routing_report),
        )
        .route("/api/lightagent/v1/skills", get(list_skills))
        .route("/api/lightagent/v1/skills/imports", post(import_skills))
        .route("/api/lightagent/v1/identity", get(identity_status))
        .route(
            "/api/lightagent/v1/identity/users",
            get(list_identity_users).post(create_identity_user),
        )
        .route(
            "/api/lightagent/v1/identity/users/{id}/suspend",
            post(suspend_identity_user),
        )
        .route(
            "/api/lightagent/v1/identity/credentials",
            get(list_identity_credentials).post(issue_identity_credential),
        )
        .route(
            "/api/lightagent/v1/identity/credentials/{key_id}/rotate",
            post(rotate_identity_credential),
        )
        .route(
            "/api/lightagent/v1/identity/credentials/{key_id}/revoke",
            post(revoke_identity_credential),
        )
        .route(
            "/api/lightagent/v1/identity/owners/{profile}",
            axum::routing::put(assign_identity_owner).delete(remove_identity_owner),
        )
        .route(
            "/api/lightagent/v1/identity/owners/{profile}/reassign",
            post(reassign_identity_owner),
        )
        .route(
            "/api/lightagent/v1/identity/grants",
            get(list_identity_grants),
        )
        .route(
            "/api/lightagent/v1/identity/grants/revoke",
            post(revoke_identity_grant),
        )
        .route("/api/lightagent/v1/identity/audit", get(identity_audit))
        .route(
            "/api/lightagent/v1/memories",
            get(list_memories).post(add_memory).delete(clear_memories),
        )
        .route("/api/lightagent/v1/memories/search", get(search_memories))
        .route("/api/lightagent/v1/memories/reflect", get(reflect_memories))
        .route(
            "/api/lightagent/v1/memories/candidates",
            get(memory_candidates),
        )
        .route("/api/lightagent/v1/memories/promote", post(promote_memory))
        .route(
            "/api/lightagent/v1/memories/items/{id}",
            axum::routing::put(update_memory).delete(forget_memory),
        )
        .route(
            "/api/lightagent/v1/knowledge",
            get(list_knowledge)
                .post(add_knowledge)
                .delete(clear_knowledge),
        )
        .route("/api/lightagent/v1/knowledge/search", get(search_knowledge))
        .route(
            "/api/lightagent/v1/extensions",
            get(list_extensions).post(install_extension),
        )
        .route(
            "/api/lightagent/v1/extensions/{name}",
            get(show_extension).delete(uninstall_extension),
        )
        .route(
            "/api/lightagent/v1/extensions/{name}/enabled",
            axum::routing::put(set_extension_enabled),
        )
        .route("/api/lightagent/v1/mcp", get(get_mcp).put(put_mcp))
        .route("/api/lightagent/v1/mcp/servers", post(add_mcp_server))
        .route(
            "/api/lightagent/v1/mcp/servers/{name}",
            axum::routing::put(update_mcp_server).delete(remove_mcp_server),
        )
        .route(
            "/api/lightagent/v1/mcp/servers/{name}/check",
            post(check_mcp_server),
        )
        .route(
            "/api/lightagent/v1/profiles",
            get(list_profiles).post(create_profile),
        )
        .route(
            "/api/lightagent/v1/profiles/{id}",
            get(show_profile).delete(delete_profile),
        )
        .route(
            "/api/lightagent/v1/profiles/{id}/active",
            axum::routing::put(use_profile),
        )
        .route(
            "/api/lightagent/v1/profiles/{id}/preset",
            post(preset_profile),
        )
        .route(
            "/api/lightagent/v1/settings",
            get(get_settings).put(save_settings),
        )
        .route("/api/lightagent/v1/config", get(get_config))
        .route(
            "/api/lightagent/v1/config/{key}",
            get(get_config_key).put(set_config_key),
        )
        .route("/api/lightagent/v1/runs", get(list_runs).post(create_run))
        .route("/api/lightagent/v1/runs/{id}", get(get_run))
        .route("/api/lightagent/v1/runs/{id}/events", get(run_events))
        .route("/api/lightagent/v1/runs/{id}/cancel", post(cancel_run))
        .route("/api/lightagent/v1/runs/{id}/continue", post(continue_run))
        .route(
            "/api/lightagent/v1/sessions",
            get(list_sessions).post(create_session),
        )
        .route("/api/lightagent/v1/sessions/search", get(search_sessions))
        .route(
            "/api/lightagent/v1/sessions/{id}/regenerate",
            post(regenerate_run),
        )
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
#[derive(Clone)]
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

/// `GET /runs`: the runs that have not finished, for the operator. The
/// desktop app checks them before it switches profile or quits.
async fn list_runs(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let mut runs = Vec::new();
    for run in state.manager.active_runs().await {
        runs.push(json!({ "id": run.id(), "status": run.status().await }));
    }
    Json(json!({ "runs": runs })).into_response()
}

/// Profile management is operator-only: it changes what every later run sees.
async fn profile_operation(
    state: &AppState,
    headers: &HeaderMap,
    request: ops::ProfileRequest,
) -> Response {
    if let Some(rejection) = deny(state, headers, Scope::Admin) {
        return rejection;
    }
    match state.manager.profiles(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn create_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ops::CreateProfile>,
) -> Response {
    profile_operation(&state, &headers, ops::ProfileRequest::Create(body)).await
}

async fn show_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    profile_operation(&state, &headers, ops::ProfileRequest::Show(id)).await
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    profile_operation(&state, &headers, ops::ProfileRequest::Delete(id)).await
}

async fn use_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    profile_operation(&state, &headers, ops::ProfileRequest::Use(id)).await
}

#[derive(Deserialize)]
struct PresetBody {
    preset: String,
}

async fn preset_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PresetBody>,
) -> Response {
    profile_operation(
        &state,
        &headers,
        ops::ProfileRequest::Preset {
            id,
            preset: body.preset,
        },
    )
    .await
}

async fn health() -> Response {
    Json(json!({ "status": "ok", "service": "lightagent" })).into_response()
}

/// `?mcp=skip`: build the catalog without connecting any MCP server.
#[derive(Deserialize)]
struct ToolListingQuery {
    #[serde(default)]
    mcp: Option<String>,
}

async fn list_tools(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
    Query(listing): Query<ToolListingQuery>,
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
        None => {
            // The catalog with permissions, when the embedding builds it.
            let include_mcp = listing.mcp.as_deref() != Some("skip");
            if let Some(catalog) = state.manager.tool_catalog(include_mcp).await {
                return match catalog {
                    Ok(body) => Json(body).into_response(),
                    Err(error) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": error })),
                    )
                        .into_response(),
                };
            }
            state.manager.tools().await
        }
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

/// Each specialist role's readiness: state, worker, model, skill, tools and
/// reason codes. An ordinary user sees only their own profile's, as an
/// untrusted run of it would have it; nothing names another profile.
#[derive(Deserialize)]
struct AssignWorkerBody {
    #[serde(default)]
    worker: Option<String>,
    #[serde(default)]
    profile: Option<String>,
}

/// `lightagent workers assign <role>`: to the profile `worker`, or the
/// built-in worker; for the active profile or the one named. Operator only.
async fn assign_worker(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(role): Path<String>,
    Json(body): Json<AssignWorkerBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let request = ops::WorkerRequest::Assign {
        role,
        worker: body.worker,
        profile: body.profile,
    };
    match state.manager.workers_change(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `lightagent workers revoke <role>`. Operator only.
async fn revoke_worker(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(role): Path<String>,
    Query(query): Query<ProfileQuery>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let request = ops::WorkerRequest::Revoke {
        role,
        profile: query.profile,
    };
    match state.manager.workers_change(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn list_workers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ProfileQuery>,
) -> Response {
    let (profile, trust) =
        match access(&state, &headers, Scope::ToolsRead, query.profile.as_deref()) {
            Ok(Access::Unscoped) => {
                let profile = match query.profile.as_deref().map(ProfileId::new) {
                    Some(Ok(profile)) => Some(profile),
                    Some(Err(_)) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": "invalid profile id" })),
                        )
                            .into_response();
                    }
                    None => None,
                };
                (profile, RunTrust::LocalOperator)
            }
            Ok(Access::User(user)) => (Some(user.profile.clone()), RunTrust::RemoteUser),
            Err(response) => return *response,
        };
    match state.manager.workers_for(profile.as_ref(), trust).await {
        Ok(workers) => Json(json!({ "workers": workers })).into_response(),
        Err(_) if trust == RunTrust::RemoteUser => operator_only(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error })),
        )
            .into_response(),
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
    let profile_dir = profiles.handle(&profile_id).dir().to_path_buf();
    // What a run loads, in its order: the global skills, the active
    // extensions' skills, then the profile's own; a later one overrides.
    let extensions_config = config_store
        .load()
        .map(|config| config.extensions)
        .unwrap_or_default();
    let extensions = lightagent_extensions::ExtensionStore::load(
        &lightagent_extensions::extension_dirs(root, &profile_dir),
    );
    let mut layers = vec![(json!({ "scope": "global" }), root.join("skills"))];
    for ext in extensions.active(&extensions_config) {
        layers.push((
            json!({ "scope": "extension", "extension": ext.name }),
            ext.skills_dir(),
        ));
    }
    layers.push((json!({ "scope": "profile" }), profile_dir.join("skills")));
    let mut found = std::collections::BTreeMap::new();
    for (source, dir) in layers {
        let store = SkillStore::load(&[dir]);
        for name in store.names() {
            if let Some(skill) = store.get(&name) {
                found.insert(
                    name,
                    json!({
                        "name": skill.name, "description": skill.description,
                        "version": skill.version, "source": source,
                    }),
                );
            }
        }
    }
    let skills: Vec<_> = found.into_values().collect();
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
    /// The config revision these values were read at; responses only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    /// On a save: refused with 409 unless the config is still at this
    /// revision. Absent, the save applies to the config as it is now.
    #[serde(default, skip_serializing)]
    expected_revision: Option<String>,
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
        revision: None,
        expected_revision: None,
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
    let (config, revision) = match store.load_with_revision() {
        Ok(loaded) => loaded,
        Err(error) => return internal(&error.to_string()),
    };
    let mut settings = ui_settings(&config);
    settings.revision = Some(revision.as_str().to_owned());
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
    let profile = match active_profile(store, &state.session_profile) {
        Ok(profile) => profile,
        Err(error) => return internal(&error),
    };
    let expected = settings
        .expected_revision
        .as_deref()
        .map(ConfigRevision::parse);
    // Kept for the active profile, which is written after the config.
    let (approval_policy, max_turns, max_tool_calls, wall_clock_secs) = (
        settings.approval_policy,
        settings.max_turns,
        settings.max_tool_calls,
        settings.wall_clock_secs,
    );
    // One locked read-modify-write: the values apply to the config as it is
    // now, so a concurrent change to any other key is kept.
    let updated = store.update(expected.as_ref(), |config| {
        apply_ui_settings(config, store, settings)
    });
    let revision = match updated {
        Ok(((), revision)) => revision,
        Err(UpdateError::Change(error)) => return internal(&error),
        Err(UpdateError::Store(error)) => return config_error(error),
    };
    if let Some((profiles, mut profile)) = profile {
        profile.approval_policy = approval_policy;
        profile.limits.max_turns = max_turns;
        profile.limits.max_tool_calls = max_tool_calls;
        profile.limits.wall_clock_secs = wall_clock_secs;
        if let Err(error) = profiles.save(&profile) {
            return internal(&error.to_string());
        }
    }
    let config = match store.load() {
        Ok(config) => config,
        Err(error) => return internal(&error.to_string()),
    };
    let mut saved = ui_settings(&config);
    saved.revision = Some(revision.as_str().to_owned());
    Json(saved).into_response()
}

/// Apply the Settings screen's values to `config`.
fn apply_ui_settings(
    config: &mut lightagent_core::Config,
    store: &ConfigStore,
    settings: UiSettings,
) -> Result<(), String> {
    config.agent.max_turns = settings.max_turns;
    config.agent.max_tool_calls = settings.max_tool_calls;
    config.agent.wall_clock_secs = settings.wall_clock_secs;
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
    apply_jev_api_key(config, store, settings.jev.api_key)?;
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
    Ok(())
}

/// A key write, as `lightagent config set <key> <value>` takes it.
#[derive(Deserialize)]
struct ConfigWrite {
    value: String,
    /// When set, refused with 409 unless the config is still at it.
    #[serde(default)]
    expected_revision: Option<String>,
}

/// `lightagent config path` and `show`: where the config lives, its
/// revision, and its content with secrets redacted.
/// The active endpoint's provider API key as status only: whether one is
/// configured and usable, and where it comes from, never the key itself or
/// the file that holds it.
fn credential_view(
    config: &lightagent_core::Config,
    revision: &ConfigRevision,
) -> serde_json::Value {
    let source = config
        .inference
        .api_key
        .as_ref()
        .map(|secret| match secret {
            SecretRef::Env { .. } => "env",
            SecretRef::File { .. } => "file",
        });
    json!({
        "provider_key_id": lightagent_core::provider_keys::active_key_id(config),
        "base_url": config.inference.base_url,
        "state": lightagent_core::secret_state(config.inference.api_key.as_ref()),
        "source": source,
        "revision": revision.as_str(),
    })
}

/// How long an endpoint check may take, in seconds.
#[derive(Deserialize)]
struct CheckBody {
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// An explicit check of the active provider endpoint (`lightagent models`):
/// it sends the configured key to that endpoint only, follows no redirect,
/// and ends within `timeout_secs` (1-30, default 10).
async fn post_provider_check(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CheckBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let seconds = body.timeout_secs.unwrap_or(10).clamp(1, 30);
    match state
        .manager
        .check_provider(std::time::Duration::from_secs(seconds))
        .await
    {
        Ok(check) => Json(check).into_response(),
        Err(error) => unavailable(&error),
    }
}

/// A provider key typed into Settings. Write-only: no `Debug`, and never
/// returned or logged.
#[derive(Deserialize)]
struct CredentialBody {
    key: String,
    #[serde(default)]
    expected_revision: Option<String>,
}

async fn get_provider_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => Json(credential_view(&config, &revision)).into_response(),
        Err(error) => config_error(error),
    }
}

/// Save or replace the active endpoint's key in the shared secrets store
/// and point the config at it, as `lightagent setup provider` does. With
/// `expected_revision`, a stale save gets 409 and writes nothing.
async fn put_provider_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CredentialBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (Some(store), Some(root)) = (&state.config_store, home(&state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    if let Err(message) = lightagent_core::provider_keys::validate_key(&body.key) {
        return bad_request(message);
    }
    let paths = LightagentPaths::rooted_at(root);
    let expected = body.expected_revision.as_deref().map(ConfigRevision::parse);
    // The key is written inside the config lock, after the revision check, so
    // a refused save changes nothing.
    let saved = store.update(expected.as_ref(), |config| {
        let provider_id = lightagent_core::provider_keys::active_key_id(config);
        let secret = lightagent_core::provider_keys::store_key(&paths, &provider_id, &body.key)?;
        lightagent_core::provider_keys::use_key(config, Some(secret));
        Ok::<(), String>(())
    });
    match saved {
        Ok(((), revision)) => match store.load() {
            Ok(config) => Json(credential_view(&config, &revision)).into_response(),
            Err(error) => config_error(error),
        },
        Err(UpdateError::Change(message)) => internal(&message),
        Err(UpdateError::Store(error)) => config_error(error),
    }
}

/// Remove the active endpoint's key: the config stops referring to it, and
/// the stored file is deleted when it is the shared store's own.
async fn delete_provider_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (Some(store), Some(root)) = (&state.config_store, home(&state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let paths = LightagentPaths::rooted_at(root);
    let removed = store.update(None, |config| {
        let provider_id = lightagent_core::provider_keys::active_key_id(config);
        let ours = lightagent_core::provider_keys::stored_reference(&paths, &provider_id);
        let previous = config.inference.api_key.clone();
        lightagent_core::provider_keys::use_key(config, None);
        Ok::<_, std::convert::Infallible>((previous == Some(ours)).then_some(provider_id))
    });
    match removed {
        Ok((stored, revision)) => {
            if let Some(provider_id) = stored
                && let Err(message) =
                    lightagent_core::provider_keys::remove_key(&paths, &provider_id)
            {
                return internal(&message);
            }
            match store.load() {
                Ok(config) => Json(credential_view(&config, &revision)).into_response(),
                Err(error) => config_error(error),
            }
        }
        Err(error) => config_error(error.into_store()),
    }
}

/// A new saved provider, as `lightagent setup provider` creates one.
#[derive(Deserialize)]
struct NewProvider {
    name: String,
    base_url: String,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// An edit of a saved provider; an absent field stays as it is.
#[derive(Deserialize)]
struct ProviderEditBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    /// Keep the key when the endpoint changes; otherwise it is released.
    #[serde(default)]
    keep_key: bool,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// The provider to make active, and its model (`null`: automatic).
#[derive(Deserialize)]
struct SelectProvider {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// The model to use on the active endpoint (`null`: automatic).
#[derive(Deserialize)]
struct SelectModel {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
struct RevisionQuery {
    #[serde(default)]
    expected_revision: Option<String>,
}

/// Every provider the picker offers, the active one and its model, the
/// revision a change must name, and when a change applies. Never a key.
fn providers_view(
    config: &lightagent_core::Config,
    revision: &ConfigRevision,
) -> serde_json::Value {
    let providers = saved_providers::list(config);
    let active = providers
        .iter()
        .find(|provider| provider.active)
        .map(|provider| provider.id.clone());
    json!({
        "providers": providers,
        "active": active,
        "model": config.inference.model,
        "revision": revision.as_str(),
        "effect": saved_providers::EFFECT,
    })
}

/// A refused provider change: invalid input (400), no such provider (404),
/// a name or endpoint another saved provider has (409, reason
/// `duplicate`), or a key file that could not be moved (500).
fn provider_error(error: saved_providers::ProviderError) -> Response {
    use saved_providers::ProviderError;
    match &error {
        ProviderError::Invalid(message) => bad_request(message),
        ProviderError::NotFound(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
        ProviderError::Duplicate(message) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": message, "reason": "duplicate" })),
        )
            .into_response(),
        ProviderError::Store(message) => internal(message),
    }
}

/// Apply a provider change in one config update, finish what waits for the
/// save (a released key file, the active profile), and answer with the
/// providers at the new revision. A stale `expected_revision` gets 409 and
/// changes nothing.
fn change_providers(
    state: &AppState,
    expected: Option<&str>,
    status: StatusCode,
    change: impl FnOnce(
        &mut lightagent_core::Config,
        &LightagentPaths,
    ) -> Result<saved_providers::Outcome, saved_providers::ProviderError>,
) -> Response {
    let (Some(store), Some(root)) = (&state.config_store, home(state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let paths = LightagentPaths::rooted_at(root);
    let expected = expected.map(ConfigRevision::parse);
    match store.update(expected.as_ref(), |config| change(config, &paths)) {
        Ok((outcome, revision)) => {
            if let Err(message) = saved_providers::finish(&paths, &outcome) {
                return internal(&message);
            }
            match store.load() {
                Ok(config) => (status, Json(providers_view(&config, &revision))).into_response(),
                Err(error) => config_error(error),
            }
        }
        Err(UpdateError::Change(error)) => provider_error(error),
        Err(UpdateError::Store(error)) => config_error(error),
    }
}

/// The providers `lightagent setup provider` offers.
async fn list_providers(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => Json(providers_view(&config, &revision)).into_response(),
        Err(error) => config_error(error),
    }
}

async fn show_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => match saved_providers::get(&config, &id) {
            Ok(provider) => Json(json!({
                "provider": provider,
                "revision": revision.as_str(),
                "effect": saved_providers::EFFECT,
            }))
            .into_response(),
            Err(error) => provider_error(error),
        },
        Err(error) => config_error(error),
    }
}

async fn create_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<NewProvider>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    change_providers(
        &state,
        body.expected_revision.as_deref(),
        StatusCode::CREATED,
        |config, _| {
            saved_providers::create(config, &body.name, &body.base_url)
                .map(|_| saved_providers::Outcome::default())
        },
    )
}

async fn update_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProviderEditBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let edit = saved_providers::ProviderEdit {
        name: body.name,
        base_url: body.base_url,
        keep_key: body.keep_key,
    };
    change_providers(
        &state,
        body.expected_revision.as_deref(),
        StatusCode::OK,
        |config, paths| {
            saved_providers::update(config, paths, &id, &edit).map(|(_, outcome)| outcome)
        },
    )
}

async fn remove_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    change_providers(
        &state,
        query.expected_revision.as_deref(),
        StatusCode::OK,
        |config, paths| saved_providers::remove(config, paths, &id),
    )
}

async fn select_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SelectProvider>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    change_providers(
        &state,
        body.expected_revision.as_deref(),
        StatusCode::OK,
        |config, _| {
            saved_providers::select(config, &body.id, body.model.as_deref())
                .map(|(_, outcome)| outcome)
        },
    )
}

async fn select_provider_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SelectModel>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    change_providers(
        &state,
        body.expected_revision.as_deref(),
        StatusCode::OK,
        |config, _| saved_providers::select_model(config, body.model.as_deref()),
    )
}

/// An explicit check of one provider's endpoint, with that endpoint's own
/// key only, ending within `timeout_secs` (1-30, default 10).
async fn check_saved_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CheckBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let base_url = match store.load() {
        Ok(config) => match saved_providers::get(&config, &id) {
            Ok(provider) => provider.base_url,
            Err(error) => return provider_error(error),
        },
        Err(error) => return config_error(error),
    };
    let seconds = body.timeout_secs.unwrap_or(10).clamp(1, 30);
    match state
        .manager
        .check_endpoint(base_url, std::time::Duration::from_secs(seconds))
        .await
    {
        Ok(check) => Json(check).into_response(),
        Err(error) => unavailable(&error),
    }
}

/// A workspace choice: an absolute folder, or `null` for the active
/// profile's own `workspace/`.
#[derive(Deserialize)]
struct WorkspaceBody {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// The workspace for the next run of the active profile, the revision a
/// change must name, and when a change applies.
fn workspace_view(
    config: &lightagent_core::Config,
    revision: &ConfigRevision,
    root: &std::path::Path,
) -> serde_json::Value {
    json!({
        "workspace": lightagent_core::workspaces::view(config, &LightagentPaths::rooted_at(root)),
        "revision": revision.as_str(),
        "effect": lightagent_core::workspaces::EFFECT,
    })
}

/// The workspace folder `lightagent setup tools` sets.
async fn get_workspace(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (Some(store), Some(root)) = (&state.config_store, home(&state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => Json(workspace_view(&config, &revision, &root)).into_response(),
        Err(error) => config_error(error),
    }
}

/// Set or clear the workspace folder as setup does, on the revision the
/// caller read: a stale one gets 409 and writes nothing; a folder that is
/// not absolute, is a file or has no parent folder gets 400.
async fn put_workspace(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<WorkspaceBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (Some(store), Some(root)) = (&state.config_store, home(&state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let expected = body.expected_revision.as_deref().map(ConfigRevision::parse);
    match store.update(expected.as_ref(), |config| {
        lightagent_core::workspaces::set(config, body.path.as_deref())
    }) {
        Ok(((), revision)) => match store.load() {
            Ok(config) => Json(workspace_view(&config, &revision, &root)).into_response(),
            Err(error) => config_error(error),
        },
        Err(UpdateError::Change(message)) => bad_request(&message),
        Err(UpdateError::Store(error)) => config_error(error),
    }
}

/// The `mcp` section with every value withheld, the revision a change must
/// name, and when a change applies.
fn mcp_view(config: &lightagent_core::Config, revision: &ConfigRevision) -> serde_json::Value {
    json!({
        "mcp": lightagent_core::mcp_servers::view(config),
        "revision": revision.as_str(),
        "effect": lightagent_core::mcp_servers::EFFECT,
    })
}

/// A change the shared MCP operations refused, by kind.
fn mcp_refusal(error: lightagent_core::mcp_servers::McpConfigError) -> Response {
    use lightagent_core::mcp_servers::McpConfigError;
    let status = match &error {
        McpConfigError::NotFound(_) => StatusCode::NOT_FOUND,
        McpConfigError::Invalid(_) => StatusCode::BAD_REQUEST,
        McpConfigError::Conflict(_) => StatusCode::CONFLICT,
    };
    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

/// One MCP change, on the revision the caller read when it names one: a
/// stale revision gets 409 and writes nothing, and so does a refused change.
fn mcp_change(
    state: &AppState,
    expected: Option<&str>,
    created: bool,
    change: impl FnOnce(
        &mut lightagent_core::Config,
    ) -> Result<(), lightagent_core::mcp_servers::McpConfigError>,
) -> Response {
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let expected = expected.map(ConfigRevision::parse);
    match store.update(expected.as_ref(), change) {
        Ok(((), revision)) => match store.load() {
            Ok(config) => {
                let status = if created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                };
                (status, Json(mcp_view(&config, &revision))).into_response()
            }
            Err(error) => config_error(error),
        },
        Err(UpdateError::Change(error)) => mcp_refusal(error),
        Err(UpdateError::Store(error)) => config_error(error),
    }
}

/// The MCP servers, switch and timeout, every value withheld.
async fn get_mcp(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => Json(mcp_view(&config, &revision)).into_response(),
        Err(error) => config_error(error),
    }
}

#[derive(Deserialize)]
struct McpSettingsBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// Switch MCP on or off and set its timeout.
async fn put_mcp(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<McpSettingsBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (enabled, timeout_secs) = (body.enabled, body.timeout_secs);
    mcp_change(&state, body.expected_revision.as_deref(), false, |config| {
        lightagent_core::mcp_servers::set(config, enabled, timeout_secs)
    })
}

#[derive(Deserialize)]
struct McpServerBody {
    server: lightagent_core::mcp_servers::ServerInput,
    #[serde(default)]
    expected_revision: Option<String>,
}

/// Add a configured MCP server.
async fn add_mcp_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<McpServerBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let McpServerBody {
        server,
        expected_revision,
    } = body;
    mcp_change(&state, expected_revision.as_deref(), true, move |config| {
        lightagent_core::mcp_servers::add(config, server)
    })
}

/// Edit a configured MCP server; a withheld value left `null` is kept.
async fn update_mcp_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<McpServerBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let McpServerBody {
        server,
        expected_revision,
    } = body;
    mcp_change(&state, expected_revision.as_deref(), false, move |config| {
        lightagent_core::mcp_servers::update(config, &name, server)
    })
}

/// Remove a configured MCP server.
async fn remove_mcp_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    mcp_change(&state, None, false, |config| {
        lightagent_core::mcp_servers::remove(config, &name)
    })
}

/// Check one MCP server on request: it is connected exactly as an operator
/// run connects it (a stdio server's command is started), its tools are
/// listed, and it is disconnected. No setting changes.
async fn check_mcp_server(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    match state.manager.check_mcp(name.clone()).await {
        Ok(Some(check)) => Json(check).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("no MCP server named {name:?} is configured or declared by an active extension")
            })),
        )
            .into_response(),
        Err(error) => internal(&error),
    }
}

/// A knowledge (RAG) operation through the embedder (`lightagent rag …`),
/// on the active profile's index. Operator only.
async fn rag_operation(
    state: &AppState,
    headers: &HeaderMap,
    request: ops::RagRequest,
) -> Response {
    if let Some(rejection) = deny(state, headers, Scope::Admin) {
        return rejection;
    }
    match state.manager.rag(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn list_knowledge(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    rag_operation(&state, &headers, ops::RagRequest::List).await
}

#[derive(Deserialize)]
struct AddKnowledgeBody {
    path: String,
    #[serde(default)]
    source: Option<String>,
}

/// Index a file, or every file in a folder. The server's working folder
/// means nothing to a client, so the path must be absolute.
async fn add_knowledge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AddKnowledgeBody>,
) -> Response {
    if !std::path::Path::new(&body.path).is_absolute() {
        if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
            return rejection;
        }
        return bad_request("the path must be an absolute file or folder path");
    }
    let request = ops::RagRequest::Add {
        path: body.path,
        source: body.source,
    };
    rag_operation(&state, &headers, request).await
}

async fn clear_knowledge(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    rag_operation(&state, &headers, ops::RagRequest::Clear).await
}

#[derive(Deserialize)]
struct KnowledgeSearchQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
}

async fn search_knowledge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<KnowledgeSearchQuery>,
) -> Response {
    let request = ops::RagRequest::Search {
        query: query.q,
        top_k: query.top_k,
    };
    rag_operation(&state, &headers, request).await
}

/// A memory operation through the embedder (`lightagent memory …`, see
/// [`ops::MemoryRequest`]), on the active profile's memories. Operator only.
async fn memory_operation(
    state: &AppState,
    headers: &HeaderMap,
    request: ops::MemoryRequest,
) -> Response {
    if let Some(rejection) = deny(state, headers, Scope::Admin) {
        return rejection;
    }
    match state.manager.memory(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn list_memories(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    memory_operation(&state, &headers, ops::MemoryRequest::List).await
}

#[derive(Deserialize)]
struct AddMemoryBody {
    text: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

async fn add_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AddMemoryBody>,
) -> Response {
    let request = ops::MemoryRequest::Add {
        text: body.text,
        kind: body.kind,
        tags: body.tags,
    };
    memory_operation(&state, &headers, request).await
}

async fn clear_memories(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    memory_operation(&state, &headers, ops::MemoryRequest::Clear).await
}

#[derive(Deserialize)]
struct UpdateMemoryBody {
    text: String,
}

async fn update_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateMemoryBody>,
) -> Response {
    let request = ops::MemoryRequest::Update {
        id,
        text: body.text,
    };
    memory_operation(&state, &headers, request).await
}

async fn forget_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    memory_operation(&state, &headers, ops::MemoryRequest::Forget(id)).await
}

#[derive(Deserialize)]
struct MemorySearchQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
}

async fn search_memories(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<MemorySearchQuery>,
) -> Response {
    let request = ops::MemoryRequest::Search {
        query: query.q,
        top_k: query.top_k,
    };
    memory_operation(&state, &headers, request).await
}

#[derive(Deserialize)]
struct ReflectQuery {
    #[serde(default)]
    topic: Option<String>,
}

async fn reflect_memories(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ReflectQuery>,
) -> Response {
    memory_operation(&state, &headers, ops::MemoryRequest::Reflect(query.topic)).await
}

#[derive(Deserialize)]
struct CandidatesQuery {
    session: String,
}

async fn memory_candidates(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CandidatesQuery>,
) -> Response {
    memory_operation(
        &state,
        &headers,
        ops::MemoryRequest::Candidates(query.session),
    )
    .await
}

#[derive(Deserialize)]
struct PromoteMemoryBody {
    session: String,
    message: usize,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

async fn promote_memory(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<PromoteMemoryBody>,
) -> Response {
    let request = ops::MemoryRequest::Promote {
        session: body.session,
        message: body.message,
        text: body.text,
        kind: body.kind,
        tags: body.tags,
    };
    memory_operation(&state, &headers, request).await
}

/// Identity administration belongs to the local operator (decision C4):
/// the desktop host's own runtime, or a server on an open loopback bind. A
/// keyed server may be reached from elsewhere, and multi-user mode serves
/// users, so both refuse whatever the caller holds.
fn local_operator(state: &AppState, headers: &HeaderMap, admitted: bool) -> Option<Response> {
    if let Some(rejection) = deny(state, headers, Scope::Admin) {
        return Some(rejection);
    }
    let local = state.auth.identity().is_none() && (admitted || !state.auth.requires_key());
    (!local).then(|| {
        ops::OpError::new(
            ops::OpCategory::Denied,
            "identity is managed only by the local operator: use `lightagent identity`, or the desktop app",
        )
        .into_response()
    })
}

/// Present when the desktop guard admitted the request.
type FromDesktop = Option<axum::Extension<desktop::Admitted>>;

/// A `lightagent identity …` command through the embedder (see
/// [`ops::IdentityRequest`]), for the local operator only. No answer
/// carries a credential's secret.
async fn identity_operation(
    state: &AppState,
    headers: &HeaderMap,
    admitted: FromDesktop,
    request: ops::IdentityRequest,
) -> Response {
    if let Some(rejection) = local_operator(state, headers, admitted.is_some()) {
        return rejection;
    }
    match state.manager.identity(request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn identity_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
) -> Response {
    identity_operation(&state, &headers, admitted, ops::IdentityRequest::Status).await
}

async fn list_identity_users(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
) -> Response {
    identity_operation(&state, &headers, admitted, ops::IdentityRequest::UserList).await
}

#[derive(Deserialize)]
struct IdentityUserBody {
    id: String,
}

async fn create_identity_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Json(body): Json<IdentityUserBody>,
) -> Response {
    let request = ops::IdentityRequest::UserCreate(body.id);
    identity_operation(&state, &headers, admitted, request).await
}

async fn suspend_identity_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(id): Path<String>,
) -> Response {
    let request = ops::IdentityRequest::UserSuspend(id);
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct PrincipalQuery {
    #[serde(default)]
    principal: Option<String>,
}

async fn list_identity_credentials(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Query(query): Query<PrincipalQuery>,
) -> Response {
    let request = ops::IdentityRequest::CredentialList {
        principal: query.principal,
    };
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct IssueCredentialBody {
    principal: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    secret_file: Option<String>,
}

async fn issue_identity_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Json(body): Json<IssueCredentialBody>,
) -> Response {
    let request = ops::IdentityRequest::CredentialIssue {
        principal: body.principal,
        expires_in: body.expires_in,
        capabilities: body.capabilities,
        profiles: body.profiles,
        secret_file: body.secret_file,
    };
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct RotateCredentialBody {
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    secret_file: Option<String>,
}

async fn rotate_identity_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(key_id): Path<String>,
    Json(body): Json<RotateCredentialBody>,
) -> Response {
    let request = ops::IdentityRequest::CredentialRotate {
        key_id,
        expires_in: body.expires_in,
        secret_file: body.secret_file,
    };
    identity_operation(&state, &headers, admitted, request).await
}

async fn revoke_identity_credential(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(key_id): Path<String>,
) -> Response {
    let request = ops::IdentityRequest::CredentialRevoke(key_id);
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct OwnerBody {
    principal: String,
}

async fn assign_identity_owner(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(profile): Path<String>,
    Json(body): Json<OwnerBody>,
) -> Response {
    let request = ops::IdentityRequest::OwnerAssign {
        profile,
        principal: body.principal,
    };
    identity_operation(&state, &headers, admitted, request).await
}

async fn reassign_identity_owner(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(profile): Path<String>,
    Json(body): Json<OwnerBody>,
) -> Response {
    let request = ops::IdentityRequest::OwnerReassign {
        profile,
        principal: body.principal,
    };
    identity_operation(&state, &headers, admitted, request).await
}

async fn remove_identity_owner(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Path(profile): Path<String>,
) -> Response {
    let request = ops::IdentityRequest::OwnerRemove(profile);
    identity_operation(&state, &headers, admitted, request).await
}

async fn list_identity_grants(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Query(query): Query<ProfileQuery>,
) -> Response {
    let request = ops::IdentityRequest::GrantsList {
        profile: query.profile,
    };
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct RevokeGrantBody {
    profile: String,
    principal: String,
    capability: String,
}

async fn revoke_identity_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Json(body): Json<RevokeGrantBody>,
) -> Response {
    let request = ops::IdentityRequest::GrantsRevoke {
        profile: body.profile,
        principal: body.principal,
        capability: body.capability,
    };
    identity_operation(&state, &headers, admitted, request).await
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn identity_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    admitted: FromDesktop,
    Query(query): Query<AuditQuery>,
) -> Response {
    let request = ops::IdentityRequest::Audit { limit: query.limit };
    identity_operation(&state, &headers, admitted, request).await
}

/// An import of skills from a repository or folder (new scope).
#[derive(Deserialize)]
struct ImportSkillsBody {
    source: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scope: Option<extension_ops::Scope>,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    activate: bool,
}

/// Import skills (see `lightagent_extensions::skill_import`): installed
/// inactive unless `activate`, never with a tool permission.
async fn import_skills(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ImportSkillsBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let paths = match extension_paths(&state) {
        Ok(paths) => paths,
        Err(response) => return *response,
    };
    let remote = body.source.starts_with("https://") || body.source.starts_with("git+https://");
    if !remote && !std::path::Path::new(&body.source).is_absolute() {
        return bad_request("the source must be an absolute folder path or an https:// Git URL");
    }
    let options = lightagent_extensions::skill_import::ImportOptions {
        scope: body.scope.unwrap_or(extension_ops::Scope::Global),
        name: body.name,
        revision: body.revision,
        replace: body.replace,
        activate: body.activate,
    };
    let source = PathBuf::from(body.source);
    match tokio::task::spawn_blocking(move || {
        lightagent_extensions::skill_import::import(&paths, &source, options)
    })
    .await
    {
        Ok(Ok(imported)) => (StatusCode::CREATED, Json(imported)).into_response(),
        Ok(Err(error)) => extension_error(error),
        Err(error) => internal(&error.to_string()),
    }
}

/// Where the extension operations act: this runtime's Lightagent home.
fn extension_paths(state: &AppState) -> Result<LightagentPaths, Box<Response>> {
    home(state).map(LightagentPaths::rooted_at).ok_or_else(|| {
        Box::new(internal(
            "Lightagent settings are unavailable in this embedding",
        ))
    })
}

fn extension_config(state: &AppState) -> Result<lightagent_core::Config, Box<Response>> {
    match &state.config_store {
        Some(store) => store.load().map_err(|error| Box::new(config_error(error))),
        None => Err(Box::new(internal(
            "Lightagent settings are unavailable in this embedding",
        ))),
    }
}

/// A refusal from the shared extension operations, by kind.
fn extension_error(error: extension_ops::ManageError) -> Response {
    let status = match &error {
        extension_ops::ManageError::NotFound(_) => StatusCode::NOT_FOUND,
        extension_ops::ManageError::Invalid(_) => StatusCode::BAD_REQUEST,
        extension_ops::ManageError::Conflict(_) => StatusCode::CONFLICT,
        extension_ops::ManageError::Unavailable(_) => StatusCode::BAD_GATEWAY,
        extension_ops::ManageError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

/// `lightagent extensions list`, from the same shared operation.
async fn list_extensions(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (paths, config) = match (extension_paths(&state), extension_config(&state)) {
        (Ok(paths), Ok(config)) => (paths, config),
        (Err(response), _) | (_, Err(response)) => return *response,
    };
    match extension_ops::list(&paths, &config) {
        Ok(listing) => Json(listing).into_response(),
        Err(error) => extension_error(error),
    }
}

/// `lightagent extensions show <name>`: the manifest, what its MCP servers
/// would run (never an environment or header value) and its source.
async fn show_extension(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (paths, config) = match (extension_paths(&state), extension_config(&state)) {
        (Ok(paths), Ok(config)) => (paths, config),
        (Err(response), _) | (_, Err(response)) => return *response,
    };
    match extension_ops::show(&paths, &config, &name) {
        Ok(detail) => Json(detail).into_response(),
        Err(error) => extension_error(error),
    }
}

#[derive(Deserialize)]
struct ExtensionEnabledBody {
    enabled: bool,
}

/// `lightagent extensions enable|disable <name>`.
async fn set_extension_enabled(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<ExtensionEnabledBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let paths = match extension_paths(&state) {
        Ok(paths) => paths,
        Err(response) => return *response,
    };
    match extension_ops::set_enabled(&paths, &name, body.enabled) {
        Ok(toggled) => Json(toggled).into_response(),
        Err(error) => extension_error(error),
    }
}

/// An install: an absolute folder or an https:// Git URL (`#subdir` picks a
/// folder in it), where to install it, and whether MCP may be switched on
/// for the extension's servers. It never is unless asked.
#[derive(Deserialize)]
struct InstallExtensionBody {
    source: String,
    #[serde(default)]
    scope: Option<extension_ops::Scope>,
    #[serde(default)]
    activate_mcp: bool,
}

/// `lightagent extensions install`, from the same shared operation.
async fn install_extension(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<InstallExtensionBody>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let paths = match extension_paths(&state) {
        Ok(paths) => paths,
        Err(response) => return *response,
    };
    // The server's working folder means nothing to a client, so a relative
    // path is refused rather than resolved against it.
    let remote = body.source.starts_with("https://") || body.source.starts_with("git+https://");
    if !remote && !std::path::Path::new(&body.source).is_absolute() {
        return bad_request("the source must be an absolute folder path or an https:// Git URL");
    }
    let options = extension_ops::InstallOptions {
        scope: body.scope.unwrap_or(extension_ops::Scope::Global),
        activate_mcp: body.activate_mcp,
    };
    let source = PathBuf::from(body.source);
    match tokio::task::spawn_blocking(move || extension_ops::install(&paths, &source, options))
        .await
    {
        Ok(Ok(installed)) => (StatusCode::CREATED, Json(installed)).into_response(),
        Ok(Err(error)) => extension_error(error),
        Err(error) => internal(&error.to_string()),
    }
}

#[derive(Deserialize)]
struct ExtensionScopeQuery {
    #[serde(default)]
    scope: Option<extension_ops::Scope>,
}

/// `lightagent extensions uninstall <name>` (`?scope=profile` for the active
/// profile's own root).
async fn uninstall_extension(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<ExtensionScopeQuery>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let paths = match extension_paths(&state) {
        Ok(paths) => paths,
        Err(response) => return *response,
    };
    let scope = query.scope.unwrap_or(extension_ops::Scope::Global);
    match tokio::task::spawn_blocking(move || extension_ops::uninstall(&paths, &name, scope)).await
    {
        Ok(Ok(removed)) => Json(removed).into_response(),
        Ok(Err(error)) => extension_error(error),
        Err(error) => internal(&error.to_string()),
    }
}

/// `lightagent config jev-routing-report`: the same dry run, from the same
/// shared operation. It reads the config and the profile list and changes
/// nothing.
async fn get_jev_routing_report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let (Some(store), Some(root)) = (&state.config_store, home(&state)) else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let config = match store.load() {
        Ok(config) => config,
        Err(error) => return config_error(error),
    };
    match ProfileStore::new(&root).list() {
        Ok(profiles) => Json(lightagent_core::config_keys::jev_routing_report(
            &config, &profiles,
        ))
        .into_response(),
        Err(error) => internal(&error.to_string()),
    }
}

async fn get_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => Json(json!({
            "path": store.path().display().to_string(),
            "revision": revision.as_str(),
            "config": config.redacted_json(),
        }))
        .into_response(),
        Err(error) => config_error(error),
    }
}

/// `lightagent config get <key>`: the same keys, the same redaction.
async fn get_config_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    match store.load_with_revision() {
        Ok((config, revision)) => match lightagent_core::config_keys::get_key(&config, &key) {
            Some(value) => {
                Json(json!({ "key": key, "value": value, "revision": revision.as_str() }))
                    .into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("unknown or unreadable config key '{key}'") })),
            )
                .into_response(),
        },
        Err(error) => config_error(error),
    }
}

/// `lightagent config set <key> <value>`, the same locked write. With
/// `expected_revision`, only when nothing changed since it was read.
async fn set_config_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(body): Json<ConfigWrite>,
) -> Response {
    if let Some(rejection) = deny(&state, &headers, Scope::Admin) {
        return rejection;
    }
    let Some(store) = &state.config_store else {
        return internal("Lightagent settings are unavailable in this embedding");
    };
    let expected = body.expected_revision.as_deref().map(ConfigRevision::parse);
    let written = store.update(expected.as_ref(), |config| {
        lightagent_core::config_keys::set_key(config, &key, &body.value)?;
        Ok::<_, String>(lightagent_core::config_keys::get_key(config, &key).unwrap_or_default())
    });
    match written {
        Ok((value, revision)) => {
            Json(json!({ "key": key, "value": value, "revision": revision.as_str() }))
                .into_response()
        }
        Err(UpdateError::Change(message)) => bad_request(&message),
        Err(UpdateError::Store(error)) => config_error(error),
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
    #[serde(default)]
    on_time_budget: OnTimeBudget,
}

/// What a run does when its time budget runs out while tool results the
/// model has not read are waiting.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OnTimeBudget {
    /// Answer from what it gathered, without tools, and end (as before).
    #[default]
    WrapUp,
    /// Pause until continued (`POST /runs/{id}/continue`) or cancelled.
    Pause,
}

/// How to continue a run paused on its time budget.
#[derive(Deserialize)]
struct ContinueBody {
    #[serde(default)]
    mode: ContinueMode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContinueMode {
    /// Renew the time budget and carry on, with tools, where it stopped.
    #[default]
    Extend,
    /// Answer now from what it gathered, without tools, and end.
    Answer,
}

/// Regenerating names the run whose answer it replaces, so a request made
/// against a transcript that has since moved on is refused, not misapplied.
#[derive(Deserialize)]
struct RegenerateBody {
    run_id: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// How a session run treats the transcript it continues.
enum Turn {
    /// A new user prompt, appended to the session.
    Append,
    /// The prompt behind the session's latest answer, from run `run_id`, is
    /// run again; a finished new answer replaces that one.
    Regenerate { run_id: String },
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateRunBody>,
) -> Response {
    start_run(state, headers, body, Turn::Append).await
}

/// Run the prompt behind session `id`'s latest answer again, in place of that
/// answer. The new run is created, authorized and streamed exactly as any
/// session run is. The saved answer stays until the new run has finished
/// with an answer; only then are the two exchanged, in one save.
async fn regenerate_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RegenerateBody>,
) -> Response {
    // Replacing a saved answer changes the session, so session write access
    // is needed as well as the run access `start_run` checks, whoever asks.
    if let Err(response) = access(
        &state,
        &headers,
        Scope::SessionsWrite,
        body.profile.as_deref(),
    ) {
        return *response;
    }
    let run = CreateRunBody {
        // Taken from the saved session once it is locked.
        message: String::new(),
        profile: body.profile,
        session_id: Some(id),
        model: body.model,
        on_time_budget: OnTimeBudget::WrapUp,
    };
    let turn = Turn::Regenerate {
        run_id: body.run_id,
    };
    start_run(state, headers, run, turn).await
}

/// Remove the session's latest answer, and the record of the run that wrote
/// it, and return the user prompt it answered. `None`, with the session
/// untouched, unless run `run_id` is the session's latest finished run and the
/// transcript ends with a user prompt and that run's answer.
fn take_latest_answer(session: &mut Session, run_id: &str) -> Option<String> {
    let latest = session.runs.last()?;
    if latest.run_id != run_id || latest.stop_reason.is_none() {
        return None;
    }
    let [.., prompt, answer] = session.messages.as_slice() else {
        return None;
    };
    if prompt.role != "user" || answer.role != "assistant" {
        return None;
    }
    let prompt = prompt.content.clone();
    session.messages.pop();
    session.runs.pop();
    session.touch();
    Some(prompt)
}

/// Why a session run was not admitted.
enum Refused {
    /// The session is missing or could not be read.
    Missing(String),
    /// The session belongs to another profile than the run's.
    ProfileMismatch,
    /// A regeneration named an answer that is not the session's latest.
    NotLatest,
    /// The prompt could not be saved.
    Unsaved(String),
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Self::Missing(error) => not_found(&error),
            Self::ProfileMismatch => bad_request("session profile does not match"),
            Self::NotLatest => (
                StatusCode::CONFLICT,
                Json(json!({"error": "only the latest answer can be regenerated"})),
            )
                .into_response(),
            Self::Unsaved(error) => internal(&error),
        }
    }
}

/// Whether a store error means the session cannot be read (missing,
/// malformed or damaged) rather than that it could not be written.
fn is_missing(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::NotFound(_) | StoreError::Unreadable { .. } | StoreError::MalformedId(_)
    )
}

/// Admit a run of session `id` for the profile `expected` (and `named`, when
/// the caller named one): the history the model sees and the prompt to run.
/// A new prompt is appended, under the store's lock, to the session as it is
/// saved now. A regeneration only reads: nothing is saved until its run has
/// finished with an answer (`record_run_in_session`).
fn admit(
    store: &SessionStore,
    id: &SessionId,
    expected: &str,
    named: Option<&str>,
    message: &str,
    replacing: Option<&str>,
    history_limit: usize,
) -> Result<(Vec<lightagent_core::provider::ProviderMessage>, String), Refused> {
    let check = |session: &Session| {
        if session.profile != expected || named.is_some_and(|value| value != session.profile) {
            Err(Refused::ProfileMismatch)
        } else {
            Ok(())
        }
    };
    let Some(run_id) = replacing else {
        return store
            .update(id, |session| {
                check(&*session)?;
                let history = model_history(session, message, history_limit);
                if session.messages.is_empty() && session.title == "agent session" {
                    session.title = session_title(message);
                }
                session.push_message(StoredMessage::new("user", message));
                Ok((history, message.to_owned()))
            })
            .map(|(admitted, _)| admitted)
            .map_err(|error| match error {
                SessionUpdateError::Change(refused) => refused,
                SessionUpdateError::Store(error) if is_missing(&error) => {
                    Refused::Missing(error.to_string())
                }
                SessionUpdateError::Store(error) => Refused::Unsaved(error.to_string()),
            });
    };
    let mut earlier = store
        .load(id)
        .map_err(|error| Refused::Missing(error.to_string()))?;
    check(&earlier)?;
    let prompt = take_latest_answer(&mut earlier, run_id).ok_or(Refused::NotLatest)?;
    // The model sees what preceded the prompt, as it did on the prompt's
    // first run.
    earlier.messages.pop();
    Ok((model_history(&earlier, &prompt, history_limit), prompt))
}

async fn start_run(
    state: Arc<AppState>,
    headers: HeaderMap,
    mut body: CreateRunBody,
    turn: Turn,
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
    // The session's run lease, held from admission until the run is recorded.
    let mut run_lease = None;
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
        // Resolved before the session lock is taken: it may ask the provider.
        let history_limit = state
            .manager
            .history_limit(
                Some(&session_profile),
                body.model.as_deref(),
                state.context_limit,
            )
            .await;
        let mut busy = state.busy_sessions.lock().await;
        if !busy.insert(id.clone()) {
            drop(busy);
            // Named when this process runs it and the caller may see it (a
            // paused run a closed page left), so it can be continued or
            // stopped.
            let mut refusal = json!({"error": "session already has an active run"});
            if let Some(active) = session_active_run(&state, &headers, &session_store, &id).await {
                refusal["active_run"] = active;
            }
            return (StatusCode::CONFLICT, Json(refusal)).into_response();
        }
        // One turn at a time on this session across every process that
        // shares this home (another server, a terminal chat, an editor): its
        // run lease, refused exactly like a session busy here.
        let lease = match session_store.lease_run(&id) {
            Ok(lease) => lease,
            Err(StoreError::Busy { .. }) => {
                busy.remove(&id);
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": "session already has an active run"})),
                )
                    .into_response();
            }
            Err(error) => {
                busy.remove(&id);
                return internal(&error.to_string());
            }
        };
        // A turn of this session that a process ended before recording is
        // recorded as interrupted before this one is admitted. It is never
        // run again.
        if let Err(error) = session_store.recover_interrupted(&id, &lease) {
            eprintln!("could not record an interrupted run of session {id:?}: {error}");
        }
        // Checked, and for a new prompt saved, on the session as it is saved
        // now, under the store's lock: a change another writer made since is
        // kept. The busy entry above stays the rule for admitting runs.
        let admitted = {
            let store = session_store.clone();
            let id = id.clone();
            let expected = session_profile.clone();
            let named = profile.clone();
            let message = body.message.clone();
            let replacing = match &turn {
                Turn::Append => None,
                Turn::Regenerate { run_id } => Some(run_id.clone()),
            };
            tokio::task::spawn_blocking(move || {
                admit(
                    &store,
                    &id,
                    &expected,
                    named.as_deref(),
                    &message,
                    replacing.as_deref(),
                    history_limit,
                )
            })
            .await
        };
        match admitted {
            Ok(Ok((admitted, prompt))) => {
                history = admitted;
                body.message = prompt;
            }
            // The lease goes before the busy entry, so a session free here
            // is free in every process.
            Ok(Err(refused)) => {
                drop(lease);
                busy.remove(&id);
                return refused.into_response();
            }
            Err(error) => {
                drop(lease);
                busy.remove(&id);
                return internal(&error.to_string());
            }
        }
        profile = Some(session_profile.clone());
        run_lease = Some(lease);
        session_id = Some(id);
    }
    let options = RunOptions {
        pause_on_time_budget: body.on_time_budget == OnTimeBudget::Pause,
    };
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
                        .start_supervised_with(
                            request,
                            owner,
                            check,
                            identity.generation(),
                            options,
                        )
                        .await;
                    match started {
                        Ok(run) => run,
                        Err(denial) => {
                            drop(run_lease.take());
                            if let Some(id) = &session_id {
                                state.busy_sessions.lock().await.remove(id);
                            }
                            return refused(denial);
                        }
                    }
                }
                None => state.manager.start_as_with(request, owner, options).await,
            }
        }
        None => {
            state
                .manager
                .start_as_with(request, RunOwner::operator_only(), options)
                .await
        }
    };
    if let Some(id) = &session_id {
        let replacing = match turn {
            Turn::Append => None,
            Turn::Regenerate { run_id } => Some(run_id),
        };
        // If this process ends before the run is recorded, the next one to
        // take the session records it as interrupted, and never reruns it.
        if let Err(error) = session_store.journal_run(id, run.id(), replacing.is_some()) {
            eprintln!("could not journal the run of session {id:?}: {error}");
        }
        tokio::spawn(record_run_in_session(
            Arc::clone(&run),
            session_store.clone(),
            Arc::clone(&state.busy_sessions),
            id.clone(),
            replacing,
            run_lease,
        ));
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({ "id": run.id(), "status": run.status().await, "session_id": session_id })),
    )
        .into_response()
}

/// Once `run` is terminal, record its events into session `id` and release
/// the session for its next run.
///
/// A run `replacing` the answer that saved run wrote is recorded only when it
/// completed with an answer: then the old answer and its run record are
/// exchanged for the new ones in one save. A failed, cancelled or empty run
/// leaves the session exactly as it was, so the saved answer survives.
async fn record_run_in_session(
    run: Arc<RunState>,
    sessions: SessionStore,
    busy: Arc<Mutex<HashSet<SessionId>>>,
    id: SessionId,
    replacing: Option<String>,
    // Released just before the busy entry, once the run is recorded.
    mut lease: Option<lightagent_store::RunLease>,
) {
    let mut seen = 0;
    loop {
        let (events, status) = run.wait_from(seen).await;
        seen += events.len();
        if status.is_terminal() {
            let events = run.events().await;
            // The stop reason, as the terminal saves it, so a
            // reloaded session still says why a run ended.
            let stop = stop_reason_of(&events).unwrap_or_else(|| format!("{status:?}"));
            let has_answer = status == RunStatus::Completed && answered(&events);
            let (store, session, replacing) = (sessions.clone(), id.clone(), replacing.clone());
            // Recorded, under the store's lock, into the session as it is
            // saved now, so a change another writer made during the run is
            // kept.
            let recorded = tokio::task::spawn_blocking(move || {
                store.update(&session, |saved| {
                    // Only a finished answer replaces the saved one; a
                    // failed, cancelled or empty run leaves it as it was.
                    if let Some(old) = &replacing
                        && (!has_answer || take_latest_answer(saved, old).is_none())
                    {
                        return Err(());
                    }
                    saved.record_run_events(&events, &stop);
                    Ok(())
                })
            })
            .await;
            match recorded {
                Ok(Ok(_)) | Ok(Err(SessionUpdateError::Change(()))) => {}
                Ok(Err(SessionUpdateError::Store(error))) => {
                    eprintln!("could not save agent session {id:?}: {error}");
                }
                Err(error) => eprintln!("could not save agent session {id:?}: {error}"),
            }
            // The turn is recorded (or kept out of the session on purpose):
            // nothing is left to recover.
            sessions.clear_run_journal(&id);
            // The lease goes first: once the busy entry is gone, the session
            // is free in every process.
            drop(lease.take());
            busy.lock().await.remove(&id);
            break;
        }
    }
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
            // Read in place: polled during stream faults, so it copies
            // nothing from the run's buffer.
            let stop_reason = run.stop_reason().await.map(|reason| format!("{reason:?}"));
            let mut body = json!({
                "id": run.id(),
                "status": run.status().await,
                "events": run.event_count().await,
                "pending_approval": run.pending().await,
                // Additive: why a terminal run stopped (`EndTurn`,
                // `MaxTokens`, `Incomplete`, …); null while it runs.
                "stop_reason": stop_reason,
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
            let start = resume_position(&headers, &query).min(run.event_count().await);
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

/// The turn of session `id` this process is running, as `{id, status}`, if
/// the caller may see that run: a run paused on its time budget or waiting
/// on an approval stays reachable from its session after the page that
/// started it is gone. A journal another process wrote names no run here.
async fn session_active_run(
    state: &AppState,
    headers: &HeaderMap,
    sessions: &SessionStore,
    id: &SessionId,
) -> Option<serde_json::Value> {
    let run_id = sessions.journaled_run(id)?;
    let (run, _) = authorized_run(state, headers, &run_id, Scope::RunsRead)
        .await
        .ok()?;
    let status = run.status().await;
    if status.is_terminal() {
        return None;
    }
    Some(json!({ "id": run.id(), "status": status }))
}

/// Continue a run paused on its time budget. The same execution carries on
/// from where it stopped (its conversation, the tool results the model has
/// not read, its turn and tool-call counts): this is not a new turn. 409
/// unless the run is paused.
async fn continue_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ContinueBody>,
) -> Response {
    match authorized_run(&state, &headers, &id, Scope::RunsWrite).await {
        Ok((run, _)) => {
            let how = match body.mode {
                ContinueMode::Extend => lightagent_core::Continuation::Extend,
                ContinueMode::Answer => lightagent_core::Continuation::WrapUp,
            };
            if run.continue_with(how).await {
                Json(json!({ "id": run.id(), "continued": true })).into_response()
            } else {
                (
                    StatusCode::CONFLICT,
                    Json(json!({ "error": "run is not paused on its time budget" })),
                )
                    .into_response()
            }
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

/// The answer for a config write that did not happen: an invalid result is
/// the caller's (400), a stale revision is a conflict that names the current
/// one (409), and a lock held elsewhere is temporary (503).
fn config_error(error: ConfigError) -> Response {
    match &error {
        ConfigError::Invalid(_) => bad_request(&error.to_string()),
        ConfigError::Conflict { current, .. } => (
            StatusCode::CONFLICT,
            Json(json!({ "error": error.to_string(), "revision": current })),
        )
            .into_response(),
        ConfigError::Busy { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
        _ => internal(&error.to_string()),
    }
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
        let sessions = user.sessions(&state);
        return match sessions.load(&id) {
            Ok(session) if user.owns(&session) => {
                session_view(&state, &headers, &sessions, &id, session).await
            }
            _ => no_session(),
        };
    }
    match state.sessions.load(&id) {
        Ok(session) => session_view(&state, &headers, &state.sessions, &id, session).await,
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

/// A saved session as `GET /sessions/{id}` shows it: with `active_run` while
/// this process runs one of its turns that the caller may see.
async fn session_view(
    state: &AppState,
    headers: &HeaderMap,
    sessions: &SessionStore,
    id: &SessionId,
    session: Session,
) -> Response {
    let Some(active) = session_active_run(state, headers, sessions, id).await else {
        return Json(session).into_response();
    };
    match serde_json::to_value(&session) {
        Ok(mut view) => {
            if let Some(fields) = view.as_object_mut() {
                fields.insert("active_run".to_owned(), active);
            }
            Json(view).into_response()
        }
        Err(error) => internal(&error.to_string()),
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
    // Applied, under the store's lock, to the session as it is saved now, so
    // a change another process made since (a turn from the terminal or an
    // editor) is kept.
    let pinned = body.pinned;
    let archived = body.archived;
    let owner = user.clone();
    let updated = tokio::task::spawn_blocking(move || {
        store.update(&id, |session| {
            if owner.as_ref().is_some_and(|user| !user.owns(session)) {
                return Err(());
            }
            if let Some(value) = title {
                session.title = value;
            }
            if let Some(value) = pinned {
                session.pinned = value;
            }
            if let Some(value) = archived {
                session.archived = value;
            }
            if let Some(value) = project {
                session.project = value;
            }
            session.touch();
            Ok(lightagent_store::SessionSummary::of(session))
        })
    })
    .await;
    drop(busy);
    match (updated, &user) {
        (Ok(Ok((summary, _))), _) => Json(summary).into_response(),
        (Ok(Err(SessionUpdateError::Change(()))), _) => no_session(),
        (Ok(Err(SessionUpdateError::Store(error))), Some(_)) if is_missing(&error) => no_session(),
        (Ok(Err(SessionUpdateError::Store(error))), None) if is_missing(&error) => {
            not_found(&error.to_string())
        }
        (Ok(Err(SessionUpdateError::Store(error))), _) => internal(&error.to_string()),
        (Err(error), _) => internal(&error.to_string()),
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
/// Whether the run's events carry any answer text.
fn answered(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, AgentEvent::Content { text } if !text.is_empty()))
}

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
    use lightagent_core::{RunId, SecretRef, StopReason};

    /// A bound on each wait: a recorder or stream that is never woken fails
    /// the test instead of hanging it.
    const WAKE_GUARD: std::time::Duration = std::time::Duration::from_secs(10);

    // The session recorder is waiting when the run ends between its read and
    // its wait: the session is still recorded and released.
    #[tokio::test]
    async fn a_run_ending_in_the_recorders_gap_is_still_recorded() {
        let root =
            std::env::temp_dir().join(format!("lightagent-session-wait-{}", RunId::new().as_str()));
        let sessions = SessionStore::new(root.clone());
        let session = Session::new("default", "agent session");
        let id = session.id.clone();
        sessions.save(&session).unwrap();
        let busy = Arc::new(Mutex::new(HashSet::from([id.clone()])));
        let run = RunState::detached();
        run.push_for_test(AgentEvent::Content {
            text: "the answer".to_owned(),
        })
        .await;
        let (paused, resume) = run.pause_next_wait();
        let recorder = tokio::spawn(record_run_in_session(
            Arc::clone(&run),
            sessions.clone(),
            Arc::clone(&busy),
            id.clone(),
            None,
            None,
        ));
        paused.await.unwrap();
        run.push_for_test(AgentEvent::RunCompleted {
            reason: StopReason::Cancelled,
        })
        .await;
        run.finish_for_test(RunStatus::Cancelled).await;
        resume.send(()).unwrap();
        tokio::time::timeout(WAKE_GUARD, recorder)
            .await
            .expect("the session recorder was never woken")
            .unwrap();
        let saved = sessions.load(&id).unwrap();
        assert_eq!(
            saved
                .messages
                .last()
                .map(|message| message.content.as_str()),
            Some("the answer")
        );
        assert_eq!(
            saved.runs.last().and_then(|run| run.stop_reason.as_deref()),
            Some("Cancelled")
        );
        assert!(!busy.lock().await.contains(&id));
        std::fs::remove_dir_all(&root).ok();
    }

    // An event stream resumed mid-run delivers what follows its position and
    // ends when the run ends between the stream's read and its wait.
    #[tokio::test]
    async fn a_resumed_event_stream_ends_when_the_run_ends_in_its_gap() {
        use futures_util::StreamExt as _;
        let run = RunState::detached();
        for text in ["one", "two", "three"] {
            run.push_for_test(AgentEvent::Content {
                text: text.to_owned(),
            })
            .await;
        }
        let (paused, resume) = run.pause_next_wait();
        let frames = tokio::spawn(event_stream(Arc::clone(&run), None, 1).collect::<Vec<_>>());
        paused.await.unwrap();
        run.push_for_test(AgentEvent::RunCompleted {
            reason: StopReason::Cancelled,
        })
        .await;
        run.finish_for_test(RunStatus::Cancelled).await;
        resume.send(()).unwrap();
        let frames = tokio::time::timeout(WAKE_GUARD, frames)
            .await
            .expect("the event stream never ended")
            .unwrap();
        // "two" and "three" from position 1, then the terminal event.
        assert_eq!(frames.len(), 3);
    }

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
