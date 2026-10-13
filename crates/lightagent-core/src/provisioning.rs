//! Account and profile provisioning for an external control service (the
//! Cloudflare Workers control website): a least-privilege boundary, not
//! activated.
//!
//! The control service manages external accounts. Lightagent independently
//! verifies the service's provisioning authority, establishes profile
//! ownership in its own identity store, and keeps every runtime permission
//! to itself. A provisioning credential can only:
//!
//! - provision a profile from a server-defined template for a user named by
//!   an `(issuer, subject)` pair, creating or resolving that user;
//! - read the status of its own provisioning operations;
//! - revoke a user's access to a profile it provisioned.
//!
//! It cannot start runs, read sessions or conversations, invoke tools, read
//! Jev diagnostics, reach provider or remote credentials, change settings,
//! create operators, trust issuers, or reassign a profile: those are other
//! capabilities, and the credential holds exactly `{provision}`.
//!
//! # Who may provision
//!
//! A *provisioner* is an operator-role principal whose credential is
//! restricted to exactly `{provision}` **and** which an operator registered
//! here with the issuers and templates it may use. Both are re-read from the
//! durable stores on every call: a revoked credential, a suspended principal
//! or a withdrawn registration stops the next call. The legacy `--key-env`
//! key, the local operator, an unrestricted operator credential and every
//! user credential are refused: provisioning is a narrow door of its own.
//!
//! # External identity
//!
//! The user is named by `(issuer, subject)`. The issuer must be registered by
//! an operator in the identity store (`trust_issuer`) *and* allowed for this
//! provisioner. Lightagent does not verify the user's sign-in itself: the
//! provisioner vouches for subjects of its issuers, and nothing it sends is
//! taken as a Lightagent principal id. The principal id is derived on the
//! server from the pair, and an existing mapping always wins.
//!
//! # Atomicity
//!
//! Three stores take part: this ledger (`<home>/provisioning/state.json`),
//! the profile directory and the identity store. They are not one
//! transaction, so an operation is an explicit, recoverable state machine:
//!
//! ```text
//! pending ──stage + rename──► profile_created ──identity txn──► completed
//!    │                            │                                │
//!    └─► failed (nothing created) └─► failed (profile quarantined) └─► revoked
//! ```
//!
//! - The ledger lock is held for the whole operation, so provisioning calls
//!   are serialized across processes. Lock order: ledger, then identity.
//! - The profile is built in `<home>/provisioning/staging/<op>` and renamed
//!   into `<home>/profiles/<id>` in one step, carrying a random marker that
//!   ties it to the operation.
//! - Creating (or resolving) the principal, recording the owner and writing
//!   the ownership binding is **one** identity transaction, idempotent by
//!   operation.
//! - Between the rename and that transaction the profile exists but has no
//!   owner. An unowned profile is operator-only, so no user can use it.
//! - A crash at any point leaves a state that the next retry of the same
//!   request, or [`ProvisioningService::reconcile`], completes or fails
//!   explicitly. A failed operation's profile is moved to
//!   `<home>/provisioning/quarantine/<op>`, never deleted.
//!
//! Nothing here is reachable from a running server: no startup path builds a
//! [`ProvisioningService`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::config::ApprovalPolicy;
use crate::identity::store::{
    ExternalIdentity, IdentityDocument, IdentityStore, IdentityStoreError, Outcome,
    PrincipalStatus, Refusal, open_private, write_durable,
};
use crate::identity::{
    Caller, Capability, OwnershipError, PrincipalId, PrincipalSource, Role, authorize_capability,
};
use crate::ids::hex;
use crate::limits::RunLimits;
use crate::profile::{AgentProfile, ProfileId, ProfileStore};

/// The ledger schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
/// The marker file a provisioned profile's directory carries.
pub const MARKER_FILE: &str = ".provisioning-operation";
/// The server-defined templates a provisioned profile may start from.
pub const TEMPLATES: [&str; 1] = ["assistant"];

const DIR: &str = "provisioning";
const STATE: &str = "state.json";
const LOCK: &str = "lock";
const AUDIT: &str = "audit.jsonl";
const STAGING: &str = "staging";
const QUARANTINE: &str = "quarantine";
const MAX_OPERATIONS: usize = 100_000;
const MAX_PROFILES_PER_PROVISIONER: u32 = 100_000;
const MAX_AUDIT_BYTES: u64 = 1024 * 1024;
const RESERVED_PROFILES: [&str; 1] = ["default"];
const PRINCIPAL_LABEL: &[u8] = b"lightagent-external-principal-v1\0";
const FINGERPRINT_LABEL: &[u8] = b"lightagent-provisioning-request-v1\0";

/// Why provisioning refused. Every variant carries a fixed code only: never
/// a subject, a path, a token or a request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProvisioningError {
    /// The caller is not an admitted provisioner, or may not do this.
    #[error("not authorized ({0})")]
    Denied(&'static str),
    /// The request is malformed.
    #[error("invalid request ({0})")]
    Invalid(&'static str),
    /// The request conflicts with the current state.
    #[error("conflict ({0})")]
    Conflict(&'static str),
    /// No such operation for this provisioner.
    #[error("no such provisioning operation")]
    UnknownOperation,
    /// A store could not be read or written; the operation is retryable.
    #[error("provisioning storage is unavailable")]
    Unavailable,
    /// The ledger does not parse, or is newer than this build.
    #[error("the provisioning state is corrupt or newer than this build")]
    Corrupt,
}

impl ProvisioningError {
    /// The stable code an API returns.
    pub fn code(self) -> &'static str {
        match self {
            Self::Denied(code) | Self::Invalid(code) | Self::Conflict(code) => code,
            Self::UnknownOperation => "unknown_operation",
            Self::Unavailable => "storage_unavailable",
            Self::Corrupt => "storage_corrupt",
        }
    }

    /// The HTTP status an API returns for it.
    pub fn http_status(self) -> u16 {
        match self {
            Self::Denied("unauthenticated") => 401,
            Self::Denied(_) => 403,
            Self::Invalid(_) => 400,
            Self::Conflict(_) => 409,
            Self::UnknownOperation => 404,
            Self::Unavailable | Self::Corrupt => 503,
        }
    }
}

impl From<io::Error> for ProvisioningError {
    fn from(_: io::Error) -> Self {
        Self::Unavailable
    }
}

/// A principal an operator allowed to provision, and within what bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provisioner {
    pub principal: PrincipalId,
    /// Issuers whose subjects it may provision (each also trusted in the
    /// identity store).
    pub issuers: BTreeSet<String>,
    /// Templates it may provision from.
    pub templates: BTreeSet<String>,
    /// The most live (not failed or revoked) operations it may hold.
    pub max_profiles: u32,
    pub created_unix: u64,
    #[serde(default)]
    pub revoked_unix: Option<u64>,
}

