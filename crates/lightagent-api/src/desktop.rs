//! The request guard of `lightagent serve --desktop`.
//!
//! In desktop mode the server answers only the desktop host that started it
//! (docs/desktop/DESKTOP_ARCHITECTURE.md §6). Every request — the event
//! stream and `/health` included — must:
//!
//! 1. name the exact bound address in `Host`, which defeats DNS rebinding;
//! 2. carry no `Origin`, and no `Sec-Fetch-Site` other than `none`, which
//!    refuses anything a browser sends cross-origin (the host's client sends
//!    neither);
//! 3. present the per-launch key as `Authorization: Bearer <key>`, compared
//!    in constant time.
//!
//! The checks run in that order and each refusal has its own category, so a
//! request refused for its Host or Origin learns nothing about the key.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    HostRejected,
    OriginRejected,
    Unauthenticated,
}

impl Refusal {
    pub fn category(self) -> &'static str {
        match self {
            Self::HostRejected => "host_rejected",
            Self::OriginRejected => "origin_rejected",
            Self::Unauthenticated => "unauthenticated",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::HostRejected | Self::OriginRejected => StatusCode::FORBIDDEN,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
        }
    }
}

/// The guard for one desktop runtime: its bound address and launch key.
#[derive(Clone)]
pub struct DesktopGuard {
    inner: Arc<Inner>,
}

struct Inner {
    host: String,
    bearer: Vec<u8>,
}

impl DesktopGuard {
    pub fn new(bound: SocketAddr, key: &str) -> Self {
        Self {
            inner: Arc::new(Inner {
                host: bound.to_string(),
                bearer: format!("Bearer {key}").into_bytes(),
            }),
        }
    }

    /// Check one request's headers.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        if host != Some(self.inner.host.as_str()) {
            return Err(Refusal::HostRejected);
        }
        if headers.contains_key(header::ORIGIN) {
            return Err(Refusal::OriginRejected);
        }
        if let Some(site) = headers.get("sec-fetch-site")
            && site.as_bytes() != b"none"
        {
            return Err(Refusal::OriginRejected);
        }
        let presented = headers
            .get(header::AUTHORIZATION)
            .map_or(&[][..], |value| value.as_bytes());
        if !constant_time_eq(presented, &self.inner.bearer) {
            return Err(Refusal::Unauthenticated);
        }
        Ok(())
    }
}

/// Axum middleware applying [`DesktopGuard::check`] to every request.
pub async fn guard(State(guard): State<DesktopGuard>, request: Request, next: Next) -> Response {
    match guard.check(request.headers()) {
        Ok(()) => next.run(request).await,
        Err(refusal) => (
            refusal.status(),
            Json(json!({
                "error": {
                    "category": refusal.category(),
                    "message": "this runtime answers only the desktop app that started it",
                }
            })),
        )
            .into_response(),
    }
}

/// Equal-length comparison whose time does not depend on where the inputs differ.
fn constant_time_eq(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    presented
        .iter()
        .zip(expected)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn guard() -> DesktopGuard {
        DesktopGuard::new("127.0.0.1:49152".parse().unwrap(), KEY)
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn good() -> Vec<(&'static str, String)> {
        vec![
            ("host", "127.0.0.1:49152".to_owned()),
            ("authorization", format!("Bearer {KEY}")),
        ]
    }

    fn with(extra: &[(&'static str, &str)]) -> HeaderMap {
        let base = good();
        let mut pairs: Vec<(&'static str, &str)> =
            base.iter().map(|(n, v)| (*n, v.as_str())).collect();
        for (name, value) in extra {
            pairs.retain(|(n, _)| n != name);
            pairs.push((*name, *value));
        }
        headers(&pairs)
    }

    #[test]
    fn the_desktop_host_is_admitted() {
        assert_eq!(guard().check(&with(&[])), Ok(()));
        assert_eq!(guard().check(&with(&[("sec-fetch-site", "none")])), Ok(()));
    }

    #[test]
    fn any_other_host_is_refused_before_the_key_is_considered() {
        for host in [
            "localhost:49152",
            "127.0.0.1:1",
            "evil.example:49152",
            "127.0.0.1",
        ] {
            assert_eq!(
                guard().check(&with(&[("host", host)])),
                Err(Refusal::HostRejected),
                "{host}"
            );
        }
        assert_eq!(
            guard().check(&headers(&[("authorization", &format!("Bearer {KEY}"))])),
            Err(Refusal::HostRejected)
        );
    }

    #[test]
    fn browser_requests_are_refused_even_with_the_key() {
        assert_eq!(
            guard().check(&with(&[("origin", "http://127.0.0.1:49152")])),
            Err(Refusal::OriginRejected)
        );
        assert_eq!(
            guard().check(&with(&[("origin", "null")])),
            Err(Refusal::OriginRejected)
        );
        for site in ["cross-site", "same-site", "same-origin"] {
            assert_eq!(
                guard().check(&with(&[("sec-fetch-site", site)])),
                Err(Refusal::OriginRejected),
                "{site}"
            );
        }
    }

    #[test]
    fn a_missing_or_wrong_key_is_unauthenticated() {
        let wrong = format!("Bearer {}", KEY.replace('0', "1"));
        for auth in [
            "",
            "Bearer",
            wrong.as_str(),
            &format!("bearer {KEY}"),
            &format!("Bearer {KEY} "),
        ] {
            let headers = if auth.is_empty() {
                headers(&[("host", "127.0.0.1:49152")])
            } else {
                with(&[("authorization", auth)])
            };
            assert_eq!(
                guard().check(&headers),
                Err(Refusal::Unauthenticated),
                "{auth:?}"
            );
        }
    }

    #[test]
    fn comparison_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
