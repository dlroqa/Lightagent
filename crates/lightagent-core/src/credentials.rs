//! Profile-scoped remote credentials: outbound keys a profile's untrusted runs
//! may present to one remote service, encrypted at rest.
//!
//! Lightagent's own API credentials are verified, so they are stored as
//! one-way verifiers. Outbound credentials (a search API key, an MCP bearer
//! token) must be presented, so they are stored **encrypted** instead:
//!
//! - **Encryption.** AES-256-GCM from `ring` (already in the build for TLS).
//!   No custom cryptography. Each record has its own key, derived by
//!   HKDF-SHA256 from the master key and a random per-record salt. The
//!   additional authenticated data binds the ciphertext to its credential id,
//!   profile, service and version, so a sealed secret copied to another
//!   record or profile does not open.
//! - **Key material outside the home.** The 256-bit master key comes from
//!   `LIGHTAGENT_CREDENTIAL_KEY_FILE` (an owner-only file that must not be
//!   inside the Lightagent home) or `LIGHTAGENT_CREDENTIAL_KEY`. It is never
//!   written beside the records, so a copy or backup of the home does not
//!   hold a usable credential. Each record carries the key's fingerprint, so
//!   the wrong key is detected and refused.
//! - **Fail closed.** No key, the wrong key, a failed integrity check, or a
//!   corrupt or newer document means no credential, never plaintext and
//!   never another credential.
//! - **Grants, not ownership.** A grant names one profile, one service, the
//!   operations and destination origins it may be used for, an optional
//!   expiry and per-run request budget. Owning a profile grants nothing;
//!   a tool being available grants nothing.
//! - **Operator-only changes**, recorded in a bounded audit log of codes and
//!   identifiers.
//!
//! The metadata document lives at `<home>/credentials/state.json`, written
//! with the identity store's atomic, locked, owner-only machinery. An
//! installation that never adds a credential has no such file and behaves as
//! before.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::identity::store::{open_private, write_durable};
use crate::identity::{Caller, Capability, Denial, Role, authorize_capability};
use crate::ids::hex;
use crate::profile::ProfileId;

/// The document schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
/// The environment variable naming the master key file.
pub const KEY_FILE_ENV: &str = "LIGHTAGENT_CREDENTIAL_KEY_FILE";
/// The environment variable holding the master key itself (64 hex digits).
pub const KEY_ENV: &str = "LIGHTAGENT_CREDENTIAL_KEY";

const DIR: &str = "credentials";
const STATE: &str = "state.json";
const LOCK: &str = "lock";
const AUDIT: &str = "audit.jsonl";
const KEY_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const HKDF_INFO: &[u8] = b"lightagent-credential-v1";
const AAD_LABEL: &str = "lightagent-credential-v1";
const FINGERPRINT_LABEL: &[u8] = b"lightagent-credential-key-fingerprint-v1";
const MAX_SECRET_BYTES: usize = 8 * 1024;
const MAX_AUDIT_BYTES: u64 = 1024 * 1024;

/// Why the vault refused. Codes only: never a secret, path or record body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("credential storage is unavailable ({0:?})")]
    Io(io::ErrorKind),
    #[error("the credential state is corrupt")]
    Corrupt,
    #[error("credential schema {0} is newer than this build supports")]
    IncompatibleSchema(u32),
    #[error("no credential key is configured")]
    KeyMissing,
    #[error("the credential key is invalid")]
    KeyInvalid,
    #[error("the credential key file must be outside the Lightagent home")]
    KeyInsideHome,
    #[error("the credential key file must be readable by its owner only")]
    KeyPermissions,
    #[error("the credential was sealed with a different key")]
    KeyMismatch,
    #[error("the credential failed its integrity check")]
    Integrity,
    #[error("not authorized: {}", .0.code())]
    Denied(Denial),
    #[error("refused: {0}")]
    Refused(&'static str),
}

impl From<io::Error> for VaultError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// The 256-bit master key. Never printed, never serialized.
pub struct MasterKey([u8; KEY_BYTES]);

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