/// Where an operation stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Recorded; the profile is not created yet.
    Pending,
    /// The profile exists (without an owner); the binding is next.
    ProfileCreated,
    /// The principal owns the profile.
    Completed,
    /// Refused on its merits; see `failure`. Nothing usable remains.
    Failed,
    /// The provisioner revoked the user's access.
    Revoked,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Revoked)
    }

    fn holds_profile(self) -> bool {
        matches!(self, Self::Pending | Self::ProfileCreated | Self::Completed)
    }
}

/// One provisioning operation. Holds identifiers and codes only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub provisioner: PrincipalId,
    idempotency_key: String,
    fingerprint: String,
    pub external: ExternalIdentity,
    pub principal: PrincipalId,
    pub profile: ProfileId,
    pub template: String,
    display_name: String,
    /// The random token in the profile's [`MARKER_FILE`].
    marker: String,
    pub state: OperationState,
    #[serde(default)]
    pub failure: Option<String>,
    pub created_unix: u64,
    pub updated_unix: u64,
}

/// The provisioning ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    schema: u32,
    generation: u64,
    #[serde(default)]
    provisioners: BTreeMap<PrincipalId, Provisioner>,
    #[serde(default)]
    operations: BTreeMap<String, Operation>,
    /// `<provisioner>\0<idempotency key>` → operation id.
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            generation: 0,
            provisioners: BTreeMap::new(),
            operations: BTreeMap::new(),
            keys: BTreeMap::new(),
        }
    }
}

impl Ledger {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn provisioner(&self, principal: &PrincipalId) -> Option<&Provisioner> {
        self.provisioners.get(principal)
    }

    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations.get(id)
    }

    pub fn operations(&self) -> impl Iterator<Item = &Operation> {
        self.operations.values()
    }
}

/// A request to provision one profile for one external user.
///
/// Unknown fields are refused, so a request can never carry a toolset, an
/// approval policy, a model, a path or any other profile configuration: the
/// template decides those.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionRequest {
    /// The registered issuer URL, exactly as registered.
    pub issuer: String,
    /// The user's stable subject at that issuer.
    pub subject: String,
    /// The profile id to create.
    pub profile_id: String,
    /// The profile's display name.
    pub display_name: String,
    /// A server-defined template ([`TEMPLATES`]).
    pub template: String,
}

/// Never prints the subject.
impl fmt::Debug for ProvisionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProvisionRequest")
            .field("profile_id", &self.profile_id)
            .field("template", &self.template)
            .finish_non_exhaustive()
    }
}

struct Validated {
    external: ExternalIdentity,
    profile: ProfileId,
    display_name: String,
    template: String,
    fingerprint: String,
}

impl ProvisionRequest {
    fn validate(&self) -> Result<Validated, ProvisioningError> {
        if !valid_issuer(&self.issuer) {
            return Err(ProvisioningError::Invalid("invalid_issuer"));
        }
        if !valid_subject(&self.subject) {
            return Err(ProvisioningError::Invalid("invalid_subject"));
        }
        let profile = ProfileId::new(self.profile_id.as_str())
            .map_err(|_| ProvisioningError::Invalid("invalid_profile_id"))?;
        if RESERVED_PROFILES.contains(&profile.as_str()) {
            return Err(ProvisioningError::Invalid("reserved_profile_id"));
        }
        let name = &self.display_name;
        let length = name.chars().count();
        if !(1..=64).contains(&length) || name.trim() != name || name.chars().any(char::is_control)
        {
            return Err(ProvisioningError::Invalid("invalid_display_name"));
        }
        if !TEMPLATES.contains(&self.template.as_str()) {
            return Err(ProvisioningError::Invalid("invalid_template"));
        }
        let mut hasher = Sha256::new();
        hasher.update(FINGERPRINT_LABEL);
        for field in [
            &self.issuer,
            &self.subject,
            &self.profile_id,
            &self.display_name,
            &self.template,
        ] {
            hasher.update(field.as_bytes());
            hasher.update([0u8]);
        }
        Ok(Validated {
            external: ExternalIdentity {
                issuer: self.issuer.clone(),
                subject: self.subject.clone(),
            },
            profile,
            display_name: self.display_name.clone(),
            template: self.template.clone(),
            fingerprint: hex(&hasher.finalize()),
        })
    }
}

/// An `https://` URL of printable ASCII, without userinfo, query or fragment.
fn valid_issuer(issuer: &str) -> bool {
    issuer.len() <= 256
        && issuer
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && issuer.bytes().all(|b| b.is_ascii_graphic())
        && !issuer.contains(['@', '?', '#', '\\'])
}

/// 1–255 printable ASCII characters (an OpenID Connect `sub` is at most 255
/// ASCII characters).
fn valid_subject(subject: &str) -> bool {
    (1..=255).contains(&subject.len()) && subject.bytes().all(|b| b.is_ascii_graphic())
}

