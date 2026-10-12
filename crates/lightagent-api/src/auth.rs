//! Scoped bearer authentication.
//!
//! The API draws the same single distinction the inference gateway does:
//! loopback is trusted, anything else needs a key. An [`AuthConfig::open`] admits
//! every request (the default for a loopback bind and for tests); a keyed config
//! requires `Authorization: Bearer <key>` **and** that the key carries the
//! [`Scope`] the route needs, so a token minted for reading runs cannot start
//! them. Refusals are the two the plan calls for: `401` for a missing or wrong
//! key, `403` for a valid key that lacks the scope.

use std::collections::HashSet;

use axum::http::{HeaderMap, StatusCode};
use lightagent_core::identity::{Caller, Capability, Denial, PrincipalSource};

/// A capability a route requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    RunsRead,
    RunsWrite,
    SessionsRead,
    SessionsWrite,
    ToolsRead,
    ApprovalsWrite,
    /// Grants every scope.
    Admin,
}

impl From<Scope> for Capability {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::RunsRead => Capability::RunsRead,
            Scope::RunsWrite => Capability::RunsWrite,
            Scope::SessionsRead => Capability::SessionsRead,
            Scope::SessionsWrite => Capability::SessionsWrite,
            Scope::ToolsRead => Capability::ToolsRead,
            Scope::ApprovalsWrite => Capability::ApprovalsWrite,
            Scope::Admin => Capability::Administer,
        }
    }
}

/// The API's authentication policy.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    key: Option<String>,
    scopes: HashSet<Scope>,
}

impl AuthConfig {
    /// Admit every request (loopback, or a test). No key, no scope checks.
    pub fn open() -> Self {
        Self {
            key: None,
            scopes: HashSet::new(),
        }
    }

    /// Require `key`, granting exactly `scopes` (add [`Scope::Admin`] for all).
    pub fn keyed(key: impl Into<String>, scopes: impl IntoIterator<Item = Scope>) -> Self {
        Self {
            key: Some(key.into()),
            scopes: scopes.into_iter().collect(),
        }
    }

    /// Whether this policy requires a key.
    pub fn requires_key(&self) -> bool {
        self.key.is_some()
    }

    /// The caller this policy establishes for `headers`, in the shared
    /// identity vocabulary. Today's API has one identity: the local operator,
    /// reached through the open loopback bind or the single configured key
    /// (the compatibility mode). A key's scopes become the caller's
    /// capability restriction, so [`lightagent_core::identity::authorize`]
    /// agrees with [`AuthConfig::authorize`] for every scope. Handlers do not
    /// call this yet; it is the seam profile ownership will use.
    pub fn caller(&self, headers: &HeaderMap) -> Result<Caller, Denial> {
        let Some(expected) = &self.key else {
            return Ok(Caller::local_operator(PrincipalSource::LocalCompatibility));
        };
        match bearer(headers) {
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                let mut caller = Caller::local_operator(PrincipalSource::LocalCompatibility);
                if !self.scopes.contains(&Scope::Admin) {
                    caller.restriction.capabilities =
                        Some(self.scopes.iter().copied().map(Capability::from).collect());
                }
                Ok(caller)
            }
            _ => Err(Denial::Unauthenticated),
        }
    }

    /// Authorize a request carrying `headers` for a route needing `required`.
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        required: Scope,
    ) -> Result<(), (StatusCode, String)> {
        let Some(expected) = &self.key else {
            return Ok(()); // open policy: loopback is the boundary
        };
        match bearer(headers) {
            // Constant-time compare, so a key sharing a prefix with the real one
            // is not rejected measurably slower than one differing at the first
            // byte — the same protection the inference gateway gives its keys.
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                if self.scopes.contains(&Scope::Admin) || self.scopes.contains(&required) {
                    Ok(())
                } else {
                    Err((
                        StatusCode::FORBIDDEN,
                        format!("the key lacks the required scope {required:?}"),
                    ))
                }
            }
            _ => Err((
                StatusCode::UNAUTHORIZED,
                "a valid `Authorization: Bearer <key>` is required".to_owned(),
            )),
        }
    }
}

/// The bearer token in `headers`, if any. The auth scheme is
/// case-insensitive per RFC 7235, and clients vary — accept `Bearer`,
/// `bearer`, `BEARER`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then_some(token.trim())
        })
}

