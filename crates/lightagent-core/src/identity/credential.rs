//! Installation-issued API credentials.
//!
//! A token is `la_<key id>_<secret>`: a public 16-hex-digit key id, used to
//! find the record, and a 64-hex-digit secret, 256 bits of OS entropy. Only a
//! verifier is stored: `v1:` and the SHA-256 of a domain-separation label, the
//! key id and the secret. A fast hash is the right tool for a uniformly
//! random 256-bit secret: a slow password hash protects *guessable* secrets,
//! and these are not; inverting the digest is a 2^256 search. The label and
//! key id bind a verifier to this purpose and this record, so a digest copied
//! between records or reused from another system verifies nothing. A server
//! key (HMAC pepper) is not used: it adds key management and would protect
//! only against brute force that 256 bits already rules out. The comparison is
//! constant-time. The plaintext exists once, in the [`IssuedToken`] returned
//! at issue.

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
    /// `v1:` and the lowercase hex SHA-256 of the domain-separated secret
    /// (see the module documentation). Never the secret.
    pub verifier: String,
    #[serde(default)]
    pub restriction: Restriction,
    pub created_unix: u64,
    /// Set when revoked; a revoked credential never authenticates again.
    #[serde(default)]
    pub revoked_unix: Option<u64>,
    /// When set, the credential stops authenticating at this time.
    #[serde(default)]
    pub expires_unix: Option<u64>,
    /// The credential this one replaced by rotation, if any.
    #[serde(default)]
    pub replaces: Option<String>,
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
    #[error("the credential is revoked")]
    Revoked,
    #[error("an expiry must be in the future")]
    Expiry,
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
        self.issue_expiring(principal, role, restriction, now_unix, None)
            .map(|(_, token)| token)
    }

    /// Issue a credential that stops authenticating at `expires_unix`.
    /// Returns its key id and the token, once.
    pub fn issue_expiring(
        &mut self,
        principal: PrincipalId,
        role: Role,
        restriction: Restriction,
        now_unix: u64,
        expires_unix: Option<u64>,
    ) -> Result<(String, IssuedToken), CredentialError> {
        if expires_unix.is_some_and(|at| at <= now_unix) {
            return Err(CredentialError::Expiry);
        }
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
                verifier: verifier(&key_id, &secret),
                restriction,
                created_unix: now_unix,
                revoked_unix: None,
                expires_unix,
                replaces: None,
            },
        );
        let token = IssuedToken(format!("{PREFIX}{key_id}_{secret}"));
        Ok((key_id, token))
    }

    /// Issue a replacement for `key_id` with the same principal, role and
    /// restriction. The old credential keeps working until it is revoked, so
    /// a client can switch over first.
    pub fn rotate(
        &mut self,
        key_id: &str,
        now_unix: u64,
        expires_unix: Option<u64>,
    ) -> Result<(String, IssuedToken), CredentialError> {
        let old = self
            .credentials
            .get(key_id)
            .ok_or(CredentialError::Unknown)?;
        if old.revoked_unix.is_some() {
            return Err(CredentialError::Revoked);
        }
        let (principal, role, restriction) =
            (old.principal.clone(), old.role, old.restriction.clone());
        let (new_id, token) =
            self.issue_expiring(principal, role, restriction, now_unix, expires_unix)?;
        if let Some(record) = self.credentials.get_mut(&new_id) {
            record.replaces = Some(key_id.to_owned());
        }
        Ok((new_id, token))
    }

    /// Revoke every credential of `principal`. Returns the key ids revoked.
    pub fn revoke_principal(&mut self, principal: &PrincipalId, now_unix: u64) -> Vec<String> {
        self.credentials
            .values_mut()
            .filter(|record| &record.principal == principal && record.revoked_unix.is_none())
            .map(|record| {
                record.revoked_unix = Some(now_unix);
                record.key_id.clone()
            })
            .collect()
    }

    /// Every record, by key id. Records hold no secret.
    pub fn records(&self) -> impl Iterator<Item = &CredentialRecord> {
        self.credentials.values()
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
        self.authenticate_at(token, now_unix())
    }

    /// [`authenticate`](Self::authenticate) at `now_unix`: an expired
    /// credential is `Expired`.
    pub fn authenticate_at(&self, token: &str, now_unix: u64) -> Result<Caller, Denial> {
        let (key_id, secret) = parse(token).ok_or(Denial::Unauthenticated)?;
        let record = self
            .credentials
            .get(key_id)
            .ok_or(Denial::Unauthenticated)?;
        if !constant_time_eq(
            verifier(key_id, secret).as_bytes(),
            record.verifier.as_bytes(),
        ) {
            return Err(Denial::Unauthenticated);
        }
        Self::caller_from(record, now_unix)
    }

    /// The caller for an already-authenticated credential, read afresh: a
    /// long-lived connection calls this to notice revocation, expiry or a
    /// changed restriction. A missing or revoked record is `Revoked`.
    pub fn caller_for(&self, key_id: &str) -> Result<Caller, Denial> {
        self.caller_for_at(key_id, now_unix())
    }

    /// [`caller_for`](Self::caller_for) at `now_unix`: an expired credential
    /// is `Expired`.
    pub fn caller_for_at(&self, key_id: &str, now_unix: u64) -> Result<Caller, Denial> {
        let record = self.credentials.get(key_id).ok_or(Denial::Revoked)?;
        Self::caller_from(record, now_unix)
    }

    fn caller_from(record: &CredentialRecord, now_unix: u64) -> Result<Caller, Denial> {
        if record.revoked_unix.is_some() {
            return Err(Denial::Revoked);
        }
        if record.expires_unix.is_some_and(|at| now_unix >= at) {
            return Err(Denial::Expired);
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

/// Separates these verifiers from every other use of SHA-256.
const VERIFIER_LABEL: &[u8] = b"lightagent api credential verifier v1\0";

/// `v1:` and SHA-256(label || key id || 0 || secret), in lowercase hex.
fn verifier(key_id: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(VERIFIER_LABEL);
    hasher.update(key_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(secret.as_bytes());
    let hash = hasher.finalize();
    let bytes: &[u8] = hash.as_ref();
    format!("v1:{}", hex(bytes))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Equal-length digests compared without an early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
