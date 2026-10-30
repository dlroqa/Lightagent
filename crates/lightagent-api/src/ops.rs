//! Typed operations the API forwards to its embedder.
//!
//! The stores these operations change (profiles, and in later slices the
//! configuration, extensions, memory and the rest) belong to the embedding
//! runtime — `lightagent serve`, in the CLI crate — so the API names the
//! request and the embedder performs it through [`crate::RunFactory`]. The CLI
//! performs the same operations for its commands, so the CLI, the Web UI and
//! the desktop app change the same state the same way. An embedder that only
//! runs agents keeps the factory's defaults, which answer `unavailable`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

/// A new profile, as `lightagent profile create` takes it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CreateProfile {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// One profile operation (`lightagent profile …`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileRequest {
    Show(String),
    Create(CreateProfile),
    /// Make the profile the active one for the CLI and later runtimes.
    Use(String),
    Preset {
        id: String,
        preset: String,
    },
    Delete(String),
}

/// A stable error category, for the UI to act on without parsing messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpCategory {
    NotFound,
    Invalid,
    Conflict,
    Denied,
    Unavailable,
    Internal,
}

impl OpCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Invalid => "invalid",
            Self::Conflict => "conflict",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Denied => StatusCode::FORBIDDEN,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Why an operation failed: a category and a message fit to show a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpError {
    pub category: OpCategory,
    pub message: String,
}

impl OpError {
    pub fn new(category: OpCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    /// The embedder does not perform this operation.
    pub fn unavailable() -> Self {
        Self::new(
            OpCategory::Unavailable,
            "this server does not perform that operation",
        )
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl IntoResponse for OpError {
    fn into_response(self) -> Response {
        (
            self.category.status(),
            Json(json!({
                "error": { "category": self.category.as_str(), "message": self.message }
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_have_stable_names_and_statuses() {
        let cases = [
            (OpCategory::NotFound, "not_found", 404),
            (OpCategory::Invalid, "invalid", 400),
            (OpCategory::Conflict, "conflict", 409),
            (OpCategory::Denied, "denied", 403),
            (OpCategory::Unavailable, "unavailable", 503),
            (OpCategory::Internal, "internal", 500),
        ];
        for (category, name, status) in cases {
            assert_eq!(category.as_str(), name);
            assert_eq!(category.status().as_u16(), status);
        }
    }

    #[test]
    fn a_create_body_needs_only_an_id() {
        let body: CreateProfile = serde_json::from_str(r#"{"id":"work"}"#).unwrap();
        assert_eq!(
            body,
            CreateProfile {
                id: "work".into(),
                name: None,
                persona: None,
                model: None
            }
        );
    }
}
