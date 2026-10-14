//! Durable identity: principals, credentials, ownership, grants and audit.
//!
//! One versioned JSON document, `<home>/identity/state.json`, holds the whole
//! authorization state. It reuses the storage Lightagent already relies on
//! (owner-private files replaced atomically), not a second database, and adds:
//!
//! - **Transactions.** [`IdentityStore::transact`] takes an exclusive
//!   cross-process lock (`File::lock` on `identity/lock`), reads the current
//!   document, applies the change to a copy, and replaces the file
//!   atomically: write a temporary file, `fsync` it, rename it over the
//!   target, `fsync` the directory. A failed change writes nothing; a crash
//!   leaves the old document or the new one, never a mix.
//! - **Fail closed.** A document that does not parse, or whose schema is newer
//!   than this build, is an error: it is never replaced by an empty state.
//!   The previous generation is kept as `state.prev.json` for recovery.
//! - **Operator-only changes.** Every change names an actor that must be an
//!   operator holding `provision` (users, credentials, ownership) or
//!   `administer` (operators, reassignment, grants, issuers). A user can never
//!   change identity state, so it cannot grant itself anything.
//! - **Profile binding.** Assigning an owner writes a random marker into the
//!   profile's directory and records it. Ownership counts only while the
//!   marker matches, so a deleted profile, or a new one reusing its id,
//!   never inherits the old owner.
//! - **Audit.** Every change appends fixed-code records to a bounded,
//!   rotated `audit.jsonl`.
//!
//! An installation without `identity/state.json` behaves as before: nothing
//! is created until an operator makes the first change, and single-user mode
//! never reads it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{
    Assignment, AuditAction, AuditOutcome, AuditRecord, Caller, Capability, CredentialError,
    CredentialStore, Denial, IssuedToken, OwnershipError, OwnershipRegistry, PrincipalId,
    Restriction, Role, authorize_capability,
};
use crate::ids::hex;
use crate::profile::{ProfileId, ProfileStore};

/// The document schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

const STATE: &str = "state.json";
const PREVIOUS: &str = "state.prev.json";
const LOCK: &str = "lock";
const AUDIT: &str = "audit.jsonl";
/// The marker file a profile's directory holds while it is owned.
pub const BINDING_FILE: &str = ".identity-binding";
const MAX_IDEMPOTENCY: usize = 10_000;
const MAX_REASSIGNMENTS: usize = 10_000;

/// Whether a principal may authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalStatus {
    Active,
    /// Every credential was revoked when it was suspended; reactivating it
    /// does not restore them.
    Suspended,
}

/// An identity asserted by an external issuer (for a future provisioning
/// service). Accepted only from an issuer the operator registered.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalIdentity {
    pub issuer: String,
    pub subject: String,
}

/// One principal. Its id never changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRecord {
    pub id: PrincipalId,
    pub role: Role,
    pub status: PrincipalStatus,
    pub created_unix: u64,
    pub updated_unix: u64,
    #[serde(default)]
    pub external: Option<ExternalIdentity>,
}

/// One reassignment, kept for history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reassignment {
    pub profile: ProfileId,
    pub from: PrincipalId,
    pub to: PrincipalId,
    pub by: PrincipalId,
    pub at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Idempotency {
    fingerprint: String,
    generation: u64,
    at_unix: u64,
}

/// The provisioning transaction that last advanced
/// [`ProvisioningContinuity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuityAction {
    Bind,
    Revoke,
}

/// The identity store's half of provisioning-ledger continuity
/// (`LA-WEB-PROVISIONING/1` r4 §10.6). It is written only by a provisioning
/// bind or revoke transaction of an explicitly initialized ledger, in the
/// same transaction as the ownership change. Every field is required: a
/// partial record does not parse, so the store fails closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisioningContinuity {
    /// The ledger's random continuity id.
    pub continuity_id: String,
    /// How many provisioning bind and revoke transactions this store has
    /// applied. Nothing else advances it: unlike the document generation,
    /// ordinary operator transactions leave it alone.
    pub sequence: u64,
    /// The ledger generation when that transaction ran.
    pub ledger_generation_seen: u64,
    /// The operation that transaction belonged to.
    pub last_operation: String,
    pub last_action: ContinuityAction,
}