/// 8–128 characters of `[A-Za-z0-9._:-]`.
fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// 32 lowercase hex digits.
fn valid_operation_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The principal id for an external identity: derived on the server, never
/// chosen by the caller. `ext-` and 40 hex digits of a domain-separated
/// SHA-256 of the pair.
pub fn derived_principal(identity: &ExternalIdentity) -> Result<PrincipalId, ProvisioningError> {
    let mut hasher = Sha256::new();
    hasher.update(PRINCIPAL_LABEL);
    hasher.update(identity.issuer.as_bytes());
    hasher.update([0u8]);
    hasher.update(identity.subject.as_bytes());
    let digest = hex(&hasher.finalize());
    PrincipalId::new(format!("ext-{}", &digest[..40])).map_err(|_| ProvisioningError::Corrupt)
}

/// The restrictive profile a template creates. A provisioned profile has no
/// tools (an untrusted run is evaluated under the strict tool policy, where
/// an empty `toolsets` grants nothing), the strict approval policy, tighter
/// run limits and the installation's default model. Only an operator can
/// widen any of it.
pub fn template_profile(
    template: &str,
    id: ProfileId,
    display_name: &str,
) -> Result<AgentProfile, ProvisioningError> {
    match template {
        "assistant" => {
            let mut profile = AgentProfile::new(
                id,
                display_name,
                "You are a helpful assistant.",
                String::new(),
            );
            profile.description = "Provisioned by the account control service.".to_owned();
            profile.toolsets = Vec::new();
            profile.approval_policy = ApprovalPolicy::Strict;
            profile.limits = RunLimits {
                max_turns: 8,
                max_tool_calls: 16,
                max_parallel_tools: 1,
                max_repeated_identical_calls: 2,
                max_tool_output_bytes: 65_536,
                wall_clock_secs: Some(300),
            };
            Ok(profile)
        }
        _ => Err(ProvisioningError::Invalid("invalid_template")),
    }
}

/// What a provisioner may learn about one of its operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OperationStatus {
    pub operation_id: String,
    pub state: OperationState,
    pub failure: Option<String>,
    pub principal_id: PrincipalId,
    pub profile_id: ProfileId,
    pub template: String,
    /// Whether the principal owns the profile now, by the identity store's
    /// effective ownership (record and binding marker).
    pub owner_bound: bool,
    pub created_unix: u64,
    pub updated_unix: u64,
}

/// The result of a provisioning call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionOutcome {
    /// The idempotency key was seen before; this is its operation.
    pub replayed: bool,
    pub status: OperationStatus,
}

/// What [`ProvisioningService::reconcile`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    pub completed: usize,
    pub failed: usize,
    pub still_pending: usize,
    pub staging_removed: usize,
}

/// One audit line: identifiers and fixed codes only. Never a subject, an
/// issuer URL, a display name, a token or a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisioningAudit {
    pub at_unix: u64,
    pub action: String,
    #[serde(default)]
    pub actor: Option<PrincipalId>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub profile: Option<ProfileId>,
    #[serde(default)]
    pub principal: Option<PrincipalId>,
    pub outcome: String,
}

/// The current time in Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// A point at which a test stops an operation, as a crash would. Outside
/// tests the points are inert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    AfterPendingRecorded,
    AfterProfileRenamed,
    AfterProfileCreatedRecorded,
    AfterIdentityCommitted,
}

/// How one step of an operation ended.
enum Step {
    /// Refused on its merits: the operation fails with this code.
    Fail(&'static str),
    /// Interrupted: the operation stays where it is and can be retried.
    Retry(ProvisioningError),
}

impl From<io::Error> for Step {
    fn from(_: io::Error) -> Self {
        Self::Retry(ProvisioningError::Unavailable)
    }
}

/// The provisioning boundary of one installation.
pub struct ProvisioningService {
    home: PathBuf,
    dir: PathBuf,
    identity: IdentityStore,
    now: Clock,
    local: Mutex<()>,
    #[cfg(test)]
    fault: Mutex<Option<Fault>>,
}

impl fmt::Debug for ProvisioningService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProvisioningService")
            .finish_non_exhaustive()
    }
}

impl ProvisioningService {
    /// The service for the installation at `home`. Does no I/O.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            dir: home.join(DIR),
            identity: IdentityStore::new(home.clone()),
            home,
            now: Arc::new(crate::credentials::system_now),
            local: Mutex::new(()),
            #[cfg(test)]
            fault: Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    #[cfg(test)]
    pub(crate) fn inject(&self, fault: Fault) {
        if let Ok(mut slot) = self.fault.lock() {
            *slot = Some(fault);
        }
    }

    /// Stop here once, as a crash would, when a test asked to.
    #[cfg(test)]
    fn crash_point(&self, at: Fault) -> Result<(), ProvisioningError> {
        let mut slot = self
            .fault
            .lock()
            .map_err(|_| ProvisioningError::Unavailable)?;
        if *slot == Some(at) {
            *slot = None;
            return Err(ProvisioningError::Unavailable);
        }
        Ok(())
    }

    #[cfg(not(test))]
    fn crash_point(&self, _at: Fault) -> Result<(), ProvisioningError> {
        Ok(())
    }

    /// Read the ledger. Absent: empty. Unreadable, corrupt or newer: an
    /// error, never an empty ledger.
    pub fn load(&self) -> Result<Ledger, ProvisioningError> {
        let bytes = match std::fs::read(self.dir.join(STATE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Ledger::default()),
            Err(_) => return Err(ProvisioningError::Unavailable),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ProvisioningError::Corrupt)?;
        if value.get("schema").and_then(serde_json::Value::as_u64)
            != Some(u64::from(SCHEMA_VERSION))
        {
            return Err(ProvisioningError::Corrupt);
        }
        serde_json::from_value(value).map_err(|_| ProvisioningError::Corrupt)
    }

    fn save(&self, ledger: &mut Ledger) -> Result<(), ProvisioningError> {
        ledger.generation += 1;
        let bytes = serde_json::to_vec_pretty(ledger).map_err(|_| ProvisioningError::Corrupt)?;
        write_durable(&self.dir.join(STATE), &bytes)?;
        Ok(())
    }

