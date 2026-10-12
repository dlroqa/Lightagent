//! The SystemOne HTTP client: one bounded, cancellable evaluation per call.

use std::fmt;
use std::time::Duration;

use lightagent_core::JevConfig;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::answer::{Response, parse_response};
use crate::error::JevError;
use crate::question::EvaluationRequest;

/// The whole-call deadline when a caller sets none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
/// A request above this size is refused before sending. Jev's documented
/// context is 64k tokens; this is roughly that much text.
pub const MAX_REQUEST_BYTES: usize = 262_144;
/// A response above this size is abandoned unread.
pub const MAX_RESPONSE_BYTES: usize = 65_536;
/// No policy may retry more often than this, whatever it asks for.
pub const MAX_RETRIES: u32 = 3;
/// A server-requested delay longer than this is treated as this long; it still
/// must fit within the call's deadline to be honored at all.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3_600);

/// Resource bounds for one evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The deadline for the whole call, every attempt and wait included.
    pub timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_request_bytes: MAX_REQUEST_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }
}

/// When a transient failure is retried.
///
/// Only [`JevError::is_transient`] failures are retried, at most
/// `max_retries` (capped at [`MAX_RETRIES`]) times, and only while the wait
/// still fits within the call's deadline. A server's `retry-after-ms` or
/// `retry-after` is honored when `respect_retry_after` is set; otherwise, and
/// when the server sends neither, the wait doubles from `initial_backoff` up to
/// `max_backoff`. A wait that would outlast the deadline ends the call with the
/// failure instead of sleeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub respect_retry_after: bool,
}

impl RetryPolicy {
    /// One attempt only. The advisory router uses this: a slow decision is
    /// worth less than the run it would delay.
    pub const fn none() -> Self {
        Self {
            max_retries: 0,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(2),
            respect_retry_after: true,
        }
    }

    /// Up to `max_retries` retries with the default backoff.
    pub const fn bounded(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Self::none()
        }
    }

    /// The wait before retry number `retries + 1`, or `None` to stop.
    fn delay(&self, error: &JevError, retries: u32) -> Option<Duration> {
        if retries >= self.max_retries.min(MAX_RETRIES) || !error.is_transient() {
            return None;
        }
        match error.retry_after().filter(|_| self.respect_retry_after) {
            Some(wait) => Some(wait),
            None => Some(
                self.initial_backoff
                    .saturating_mul(1 << retries.min(16))
                    .min(self.max_backoff),
            ),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::none()
    }
}

/// The bearer key. Its `Debug` never shows the value.
#[derive(Clone)]
struct ApiKey(String);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// A SystemOne client bound to one endpoint, key, limits and retry policy.
#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<ApiKey>,
    limits: Limits,
    retry: RetryPolicy,
}

impl fmt::Debug for JevClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JevClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("limits", &self.limits)
            .field("retry", &self.retry)
            .finish()
    }
}

impl JevClient {
    /// A client for `base_url` — the service root or the full `/v1/systemone`
    /// endpoint. Redirects are never followed.
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        limits: Limits,
        retry: RetryPolicy,
    ) -> Result<Self, JevError> {
        if base_url.trim().is_empty() {
            return Err(JevError::NotConfigured);
        }
        // This workspace builds reqwest with `rustls-no-provider`; building a
        // client without an installed provider panics.
        crate::tls::ensure_provider();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(limits.timeout)
            .build()
            .map_err(|_| JevError::Transport)?;
        Ok(Self {
            http,
            endpoint: systemone_endpoint(base_url),
            api_key: api_key.filter(|key| !key.is_empty()).map(ApiKey),
            limits,
            retry,
        })
    }

    /// A client from Lightagent's `platform.jev` settings, resolving the key
    /// from its secret reference at the point of use. Whether Jev is enabled is
    /// the caller's decision; this only builds the transport.
    pub fn from_config(config: &JevConfig) -> Result<Self, JevError> {
        let base_url = config
            .endpoint
            .base_url
            .as_deref()
            .ok_or(JevError::NotConfigured)?;
        let api_key = config
            .endpoint
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve());
        Self::new(
            base_url,
            api_key,
            Limits {
                timeout: Duration::from_secs(config.timeout_secs.max(1)),
                ..Limits::default()
            },
            RetryPolicy::none(),
        )
    }

    /// This client with a different retry policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The resolved `/v1/systemone` endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Evaluate `request` once, within the deadline and until `cancel` fires.
    ///
    /// Cancellation wins over everything, including a response already in
    /// flight. Nothing is sent for an invalid or oversized request.
    pub async fn evaluate(
        &self,
        request: &EvaluationRequest,
        cancel: &CancellationToken,
    ) -> Result<Response, JevError> {
        request.validate()?;
        let body = serde_json::to_vec(request)
            .map_err(|_| JevError::InvalidRequest("the request cannot be serialized"))?;
        if body.len() > self.limits.max_request_bytes {
            return Err(JevError::RequestTooLarge {
                limit: self.limits.max_request_bytes,
            });
        }
        let deadline = Instant::now() + self.limits.timeout;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(JevError::Cancelled),
            result = tokio::time::timeout_at(deadline, self.attempts(&body, request, deadline)) => {
                result.unwrap_or(Err(JevError::Timeout))
            }
        }
    }

    async fn attempts(
        &self,
        body: &[u8],
        request: &EvaluationRequest,
        deadline: Instant,
    ) -> Result<Response, JevError> {
        let mut retries = 0;
        loop {
            let error = match self.send(body).await {
                Ok(value) => return parse_response(&value, request),
                Err(error) => error,
            };
            let Some(wait) = self.retry.delay(&error, retries) else {
                return Err(error);
            };
            if Instant::now() + wait >= deadline {
                return Err(error);
            }
            retries += 1;
            tokio::time::sleep(wait).await;
        }
    }

    /// One HTTP attempt. An error status's body is never read.
    async fn send(&self, body: &[u8]) -> Result<Value, JevError> {
        let mut request = self
            .http
            .post(&self.endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec());
        if let Some(ApiKey(key)) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let mut response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status, response.headers()));
        }
        let limit = self.limits.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(JevError::ResponseTooLarge { limit });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(JevError::ResponseTooLarge { limit });
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| JevError::Malformed("the response is not JSON"))
    }
}