/// Compare two byte strings without leaking their contents through timing.
///
/// Reproduced from the inference gateway's own `constant_time_eq` rather than
/// imported — this crate depends on no `lightweight-*` crate — so the API's key
/// check has the same protection the gateway's does. The length difference is
/// unavoidable and harmless; what matters is that a key sharing a prefix with the
/// real one does not take measurably longer to reject than one that differs at
/// the first byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = format!("Bearer {token}").parse() {
            headers.insert("authorization", value);
        }
        headers
    }

    #[test]
    fn an_open_policy_admits_everything() {
        let auth = AuthConfig::open();
        assert!(auth.authorize(&HeaderMap::new(), Scope::RunsWrite).is_ok());
    }

    #[test]
    fn a_keyed_policy_needs_the_key() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        let (status, _) = auth
            .authorize(&HeaderMap::new(), Scope::RunsRead)
            .unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = auth
            .authorize(&headers_with("wrong"), Scope::RunsRead)
            .unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            auth.authorize(&headers_with("secret"), Scope::RunsRead)
                .is_ok()
        );
    }

    #[test]
    fn a_valid_key_without_the_scope_is_forbidden() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        let (status, _) = auth
            .authorize(&headers_with("secret"), Scope::RunsWrite)
            .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn admin_grants_every_scope() {
        let auth = AuthConfig::keyed("secret", [Scope::Admin]);
        assert!(
            auth.authorize(&headers_with("secret"), Scope::SessionsWrite)
                .is_ok()
        );
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        for scheme in ["Bearer", "bearer", "BEARER"] {
            let mut headers = HeaderMap::new();
            if let Ok(value) = format!("{scheme} secret").parse() {
                headers.insert("authorization", value);
            }
            assert!(
                auth.authorize(&headers, Scope::RunsRead).is_ok(),
                "scheme {scheme:?} should be accepted"
            );
        }
    }

    #[test]
    fn constant_time_eq_matches_only_equal_slices() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreu")); // differs at the last byte
        assert!(!constant_time_eq(b"secret", b"xecret")); // differs at the first byte
        assert!(!constant_time_eq(b"secret", b"secre")); // shorter
        assert!(!constant_time_eq(b"secret", b"secrets")); // longer
        assert!(constant_time_eq(b"", b""));
    }

    const ALL_SCOPES: [Scope; 7] = [
        Scope::RunsRead,
        Scope::RunsWrite,
        Scope::SessionsRead,
        Scope::SessionsWrite,
        Scope::ToolsRead,
        Scope::ApprovalsWrite,
        Scope::Admin,
    ];

    /// The shared identity seam agrees with today's gate for every policy,
    /// presented key and required scope, so adopting it changes nothing.
    #[test]
    fn the_identity_caller_agrees_with_the_scope_gate() {
        use lightagent_core::identity::{LOCAL_OPERATOR, Role, authorize_capability};
        let mut policies = vec![
            AuthConfig::open(),
            AuthConfig::keyed("secret", [Scope::Admin]),
        ];
        for scope in ALL_SCOPES {
            policies.push(AuthConfig::keyed("secret", [scope]));
        }
        policies.push(AuthConfig::keyed(
            "secret",
            [Scope::RunsRead, Scope::SessionsRead],
        ));
        for policy in &policies {
            for headers in [
                HeaderMap::new(),
                headers_with("wrong"),
                headers_with("secret"),
            ] {
                for required in ALL_SCOPES {
                    let gate = policy.authorize(&headers, required);
                    let seam = policy
                        .caller(&headers)
                        .and_then(|caller| authorize_capability(&caller, required.into()));
                    match (&gate, &seam) {
                        (Ok(()), Ok(())) => {}
                        (Err((StatusCode::UNAUTHORIZED, _)), Err(Denial::Unauthenticated)) => {}
                        (Err((StatusCode::FORBIDDEN, _)), Err(Denial::MissingCapability)) => {}
                        _ => panic!("{policy:?} {required:?}: gate {gate:?} vs seam {seam:?}"),
                    }
                }
                if let Ok(caller) = policy.caller(&headers) {
                    assert_eq!(caller.principal.id.as_str(), LOCAL_OPERATOR);
                    assert_eq!(caller.principal.role, Role::Operator);
                    assert_eq!(caller.principal.source, PrincipalSource::LocalCompatibility);
                }
            }
        }
    }

    #[test]
    fn every_scope_maps_to_its_own_capability() {
        let mapped: std::collections::BTreeSet<Capability> =
            ALL_SCOPES.into_iter().map(Capability::from).collect();
        assert_eq!(mapped.len(), ALL_SCOPES.len());
        assert_eq!(Capability::from(Scope::Admin), Capability::Administer);
        assert!(
            !mapped.contains(&Capability::Provision),
            "no API scope provisions today"
        );
    }
}