    /// Hold the in-process and cross-process ledger locks.
    fn exclusive<R>(
        &self,
        work: impl FnOnce() -> Result<R, ProvisioningError>,
    ) -> Result<R, ProvisioningError> {
        let _local = self
            .local
            .lock()
            .map_err(|_| ProvisioningError::Unavailable)?;
        crate::paths::create_private_dir(&self.dir).map_err(|_| ProvisioningError::Unavailable)?;
        let lock = open_private(&self.dir.join(LOCK), false)?;
        lock.lock()?;
        let result = work();
        drop(lock);
        result
    }

    fn require_administrator(actor: &Caller) -> Result<(), ProvisioningError> {
        if actor.principal.role != Role::Operator
            || authorize_capability(actor, Capability::Administer).is_err()
        {
            return Err(ProvisioningError::Denied("operator_required"));
        }
        Ok(())
    }

    /// Allow `principal` (an active operator-role principal) to provision
    /// subjects of `issuers` from `templates`. Operator `administer` only.
    /// Re-registering replaces the bounds.
    pub fn register_provisioner(
        &self,
        actor: &Caller,
        principal: &PrincipalId,
        issuers: BTreeSet<String>,
        templates: BTreeSet<String>,
        max_profiles: u32,
    ) -> Result<(), ProvisioningError> {
        Self::require_administrator(actor)?;
        let now = (self.now)();
        let result = self.exclusive(|| {
            let identity = self.identity_document()?;
            let record = identity
                .principal(principal)
                .ok_or(ProvisioningError::Invalid("unknown_principal"))?;
            if record.role != Role::Operator || record.status != PrincipalStatus::Active {
                return Err(ProvisioningError::Invalid(
                    "provisioner_must_be_active_operator",
                ));
            }
            if issuers.is_empty()
                || issuers
                    .iter()
                    .any(|issuer| !valid_issuer(issuer) || !identity.is_trusted_issuer(issuer))
            {
                return Err(ProvisioningError::Invalid("issuer_not_trusted"));
            }
            if templates.is_empty()
                || templates
                    .iter()
                    .any(|template| !TEMPLATES.contains(&template.as_str()))
            {
                return Err(ProvisioningError::Invalid("invalid_template"));
            }
            if !(1..=MAX_PROFILES_PER_PROVISIONER).contains(&max_profiles) {
                return Err(ProvisioningError::Invalid("invalid_quota"));
            }
            let mut ledger = self.load()?;
            ledger.provisioners.insert(
                principal.clone(),
                Provisioner {
                    principal: principal.clone(),
                    issuers,
                    templates,
                    max_profiles,
                    created_unix: now,
                    revoked_unix: None,
                },
            );
            self.save(&mut ledger)
        });
        self.audit(
            now,
            Event {
                action: "provisioner_registered",
                actor: Some(&actor.principal.id),
                principal: Some(principal),
                ..Event::default()
            },
            outcome(&result),
        );
        result
    }

    /// Withdraw a provisioner's registration. Its next call is refused, and
    /// its unfinished operations fail at reconciliation. What it already
    /// provisioned stays as it is. Operator `administer` only; idempotent.
    pub fn revoke_provisioner(
        &self,
        actor: &Caller,
        principal: &PrincipalId,
    ) -> Result<(), ProvisioningError> {
        Self::require_administrator(actor)?;
        let now = (self.now)();
        let result = self.exclusive(|| {
            let mut ledger = self.load()?;
            let record = ledger
                .provisioners
                .get_mut(principal)
                .ok_or(ProvisioningError::Invalid("unknown_provisioner"))?;
            record.revoked_unix.get_or_insert(now);
            self.save(&mut ledger)
        });
        self.audit(
            now,
            Event {
                action: "provisioner_revoked",
                actor: Some(&actor.principal.id),
                principal: Some(principal),
                ..Event::default()
            },
            outcome(&result),
        );
        result
    }

    fn identity_document(&self) -> Result<IdentityDocument, ProvisioningError> {
        self.identity
            .load()
            .map_err(|_| ProvisioningError::Unavailable)
    }

    /// Admit `caller` as a provisioner, from the current stores: an issued,
    /// unrevoked, unexpired credential of an active operator-role principal,
    /// restricted to exactly `{provision}`, whose principal is a registered,
    /// unrevoked provisioner. Returns the caller as the store sees it now.
    fn admit(
        &self,
        caller: &Caller,
        identity: &IdentityDocument,
        ledger: &Ledger,
        now: u64,
    ) -> Result<(Caller, Provisioner), ProvisioningError> {
        let PrincipalSource::Credential { key_id } = &caller.principal.source else {
            return Err(ProvisioningError::Denied("unauthenticated"));
        };
        let current = identity
            .credentials()
            .caller_for_at(key_id, now)
            .map_err(|_| ProvisioningError::Denied("unauthenticated"))?;
        if current.principal.id != caller.principal.id {
            return Err(ProvisioningError::Denied("unauthenticated"));
        }
        if current.principal.role != Role::Operator {
            return Err(ProvisioningError::Denied("not_a_provisioner"));
        }
        let exactly: BTreeSet<Capability> = [Capability::Provision].into();
        if current.restriction.capabilities.as_ref() != Some(&exactly) {
            return Err(ProvisioningError::Denied(
                "credential_not_provisioning_only",
            ));
        }
        if identity
            .principal(&current.principal.id)
            .is_none_or(|record| record.status != PrincipalStatus::Active)
        {
            return Err(ProvisioningError::Denied("provisioner_inactive"));
        }
        let provisioner = ledger
            .provisioners
            .get(&current.principal.id)
            .ok_or(ProvisioningError::Denied("not_a_provisioner"))?;
        if provisioner.revoked_unix.is_some() {
            return Err(ProvisioningError::Denied("provisioner_revoked"));
        }
        Ok((current, provisioner.clone()))
    }