impl MasterKey {
    /// A fresh key from the OS entropy source.
    pub fn generate() -> Result<Self, VaultError> {
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| VaultError::KeyInvalid)?;
        Ok(Self(bytes))
    }

    /// Parse 64 hex digits.
    pub fn from_hex(text: &str) -> Result<Self, VaultError> {
        let bytes = unhex(text.trim()).ok_or(VaultError::KeyInvalid)?;
        let bytes: [u8; KEY_BYTES] = bytes.try_into().map_err(|_| VaultError::KeyInvalid)?;
        Ok(Self(bytes))
    }

    /// The key as 64 hex digits, to write to a key file once.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Read a key file. It must be outside `home` and, on Unix, readable by
    /// its owner only.
    pub fn from_file(path: &Path, home: &Path) -> Result<Self, VaultError> {
        let file = path.canonicalize()?;
        if let Ok(home) = home.canonicalize()
            && file.starts_with(&home)
        {
            return Err(VaultError::KeyInsideHome);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if std::fs::metadata(&file)?.permissions().mode() & 0o077 != 0 {
                return Err(VaultError::KeyPermissions);
            }
        }
        Self::from_hex(&std::fs::read_to_string(&file)?)
    }

    /// The key configured for this process: [`KEY_FILE_ENV`], else
    /// [`KEY_ENV`]. `Ok(None)` when neither is set.
    pub fn from_environment(home: &Path) -> Result<Option<Self>, VaultError> {
        if let Some(path) = std::env::var_os(KEY_FILE_ENV).filter(|value| !value.is_empty()) {
            return Self::from_file(Path::new(&path), home).map(Some);
        }
        match std::env::var(KEY_ENV) {
            Ok(text) if !text.is_empty() => Self::from_hex(&text).map(Some),
            _ => Ok(None),
        }
    }

    /// A short public fingerprint, recorded with every sealed secret.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(FINGERPRINT_LABEL);
        hasher.update(self.0);
        hex(&hasher.finalize()[..8])
    }
}

/// An outbound secret in memory. Never printed, never serialized.
pub struct ExternalSecret(String);

impl fmt::Debug for ExternalSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExternalSecret(<redacted>)")
    }
}

impl ExternalSecret {
    /// A secret to seal. Empty, over 8 KiB, or containing a line break (it
    /// is sent as a header value) is refused.
    pub fn new(value: impl Into<String>) -> Result<Self, VaultError> {
        let value = value.into();
        let value = value.trim().to_owned();
        if value.is_empty() || value.len() > MAX_SECRET_BYTES || value.contains(['\r', '\n', '\0'])
        {
            return Err(VaultError::Refused("invalid_secret"));
        }
        Ok(Self(value))
    }

    /// The value, at the point it is presented to the remote service.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// The remote service a credential is for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteService {
    /// The `web.search` backend.
    WebSearch,
    /// The HTTP MCP server configured under this name.
    Mcp { server: String },
}

impl RemoteService {
    /// Parse `web-search` or `mcp:<server>`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "web-search" => Some(Self::WebSearch),
            other => other
                .strip_prefix("mcp:")
                .filter(|server| {
                    (1..=64).contains(&server.len())
                        && server
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .map(|server| Self::Mcp {
                    server: server.to_owned(),
                }),
        }
    }

    /// The text form, as [`parse`](Self::parse) reads it.
    pub fn label(&self) -> String {
        match self {
            Self::WebSearch => "web-search".to_owned(),
            Self::Mcp { server } => format!("mcp:{server}"),
        }
    }
}

/// `scheme://host[:port]` of an `http(s)` URL, lowercased, without userinfo,
/// path or query. `None` for anything else.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// A sealed secret: AES-256-GCM under a per-record HKDF-derived key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSecret {
    pub key_fingerprint: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSecret")
            .field("key_fingerprint", &self.key_fingerprint)
            .finish_non_exhaustive()
    }
}

/// One credential grant. Holds no plaintext: the secret is sealed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialGrant {
    pub id: String,
    pub profile: ProfileId,
    pub service: RemoteService,
    /// Tool or operation names it may be used for; `None`: every operation
    /// of the service.
    #[serde(default)]
    pub operations: Option<BTreeSet<String>>,
    /// Origins (`https://host[:port]`) it may be sent to. Never empty.
    pub destinations: BTreeSet<String>,
    pub created_unix: u64,
    #[serde(default)]
    pub expires_unix: Option<u64>,
    #[serde(default)]
    pub revoked_unix: Option<u64>,
    /// Bumped by every rotation; a run holding an older version stops.
    pub version: u32,
    #[serde(default)]
    pub rotated_unix: Option<u64>,
    /// The most requests one run may make with it.
    #[serde(default)]
    pub max_requests_per_run: Option<u32>,
    pub sealed: SealedSecret,
}

