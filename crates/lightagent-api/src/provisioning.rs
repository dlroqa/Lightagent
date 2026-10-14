//! The provisioning HTTP contract for the external account-control service.
//!
//! Compiled only with the `provisioning-api` feature, which no release or
//! production build enables, and never merged into [`crate::router`]: on a
//! running server every `/api/lightagent/v1/provisioning/...` path is the
//! ordinary unknown-endpoint `404`. It exists so the contract and its
//! security properties are built and tested before anything serves it.
//!
//! | Method and path | Success |
//! | --- | --- |
//! | `PUT /api/lightagent/v1/provisioning/profiles/{profile_id}` | `201` created, `200` replayed, `409` failed |
//! | `GET /api/lightagent/v1/provisioning/operations/{operation_id}` | `200` |
//! | `POST /api/lightagent/v1/provisioning/operations/{operation_id}/revoke` | `200` |
//!
//! Every request needs a provisioning credential (an operator-role
//! credential restricted to exactly `provision`, of a registered
//! provisioner) as `Authorization: Bearer`. Bodies are JSON, at most 8 KiB,
//! and unknown fields are refused. Errors are `{"error": text, "code": code}`
//! with a stable code. See `docs/architecture/profile-provisioning.md`.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use lightagent_core::identity::Caller;
use lightagent_core::provisioning::{
    OperationState, ProvisionRequest, ProvisioningError, ProvisioningService,
};
use serde::Deserialize;
use serde_json::json;

use crate::AuthConfig;

/// The largest request body accepted.
pub const MAX_BODY_BYTES: usize = 8 * 1024;

/// What the provisioning routes need: the service and a multi-user auth
/// policy (an identity authority) to establish the caller.
#[derive(Clone)]
pub struct ProvisioningApi {
    pub service: Arc<ProvisioningService>,
    pub auth: Arc<AuthConfig>,
}

/// The provisioning routes. Not part of [`crate::router`].
pub fn routes(api: ProvisioningApi) -> Router {
    Router::new()
        .route(
            "/api/lightagent/v1/provisioning/profiles/{profile_id}",
            put(provision),
        )
        .route(
            "/api/lightagent/v1/provisioning/operations/{operation_id}",
            get(status),
        )
        .route(
            "/api/lightagent/v1/provisioning/operations/{operation_id}/revoke",
            post(revoke),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(api)
}

/// The body of a provisioning request; the profile id is the path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionBody {
    issuer: String,
    subject: String,
    display_name: String,
    template: String,
}

fn error(status: StatusCode, code: &str, text: &str) -> Response {
    (status, Json(json!({ "error": text, "code": code }))).into_response()
}

fn refused(failure: ProvisioningError) -> Response {
    let status =
        StatusCode::from_u16(failure.http_status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    error(status, failure.code(), &failure.to_string())
}

/// The caller, from the identity authority. Without one, nothing is served.
fn caller(api: &ProvisioningApi, headers: &HeaderMap) -> Result<Caller, Box<Response>> {
    if api.auth.identity().is_none() {
        return Err(Box::new(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_unavailable",
            "provisioning requires the identity store",
        )));
    }
    api.auth.caller(headers).map_err(|denial| {
        let (status, text) = crate::auth::rejection(denial);
        Box::new(error(status, "unauthenticated", &text))
    })
}

/// Run a blocking service call off the async runtime.
async fn blocking<R: Send + 'static>(
    work: impl FnOnce() -> Result<R, ProvisioningError> + Send + 'static,
) -> Result<R, ProvisioningError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or(Err(ProvisioningError::Unavailable))
}

async fn provision(
    State(api): State<ProvisioningApi>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    body: Result<Json<ProvisionBody>, JsonRejection>,
) -> Response {
    let caller = match caller(&api, &headers) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            let (status, code) = match rejection.status() {
                StatusCode::PAYLOAD_TOO_LARGE => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
                }
                StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                    (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
                }
                // Malformed JSON, a missing field and an unknown one alike.
                _ => (StatusCode::BAD_REQUEST, "invalid_request"),
            };
            return error(
                status,
                code,
                "the request body is not a valid provisioning request",
            );
        }
    };
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    else {
        return error(
            StatusCode::BAD_REQUEST,
            "missing_idempotency_key",
            "an Idempotency-Key header is required",
        );
    };
    let request = ProvisionRequest {
        issuer: body.issuer,
        subject: body.subject,
        profile_id,
        display_name: body.display_name,
        template: body.template,
    };
    let service = Arc::clone(&api.service);
    match blocking(move || service.provision(&caller, &key, &request)).await {
        Ok(outcome) => {
            let status = match (outcome.status.state, outcome.replayed) {
                (OperationState::Failed, _) => StatusCode::CONFLICT,
                (_, true) => StatusCode::OK,
                (_, false) => StatusCode::CREATED,
            };
            let mut body = json!({ "operation": outcome.status, "replayed": outcome.replayed });
            if let Some(code) = &outcome.status.failure {
                body["code"] = json!(code);
                body["error"] = json!("provisioning failed");
            }
            (status, Json(body)).into_response()
        }
        Err(failure) => refused(failure),
    }
}

async fn status(
    State(api): State<ProvisioningApi>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
) -> Response {
    let caller = match caller(&api, &headers) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let service = Arc::clone(&api.service);
    match blocking(move || service.status(&caller, &operation_id)).await {
        Ok(status) => Json(json!({ "operation": status })).into_response(),
        Err(failure) => refused(failure),
    }
}

async fn revoke(
    State(api): State<ProvisioningApi>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
) -> Response {
    let caller = match caller(&api, &headers) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let service = Arc::clone(&api.service);
    match blocking(move || service.revoke(&caller, &operation_id)).await {
        Ok(status) => Json(json!({ "operation": status })).into_response(),
        Err(failure) => refused(failure),
    }
}