    /// Provision `request` under `idempotency_key` as `caller`.
    ///
    /// The same key with the same request returns the same operation (and
    /// resumes it if it was interrupted); the same key with a different
    /// request is `idempotency_conflict`. A failed operation is an outcome,
    /// returned in the status, not an error.
    pub fn provision(
        &self,
        caller: &Caller,
        idempotency_key: &str,
        request: &ProvisionRequest,
    ) -> Result<ProvisionOutcome, ProvisioningError> {
        let now = (self.now)();
        let mut subject = (None, None);
        let result = self.exclusive(|| {
            let identity = self.identity_document()?;
            let mut ledger = self.load()?;
            let (actor, provisioner) = self.admit(caller, &identity, &ledger, now)?;
            if !valid_key(idempotency_key) {
                return Err(ProvisioningError::Invalid("invalid_idempotency_key"));
            }
            let request = request.validate()?;
            subject.0 = Some(request.profile.clone());
            if actor
                .restriction
                .profiles
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&request.profile))
            {
                return Err(ProvisioningError::Denied("profile_outside_credential"));
            }
            let scoped = format!("{}\0{idempotency_key}", provisioner.principal);
            if let Some(id) = ledger.keys.get(&scoped).cloned() {
                let operation = ledger
                    .operations
                    .get(&id)
                    .ok_or(ProvisioningError::Corrupt)?;
                if operation.fingerprint != request.fingerprint {
                    return Err(ProvisioningError::Conflict("idempotency_conflict"));
                }
                subject.1 = Some(id.clone());
                let status = self.resume(&mut ledger, &id, &actor, now)?;
                return Ok(ProvisionOutcome {
                    replayed: true,
                    status,
                });
            }
            let id = self.begin(
                &mut ledger,
                &identity,
                &provisioner,
                idempotency_key,
                request,
                now,
            )?;
            subject.1 = Some(id.clone());
            self.crash_point(Fault::AfterPendingRecorded)?;
            let status = self.resume(&mut ledger, &id, &actor, now)?;
            Ok(ProvisionOutcome {
                replayed: false,
                status,
            })
        });
        let (profile, operation) = subject;
        self.audit(
            now,
            Event {
                action: match &result {
                    Ok(done) if done.replayed => "provision_replayed",
                    _ => "provision",
                },
                actor: Some(&caller.principal.id),
                operation: operation.as_deref(),
                profile: profile.as_ref(),
                principal: result.as_ref().ok().map(|done| &done.status.principal_id),
            },
            match &result {
                Ok(done) => state_outcome(&done.status),
                Err(_) => outcome(&result),
            },
        );
        result
    }

    /// Record a new operation as pending, after every check that does not
    /// need the profile to exist.
    fn begin(
        &self,
        ledger: &mut Ledger,
        identity: &IdentityDocument,
        provisioner: &Provisioner,
        idempotency_key: &str,
        request: Validated,
        now: u64,
    ) -> Result<String, ProvisioningError> {
        if !provisioner.issuers.contains(&request.external.issuer)
            || !identity.is_trusted_issuer(&request.external.issuer)
        {
            return Err(ProvisioningError::Denied("issuer_not_allowed"));
        }
        if !provisioner.templates.contains(&request.template) {
            return Err(ProvisioningError::Denied("template_not_allowed"));
        }
        let live = ledger
            .operations
            .values()
            .filter(|op| op.provisioner == provisioner.principal && op.state.holds_profile())
            .count();
        if live >= provisioner.max_profiles as usize {
            return Err(ProvisioningError::Denied("quota_exceeded"));
        }
        if ledger.operations.len() >= MAX_OPERATIONS {
            return Err(ProvisioningError::Denied("ledger_full"));
        }
        // A profile id is available only if nothing knows it: no directory,
        // no ownership record (even a stale one) and no live or revoked
        // operation. Reusing an id never inherits an earlier owner.
        let handle = ProfileStore::new(&self.home).handle(&request.profile);
        let known = std::fs::symlink_metadata(handle.dir()).is_ok()
            || identity
                .recorded_ownership()
                .get(&request.profile)
                .is_some()
            || ledger
                .operations
                .values()
                .any(|op| op.profile == request.profile && op.state != OperationState::Failed);
        if known {
            return Err(ProvisioningError::Conflict("profile_unavailable"));
        }
        // An existing mapping always wins; else the derived id, which must
        // be free.
        let principal = match identity.external(&request.external) {
            Some(existing) => {
                let record = identity
                    .principal(existing)
                    .ok_or(ProvisioningError::Corrupt)?;
                if record.role != Role::User {
                    return Err(ProvisioningError::Conflict("external_identity_conflict"));
                }
                if record.status != PrincipalStatus::Active {
                    return Err(ProvisioningError::Conflict("principal_inactive"));
                }
                existing.clone()
            }
            None => {
                let derived = derived_principal(&request.external)?;
                if identity.principal(&derived).is_some() {
                    return Err(ProvisioningError::Conflict("external_identity_conflict"));
                }
                derived
            }
        };
        let id = random_hex::<16>()?;
        let marker = random_hex::<16>()?;
        if ledger.operations.contains_key(&id) {
            return Err(ProvisioningError::Unavailable);
        }
        ledger.keys.insert(
            format!("{}\0{idempotency_key}", provisioner.principal),
            id.clone(),
        );
        ledger.operations.insert(
            id.clone(),
            Operation {
                id: id.clone(),
                provisioner: provisioner.principal.clone(),
                idempotency_key: idempotency_key.to_owned(),
                fingerprint: request.fingerprint,
                external: request.external,
                principal,
                profile: request.profile,
                template: request.template,
                display_name: request.display_name,
                marker,
                state: OperationState::Pending,
                failure: None,
                created_unix: now,
                updated_unix: now,
            },
        );
        self.save(ledger)?;
        Ok(id)
    }

    /// Drive operation `id` as far as it goes.
    fn resume(
        &self,
        ledger: &mut Ledger,
        id: &str,
        actor: &Caller,
        now: u64,
    ) -> Result<OperationStatus, ProvisioningError> {
        loop {
            let operation = ledger
                .operations
                .get(id)
                .cloned()
                .ok_or(ProvisioningError::Corrupt)?;
            let step = match operation.state {
                OperationState::Pending => self
                    .materialize(&operation)
                    .map(|()| OperationState::ProfileCreated),
                OperationState::ProfileCreated => self
                    .bind(&operation, actor, now)
                    .map(|()| OperationState::Completed),
                _ => return self.status_of(&operation),
            };
            match step {
                Ok(next) => {
                    self.transition(ledger, id, next, None, now)?;
                    if next == OperationState::ProfileCreated {
                        self.crash_point(Fault::AfterProfileCreatedRecorded)?;
                    }
                }
                Err(Step::Fail(code)) => {
                    let created = operation.state == OperationState::ProfileCreated;
                    self.transition(ledger, id, OperationState::Failed, Some(code), now)?;
                    if created {
                        self.quarantine(&operation);
                    }
                    self.clear_staging(&operation.id);
                }
                Err(Step::Retry(error)) => return Err(error),
            }
        }
    }

    fn transition(
        &self,
        ledger: &mut Ledger,
        id: &str,
        state: OperationState,
        failure: Option<&str>,
        now: u64,
    ) -> Result<(), ProvisioningError> {
        let operation = ledger
            .operations
            .get_mut(id)
            .ok_or(ProvisioningError::Corrupt)?;
        operation.state = state;
        operation.failure = failure.map(str::to_owned);
        operation.updated_unix = now;
        self.save(ledger)
    }

    fn marker_matches(&self, operation: &Operation) -> bool {
        let handle = ProfileStore::new(&self.home).handle(&operation.profile);
        handle.manifest_file().is_file()
            && std::fs::read_to_string(handle.dir().join(MARKER_FILE))
                .is_ok_and(|marker| marker.trim() == operation.marker)
    }

    /// Create the operation's profile: built in staging, then renamed into
    /// place in one step. A profile already there counts only if it carries
    /// this operation's marker.
    fn materialize(&self, operation: &Operation) -> Result<(), Step> {
        let profiles = ProfileStore::new(&self.home);
        let target = profiles.handle(&operation.profile).dir().to_path_buf();
        if std::fs::symlink_metadata(&target).is_ok() {
            return if self.marker_matches(operation) {
                Ok(())
            } else {
                Err(Step::Fail("profile_unavailable"))
            };
        }
        let staging = self.dir.join(STAGING).join(&operation.id);
        if std::fs::symlink_metadata(&staging).is_ok() {
            // Ours, left by an interrupted attempt.
            std::fs::remove_dir_all(&staging)?;
        }
        let profile = template_profile(
            &operation.template,
            operation.profile.clone(),
            &operation.display_name,
        )
        .map_err(|_| Step::Fail("invalid_template"))?;
        let staged = ProfileStore::new(&staging);
        staged
            .create(&profile)
            .map_err(|_| Step::Retry(ProvisioningError::Unavailable))?;
        let staged_dir = staged.handle(&operation.profile).dir().to_path_buf();
        write_durable(&staged_dir.join(MARKER_FILE), operation.marker.as_bytes())?;
        let parent = target
            .parent()
            .ok_or(Step::Retry(ProvisioningError::Corrupt))?;
        crate::paths::create_private_dir(parent)
            .map_err(|_| Step::Retry(ProvisioningError::Unavailable))?;
        if let Err(error) = std::fs::rename(&staged_dir, &target) {
            // Someone else's profile took the id meanwhile: never touched.
            let outcome = if std::fs::symlink_metadata(&target).is_ok() {
                if self.marker_matches(operation) {
                    Ok(())
                } else {
                    Err(Step::Fail("profile_unavailable"))
                }
            } else {
                Err(Step::from(error))
            };
            self.clear_staging(&operation.id);
            return outcome;
        }
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        self.clear_staging(&operation.id);
        self.crash_point(Fault::AfterProfileRenamed)
            .map_err(Step::Retry)?;
        // The rename landed where something else may have appeared first
        // (a directory replaced while empty): check it is ours.
        if !self.marker_matches(operation) {
            return Err(Step::Fail("profile_unavailable"));
        }
        Ok(())
    }

    /// Create or resolve the principal, record it as the owner and bind the
    /// profile: one identity transaction, idempotent by operation.
    fn bind(&self, operation: &Operation, actor: &Caller, now: u64) -> Result<(), Step> {
        if !self.marker_matches(operation) {
            return Err(Step::Fail("profile_unavailable"));
        }
        let key = format!("provisioning:{}", operation.id);
        let outcome =
            self.identity
                .transact(actor, now, Some((&key, &operation.fingerprint)), |tx| {
                    match tx.document().external(&operation.external).cloned() {
                        Some(existing) if existing != operation.principal => {
                            return Err(IdentityStoreError::Refused(
                                Refusal::DuplicateExternalIdentity,
                            ));
                        }
                        Some(_) => {}
                        None => {
                            if tx.document().principal(&operation.principal).is_some() {
                                return Err(IdentityStoreError::Refused(Refusal::PrincipalExists));
                            }
                            tx.create_principal(
                                operation.principal.clone(),
                                Role::User,
                                Some(operation.external.clone()),
                            )?;
                        }
                    }
                    if tx
                        .document()
                        .recorded_ownership()
                        .get(&operation.profile)
                        .is_some_and(|record| record.owner != operation.principal)
                    {
                        return Err(IdentityStoreError::Ownership(OwnershipError::Conflict));
                    }
                    tx.assign_owner(&operation.profile, &operation.principal)?;
                    Ok(())
                });
        match outcome {
            Ok(Outcome::Applied { .. } | Outcome::Replayed { .. }) => {
                self.crash_point(Fault::AfterIdentityCommitted)
                    .map_err(Step::Retry)?;
                Ok(())
            }
            Err(error) => Err(identity_step(error)),
        }
    }

    /// Move a failed operation's profile out of `profiles/`, if it is still
    /// ours and has no owner. Never deletes it.
    fn quarantine(&self, operation: &Operation) {
        if !self.marker_matches(operation) {
            return;
        }
        let owned = self
            .identity
            .load()
            .map(|document| {
                document
                    .recorded_ownership()
                    .get(&operation.profile)
                    .is_some()
            })
            .unwrap_or(true);
        if owned {
            return;
        }
        let quarantine = self.dir.join(QUARANTINE);
        if crate::paths::create_private_dir(&quarantine).is_ok() {
            let source = ProfileStore::new(&self.home)
                .handle(&operation.profile)
                .dir()
                .to_path_buf();
            let _ = std::fs::rename(source, quarantine.join(&operation.id));
        }
    }

    fn clear_staging(&self, id: &str) {
        let _ = std::fs::remove_dir_all(self.dir.join(STAGING).join(id));
    }

    fn status_of(&self, operation: &Operation) -> Result<OperationStatus, ProvisioningError> {
        let document = self.identity_document()?;
        let owner_bound = self
            .identity
            .effective_ownership(&document)
            .get(&operation.profile)
            .is_some_and(|record| record.owner == operation.principal);
        Ok(OperationStatus {
            operation_id: operation.id.clone(),
            state: operation.state,
            failure: operation.failure.clone(),
            principal_id: operation.principal.clone(),
            profile_id: operation.profile.clone(),
            template: operation.template.clone(),
            owner_bound,
            created_unix: operation.created_unix,
            updated_unix: operation.updated_unix,
        })
    }

    /// The status of one of `caller`'s operations. Another provisioner's
    /// operation is answered exactly like a missing one.
    pub fn status(
        &self,
        caller: &Caller,
        operation_id: &str,
    ) -> Result<OperationStatus, ProvisioningError> {
        let now = (self.now)();
        let result = self.exclusive(|| {
            let identity = self.identity_document()?;
            let ledger = self.load()?;
            let (_, provisioner) = self.admit(caller, &identity, &ledger, now)?;
            let operation = owned_operation(&ledger, &provisioner, operation_id)?;
            self.status_of(operation)
        });
        if result.is_err() {
            self.audit(
                now,
                Event {
                    action: "status",
                    actor: Some(&caller.principal.id),
                    operation: valid_operation_id(operation_id).then_some(operation_id),
                    ..Event::default()
                },
                outcome(&result),
            );
        }
        result
    }

    /// Revoke the user's access to the profile `operation_id` provisioned:
    /// the ownership record and binding are removed in one identity
    /// transaction, so the user's next request, and every running run at
    /// its next revalidation, is refused. The profile itself is kept for the
    /// operator. Idempotent. A profile an operator has since reassigned is
    /// not the provisioner's to revoke.
    pub fn revoke(
        &self,
        caller: &Caller,
        operation_id: &str,
    ) -> Result<OperationStatus, ProvisioningError> {
        let now = (self.now)();
        let mut subject = (None, None);
        let result = self.exclusive(|| {
            let identity = self.identity_document()?;
            let mut ledger = self.load()?;
            let (actor, provisioner) = self.admit(caller, &identity, &ledger, now)?;
            let operation = owned_operation(&ledger, &provisioner, operation_id)?.clone();
            subject = (
                Some(operation.profile.clone()),
                Some(operation.principal.clone()),
            );
            match operation.state {
                OperationState::Revoked => return self.status_of(&operation),
                OperationState::Completed => {}
                OperationState::Pending | OperationState::ProfileCreated => {
                    return Err(ProvisioningError::Conflict("operation_in_progress"));
                }
                OperationState::Failed => {
                    return Err(ProvisioningError::Conflict("not_revocable"));
                }
            }
            let key = format!("provisioning-revoke:{}", operation.id);
            let outcome =
                self.identity
                    .transact(&actor, now, Some((&key, &operation.fingerprint)), |tx| {
                        let owner = tx
                            .document()
                            .recorded_ownership()
                            .get(&operation.profile)
                            .map(|record| record.owner.clone());
                        match owner {
                            Some(owner) if owner != operation.principal => {
                                Err(IdentityStoreError::Ownership(OwnershipError::Conflict))
                            }
                            Some(_) => tx.remove_profile(&operation.profile),
                            None => Ok(()),
                        }
                    });
            match outcome {
                Ok(_) => {}
                Err(IdentityStoreError::Ownership(OwnershipError::Conflict)) => {
                    return Err(ProvisioningError::Conflict("ownership_changed"));
                }
                Err(IdentityStoreError::Denied(_)) => {
                    return Err(ProvisioningError::Denied("not_a_provisioner"));
                }
                Err(_) => return Err(ProvisioningError::Unavailable),
            }
            self.transition(
                &mut ledger,
                &operation.id,
                OperationState::Revoked,
                None,
                now,
            )?;
            let revoked = ledger
                .operations
                .get(&operation.id)
                .ok_or(ProvisioningError::Corrupt)?;
            self.status_of(revoked)
        });
        let (profile, principal) = subject;
        self.audit(
            now,
            Event {
                action: "revoke",
                actor: Some(&caller.principal.id),
                operation: valid_operation_id(operation_id).then_some(operation_id),
                profile: profile.as_ref(),
                principal: principal.as_ref(),
            },
            match &result {
                Ok(status) => state_outcome(status),
                Err(_) => outcome(&result),
            },
        );
        result
    }

    /// Finish or fail every interrupted operation, as `actor` (an operator
    /// holding `administer`), and clear abandoned staging. An operation of a
    /// provisioner that was revoked or removed fails rather than completes.
    /// Run it after a crash or restore, before the service takes requests.
    pub fn reconcile(&self, actor: &Caller) -> Result<ReconcileReport, ProvisioningError> {
        Self::require_administrator(actor)?;
        let now = (self.now)();
        let result = self.exclusive(|| {
            let mut ledger = self.load()?;
            let mut report = ReconcileReport::default();
            let open: Vec<Operation> = ledger
                .operations
                .values()
                .filter(|op| !op.state.is_terminal())
                .cloned()
                .collect();
            for operation in open {
                let admitted = ledger
                    .provisioners
                    .get(&operation.provisioner)
                    .is_some_and(|record| record.revoked_unix.is_none());
                if !admitted {
                    self.transition(
                        &mut ledger,
                        &operation.id,
                        OperationState::Failed,
                        Some("provisioner_revoked"),
                        now,
                    )?;
                    if operation.state == OperationState::ProfileCreated {
                        self.quarantine(&operation);
                    }
                    self.clear_staging(&operation.id);
                    report.failed += 1;
                    continue;
                }
                match self.resume(&mut ledger, &operation.id, actor, now) {
                    Ok(status) if status.state == OperationState::Completed => {
                        report.completed += 1;
                    }
                    Ok(_) => report.failed += 1,
                    Err(_) => report.still_pending += 1,
                }
            }
            if let Ok(entries) = std::fs::read_dir(self.dir.join(STAGING)) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let live = ledger
                        .operations
                        .get(&name)
                        .is_some_and(|op| !op.state.is_terminal());
                    if !live && std::fs::remove_dir_all(entry.path()).is_ok() {
                        report.staging_removed += 1;
                    }
                }
            }
            Ok(report)
        });
        self.audit(
            now,
            Event {
                action: "reconcile",
                actor: Some(&actor.principal.id),
                ..Event::default()
            },
            outcome(&result),
        );
        result
    }

    fn audit(&self, now: u64, event: Event<'_>, outcome: String) {
        let record = ProvisioningAudit {
            at_unix: now,
            action: event.action.to_owned(),
            actor: event.actor.cloned(),
            operation: event.operation.map(str::to_owned),
            profile: event.profile.cloned(),
            principal: event.principal.cloned(),
            outcome,
        };
        let Ok(mut line) = serde_json::to_vec(&record) else {
            return;
        };
        line.push(b'\n');
        if crate::paths::create_private_dir(&self.dir).is_err() {
            return;
        }
        let path = self.dir.join(AUDIT);
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_AUDIT_BYTES) {
            let _ = std::fs::rename(&path, self.dir.join(format!("{AUDIT}.1")));
        }
        if let Ok(mut file) = open_private(&path, true) {
            let _ = file.write_all(&line);
        }
    }

    /// The audit trail, oldest first.
    pub fn audit_log(&self) -> Result<Vec<ProvisioningAudit>, ProvisioningError> {
        let mut records = Vec::new();
        for name in [format!("{AUDIT}.1"), AUDIT.to_owned()] {
            let file = match std::fs::File::open(self.dir.join(name)) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(ProvisioningError::Unavailable),
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

    /// Every file and directory the service writes (for privacy tests).
    pub fn root(&self) -> &Path {
        &self.dir
    }
}

/// What an audit line is about.
#[derive(Clone, Copy, Default)]
struct Event<'a> {
    action: &'static str,
    actor: Option<&'a PrincipalId>,
    operation: Option<&'a str>,
    profile: Option<&'a ProfileId>,
    principal: Option<&'a PrincipalId>,
}