impl CredentialGrant {
    /// Whether it may be used at `now`, and why not.
    pub fn state(&self, now: u64) -> Result<(), CredentialDenial> {
        if self.revoked_unix.is_some() {
            return Err(CredentialDenial::Revoked);
        }
        if self.expires_unix.is_some_and(|at| now >= at) {
            return Err(CredentialDenial::Expired);
        }
        Ok(())
    }

    fn aad(&self) -> String {
        format!(
            "{AAD_LABEL}\0{}\0{}\0{}\0{}",
            self.id,
            self.profile.as_str(),
            self.service.label(),
            self.version
        )
    }
}

/// The whole credential metadata document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialDocument {
    schema: u32,
    generation: u64,
    #[serde(default)]
    grants: BTreeMap<String, CredentialGrant>,
}

impl Default for CredentialDocument {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            generation: 0,
            grants: BTreeMap::new(),
        }
    }
}

impl CredentialDocument {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn grants(&self) -> impl Iterator<Item = &CredentialGrant> {
        self.grants.values()
    }

    pub fn get(&self, id: &str) -> Option<&CredentialGrant> {
        self.grants.get(id)
    }

    /// Every grant of `profile` for `service`, newest first.
    pub fn for_profile(
        &self,
        profile: &ProfileId,
        service: &RemoteService,
    ) -> Vec<&CredentialGrant> {
        let mut found: Vec<&CredentialGrant> = self
            .grants
            .values()
            .filter(|grant| &grant.profile == profile && &grant.service == service)
            .collect();
        found.sort_by_key(|grant| std::cmp::Reverse(grant.created_unix));
        found
    }
}

/// Why a credential may not be used for a request. Fixed codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialDenial {
    /// No grant for this profile and service.
    NotGranted,
    Revoked,
    Expired,
    /// Rotated since this run obtained it.
    Superseded,
    /// The destination's origin is not one the grant names.
    Destination,
    /// The operation is not one the grant names.
    Operation,
    /// The run's request budget for it is spent.
    Budget,
    /// The vault or its key is unavailable, or the record failed to open.
    Unavailable,
}

impl CredentialDenial {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotGranted => "not_granted",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
            Self::Destination => "destination",
            Self::Operation => "operation",
            Self::Budget => "budget",
            Self::Unavailable => "unavailable",
        }
    }
}

/// What a new grant allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSpec {
    pub profile: ProfileId,
    pub service: RemoteService,
    pub operations: Option<BTreeSet<String>>,
    pub destinations: BTreeSet<String>,
    pub expires_unix: Option<u64>,
    pub max_requests_per_run: Option<u32>,
}

/// One audit line: codes and identifiers only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialAudit {
    pub at_unix: u64,
    pub action: String,
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub profile: Option<ProfileId>,
    #[serde(default)]
    pub service: Option<String>,
    pub outcome: String,
}

/// The credential vault of one installation.
#[derive(Debug)]
pub struct CredentialVault {
    dir: PathBuf,
    local: Mutex<()>,
}

