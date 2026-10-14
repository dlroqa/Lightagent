//! An independent reading of the `LA-WEB-PROVISIONING/1` protocol layer
//! (candidate `la-web-provisioning-1/r1`, specification §4–§10, steps 1–12),
//! written from the specification text to check Web's fixtures from Rust.
//!
//! **L0 only.** Nothing in the workspace depends on this crate. It mounts no
//! route, holds no key, records nothing on disk and never reaches
//! `ProvisioningService`. A production gateway is a later, separately
//! approved stage.
//!
//! Every refusal is the first failing step's code, as §10 requires.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

pub mod continuity;
pub mod model;
pub mod ordering;
pub mod web;

pub const PROTOCOL: &str = "LA-WEB-PROVISIONING/1";
pub const PURPOSE: &str = "provisioning-command";
pub const MAX_MESSAGE_BYTES: usize = 8192;
pub const MAX_NESTING_DEPTH: usize = 16;
pub const MAX_LIFETIME_SECONDS: u64 = 120;
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 30;
/// §6.1: protocol and purpose, each followed by `0x00`.
pub const SIGNING_CONTEXT: &[u8] = b"LA-WEB-PROVISIONING/1\0provisioning-command\0";
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const OPERATIONS: [&str; 3] = ["profile.create", "profile.status", "profile.revoke"];
/// §7, in JCS name order.
const ENVELOPE: [&str; 13] = [
    "audience",
    "commandId",
    "expiresAt",
    "issuedAt",
    "jobId",
    "keyId",
    "nonce",
    "operation",
    "payload",
    "protocol",
    "purpose",
    "requestId",
    "signature",
];
const CREATE_PAYLOAD: [&str; 6] = [
    "displayName",
    "idempotencyKey",
    "issuer",
    "profileId",
    "subject",
    "template",
];
const OPERATION_PAYLOAD: [&str; 2] = ["operationId", "profileId"];
/// The Ed25519 group order `L`, little-endian (RFC 8032 §5.1).
const GROUP_ORDER: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

/// A protocol error code (§11).
pub type Code = &'static str;

/// A parsed JSON value. Numbers keep their exact lexeme (§4.2), and object
/// members keep their order and duplicates, so later steps can judge both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    fn member(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

// --- §4.1, §4.2 strict JSON ----------------------------------------------------

/// Parse one strict JSON text: UTF-8 without a BOM, RFC 8259 syntax, no lone
/// surrogate escapes, depth ≤ 16. Duplicates are kept for [`has_duplicate`].
pub fn parse_strict(body: &[u8]) -> Result<Value, Code> {
    const MALFORMED: Code = "MALFORMED_JSON";
    if body.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(MALFORMED);
    }
    // `from_utf8` refuses overlong forms, encoded surrogates and truncation.
    let text = std::str::from_utf8(body).map_err(|_| MALFORMED)?;
    let mut parser = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    parser.ws();
    let value = parser.value(1).map_err(|()| MALFORMED)?;
    parser.ws();
    if parser.i != parser.s.len() {
        return Err(MALFORMED);
    }
    Ok(value)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) -> Result<u8, ()> {
        let b = self.peek().ok_or(())?;
        self.i += 1;
        Ok(b)
    }

    fn expect(&mut self, b: u8) -> Result<(), ()> {
        if self.bump()? == b { Ok(()) } else { Err(()) }
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// `depth` is the depth a container starting here would have.
    fn value(&mut self, depth: usize) -> Result<Value, ()> {
        match self.peek().ok_or(())? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::String),
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(()),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, ()> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(())
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, ()> {
        if depth > MAX_NESTING_DEPTH {
            return Err(());
        }
        self.expect(b'{')?;
        let mut members = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.ws();
            let name = self.string()?;
            self.ws();
            self.expect(b':')?;
            self.ws();
            let value = self.value(depth + 1)?;
            members.push((name, value));
            self.ws();
            match self.bump()? {
                b',' => continue,
                b'}' => return Ok(Value::Object(members)),
                _ => return Err(()),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, ()> {
        if depth > MAX_NESTING_DEPTH {
            return Err(());
        }
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.bump()? {
                b',' => continue,
                b']' => return Ok(Value::Array(items)),
                _ => return Err(()),
            }
        }
    }

    fn string(&mut self) -> Result<String, ()> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.bump()? {
                b'"' => break,
                b'\\' => {
                    let c = match self.bump()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err(()),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b if b < 0x20 => return Err(()),
                // Bytes of a multi-byte character are all ≥ 0x80, so copying
                // them one by one keeps the (already validated) UTF-8.
                b => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| ())
    }

    fn hex4(&mut self) -> Result<u32, ()> {
        let digits = self.s.get(self.i..self.i + 4).ok_or(())?;
        let text = std::str::from_utf8(digits).map_err(|_| ())?;
        if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(());
        }
        self.i += 4;
        u32::from_str_radix(text, 16).map_err(|_| ())
    }

    /// After `\u`: one scalar value, from a BMP escape or a surrogate pair.
    fn unicode_escape(&mut self) -> Result<char, ()> {
        let first = self.hex4()?;
        let scalar = match first {
            0xd800..=0xdbff => {
                self.expect(b'\\')?;
                self.expect(b'u')?;
                let low = self.hex4()?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return Err(());
                }
                0x10000 + ((first - 0xd800) << 10) + (low - 0xdc00)
            }
            0xdc00..=0xdfff => return Err(()),
            other => other,
        };
        char::from_u32(scalar).ok_or(())
    }

    fn number(&mut self) -> Result<Value, ()> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.bump()? {
            b'0' => {}
            b'1'..=b'9' => self.digits(),
            _ => return Err(()),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            self.some_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            self.some_digits()?;
        }
        let lexeme = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| ())?;
        Ok(Value::Number(lexeme.to_owned()))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn some_digits(&mut self) -> Result<(), ()> {
        let start = self.i;
        self.digits();
        if self.i == start { Err(()) } else { Ok(()) }
    }
}