/// The whole durable authorization state. Holds no secret: credentials are
/// verifiers only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityDocument {
    schema: u32,
    generation: u64,
    #[serde(default)]
    principals: BTreeMap<PrincipalId, PrincipalRecord>,
    #[serde(default)]
    trusted_issuers: BTreeSet<String>,
    #[serde(default)]
    credentials: CredentialStore,
    #[serde(default)]
    ownership: OwnershipRegistry,
    #[serde(default)]
    bindings: BTreeMap<ProfileId, String>,
    #[serde(default)]
    reassignments: VecDeque<Reassignment>,
    #[serde(default)]
    idempotency: BTreeMap<String, Idempotency>,
    /// Absent until an initialized provisioning ledger binds or revokes, and
    /// then never written by anything else. Not serialized while absent, so
    /// a document without it keeps its exact bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provisioning_continuity: Option<ProvisioningContinuity>,
}

impl Default for IdentityDocument {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            generation: 0,
            principals: BTreeMap::new(),
            trusted_issuers: BTreeSet::new(),
            credentials: CredentialStore::default(),
            ownership: OwnershipRegistry::default(),
            bindings: BTreeMap::new(),
            reassignments: VecDeque::new(),
            idempotency: BTreeMap::new(),
            provisioning_continuity: None,
        }
    }
}

impl IdentityDocument {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn principal(&self, id: &PrincipalId) -> Option<&PrincipalRecord> {
        self.principals.get(id)
    }

    pub fn principals(&self) -> impl Iterator<Item = &PrincipalRecord> {
        self.principals.values()
    }

    pub fn credentials(&self) -> &CredentialStore {
        &self.credentials
    }

    /// Ownership as recorded, before the binding check. Use
    /// [`IdentityStore::effective_ownership`] for authorization.
    pub fn recorded_ownership(&self) -> &OwnershipRegistry {
        &self.ownership
    }

    pub fn reassignments(&self) -> impl Iterator<Item = &Reassignment> {
        self.reassignments.iter()
    }

    /// Whether an operator registered `issuer` (exact match).
    pub fn is_trusted_issuer(&self, issuer: &str) -> bool {
        self.trusted_issuers.contains(issuer)
    }

    /// The provisioning-ledger continuity marker, if a provisioning
    /// transaction of an initialized ledger has written one.
    pub fn provisioning_continuity(&self) -> Option<&ProvisioningContinuity> {
        self.provisioning_continuity.as_ref()
    }

    /// Whether a retained idempotency record's key starts with `prefix`.
    /// Records are pruned, so `false` proves nothing about older ones.
    pub fn has_idempotency_key_with_prefix(&self, prefix: &str) -> bool {
        self.idempotency.keys().any(|key| key.starts_with(prefix))
    }

    /// The principal an external identity maps to, if any.
    pub fn external(&self, identity: &ExternalIdentity) -> Option<&PrincipalId> {
        self.principals
            .values()
            .find(|record| record.external.as_ref() == Some(identity))
            .map(|record| &record.id)
    }
}

