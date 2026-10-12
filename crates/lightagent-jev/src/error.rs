//! Why a Jev call produced no usable answer.

use std::time::Duration;

/// A typed Jev failure.
///
/// No variant carries a response body, a URL, a header or an underlying
/// transport error: those can echo request content or credentials, so a
/// `JevError` is always safe to display, log or send to a client.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JevError {
    /// No endpoint is configured.
    #[error("Jev endpoint is not configured")]
    NotConfigured,
    /// The request would be refused by TypeSafe; nothing was sent.
    #[error("Jev request is invalid: {0}")]
    InvalidRequest(&'static str),
    /// The serialized request exceeds the configured limit; nothing was sent.
    #[error("Jev request exceeds {limit} bytes")]
    RequestTooLarge { limit: usize },
    /// The caller's run was cancelled.
    #[error("Jev request was cancelled")]
    Cancelled,
    /// The overall deadline elapsed.
    #[error("Jev request timed out")]
    Timeout,
    /// The endpoint could not be reached.
    #[error("could not reach Jev")]
    Connect,
    /// The connection failed after it was established.
    #[error("Jev transport failed")]
    Transport,
    /// `401` or `403`: the key is missing, invalid or not permitted.
    #[error("Jev rejected the API key")]
    Unauthorized,
    /// `400` or `422`: TypeSafe refused the request body.
    #[error("Jev rejected the request")]
    Rejected,
    /// `429`: the account's rate limit was reached.
    #[error("Jev rate limit reached")]
    RateLimited { retry_after: Option<Duration> },
    /// `529`: TypeSafe is temporarily overloaded.
    #[error("Jev is overloaded")]
    Overloaded { retry_after: Option<Duration> },
    /// Any other non-success status, including redirects, which are never
    /// followed.
    #[error("Jev returned HTTP {status}")]
    Status {
        status: u16,
        retry_after: Option<Duration>,
    },
    /// The response exceeds the configured limit.
    #[error("Jev response exceeds {limit} bytes")]
    ResponseTooLarge { limit: usize },
    /// The response is not a valid answer to the questions asked.
    #[error("Jev response is malformed: {0}")]
    Malformed(&'static str),
}

impl JevError {
    /// A stable, low-cardinality code for metrics and fallback reasons.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::InvalidRequest(_) => "invalid_request",
            Self::RequestTooLarge { .. } => "request_too_large",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Transport => "transport",
            Self::Unauthorized => "unauthorized",
            Self::Rejected => "rejected",
            Self::RateLimited { .. } => "rate_limited",
            Self::Overloaded { .. } => "overloaded",
            Self::Status { .. } => "status",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::Malformed(_) => "malformed",
        }
    }

    /// The server's requested delay before retrying, when it sent one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after }
            | Self::Overloaded { retry_after }
            | Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether repeating the same request may succeed. Only transient,
    /// side-effect-free failures qualify; a malformed answer never does.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Connect
                | Self::RateLimited { .. }
                | Self::Overloaded { .. }
                | Self::Status {
                    status: 408 | 500 | 502 | 503 | 504,
                    ..
                }
        )
    }
}