impl CredentialVault {
    /// The vault under `home` (`<home>/credentials`). Does no I/O.
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            dir: home.as_ref().join(DIR),
            local: Mutex::new(()),
        }
    }

    /// Whether any credential state has been written.
    pub fn exists(&self) -> bool {
        self.dir.join(STATE).exists()
    }

    /// Read the metadata. Absent: an empty document. Unreadable, corrupt or
    /// newer: an error, never an empty document.
    pub fn load(&self) -> Result<CredentialDocument, VaultError> {
        let bytes = match std::fs::read(self.dir.join(STATE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CredentialDocument::default());
            }
            Err(error) => return Err(error.into()),
        };
        let probe: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| VaultError::Corrupt)?;
        let schema = probe
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .ok_or(VaultError::Corrupt)?;
        if schema > u64::from(SCHEMA_VERSION) {
            return Err(VaultError::IncompatibleSchema(
                u32::try_from(schema).unwrap_or(u32::MAX),
            ));
        }
        if schema < u64::from(SCHEMA_VERSION) {
            return Err(VaultError::Corrupt);
        }
        serde_json::from_value(probe).map_err(|_| VaultError::Corrupt)
    }

    fn require_operator(actor: &Caller) -> Result<(), VaultError> {
        if actor.principal.role != Role::Operator {
            return Err(VaultError::Denied(Denial::MissingCapability));
        }
        authorize_capability(actor, Capability::Administer).map_err(VaultError::Denied)
    }

    /// Apply `change` under the cross-process lock and replace the document
    /// atomically. A failed change writes nothing.
    fn transact<R>(
        &self,
        actor: &Caller,
        change: impl FnOnce(&mut CredentialDocument) -> Result<R, VaultError>,
    ) -> Result<R, VaultError> {
        Self::require_operator(actor)?;
        let _local = self
            .local
            .lock()
            .map_err(|_| VaultError::Io(io::ErrorKind::Other))?;
        crate::paths::create_private_dir(&self.dir)
            .map_err(|_| VaultError::Io(io::ErrorKind::PermissionDenied))?;
        let lock = open_private(&self.dir.join(LOCK), false)?;
        lock.lock()?;
        let mut document = self.load()?;
        let value = change(&mut document)?;
        document.generation += 1;
        let bytes = serde_json::to_vec_pretty(&document).map_err(|_| VaultError::Corrupt)?;
        write_durable(&self.dir.join(STATE), &bytes)?;
        Ok(value)
    }

    /// Seal `secret` into a new grant described by `spec`.
    pub fn add(
        &self,
        actor: &Caller,
        now: u64,
        key: &MasterKey,
        spec: GrantSpec,
        secret: &ExternalSecret,
    ) -> Result<String, VaultError> {
        if spec.destinations.is_empty()
            || spec
                .destinations
                .iter()
                .any(|origin| origin_of(origin).as_deref() != Some(origin.as_str()))
        {
            return Err(VaultError::Refused("invalid_destination"));
        }
        if spec.expires_unix.is_some_and(|at| at <= now) {
            return Err(VaultError::Refused("expiry_in_past"));
        }
        if spec.max_requests_per_run == Some(0) {
            return Err(VaultError::Refused("invalid_budget"));
        }
        if spec
            .operations
            .as_ref()
            .is_some_and(|operations| operations.is_empty())
        {
            return Err(VaultError::Refused("invalid_operations"));
        }
        let result = self.transact(actor, |document| {
            let id = new_id()?;
            if document.grants.contains_key(&id) {
                return Err(VaultError::KeyInvalid);
            }
            let mut grant = CredentialGrant {
                id: id.clone(),
                profile: spec.profile.clone(),
                service: spec.service.clone(),
                operations: spec.operations.clone(),
                destinations: spec.destinations.clone(),
                created_unix: now,
                expires_unix: spec.expires_unix,
                revoked_unix: None,
                version: 1,
                rotated_unix: None,
                max_requests_per_run: spec.max_requests_per_run,
                sealed: SealedSecret {
                    key_fingerprint: String::new(),
                    salt: String::new(),
                    nonce: String::new(),
                    ciphertext: String::new(),
                },
            };
            grant.sealed = seal(key, &grant.aad(), secret)?;
            document.grants.insert(id.clone(), grant);
            Ok(id)
        });
        self.record(
            now,
            "credential_added",
            result.as_ref().ok().map(String::as_str),
            Some(&spec.profile),
            Some(&spec.service),
            &result,
        );
        result
    }

    /// Replace a grant's secret: a new version, sealed afresh. Runs holding
    /// the previous version stop using it at their next request.
    pub fn rotate(
        &self,
        actor: &Caller,
        now: u64,
        key: &MasterKey,
        id: &str,
        secret: &ExternalSecret,
    ) -> Result<u32, VaultError> {
        let mut subject = None;
        let result = self.transact(actor, |document| {
            let grant = document
                .grants
                .get_mut(id)
                .ok_or(VaultError::Refused("unknown_credential"))?;
            subject = Some((grant.profile.clone(), grant.service.clone()));
            if grant.revoked_unix.is_some() {
                return Err(VaultError::Refused("revoked"));
            }
            grant.version += 1;
            grant.rotated_unix = Some(now);
            grant.sealed = seal(key, &grant.aad(), secret)?;
            Ok(grant.version)
        });
        let (profile, service) = subject.unzip();
        self.record(
            now,
            "credential_rotated",
            Some(id),
            profile.as_ref(),
            service.as_ref(),
            &result,
        );
        result
    }

    /// Revoke a grant for good. Runs using it stop at their next request.
    pub fn revoke(&self, actor: &Caller, now: u64, id: &str) -> Result<(), VaultError> {
        let mut subject = None;
        let result = self.transact(actor, |document| {
            let grant = document
                .grants
                .get_mut(id)
                .ok_or(VaultError::Refused("unknown_credential"))?;
            subject = Some((grant.profile.clone(), grant.service.clone()));
            grant.revoked_unix.get_or_insert(now);
            Ok(())
        });
        let (profile, service) = subject.unzip();
        self.record(
            now,
            "credential_revoked",
            Some(id),
            profile.as_ref(),
            service.as_ref(),
            &result,
        );
        result
    }

    /// Re-seal every active grant from `old` to `new` in one transaction.
    /// Nothing changes unless every grant opens with `old`.
    pub fn rekey(
        &self,
        actor: &Caller,
        now: u64,
        old: &MasterKey,
        new: &MasterKey,
    ) -> Result<usize, VaultError> {
        let result = self.transact(actor, |document| {
            let mut count = 0;
            for grant in document.grants.values_mut() {
                if grant.revoked_unix.is_some() {
                    continue;
                }
                let secret = open(old, &grant.aad(), &grant.sealed)?;
                grant.sealed = seal(new, &grant.aad(), &secret)?;
                count += 1;
            }
            Ok(count)
        });
        self.record(now, "key_rotated", None, None, None, &result);
        result
    }

    /// Open the secret of `grant` (as read from the current document).
    pub fn open(
        &self,
        key: &MasterKey,
        grant: &CredentialGrant,
    ) -> Result<ExternalSecret, VaultError> {
        open(key, &grant.aad(), &grant.sealed)
    }

    /// Record a refused use. Never fails the caller.
    pub fn record_denial(
        &self,
        now: u64,
        id: Option<&str>,
        profile: &ProfileId,
        service: &RemoteService,
        denial: CredentialDenial,
    ) {
        self.append(&CredentialAudit {
            at_unix: now,
            action: "credential_use".to_owned(),
            credential: id.map(str::to_owned),
            profile: Some(profile.clone()),
            service: Some(service.label()),
            outcome: format!("denied:{}", denial.code()),
        });
    }

    fn record<R>(
        &self,
        now: u64,
        action: &str,
        id: Option<&str>,
        profile: Option<&ProfileId>,
        service: Option<&RemoteService>,
        result: &Result<R, VaultError>,
    ) {
        self.append(&CredentialAudit {
            at_unix: now,
            action: action.to_owned(),
            credential: id.map(str::to_owned),
            profile: profile.cloned(),
            service: service.map(RemoteService::label),
            outcome: match result {
                Ok(_) => "ok".to_owned(),
                Err(error) => format!("refused:{}", error_code(*error)),
            },
        });
    }

    fn append(&self, record: &CredentialAudit) {
        let Ok(mut line) = serde_json::to_vec(record) else {
            return;
        };
        line.push(b'\n');
        if crate::paths::create_private_dir(&self.dir).is_err() {
            return;
        }
        let path = self.dir.join(AUDIT);
        // Bounded: one rotated predecessor is kept.
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_AUDIT_BYTES) {
            let _ = std::fs::rename(&path, self.dir.join(format!("{AUDIT}.1")));
        }
        if let Ok(mut file) = open_private(&path, true) {
            let _ = file.write_all(&line);
        }
    }

    /// The audit trail, oldest first.
    pub fn audit(&self) -> Result<Vec<CredentialAudit>, VaultError> {
        let mut records = VecDeque::new();
        for name in [format!("{AUDIT}.1"), AUDIT.to_owned()] {
            let file = match std::fs::File::open(self.dir.join(name)) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for line in io::BufReader::new(file).lines() {
                if let Ok(record) = serde_json::from_str::<CredentialAudit>(&line?) {
                    records.push_back(record);
                }
            }
        }
        Ok(records.into())
    }

    /// Every file the vault writes (for privacy tests).
    pub fn files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }
}