/// Why the store refused. Messages hold codes and counts only: no path,
/// token, verifier or profile content.
#[derive(Debug, thiserror::Error)]
pub enum IdentityStoreError {
    #[error("identity storage is unavailable ({0})")]
    Io(io::ErrorKind),
    #[error("the identity state is corrupt; restore state.prev.json or a backup")]
    Corrupt,
    #[error("identity schema {found} is newer than this build supports ({supported})")]
    IncompatibleSchema { found: u32, supported: u32 },
    #[error("identity schema {0} cannot be migrated")]
    MigrationFailed(u32),
    #[error("not authorized: {}", .0.code())]
    Denied(Denial),
    #[error("refused: {}", .0.code())]
    Refused(Refusal),
    #[error(transparent)]
    Ownership(#[from] OwnershipError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
}

impl From<io::Error> for IdentityStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// A change the store refuses on its merits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    PrincipalExists,
    UnknownPrincipal,
    PrincipalInactive,
    DuplicateExternalIdentity,
    UntrustedIssuer,
    InvalidIssuer,
    ProfileMissing,
    OwnerMustBeUser,
    OperatorOnlyRestriction,
    UnknownCredential,
    IdempotencyConflict,
    /// The provisioning continuity marker does not follow from the ledger's.
    ContinuityMismatch,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::PrincipalExists => "principal_exists",
            Self::UnknownPrincipal => "unknown_principal",
            Self::PrincipalInactive => "principal_inactive",
            Self::DuplicateExternalIdentity => "duplicate_external_identity",
            Self::UntrustedIssuer => "untrusted_issuer",
            Self::InvalidIssuer => "invalid_issuer",
            Self::ProfileMissing => "profile_missing",
            Self::OwnerMustBeUser => "owner_must_be_user",
            Self::OperatorOnlyRestriction => "operator_only_restriction",
            Self::UnknownCredential => "unknown_credential",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::ContinuityMismatch => "continuity_mismatch",
        }
    }
}

/// How much audit history is kept: `max_files` files of at most
/// `max_file_bytes` each, oldest discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditRetention {
    pub max_file_bytes: u64,
    pub max_files: usize,
}

impl Default for AuditRetention {
    fn default() -> Self {
        Self {
            max_file_bytes: 1024 * 1024,
            max_files: 8,
        }
    }
}

/// One stored audit line: the decision record, the generation it belongs to,
/// and the principal a change was about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAudit {
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub subject: Option<PrincipalId>,
    #[serde(flatten)]
    pub record: AuditRecord,
}

/// The result of a transaction.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<R> {
    /// Applied and committed as `generation`.
    Applied { generation: u64, value: R },
    /// The idempotency key was already applied with the same request.
    Replayed { generation: u64 },
}

/// The durable identity store of one installation.
#[derive(Debug)]
pub struct IdentityStore {
    root: PathBuf,
    dir: PathBuf,
    retention: AuditRetention,
    local: Mutex<()>,
}

