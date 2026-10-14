//! Web's side of the candidate, read independently from the specification:
//! response classification (§12.2–§12.4), ownership assessment (§13.1) and
//! job resolution (§13.2 R1–R3). Lightagent never runs this. It exists to
//! check that the conclusions Web draws from harness answers are the ones
//! PR #26's service semantics justify.

use std::collections::BTreeMap;

use super::{MAX_MESSAGE_BYTES, PROTOCOL, Value, parse_strict, prefixed_id, safe_integer};

const OPERATION_STATES: [&str; 5] = [
    "pending",
    "profile_created",
    "completed",
    "failed",
    "revoked",
];
const RESULT_MEMBERS: [&str; 9] = [
    "createdUnix",
    "failure",
    "ownerBound",
    "operationId",
    "principalId",
    "profileId",
    "state",
    "template",
    "updatedUnix",
];

/// One entry of `errors.json`.
#[derive(Clone, Debug)]
pub struct RegisteredCode {
    pub http: u16,
    pub class: String,
    pub operations: Vec<String>,
}

/// The normative registry (§11): error codes and operation failure codes.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub codes: BTreeMap<String, RegisteredCode>,
    pub failure_codes: Vec<String>,
}

/// What Web knows about the command a response answers.
#[derive(Clone, Debug)]
pub struct Request {
    pub command_id: String,
    pub audience: String,
    pub operation: String,
    pub profile_id: String,
    /// For status and revoke.
    pub operation_id: Option<String>,
}

/// §12.4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classified {
    Operation {
        result: Value,
        replayed: Option<bool>,
        ownership: &'static str,
    },
    Refused {
        code: String,
        class: String,
    },
    Uncertain {
        reason: String,
    },
}

const INVALID: &str = "RESPONSE_INVALID";

fn uncertain(reason: &str) -> Classified {
    Classified::Uncertain {
        reason: reason.to_owned(),
    }
}

pub fn classify_response(
    registry: &Registry,
    request: &Request,
    http: u16,
    body: &[u8],
) -> Classified {
    if body.len() > MAX_MESSAGE_BYTES {
        return uncertain("RESPONSE_TOO_LARGE");
    }
    let Ok(doc) = parse_strict(body) else {
        return uncertain(INVALID);
    };
    if super::has_duplicate(&doc) {
        return uncertain(INVALID);
    }
    match http {
        200 | 201 => {
            operation_response(registry, request, http, &doc).unwrap_or_else(|| uncertain(INVALID))
        }
        400..=599 => {
            error_response(registry, request, http, &doc).unwrap_or_else(|| uncertain(INVALID))
        }
        _ => uncertain(INVALID),
    }
}

fn members(value: &Value) -> Option<&[(String, Value)]> {
    match value {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

fn exactly(value: &Value, names: &[&str]) -> bool {
    members(value).is_some_and(|m| {
        m.len() == names.len() && m.iter().all(|(k, _)| names.contains(&k.as_str()))
    })
}

fn string<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.member(name).and_then(Value::str)
}

fn operation_response(
    registry: &Registry,
    request: &Request,
    http: u16,
    doc: &Value,
) -> Option<Classified> {
    let create = request.operation == "profile.create";
    let top: &[&str] = if create {
        &[
            "audience",
            "commandId",
            "operation",
            "protocol",
            "replayed",
            "result",
        ]
    } else {
        &["audience", "commandId", "operation", "protocol", "result"]
    };
    if !exactly(doc, top)
        || string(doc, "protocol")? != PROTOCOL
        || string(doc, "commandId")? != request.command_id
        || string(doc, "audience")? != request.audience
        || string(doc, "operation")? != request.operation
    {
        return None;
    }
    let replayed = if create {
        let Value::Bool(replayed) = doc.member("replayed")? else {
            return None;
        };
        // 201 when this transmission created the operation, 200 otherwise.
        if (http == 201) == *replayed {
            return None;
        }
        Some(*replayed)
    } else {
        if http != 200 {
            return None;
        }
        None
    };
    let result = doc.member("result")?;
    if !exactly(result, &RESULT_MEMBERS) {
        return None;
    }
    let operation_id = string(result, "operationId")?;
    let state = string(result, "state")?;
    if operation_id.len() != 32
        || !operation_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || request.operation_id.as_deref().is_some_and(|id| id != operation_id)
        || !OPERATION_STATES.contains(&state)
        // §10.4 P4: a create answer is terminal.
        || (create && !matches!(state, "completed" | "failed" | "revoked"))
        || (request.operation == "profile.revoke" && state != "revoked")
    {
        return None;
    }
    match (state, result.member("failure")?) {
        ("failed", Value::String(code)) if registry.failure_codes.contains(code) => {}
        ("failed", _) => return None,
        (_, Value::Null) => {}
        _ => return None,
    }
    let principal = string(result, "principalId")?;
    let principal_ok = (1..=64).contains(&principal.len())
        && principal.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && matches!(b, b'_' | b'-'))
        });
    if !principal_ok
        || string(result, "profileId")? != request.profile_id
        || !prefixed_id(result.member("profileId")?, "agt_")
        || string(result, "template")? != "assistant"
    {
        return None;
    }
    let Value::Bool(owner_bound) = result.member("ownerBound")? else {
        return None;
    };
    let unix = |name: &str| match result.member(name)? {
        Value::Number(lexeme) => safe_integer(lexeme),
        _ => None,
    };
    let (created, updated) = (unix("createdUnix")?, unix("updatedUnix")?);
    if updated < created {
        return None;
    }
    Some(Classified::Operation {
        result: result.clone(),
        replayed,
        ownership: assess_ownership(state, *owner_bound),
    })
}

