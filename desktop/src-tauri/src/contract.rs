//! The operation contract: the only requests the UI may have the host make.
//!
//! The contract is `docs/desktop/contract/operations.json`, embedded at build
//! time and kept equal to the server's routes by
//! `scripts/check-desktop-contract.py`. A request that matches no operation,
//! carries an unlisted query parameter, or has a path segment outside the
//! identifier alphabet is refused before any connection is made.

use serde::Deserialize;

const CONTRACT: &str = include_str!("../../../docs/desktop/contract/operations.json");

/// The protocol version the contract describes; the runtime's handshake must
/// report the same.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct Contract {
    pub protocol: u32,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub raw_body: bool,
}

impl Contract {
    pub fn load() -> Result<Self, String> {
        let contract: Self = serde_json::from_str(CONTRACT)
            .map_err(|error| format!("the operation contract is malformed: {error}"))?;
        if contract.protocol != PROTOCOL {
            return Err(format!(
                "the operation contract is protocol {}, the host speaks {PROTOCOL}",
                contract.protocol
            ));
        }
        Ok(contract)
    }

    /// The operation `method target` names, where `target` is a path with an
    /// optional query string.
    pub fn resolve(&self, method: &str, target: &str) -> Result<&Operation, String> {
        if target.contains('#') {
            return Err("a request target may not carry a fragment".to_owned());
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.method == method && matches(&operation.path, path))
            .ok_or_else(|| format!("{method} {path} is not in the desktop operation contract"))?;
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let key = pair.split_once('=').map_or(pair, |(key, _)| key);
            if !operation.query.iter().any(|allowed| allowed == key) {
                return Err(format!(
                    "query parameter {key:?} is not allowed for {}",
                    operation.id
                ));
            }
        }
        Ok(operation)
    }
}

fn matches(template: &str, path: &str) -> bool {
    let template: Vec<&str> = template.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    template.len() == path.len()
        && template.iter().zip(&path).all(|(expected, actual)| {
            if expected.starts_with('{') && expected.ends_with('}') {
                valid_segment(actual)
            } else {
                expected == actual
            }
        })
}

/// An identifier segment: letters, digits, `-`, `_`, `.` and `:`, at most
/// 200 bytes, and never only dots.
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 200
        && !segment.bytes().all(|byte| byte == b'.')
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        Contract::load().expect("the embedded contract parses")
    }

    #[test]
    fn known_operations_resolve() {
        let contract = contract();
        let cases = [
            ("GET", "/health", "health"),
            ("GET", "/api/lightagent/v1/sessions", "sessions.list"),
            (
                "GET",
                "/api/lightagent/v1/sessions/search?q=hello",
                "sessions.search",
            ),
            ("GET", "/api/lightagent/v1/sessions/abc-123", "sessions.get"),
            (
                "GET",
                "/api/lightagent/v1/runs/r_1/events?after=4",
                "runs.events",
            ),
            (
                "POST",
                "/api/lightagent/v1/approvals/run-7",
                "approvals.respond",
            ),
        ];
        for (method, target, id) in cases {
            assert_eq!(
                contract.resolve(method, target).map(|op| op.id.as_str()),
                Ok(id),
                "{method} {target}"
            );
        }
        assert!(
            contract
                .resolve("GET", "/api/lightagent/v1/runs/r/events")
                .expect("stream")
                .stream
        );
    }

    #[test]
    fn everything_else_is_refused() {
        let contract = contract();
        for (method, target) in [
            ("GET", "/api/lightagent/v1/memory"),
            ("DELETE", "/api/lightagent/v1/tools"),
            ("GET", "/api/lightagent/v1/sessions/.."),
            ("GET", "/api/lightagent/v1/sessions/a%2Fb"),
            ("GET", "/api/lightagent/v1/sessions/a/../../b"),
            ("GET", "/api/lightagent/v1/sessions?evil=1"),
            ("GET", "/api/lightagent/v1/runs/r/events?after=1&token=x"),
            ("GET", "/api/lightagent/v1/sessions#x"),
            ("GET", "http://example.com/api/lightagent/v1/tools"),
        ] {
            assert!(
                contract.resolve(method, target).is_err(),
                "{method} {target} must be refused"
            );
        }
    }
}