/// `ok`, or `refused:<code>`.
fn outcome<R>(result: &Result<R, ProvisioningError>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => format!("refused:{}", error.code()),
    }
}

/// An operation's state as an audit outcome: `completed`, `failed:<code>`…
fn state_outcome(status: &OperationStatus) -> String {
    let state = match status.state {
        OperationState::Pending => "pending",
        OperationState::ProfileCreated => "profile_created",
        OperationState::Completed => "completed",
        OperationState::Revoked => "revoked",
        OperationState::Failed => {
            return format!("failed:{}", status.failure.as_deref().unwrap_or("unknown"));
        }
    };
    state.to_owned()
}

/// `operation_id` if it belongs to `provisioner`; another's, a malformed id
/// and a missing one are all `UnknownOperation`.
fn owned_operation<'a>(
    ledger: &'a Ledger,
    provisioner: &Provisioner,
    operation_id: &str,
) -> Result<&'a Operation, ProvisioningError> {
    if !valid_operation_id(operation_id) {
        return Err(ProvisioningError::UnknownOperation);
    }
    ledger
        .operations
        .get(operation_id)
        .filter(|op| op.provisioner == provisioner.principal)
        .ok_or(ProvisioningError::UnknownOperation)
}

/// What an identity-store refusal means for the operation: a conflict on
/// its merits fails it; anything else leaves it retryable.
fn identity_step(error: IdentityStoreError) -> Step {
    match error {
        IdentityStoreError::Refused(Refusal::UntrustedIssuer | Refusal::InvalidIssuer) => {
            Step::Fail("issuer_not_trusted")
        }
        IdentityStoreError::Refused(Refusal::PrincipalInactive) => Step::Fail("principal_inactive"),
        IdentityStoreError::Refused(
            Refusal::DuplicateExternalIdentity
            | Refusal::PrincipalExists
            | Refusal::OwnerMustBeUser,
        ) => Step::Fail("external_identity_conflict"),
        IdentityStoreError::Refused(Refusal::ProfileMissing) => Step::Fail("profile_unavailable"),
        IdentityStoreError::Ownership(OwnershipError::Conflict) => Step::Fail("ownership_changed"),
        IdentityStoreError::Denied(_) => {
            Step::Retry(ProvisioningError::Denied("not_a_provisioner"))
        }
        _ => Step::Retry(ProvisioningError::Unavailable),
    }
}

fn random_hex<const N: usize>() -> Result<String, ProvisioningError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| ProvisioningError::Unavailable)?;
    Ok(hex(&bytes))
}

#[cfg(test)]
mod tests;