fn error_code(error: VaultError) -> &'static str {
    match error {
        VaultError::Io(_) => "io",
        VaultError::Corrupt => "corrupt",
        VaultError::IncompatibleSchema(_) => "incompatible_schema",
        VaultError::KeyMissing => "key_missing",
        VaultError::KeyInvalid => "key_invalid",
        VaultError::KeyInsideHome => "key_inside_home",
        VaultError::KeyPermissions => "key_permissions",
        VaultError::KeyMismatch => "key_mismatch",
        VaultError::Integrity => "integrity",
        VaultError::Denied(denial) => denial.code(),
        VaultError::Refused(code) => code,
    }
}

fn new_id() -> Result<String, VaultError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| VaultError::KeyInvalid)?;
    Ok(hex(&bytes))
}

fn record_key(key: &MasterKey, salt: &[u8]) -> Result<LessSafeKey, VaultError> {
    let prk = Salt::new(HKDF_SHA256, salt).extract(&key.0);
    let info = [HKDF_INFO];
    let okm = prk
        .expand(&info, &AES_256_GCM)
        .map_err(|_| VaultError::Integrity)?;
    Ok(LessSafeKey::new(UnboundKey::from(okm)))
}

fn seal(key: &MasterKey, aad: &str, secret: &ExternalSecret) -> Result<SealedSecret, VaultError> {
    let mut salt = [0u8; SALT_BYTES];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| VaultError::KeyInvalid)?;
    getrandom::fill(&mut nonce).map_err(|_| VaultError::KeyInvalid)?;
    let sealing = record_key(key, &salt)?;
    let mut buffer = secret.expose().as_bytes().to_vec();
    sealing
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad.as_bytes()),
            &mut buffer,
        )
        .map_err(|_| VaultError::Integrity)?;
    Ok(SealedSecret {
        key_fingerprint: key.fingerprint(),
        salt: hex(&salt),
        nonce: hex(&nonce),
        ciphertext: hex(&buffer),
    })
}