/// §4.2: a member name repeated in any one object, compared after unescaping.
pub fn has_duplicate(value: &Value) -> bool {
    match value {
        Value::Object(members) => {
            let mut seen = BTreeSet::new();
            members
                .iter()
                .any(|(name, value)| !seen.insert(name.as_str()) || has_duplicate(value))
        }
        Value::Array(items) => items.iter().any(has_duplicate),
        _ => false,
    }
}

/// §4.3: only objects, strings and non-negative safe integers.
pub fn in_value_profile(value: &Value) -> bool {
    match value {
        Value::Object(members) => members.iter().all(|(_, v)| in_value_profile(v)),
        Value::String(_) => true,
        Value::Number(lexeme) => safe_integer(lexeme).is_some(),
        Value::Null | Value::Bool(_) | Value::Array(_) => false,
    }
}

fn safe_integer(lexeme: &str) -> Option<u64> {
    let canonical = lexeme == "0"
        || (lexeme
            .as_bytes()
            .first()
            .is_some_and(|b| (b'1'..=b'9').contains(b))
            && lexeme.bytes().all(|b| b.is_ascii_digit()));
    if !canonical || lexeme.len() > 16 {
        return None;
    }
    lexeme
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= MAX_SAFE_INTEGER)
}

// --- §5 canonical form -------------------------------------------------------

/// RFC 8785 serialization of a value inside the §4.3 profile.
pub fn jcs(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_jcs(value, &mut out);
    out
}

fn write_jcs(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(members) => {
            let mut sorted: Vec<&(String, Value)> = members.iter().collect();
            sorted.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push(b'{');
            for (i, (name, value)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_jcs_string(name, out);
                out.push(b':');
                write_jcs(value, out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_jcs(item, out);
            }
            out.push(b']');
        }
        Value::String(s) => write_jcs_string(s, out),
        // Inside the profile every number is a canonical integer lexeme.
        Value::Number(lexeme) => out.extend_from_slice(lexeme.as_bytes()),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Null => out.extend_from_slice(b"null"),
    }
}

fn write_jcs_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\r' => out.extend_from_slice(b"\\r"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c => {
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

/// §9.1: `SHA-256("LA-WEB-PROVISIONING/1" 0 "payload" 0 operation 0 ‖ JCS(payload))`.
pub fn payload_hash(operation: &str, payload: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"LA-WEB-PROVISIONING/1\0payload\0");
    hasher.update(operation.as_bytes());
    hasher.update([0]);
    hasher.update(jcs(payload));
    hex_lower(&hasher.finalize())
}

pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- §4.4, §4.5, §8.1 field rules ---------------------------------------------

/// Strict base64url without padding: exactly the length `bytes` needs, and
/// zero padding bits, so each byte string has one encoding.
pub fn base64url(text: &str, bytes: usize) -> Option<Vec<u8>> {
    if text.len() != (bytes * 8).div_ceil(6) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (out.len() == bytes && acc == 0).then_some(out)
}

