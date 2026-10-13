//! Installation-issued API credentials.
//!
//! A token is `la_<key id>_<secret>`: a public 16-hex-digit key id, used to
//! find the record, and a 64-hex-digit secret, 256 bits of OS entropy. Only the
//! secret's SHA-256 is stored. A fast hash is the right tool for a
//! high-entropy random secret (a slow password hash protects guessable
//! secrets, which these are not), and the comparison is constant-time. The
//! plaintext exists once, in the [`IssuedToken`] returned at issue.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Caller, Denial, Principal, PrincipalId, PrincipalSource, Restriction, Role};
use crate::ids::hex;

const PREFIX: &str = "la_";
const KEY_ID_BYTES: usize = 8;
const SECRET_BYTES: usize = 32;

/// A stored credential. Holds no secret: only the secret's digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub key_id: String,
    pub principal: PrincipalId,
    pub role: Role,
    /// Lowercase hex SHA-256 of the secret part of the token.
    pub secret_sha256: String,
    #[serde(default)]
    pub restriction: Restriction,
    pub created_unix: u64,
    /// Set when revoked; a revoked credential never authenticates again.
    #[serde(default)]
    pub revoked_unix: Option<u64>,
}

/// The plaintext token, shown once. Its `Debug` never prints the secret, and
/// it is deliberately not serializable.
pub struct IssuedToken(String);

impl IssuedToken {
    /// The token, to hand to the caller exactly once.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IssuedToken(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// No entropy: a credential is never issued from a predictable secret.
    #[error("the OS entropy source is unavailable")]
    Entropy,
    #[error("no such credential")]
    Unknown,
}

/// Every credential this installation issued, keyed by key id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStore {
    #[serde(default)]
    credentials: BTreeMap<String, CredentialRecord>,
}

impl CredentialStore {
    /// Issue a credential for `principal`. Returns the token once; the store
    /// keeps only its digest.
    pub fn issue(
        &mut self,
        principal: PrincipalId,
        role: Role,
        restriction: Restriction,
        now_unix: u64,
    ) -> Result<IssuedToken, CredentialError> {
        let mut key = [0u8; KEY_ID_BYTES];
        let mut secret = [0u8; SECRET_BYTES];
        getrandom::fill(&mut key).map_err(|_| CredentialError::Entropy)?;
        getrandom::fill(&mut secret).map_err(|_| CredentialError::Entropy)?;
        let key_id = hex(&key);
        if self.credentials.contains_key(&key_id) {
            // 64 random bits colliding means the entropy source is broken.
            return Err(CredentialError::Entropy);
        }
        let secret = hex(&secret);
        self.credentials.insert(
            key_id.clone(),
            CredentialRecord {
                key_id: key_id.clone(),
                principal,
                role,
                secret_sha256: digest(&secret),
                restriction,
                created_unix: now_unix,
                revoked_unix: None,
            },
        );
        Ok(IssuedToken(format!("{PREFIX}{key_id}_{secret}")))
    }

    /// Revoke a credential. Takes effect at the next request it is used on.
    pub fn revoke(&mut self, key_id: &str, now_unix: u64) -> Result<(), CredentialError> {
        let record = self
            .credentials
            .get_mut(key_id)
            .ok_or(CredentialError::Unknown)?;
        record.revoked_unix.get_or_insert(now_unix);
        Ok(())
    }

    pub fn get(&self, key_id: &str) -> Option<&CredentialRecord> {
        self.credentials.get(key_id)
    }

    /// Authenticate a presented bearer token. A malformed, unknown or wrong
    /// token is `Unauthenticated`; a correct but revoked one is `Revoked`.
    pub fn authenticate(&self, token: &str) -> Result<Caller, Denial> {
        let (key_id, secret) = parse(token).ok_or(Denial::Unauthenticated)?;
        let record = self
            .credentials
            .get(key_id)
            .ok_or(Denial::Unauthenticated)?;
        if !constant_time_eq(digest(secret).as_bytes(), record.secret_sha256.as_bytes()) {
            return Err(Denial::Unauthenticated);
        }
        Self::caller_from(record)
    }

    /// The caller for an already-authenticated credential, read afresh: a
    /// long-lived connection calls this to notice revocation or a changed
    /// restriction. A missing or revoked record is `Revoked`.
    pub fn caller_for(&self, key_id: &str) -> Result<Caller, Denial> {
        let record = self.credentials.get(key_id).ok_or(Denial::Revoked)?;
        Self::caller_from(record)
    }

    fn caller_from(record: &CredentialRecord) -> Result<Caller, Denial> {
        if record.revoked_unix.is_some() {
            return Err(Denial::Revoked);
        }
        Ok(Caller {
            principal: Principal {
                id: record.principal.clone(),
                role: record.role,
                source: PrincipalSource::Credential {
                    key_id: record.key_id.clone(),
                },
            },
            restriction: record.restriction.clone(),
        })
    }
}

/// Split `la_<16 hex>_<64 hex>`; anything else is not a token.
fn parse(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix(PREFIX)?;
    let (key_id, secret) = rest.split_once('_')?;
    let lower_hex = |value: &str, len: usize| {
        value.len() == len
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (lower_hex(key_id, KEY_ID_BYTES * 2) && lower_hex(secret, SECRET_BYTES * 2))
        .then_some((key_id, secret))
}

fn digest(secret: &str) -> String {
    let hash = Sha256::digest(secret.as_bytes());
    let bytes: &[u8] = hash.as_ref();
    hex(bytes)
}

/// Equal-length digests compared without an early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