fn open(key: &MasterKey, aad: &str, sealed: &SealedSecret) -> Result<ExternalSecret, VaultError> {
    if sealed.key_fingerprint != key.fingerprint() {
        return Err(VaultError::KeyMismatch);
    }
    let salt = unhex(&sealed.salt).ok_or(VaultError::Integrity)?;
    let nonce: [u8; NONCE_LEN] = unhex(&sealed.nonce)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(VaultError::Integrity)?;
    let mut buffer = unhex(&sealed.ciphertext).ok_or(VaultError::Integrity)?;
    let opening = record_key(key, &salt)?;
    let plain = opening
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad.as_bytes()),
            &mut buffer,
        )
        .map_err(|_| VaultError::Integrity)?;
    let text = std::str::from_utf8(plain).map_err(|_| VaultError::Integrity)?;
    ExternalSecret::new(text).map_err(|_| VaultError::Integrity)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// The time a credential's expiry is judged at, in Unix seconds.
pub type Clock = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

/// A run's use of one credential: re-checked before every request.
///
/// It holds the grant's identity, the version it opened, and the run's
/// request budget; never an earlier decision. Each check re-reads the
/// vault's metadata, so a revocation, expiry or rotation made by another
/// process stops the next request.
pub struct CredentialUse {
    vault: CredentialVault,
    id: String,
    profile: ProfileId,
    service: RemoteService,
    version: u32,
    remaining: Option<AtomicU32>,
    now: Clock,
}