fn error_response(
    registry: &Registry,
    request: &Request,
    http: u16,
    doc: &Value,
) -> Option<Classified> {
    if !exactly(doc, &["code", "commandId", "protocol"]) || string(doc, "protocol")? != PROTOCOL {
        return None;
    }
    match doc.member("commandId")? {
        Value::Null => {}
        Value::String(id) if *id == request.command_id => {}
        _ => return None,
    }
    let code = string(doc, "code")?;
    let entry = registry.codes.get(code)?;
    if entry.http != http || !entry.operations.contains(&request.operation) {
        return None;
    }
    Some(if entry.class == "indeterminate" {
        uncertain(code)
    } else {
        Classified::Refused {
            code: code.to_owned(),
            class: entry.class.clone(),
        }
    })
}

/// §13.1: what one observation of `(state, ownerBound)` means for Web. It
/// never moves Web's state; it only sets or clears needs attention.
pub fn assess_ownership(state: &str, owner_bound: bool) -> &'static str {
    match (state, owner_bound) {
        ("completed", true) => "bound",
        ("completed", false) => "attention:OWNERSHIP_LOST",
        ("revoked", true) => "attention:UNEXPECTED_BINDING",
        (_, true) => "bound-informational",
        (_, false) => "unbound",
    }
}

/// One transmission of a create job, as Web journals it (§13.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transmission {
    pub issued_at: u64,
    pub expires_at: u64,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Operation { operation_id: String, state: String },
    Refused { code: String },
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Unresolved,
    Resolved { operation_id: String, state: String },
    Failed { code: String },
    Attention { code: String },
}

fn class(entry: Option<&RegisteredCode>) -> Option<&str> {
    entry.map(|e| e.class.as_str())
}

/// Seconds after `expiresAt` (Web clock) after which no copy of a
/// transmission can start executing on a conforming harness (§9.3).
pub const ACCEPTANCE_HORIZON_SECONDS: u64 = 60;

/// §13.2 R1–R3 over every transmission of one create job.
pub fn resolve_create_job(registry: &Registry, transmissions: &[Transmission]) -> Resolution {
    // R1, with two operation ids for one key as an anomaly.
    let operations: Vec<(&str, &str)> = transmissions
        .iter()
        .filter_map(|t| match &t.outcome {
            Outcome::Operation {
                operation_id,
                state,
            } => Some((operation_id.as_str(), state.as_str())),
            _ => None,
        })
        .collect();
    if let Some((first, _)) = operations.first() {
        if operations.iter().any(|(id, _)| id != first) {
            return Resolution::Attention {
                code: "OPERATION_MISMATCH".to_owned(),
            };
        }
        let (id, state) = operations.last().expect("non-empty");
        return Resolution::Resolved {
            operation_id: (*id).to_owned(),
            state: (*state).to_owned(),
        };
    }
    let refusals: Vec<(&Transmission, &str, Option<&RegisteredCode>)> = transmissions
        .iter()
        .filter_map(|t| match &t.outcome {
            Outcome::Refused { code } => Some((t, code.as_str(), registry.codes.get(code))),
            _ => None,
        })
        .collect();
    // A Web defect or an unknown code is never a failure.
    if let Some((_, code, _)) = refusals
        .iter()
        .find(|(_, code, entry)| *code == "IDEMPOTENCY_CONFLICT" || entry.is_none())
    {
        return Resolution::Attention {
            code: (*code).to_owned(),
        };
    }
    // R2.
    if let Some((_, code, _)) = refusals
        .iter()
        .rev()
        .find(|(_, _, entry)| class(*entry) == Some("payload"))
    {
        return Resolution::Failed {
            code: (*code).to_owned(),
        };
    }
    // R3.
    let uncertain: Vec<&Transmission> = transmissions
        .iter()
        .filter(|t| t.outcome == Outcome::Uncertain)
        .collect();
    if uncertain.is_empty() {
        return match refusals.last() {
            Some((_, code, _)) => Resolution::Failed {
                code: (*code).to_owned(),
            },
            None => Resolution::Unresolved,
        };
    }
    let merits: Vec<&(&Transmission, &str, Option<&RegisteredCode>)> = refusals
        .iter()
        .filter(|(_, _, entry)| class(*entry) == Some("merits"))
        .collect();
    let ruled_out = uncertain.iter().all(|u| {
        merits
            .iter()
            .any(|(t, _, _)| t.issued_at >= u.expires_at + ACCEPTANCE_HORIZON_SECONDS)
    });
    match merits.iter().max_by_key(|(t, _, _)| t.issued_at) {
        Some((_, code, _)) if ruled_out => Resolution::Failed {
            code: (*code).to_owned(),
        },
        _ => Resolution::Unresolved,
    }
}