/// Accept both the TypeSafe service root and its documented SystemOne endpoint,
/// so a pasted full endpoint never becomes `/v1/systemone/v1/systemone`.
pub fn systemone_endpoint(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/v1/systemone") {
        base_url.to_owned()
    } else {
        format!("{base_url}/v1/systemone")
    }
}

/// Classify a transport failure without keeping it: a `reqwest::Error` can
/// carry the URL.
fn transport_error(error: reqwest::Error) -> JevError {
    if error.is_timeout() {
        JevError::Timeout
    } else if error.is_connect() {
        JevError::Connect
    } else {
        JevError::Transport
    }
}

fn status_error(status: StatusCode, headers: &HeaderMap) -> JevError {
    let retry_after = retry_after(headers);
    match status.as_u16() {
        401 | 403 => JevError::Unauthorized,
        400 | 422 => JevError::Rejected,
        429 => JevError::RateLimited { retry_after },
        529 => JevError::Overloaded { retry_after },
        status => JevError::Status {
            status,
            retry_after,
        },
    }
}

/// `retry-after-ms` (fractional milliseconds) first, then `retry-after` in
/// whole seconds. An HTTP-date or malformed value is ignored.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    if let Some(millis) = header("retry-after-ms")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|millis| millis.is_finite() && *millis >= 0.0)
    {
        // Capped before conversion: `from_secs_f64` panics on overflow.
        let seconds = (millis / 1_000.0).min(MAX_RETRY_AFTER.as_secs_f64());
        return Some(Duration::from_secs_f64(seconds));
    }
    header(RETRY_AFTER.as_str())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn systemone_endpoint_accepts_a_service_root_or_full_endpoint() {
        assert_eq!(
            systemone_endpoint("https://api.typesafe.ai"),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            systemone_endpoint("https://api.typesafe.ai/v1/systemone/"),
            "https://api.typesafe.ai/v1/systemone"
        );
    }

    #[test]
    fn retry_after_prefers_milliseconds_and_ignores_dates() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert("retry-after-ms", HeaderValue::from_static("250"));
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(250)));
        headers.insert("retry-after-ms", HeaderValue::from_static("1e300"));
        assert_eq!(retry_after(&headers), Some(MAX_RETRY_AFTER));
        headers.insert("retry-after-ms", HeaderValue::from_static("-1"));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, HeaderValue::from_static("999999999"));
        assert_eq!(retry_after(&headers), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn statuses_map_to_typed_errors() {
        let empty = HeaderMap::new();
        for (status, kind) in [
            (401, "unauthorized"),
            (403, "unauthorized"),
            (422, "rejected"),
            (429, "rate_limited"),
            (529, "overloaded"),
            (302, "status"),
            (503, "status"),
        ] {
            assert_eq!(
                status_error(StatusCode::from_u16(status).unwrap(), &empty).kind(),
                kind
            );
        }
    }

    #[test]
    fn retries_are_bounded_and_only_for_transient_failures() {
        let policy = RetryPolicy {
            max_retries: 50,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(300),
            respect_retry_after: true,
        };
        let busy = JevError::Status {
            status: 503,
            retry_after: None,
        };
        assert_eq!(policy.delay(&busy, 0), Some(Duration::from_millis(100)));
        assert_eq!(policy.delay(&busy, 1), Some(Duration::from_millis(200)));
        assert_eq!(policy.delay(&busy, 2), Some(Duration::from_millis(300)));
        assert_eq!(policy.delay(&busy, MAX_RETRIES), None, "hard cap");
        let limited = JevError::RateLimited {
            retry_after: Some(Duration::from_secs(1)),
        };
        assert_eq!(policy.delay(&limited, 0), Some(Duration::from_secs(1)));
        let ignoring = RetryPolicy {
            respect_retry_after: false,
            ..policy
        };
        assert_eq!(
            ignoring.delay(&limited, 0),
            Some(Duration::from_millis(100))
        );
        for permanent in [
            JevError::Unauthorized,
            JevError::Rejected,
            JevError::Malformed("x"),
            JevError::Timeout,
            JevError::Cancelled,
            JevError::Status {
                status: 404,
                retry_after: None,
            },
        ] {
            assert_eq!(policy.delay(&permanent, 0), None, "{permanent:?}");
        }
        assert_eq!(RetryPolicy::none().delay(&busy, 0), None);
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let client = JevClient::new(
            "http://127.0.0.1:9",
            Some("jev-private-key-value".to_owned()),
            Limits::default(),
            RetryPolicy::none(),
        )
        .unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("jev-private-key-value"));
        assert!(debug.contains("<redacted>"));
        assert!(matches!(
            JevClient::new(" ", None, Limits::default(), RetryPolicy::none()),
            Err(JevError::NotConfigured)
        ));
    }
}