impl fmt::Debug for CredentialUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialUse")
            .field("id", &self.id)
            .field("profile", &self.profile)
            .field("service", &self.service)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl CredentialUse {
    /// The use of `grant` (as opened at `version`) by one run.
    pub fn new(home: &Path, grant: &CredentialGrant, now: Clock) -> Self {
        Self {
            vault: CredentialVault::new(home),
            id: grant.id.clone(),
            profile: grant.profile.clone(),
            service: grant.service.clone(),
            version: grant.version,
            remaining: grant.max_requests_per_run.map(AtomicU32::new),
            now,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the credential may be presented to `url` now, for
    /// `operation` when the request performs one (a tool call); protocol
    /// requests pass `None` and are still checked for the grant, destination
    /// and budget. A refusal is audited and the request must not be sent,
    /// with this credential or any other.
    pub fn authorize(&self, operation: Option<&str>, url: &str) -> Result<(), CredentialDenial> {
        let decision = self.decide(operation, url);
        if let Err(denial) = decision {
            self.vault.record_denial(
                (self.now)(),
                Some(&self.id),
                &self.profile,
                &self.service,
                denial,
            );
        }
        decision
    }

    fn decide(&self, operation: Option<&str>, url: &str) -> Result<(), CredentialDenial> {
        let document = self
            .vault
            .load()
            .map_err(|_| CredentialDenial::Unavailable)?;
        let grant = document.get(&self.id).ok_or(CredentialDenial::NotGranted)?;
        if grant.profile != self.profile || grant.service != self.service {
            return Err(CredentialDenial::NotGranted);
        }
        grant.state((self.now)())?;
        if grant.version != self.version {
            return Err(CredentialDenial::Superseded);
        }
        let origin = origin_of(url).ok_or(CredentialDenial::Destination)?;
        if !grant.destinations.contains(&origin) {
            return Err(CredentialDenial::Destination);
        }
        if let (Some(operation), Some(operations)) = (operation, grant.operations.as_ref())
            && !operations.contains(operation)
        {
            return Err(CredentialDenial::Operation);
        }
        if let Some(remaining) = &self.remaining {
            remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .map_err(|_| CredentialDenial::Budget)?;
        }
        Ok(())
    }
}

/// The current time in Unix seconds.
pub fn system_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// What an untrusted run of a profile may present to one service.
pub enum Resolved {
    /// The vault holds nothing for this profile and service.
    Absent,
    /// A grant exists but may not be used now (revoked, expired, wrong or
    /// missing key, failed integrity): the run gets no credential for the
    /// service, never a fallback.
    Denied(CredentialDenial),
    /// The newest usable grant, opened.
    Active {
        secret: ExternalSecret,
        guard: std::sync::Arc<CredentialUse>,
    },
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => f.write_str("Absent"),
            Self::Denied(denial) => write!(f, "Denied({})", denial.code()),
            Self::Active { guard, .. } => write!(f, "Active({})", guard.id()),
        }
    }
}

/// Resolve the credential an untrusted run of `profile` may use for
/// `service`, at run start. The vault is read once here and again before
/// every request ([`CredentialUse::authorize`]).
pub fn resolve(
    home: &Path,
    key: Option<&MasterKey>,
    profile: &ProfileId,
    service: &RemoteService,
    now: Clock,
) -> Resolved {
    let vault = CredentialVault::new(home);
    if !vault.exists() {
        return Resolved::Absent;
    }
    let document = match vault.load() {
        Ok(document) => document,
        // Unreadable state: deny, so nothing falls back.
        Err(_) => return Resolved::Denied(CredentialDenial::Unavailable),
    };
    let grants = document.for_profile(profile, service);
    if grants.is_empty() {
        return Resolved::Absent;
    }
    let at = now();
    let Some(grant) = grants.iter().find(|grant| grant.state(at).is_ok()) else {
        let denial = grants[0]
            .state(at)
            .err()
            .unwrap_or(CredentialDenial::NotGranted);
        vault.record_denial(at, Some(&grants[0].id), profile, service, denial);
        return Resolved::Denied(denial);
    };
    let Some(key) = key else {
        vault.record_denial(
            at,
            Some(&grant.id),
            profile,
            service,
            CredentialDenial::Unavailable,
        );
        return Resolved::Denied(CredentialDenial::Unavailable);
    };
    match vault.open(key, grant) {
        Ok(secret) => Resolved::Active {
            secret,
            guard: std::sync::Arc::new(CredentialUse::new(home, grant, now.clone())),
        },
        Err(_) => {
            vault.record_denial(
                at,
                Some(&grant.id),
                profile,
                service,
                CredentialDenial::Unavailable,
            );
            Resolved::Denied(CredentialDenial::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests;