impl IdentityStore {
    /// The store under `home` (`<home>/identity`). Does no I/O.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let root = home.into();
        Self {
            dir: root.join("identity"),
            root,
            retention: AuditRetention::default(),
            local: Mutex::new(()),
        }
    }

    pub fn with_retention(mut self, retention: AuditRetention) -> Self {
        self.retention = retention;
        self
    }

    /// Whether any identity state has been written.
    pub fn exists(&self) -> bool {
        self.dir.join(STATE).exists()
    }

    /// Read the current document. Absent: an empty one (generation 0).
    /// Unreadable or newer: an error, never a silent empty state.
    pub fn load(&self) -> Result<IdentityDocument, IdentityStoreError> {
        let bytes = match std::fs::read(self.dir.join(STATE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(IdentityDocument::default());
            }
            Err(error) => return Err(error.into()),
        };
        parse_document(&bytes)
    }

    /// Ownership that counts for authorization: recorded ownership of
    /// profiles that exist and still carry their binding marker.
    pub fn effective_ownership(&self, document: &IdentityDocument) -> OwnershipRegistry {
        let mut effective = document.ownership.clone();
        let profiles = ProfileStore::new(&self.root);
        let owned: Vec<ProfileId> = document
            .ownership
            .profiles()
            .map(|(profile, _)| profile.clone())
            .collect();
        for profile in owned {
            let handle = profiles.handle(&profile);
            let bound = handle.manifest_file().is_file()
                && document.bindings.get(&profile).is_some_and(|token| {
                    std::fs::read_to_string(handle.dir().join(BINDING_FILE))
                        .is_ok_and(|marker| marker.trim() == token)
                });
            if !bound {
                effective.remove(&profile);
            }
        }
        effective
    }

    /// Apply `change` as `actor`, atomically.
    ///
    /// `idempotency` is `(key, fingerprint)`: a key already applied with the
    /// same fingerprint returns [`Outcome::Replayed`] without changing
    /// anything; with a different fingerprint it is refused.
    pub fn transact<R>(
        &self,
        actor: &Caller,
        now_unix: u64,
        idempotency: Option<(&str, &str)>,
        change: impl FnOnce(&mut Transaction<'_>) -> Result<R, IdentityStoreError>,
    ) -> Result<Outcome<R>, IdentityStoreError> {
        // Only an operator holding provision or administer changes anything.
        let admitted = actor.principal.role == Role::Operator
            && (authorize_capability(actor, Capability::Provision).is_ok()
                || authorize_capability(actor, Capability::Administer).is_ok());
        if !admitted {
            self.append_audit_quietly(&[StoredAudit {
                generation: None,
                subject: None,
                record: AuditRecord::decision(
                    now_unix,
                    Some(actor),
                    AuditAction::Request(Capability::Administer),
                    None,
                    Err(Denial::MissingCapability),
                ),
            }]);
            return Err(IdentityStoreError::Denied(Denial::MissingCapability));
        }
        let _local = self.local.lock().map_err(|_| io::Error::other("lock"))?;
        crate::paths::create_private_dir(&self.dir)
            .map_err(|_| IdentityStoreError::Io(io::ErrorKind::PermissionDenied))?;
        let lock = open_private(&self.dir.join(LOCK), false)?;
        lock.lock()?;

        let current = self.load()?;
        if let Some((key, fingerprint)) = idempotency
            && let Some(seen) = current.idempotency.get(key)
        {
            if seen.fingerprint == fingerprint {
                self.append_audit_quietly(&[StoredAudit {
                    generation: Some(current.generation),
                    subject: None,
                    record: AuditRecord::decision(
                        now_unix,
                        Some(actor),
                        AuditAction::IdempotentReplay,
                        None,
                        Ok(()),
                    ),
                }]);
                return Ok(Outcome::Replayed {
                    generation: seen.generation,
                });
            }
            return Err(IdentityStoreError::Refused(Refusal::IdempotencyConflict));
        }

        let generation = current.generation + 1;
        let mut transaction = Transaction {
            store: self,
            document: current.clone(),
            actor,
            now_unix,
            events: Vec::new(),
        };
        let value = match change(&mut transaction) {
            Ok(value) => value,
            Err(error) => {
                if let IdentityStoreError::Denied(denial) = &error {
                    self.append_audit_quietly(&[StoredAudit {
                        generation: None,
                        subject: None,
                        record: AuditRecord::decision(
                            now_unix,
                            Some(actor),
                            AuditAction::Request(Capability::Administer),
                            None,
                            Err(*denial),
                        ),
                    }]);
                }
                return Err(error);
            }
        };
        let Transaction {
            mut document,
            events,
            ..
        } = transaction;
        document.generation = generation;
        document.schema = SCHEMA_VERSION;
        if let Some((key, fingerprint)) = idempotency {
            document.idempotency.insert(
                key.to_owned(),
                Idempotency {
                    fingerprint: fingerprint.to_owned(),
                    generation,
                    at_unix: now_unix,
                },
            );
            while document.idempotency.len() > MAX_IDEMPOTENCY {
                let oldest = document
                    .idempotency
                    .iter()
                    .min_by_key(|(_, entry)| entry.generation)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(key) => document.idempotency.remove(&key),
                    None => break,
                };
            }
        }
        while document.reassignments.len() > MAX_REASSIGNMENTS {
            document.reassignments.pop_front();
        }

        // Audit first, tagged with the generation it describes: a crash
        // after this leaves records for a generation that was never
        // committed, which readers detect, rather than a change with none.
        let records: Vec<StoredAudit> = events
            .into_iter()
            .map(|(action, profile, subject, key_id)| {
                let mut record =
                    AuditRecord::decision(now_unix, Some(actor), action, profile.as_ref(), Ok(()));
                if key_id.is_some() {
                    record.key_id = key_id;
                }
                StoredAudit {
                    generation: Some(generation),
                    subject,
                    record,
                }
            })
            .collect();
        self.append_audit(&records)?;

        let bytes =
            serde_json::to_vec_pretty(&document).map_err(|_| IdentityStoreError::Corrupt)?;
        if let Ok(previous) = std::fs::read(self.dir.join(STATE)) {
            write_durable(&self.dir.join(PREVIOUS), &previous)?;
        }
        write_durable(&self.dir.join(STATE), &bytes)?;
        drop(lock);
        Ok(Outcome::Applied { generation, value })
    }

    /// Append audit records, rotating when the current file is full.
    pub fn append_audit(&self, records: &[StoredAudit]) -> Result<(), IdentityStoreError> {
        if records.is_empty() {
            return Ok(());
        }
        crate::paths::create_private_dir(&self.dir)
            .map_err(|_| IdentityStoreError::Io(io::ErrorKind::PermissionDenied))?;
        let mut lines = Vec::new();
        for record in records {
            let mut line = serde_json::to_vec(record).map_err(|_| IdentityStoreError::Corrupt)?;
            line.push(b'\n');
            lines.extend(line);
        }
        let current = self.dir.join(AUDIT);
        let size = std::fs::metadata(&current)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if size > 0 && size + lines.len() as u64 > self.retention.max_file_bytes {
            self.rotate_audit()?;
        }
        let mut file = open_private(&current, true)?;
        file.write_all(&lines)?;
        file.sync_all()?;
        Ok(())
    }

    fn append_audit_quietly(&self, records: &[StoredAudit]) {
        let _ = self.append_audit(records);
    }

    fn rotated(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(AUDIT)
        } else {
            self.dir.join(format!("audit.{index}.jsonl"))
        }
    }

    fn rotate_audit(&self) -> Result<(), IdentityStoreError> {
        let keep = self.retention.max_files.max(1);
        let _ = std::fs::remove_file(self.rotated(keep - 1));
        for index in (0..keep.saturating_sub(1)).rev() {
            let from = self.rotated(index);
            if from.exists() {
                std::fs::rename(&from, self.rotated(index + 1))?;
            }
        }
        Ok(())
    }

    /// Every retained audit record, oldest first.
    pub fn audit(&self) -> Result<Vec<StoredAudit>, IdentityStoreError> {
        let mut records = Vec::new();
        for index in (0..self.retention.max_files.max(1)).rev() {
            let Ok(file) = File::open(self.rotated(index)) else {
                continue;
            };
            for line in io::BufReader::new(file).lines() {
                let line = line?;
                if let Ok(record) = serde_json::from_str(&line) {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }

    /// The files this store writes, for tests and backups.
    pub fn files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }
}

/// Parse a stored document, migrating older schemas and refusing newer
/// ones. Never returns an empty state for bytes it cannot read.
fn parse_document(bytes: &[u8]) -> Result<IdentityDocument, IdentityStoreError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| IdentityStoreError::Corrupt)?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .and_then(|schema| u32::try_from(schema).ok())
        .ok_or(IdentityStoreError::Corrupt)?;
    let value = match schema {
        SCHEMA_VERSION => value,
        newer if newer > SCHEMA_VERSION => {
            return Err(IdentityStoreError::IncompatibleSchema {
                found: newer,
                supported: SCHEMA_VERSION,
            });
        }
        older => migrate(value, older)?,
    };
    serde_json::from_value(value).map_err(|_| IdentityStoreError::Corrupt)
}