fn prefixed_id(value: &Value, prefix: &str) -> bool {
    value
        .str()
        .and_then(|s| s.strip_prefix(prefix))
        .is_some_and(|rest| {
            (8..=60).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn key_id(value: &Value) -> bool {
    value.str().is_some_and(|s| {
        (3..=64).contains(&s.len())
            && s.bytes().enumerate().all(|(i, b)| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || (i > 0 && matches!(b, b'.' | b'_' | b'-'))
            })
    })
}

/// Integer Unix seconds with exactly 10 digits.
fn timestamp(value: &Value) -> Option<u64> {
    match value {
        Value::Number(lexeme) if lexeme.len() == 10 => safe_integer(lexeme),
        _ => None,
    }
}

/// `https://` + lowercase DNS host + optional `:port` (1–65535), ≤ 256 bytes.
fn issuer(value: &Value) -> bool {
    let Some(s) = value.str() else { return false };
    let Some(rest) = s.strip_prefix("https://") else {
        return false;
    };
    if s.len() > 256 {
        return false;
    }
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    let port_ok = port.is_none_or(|p| {
        !p.is_empty()
            && p.len() <= 5
            && !p.starts_with('0')
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
    });
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    port_ok && host_ok
}

/// §8.1: NFC; 1–64 scalar values; no Cc; no `White_Space` at either end.
/// `char::is_control` is exactly general category Cc, and
/// `char::is_whitespace` is exactly `White_Space`.
pub fn display_name(s: &str) -> bool {
    let count = s.chars().count();
    (1..=64).contains(&count)
        && !s.chars().any(char::is_control)
        && !s.starts_with(char::is_whitespace)
        && !s.ends_with(char::is_whitespace)
        && icu_normalizer::ComposingNormalizerBorrowed::new_nfc().is_normalized(s)
}

fn idempotency_key(value: &Value) -> bool {
    value.str().is_some_and(|s| {
        (16..=128).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn operation_id(value: &Value) -> bool {
    value.str().is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

// --- §10 verification ----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key: [u8; 32],
    pub revoked: bool,
}

/// One harness's verifier state: its id, its trusted keys, its clock and the
/// nonces it has recorded. In memory only.
#[derive(Clone, Debug)]
pub struct Verifier {
    pub harness_id: String,
    pub keys: Vec<TrustedKey>,
    pub now: u64,
    pub seen_nonces: BTreeSet<String>,
}

/// A command that passed steps 1–12.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub operation: String,
    pub command: Value,
    pub payload_hash: String,
    /// `JCS(command without "signature")`.
    pub unsigned_canonical: Vec<u8>,
}

impl Verifier {
    /// Steps 1 (size only) to 12. Records the nonce on success.
    pub fn verify(&mut self, body: &[u8]) -> Result<Verified, Code> {
        // 1
        if body.len() > MAX_MESSAGE_BYTES {
            return Err("PAYLOAD_TOO_LARGE");
        }
        // 2
        let doc = parse_strict(body)?;
        // 3
        if has_duplicate(&doc) {
            return Err("DUPLICATE_KEY");
        }
        // 4
        if !in_value_profile(&doc) {
            return Err("INVALID_JSON_VALUE");
        }
        // 5
        let Value::Object(members) = &doc else {
            return Err("INVALID_ENVELOPE");
        };
        match doc.member("protocol") {
            Some(Value::String(p)) if p == PROTOCOL => {}
            Some(Value::String(_)) => return Err("UNSUPPORTED_PROTOCOL"),
            _ => return Err("INVALID_ENVELOPE"),
        }
        // 6
        closed(members, &ENVELOPE)?;
        let field = |name: &str| doc.member(name).expect("closed envelope");
        // 7
        let purpose = field("purpose").str().ok_or("INVALID_FIELD")?;
        if purpose != PURPOSE {
            return Err("WRONG_PURPOSE");
        }
        let operation = field("operation").str().ok_or("INVALID_FIELD")?;
        if !OPERATIONS.contains(&operation) {
            return Err("UNSUPPORTED_OPERATION");
        }
        let checks: [(&str, bool); 10] = [
            ("audience", prefixed_id(field("audience"), "hns_")),
            ("commandId", prefixed_id(field("commandId"), "cmd_")),
            ("expiresAt", timestamp(field("expiresAt")).is_some()),
            ("issuedAt", timestamp(field("issuedAt")).is_some()),
            ("jobId", prefixed_id(field("jobId"), "prv_")),
            ("keyId", key_id(field("keyId"))),
            (
                "nonce",
                field("nonce")
                    .str()
                    .and_then(|n| base64url(n, 32))
                    .is_some(),
            ),
            ("payload", matches!(field("payload"), Value::Object(_))),
            ("requestId", prefixed_id(field("requestId"), "req_")),
            (
                "signature",
                field("signature")
                    .str()
                    .and_then(|s| base64url(s, 64))
                    .is_some(),
            ),
        ];
        if checks.iter().any(|(_, ok)| !ok) {
            return Err("INVALID_FIELD");
        }
        // 8
        let payload = field("payload");
        let Value::Object(payload_members) = payload else {
            unreachable!("checked in step 7")
        };
        if operation == "profile.create" {
            closed(payload_members, &CREATE_PAYLOAD)?;
            let p = |name: &str| payload.member(name).expect("closed payload");
            let rules: [(bool, Code); 6] = [
                (
                    p("displayName").str().is_some_and(display_name),
                    "INVALID_DISPLAY_NAME",
                ),
                (
                    idempotency_key(p("idempotencyKey")),
                    "INVALID_IDEMPOTENCY_KEY",
                ),
                (issuer(p("issuer")), "INVALID_ISSUER"),
                (prefixed_id(p("profileId"), "agt_"), "INVALID_PROFILE_ID"),
                (prefixed_id(p("subject"), "usr_"), "INVALID_SUBJECT"),
                (p("template").str() == Some("assistant"), "INVALID_TEMPLATE"),
            ];
            if let Some((_, code)) = rules.iter().find(|(ok, _)| !ok) {
                return Err(code);
            }
        } else {
            closed(payload_members, &OPERATION_PAYLOAD)?;
            let p = |name: &str| payload.member(name).expect("closed payload");
            if !operation_id(p("operationId")) {
                return Err("INVALID_OPERATION_ID");
            }
            if !prefixed_id(p("profileId"), "agt_") {
                return Err("INVALID_PROFILE_ID");
            }
        }
        let issued = timestamp(field("issuedAt")).expect("step 7");
        let expires = timestamp(field("expiresAt")).expect("step 7");
        if !(expires > issued && expires - issued <= MAX_LIFETIME_SECONDS) {
            return Err("INVALID_LIFETIME");
        }
        // 9
        if jcs(&doc) != body {
            return Err("NON_CANONICAL");
        }
        // 10
        let key_id = field("keyId").str().expect("step 7");
        let key = self
            .keys
            .iter()
            .find(|k| k.key_id == key_id)
            .ok_or("UNKNOWN_KEY")?;
        if key.revoked {
            return Err("KEY_REVOKED");
        }
        // 11
        let signature = base64url(field("signature").str().expect("step 7"), 64).expect("step 7");
        let unsigned = Value::Object(
            members
                .iter()
                .filter(|(name, _)| name != "signature")
                .cloned()
                .collect(),
        );
        let unsigned_canonical = jcs(&unsigned);
        let mut signing_input = SIGNING_CONTEXT.to_vec();
        signing_input.extend_from_slice(&unsigned_canonical);
        if !scalar_is_canonical(&signature[32..]) {
            return Err("BAD_SIGNATURE");
        }
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.public_key)
            .verify(&signing_input, &signature)
            .map_err(|_| "BAD_SIGNATURE")?;
        // 12
        if field("audience").str() != Some(self.harness_id.as_str()) {
            return Err("WRONG_AUDIENCE");
        }
        if issued > self.now + MAX_CLOCK_SKEW_SECONDS {
            return Err("NOT_YET_VALID");
        }
        if self.now >= expires + MAX_CLOCK_SKEW_SECONDS {
            return Err("EXPIRED");
        }
        let nonce = field("nonce").str().expect("step 7");
        if !self.seen_nonces.insert(nonce.to_owned()) {
            return Err("REPLAY_DETECTED");
        }
        Ok(Verified {
            operation: operation.to_owned(),
            payload_hash: payload_hash(operation, payload),
            command: doc.clone(),
            unsigned_canonical,
        })
    }
}

/// Unknown members first, then missing ones (§10 steps 6 and 8).
fn closed(members: &[(String, Value)], allowed: &[&str]) -> Result<(), Code> {
    if members
        .iter()
        .any(|(name, _)| !allowed.contains(&name.as_str()))
    {
        return Err("UNKNOWN_FIELD");
    }
    if allowed
        .iter()
        .any(|name| !members.iter().any(|(m, _)| m == name))
    {
        return Err("MISSING_FIELD");
    }
    Ok(())
}

/// RFC 8032 §5.1.7: the scalar `S` (little-endian) must be below `L`.
/// Checked here explicitly rather than trusted to the library (§6.2).
pub fn scalar_is_canonical(s: &[u8]) -> bool {
    for i in (0..32).rev() {
        match s[i].cmp(&GROUP_ORDER[i]) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // `ring` also refuses S ≥ L today, so the fixture
    // `signature-noncanonical-s` cannot tell whether this check exists. Test
    // it on its own (§6.2 requires it explicitly).
    #[test]
    fn the_scalar_must_be_below_the_group_order() {
        let mut below = GROUP_ORDER;
        below[0] -= 1;
        assert!(scalar_is_canonical(&below));
        assert!(scalar_is_canonical(&[0; 32]));
        assert!(!scalar_is_canonical(&GROUP_ORDER));
        let mut above = GROUP_ORDER;
        above[0] += 1;
        assert!(!scalar_is_canonical(&above));
        assert!(!scalar_is_canonical(&[0xff; 32]));
    }
}