/// Bring an older document up to [`SCHEMA_VERSION`], one version at a time.
/// Schema 1 is the first, so every older number is refused; a future
/// version adds its step here, and the first write after a migration keeps
/// the original as `state.prev.json`.
fn migrate(_value: serde_json::Value, from: u32) -> Result<serde_json::Value, IdentityStoreError> {
    Err(IdentityStoreError::MigrationFailed(from))
}

/// Open (creating) a file only this user can read: `0600` on Unix.
pub(crate) fn open_private(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true);
    if append {
        options.append(true);
    } else {
        options.read(true).write(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Replace `path` with `bytes` durably: temporary sibling, `fsync`, rename,
/// directory `fsync`.
pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

type Event = (
    AuditAction,
    Option<ProfileId>,
    Option<PrincipalId>,
    Option<String>,
);

/// A change in progress. Every operation checks the actor's capability.
pub struct Transaction<'a> {
    store: &'a IdentityStore,
    document: IdentityDocument,
    actor: &'a Caller,
    now_unix: u64,
    events: Vec<Event>,
}

impl Transaction<'_> {
    fn require(&self, capability: Capability) -> Result<(), IdentityStoreError> {
        authorize_capability(self.actor, capability).map_err(IdentityStoreError::Denied)
    }

    /// `provision` for users, `administer` for operators.
    fn require_for(&self, role: Role) -> Result<(), IdentityStoreError> {
        self.require(match role {
            Role::User => Capability::Provision,
            Role::Operator => Capability::Administer,
        })
    }

    fn event(
        &mut self,
        action: AuditAction,
        profile: Option<&ProfileId>,
        subject: Option<&PrincipalId>,
        key_id: Option<&str>,
    ) {
        self.events.push((
            action,
            profile.cloned(),
            subject.cloned(),
            key_id.map(str::to_owned),
        ));
    }

    fn active_principal(&self, id: &PrincipalId) -> Result<&PrincipalRecord, IdentityStoreError> {
        let record = self
            .document
            .principals
            .get(id)
            .ok_or(IdentityStoreError::Refused(Refusal::UnknownPrincipal))?;
        if record.status != PrincipalStatus::Active {
            return Err(IdentityStoreError::Refused(Refusal::PrincipalInactive));
        }
        Ok(record)
    }

    pub fn document(&self) -> &IdentityDocument {
        &self.document
    }

    /// Replace the provisioning continuity marker. Only the provisioning
    /// ledger calls this, inside its bind and revoke transactions, so the
    /// marker commits with the ownership change or not at all.
    pub fn set_provisioning_continuity(
        &mut self,
        continuity: ProvisioningContinuity,
    ) -> Result<(), IdentityStoreError> {
        self.require(Capability::Provision)?;
        self.document.provisioning_continuity = Some(continuity);
        Ok(())
    }

    /// Accept external identities from `issuer` (an exact `https://` URL).
    pub fn trust_issuer(&mut self, issuer: &str) -> Result<(), IdentityStoreError> {
        self.require(Capability::Administer)?;
        let valid = issuer.starts_with("https://")
            && issuer.len() <= 256
            && !issuer.contains(char::is_whitespace);
        if !valid {
            return Err(IdentityStoreError::Refused(Refusal::InvalidIssuer));
        }
        self.document.trusted_issuers.insert(issuer.to_owned());
        self.event(AuditAction::IssuerTrusted, None, None, None);
        Ok(())
    }

    /// Create a principal. Its id is permanent; an external identity maps to
    /// at most one principal and only from a trusted issuer.
    pub fn create_principal(
        &mut self,
        id: PrincipalId,
        role: Role,
        external: Option<ExternalIdentity>,
    ) -> Result<(), IdentityStoreError> {
        self.require_for(role)?;
        if self.document.principals.contains_key(&id) {
            return Err(IdentityStoreError::Refused(Refusal::PrincipalExists));
        }
        if let Some(identity) = &external {
            if !self.document.trusted_issuers.contains(&identity.issuer) {
                return Err(IdentityStoreError::Refused(Refusal::UntrustedIssuer));
            }
            if self.document.external(identity).is_some() {
                return Err(IdentityStoreError::Refused(
                    Refusal::DuplicateExternalIdentity,
                ));
            }
        }
        self.document.principals.insert(
            id.clone(),
            PrincipalRecord {
                id: id.clone(),
                role,
                status: PrincipalStatus::Active,
                created_unix: self.now_unix,
                updated_unix: self.now_unix,
                external,
            },
        );
        self.event(AuditAction::PrincipalCreated, None, Some(&id), None);
        Ok(())
    }

    /// Suspend (revoking every credential) or reactivate a principal.
    pub fn set_status(
        &mut self,
        id: &PrincipalId,
        status: PrincipalStatus,
    ) -> Result<(), IdentityStoreError> {
        let role = self
            .document
            .principals
            .get(id)
            .map(|record| record.role)
            .ok_or(IdentityStoreError::Refused(Refusal::UnknownPrincipal))?;
        self.require_for(role)?;
        if status == PrincipalStatus::Suspended {
            for key_id in self
                .document
                .credentials
                .revoke_principal(id, self.now_unix)
            {
                self.event(
                    AuditAction::CredentialRevoked,
                    None,
                    Some(id),
                    Some(&key_id),
                );
            }
        }
        if let Some(record) = self.document.principals.get_mut(id) {
            record.status = status;
            record.updated_unix = self.now_unix;
        }
        let action = match status {
            PrincipalStatus::Suspended => AuditAction::PrincipalSuspended,
            PrincipalStatus::Active => AuditAction::PrincipalReactivated,
        };
        self.event(action, None, Some(id), None);
        Ok(())
    }

    /// Issue a credential for an active principal, with the principal's own
    /// role (never one the caller picks). A user credential may only narrow.
    pub fn issue_credential(
        &mut self,
        principal: &PrincipalId,
        restriction: Restriction,
        expires_unix: Option<u64>,
    ) -> Result<(String, IssuedToken), IdentityStoreError> {
        let role = self.active_principal(principal)?.role;
        self.require_for(role)?;
        if role == Role::User
            && restriction
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.iter().any(|c| c.operator_only()))
        {
            return Err(IdentityStoreError::Refused(
                Refusal::OperatorOnlyRestriction,
            ));
        }
        let (key_id, token) = self.document.credentials.issue_expiring(
            principal.clone(),
            role,
            restriction,
            self.now_unix,
            expires_unix,
        )?;
        self.event(
            AuditAction::CredentialIssued,
            None,
            Some(principal),
            Some(&key_id),
        );
        Ok((key_id, token))
    }

    /// Issue a replacement for `key_id`; the old one keeps working until
    /// revoked.
    pub fn rotate_credential(
        &mut self,
        key_id: &str,
        expires_unix: Option<u64>,
    ) -> Result<(String, IssuedToken), IdentityStoreError> {
        let principal = self
            .document
            .credentials
            .get(key_id)
            .map(|record| record.principal.clone())
            .ok_or(IdentityStoreError::Refused(Refusal::UnknownCredential))?;
        let role = self.active_principal(&principal)?.role;
        self.require_for(role)?;
        let (new_id, token) =
            self.document
                .credentials
                .rotate(key_id, self.now_unix, expires_unix)?;
        self.event(
            AuditAction::CredentialRotated,
            None,
            Some(&principal),
            Some(&new_id),
        );
        Ok((new_id, token))
    }

    pub fn revoke_credential(&mut self, key_id: &str) -> Result<(), IdentityStoreError> {
        let record = self
            .document
            .credentials
            .get(key_id)
            .ok_or(IdentityStoreError::Refused(Refusal::UnknownCredential))?;
        let (principal, role) = (record.principal.clone(), record.role);
        self.require_for(role)?;
        self.document.credentials.revoke(key_id, self.now_unix)?;
        self.event(
            AuditAction::CredentialRevoked,
            None,
            Some(&principal),
            Some(key_id),
        );
        Ok(())
    }

    fn profile_exists(&self, profile: &ProfileId) -> bool {
        ProfileStore::new(&self.store.root)
            .handle(profile)
            .manifest_file()
            .is_file()
    }

    /// Write a fresh binding marker into the profile's directory and record
    /// it. Written before the commit, so a record never names a marker that
    /// does not exist.
    fn bind(&mut self, profile: &ProfileId) -> Result<(), IdentityStoreError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| CredentialError::Entropy)?;
        let token = hex(&bytes);
        let marker = ProfileStore::new(&self.store.root)
            .handle(profile)
            .dir()
            .join(BINDING_FILE);
        write_durable(&marker, token.as_bytes())?;
        self.document.bindings.insert(profile.clone(), token);
        Ok(())
    }

    /// Record `owner` (an active user) for an existing, unowned profile.
    /// Idempotent for the same owner; a different owner is a conflict.
    pub fn assign_owner(
        &mut self,
        profile: &ProfileId,
        owner: &PrincipalId,
    ) -> Result<Assignment, IdentityStoreError> {
        self.require(Capability::Provision)?;
        if self.active_principal(owner)?.role != Role::User {
            return Err(IdentityStoreError::Refused(Refusal::OwnerMustBeUser));
        }
        if !self.profile_exists(profile) {
            return Err(IdentityStoreError::Refused(Refusal::ProfileMissing));
        }
        let assignment = self
            .document
            .ownership
            .assign(profile.clone(), owner.clone())?;
        if assignment == Assignment::Created {
            self.bind(profile)?;
            self.event(AuditAction::OwnerAssigned, Some(profile), Some(owner), None);
        }
        Ok(assignment)
    }

    /// Move a profile to a new owner (operator `administer` only). The
    /// previous owner's grants are dropped, and the move is kept in history.
    pub fn reassign_owner(
        &mut self,
        profile: &ProfileId,
        owner: &PrincipalId,
    ) -> Result<Assignment, IdentityStoreError> {
        self.require(Capability::Administer)?;
        if self.active_principal(owner)?.role != Role::User {
            return Err(IdentityStoreError::Refused(Refusal::OwnerMustBeUser));
        }
        if !self.profile_exists(profile) {
            return Err(IdentityStoreError::Refused(Refusal::ProfileMissing));
        }
        let previous = self
            .document
            .ownership
            .get(profile)
            .map(|record| record.owner.clone());
        let assignment = self
            .document
            .ownership
            .reassign(profile.clone(), owner.clone());
        match (assignment, previous) {
            (Assignment::Reassigned, Some(from)) => {
                self.document.reassignments.push_back(Reassignment {
                    profile: profile.clone(),
                    from,
                    to: owner.clone(),
                    by: self.actor.principal.id.clone(),
                    at_unix: self.now_unix,
                });
                self.event(
                    AuditAction::OwnerReassigned,
                    Some(profile),
                    Some(owner),
                    None,
                );
            }
            (Assignment::Created, _) => {
                self.bind(profile)?;
                self.event(AuditAction::OwnerAssigned, Some(profile), Some(owner), None);
            }
            _ => {}
        }
        Ok(assignment)
    }

    /// Grant `capability` on an owned profile to an active principal
    /// (operator `administer` only; never an operator-only capability).
    pub fn grant(
        &mut self,
        profile: &ProfileId,
        principal: &PrincipalId,
        capability: Capability,
    ) -> Result<(), IdentityStoreError> {
        self.require(Capability::Administer)?;
        self.active_principal(principal)?;
        self.document
            .ownership
            .grant(profile, principal.clone(), capability)?;
        self.event(
            AuditAction::GrantAdded,
            Some(profile),
            Some(principal),
            None,
        );
        Ok(())
    }

    pub fn revoke_grant(
        &mut self,
        profile: &ProfileId,
        principal: &PrincipalId,
        capability: Capability,
    ) -> Result<(), IdentityStoreError> {
        self.require(Capability::Provision)?;
        self.document
            .ownership
            .revoke(profile, principal, capability);
        self.event(
            AuditAction::GrantRevoked,
            Some(profile),
            Some(principal),
            None,
        );
        Ok(())
    }

    /// Forget a profile's ownership and binding (deleted or released).
    pub fn remove_profile(&mut self, profile: &ProfileId) -> Result<(), IdentityStoreError> {
        self.require(Capability::Provision)?;
        let removed = self.document.ownership.remove(profile);
        self.document.bindings.remove(profile);
        let marker = ProfileStore::new(&self.store.root)
            .handle(profile)
            .dir()
            .join(BINDING_FILE);
        let _ = std::fs::remove_file(marker);
        if let Some(record) = removed {
            self.event(
                AuditAction::OwnerRemoved,
                Some(profile),
                Some(&record.owner),
                None,
            );
        }
        Ok(())
    }
}

/// Convenience for audit readers: whether a record was a refusal.
pub fn is_denial(record: &StoredAudit) -> bool {
    matches!(record.record.outcome, AuditOutcome::Denied(_))
}

#[cfg(test)]
mod tests;
