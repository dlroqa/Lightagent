//! Caller identity and profile authorization: the shared vocabulary.
//!
//! This module decides *whether a caller may act on a profile*. It answers in
//! the order every entry point will apply it:
//!
//! ```text
//! credential ─► Principal ─► capability check ─► profile ownership check ─► AuthorizedProfile
//! ```
//!
//! It is pure policy over values: no HTTP, no files. Entry points resolve a
//! [`Caller`] (from a bearer credential, the loopback compatibility mode or
//! the local OS user) and ask [`authorize`] before they build anything
//! profile-specific. An [`AuthorizedProfile`] can only be obtained from
//! [`authorize`], so code that requires one cannot run on an unchecked,
//! client-supplied profile id.
//!
//! What this module deliberately does not consult: Jev routing or
//! classification permissions, tool approvals, and toolsets. Those govern
//! what an *authorized* run may do; none of them authorizes a caller.

mod audit;
mod credential;
mod ownership;

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::profile::ProfileId;

pub use audit::{AuditAction, AuditOutcome, AuditRecord};
pub use credential::{CredentialError, CredentialRecord, CredentialStore, IssuedToken};
pub use ownership::{Assignment, OwnershipError, OwnershipRegistry, ProfileOwnership};

/// A server-verified identity. Never derived from a profile id.
///
/// The same rule as a profile id: `^[a-z0-9][a-z0-9_-]{0,63}$`, so it is safe
/// in a path, a log line and an audit record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn new(id: impl Into<String>) -> Result<Self, IdentityError> {
        let id = id.into();
        // Same alphabet and bound as profile ids; the two never share meaning.
        if ProfileId::new(id.clone()).is_ok() {
            Ok(Self(id))
        } else {
            Err(IdentityError::InvalidPrincipal)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PrincipalId {
    type Error = IdentityError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PrincipalId> for String {
    fn from(value: PrincipalId) -> Self {
        value.0
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identity errors carry fixed text only: never a token or a message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("a principal id must match ^[a-z0-9][a-z0-9_-]{{0,63}}$")]
    InvalidPrincipal,
}

/// What a principal is allowed to be, before any profile is considered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Administers the installation: every capability, every profile.
    Operator,
    /// Acts only on profiles it owns or was granted.
    User,
}

/// How the principal was established. Recorded for audit; it never widens
/// what the role allows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PrincipalSource {
    /// Today's single-user API: an open loopback bind, or the one
    /// `--key-env` key. Always the local operator.
    LocalCompatibility,
    /// The terminal or ACP: the local OS user running the process.
    LocalProcess,
    /// A credential issued by this installation, identified by its public id.
    Credential { key_id: String },
}

/// An authenticated caller's identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: PrincipalId,
    pub role: Role,
    pub source: PrincipalSource,
}

/// The id of the local operator in the compatibility and local modes.
pub const LOCAL_OPERATOR: &str = "local-operator";

impl Principal {
    /// The operator of a single-user installation, for the compatibility
    /// mode (`source = LocalCompatibility`) or the terminal and ACP
    /// (`LocalProcess`). It is authorized exactly as every caller is today.
    pub fn local_operator(source: PrincipalSource) -> Self {
        Self {
            id: PrincipalId(LOCAL_OPERATOR.to_owned()),
            role: Role::Operator,
            source,
        }
    }
}

/// An action a caller asks to take. One per API scope, plus the operator-only
/// actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    RunsRead,
    RunsWrite,
    SessionsRead,
    SessionsWrite,
    /// The tool catalog and the profile's Jev status.
    ToolsRead,
    ApprovalsWrite,
    /// Installation settings, connection checks, credentials and ownership.
    Administer,
    /// Creating profiles and assigning their owners (the control plane).
    Provision,
}

impl Capability {
    pub const ALL: [Capability; 8] = [
        Self::RunsRead,
        Self::RunsWrite,
        Self::SessionsRead,
        Self::SessionsWrite,
        Self::ToolsRead,
        Self::ApprovalsWrite,
        Self::Administer,
        Self::Provision,
    ];

    /// Whether only an operator may hold this capability, whatever a
    /// credential or grant says.
    pub fn operator_only(self) -> bool {
        matches!(self, Self::Administer | Self::Provision)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunsRead => "runs_read",
            Self::RunsWrite => "runs_write",
            Self::SessionsRead => "sessions_read",
            Self::SessionsWrite => "sessions_write",
            Self::ToolsRead => "tools_read",
            Self::ApprovalsWrite => "approvals_write",
            Self::Administer => "administer",
            Self::Provision => "provision",
        }
    }
}

/// Limits a credential places on its principal. It can only narrow: a
/// restriction never adds a capability or a profile the role and ownership do
/// not already allow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restriction {
    /// `None`: whatever the role allows.
    pub capabilities: Option<BTreeSet<Capability>>,
    /// `None`: whatever ownership allows.
    pub profiles: Option<BTreeSet<ProfileId>>,
}

/// A resolved, authenticated caller: who, and what their credential allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub principal: Principal,
    pub restriction: Restriction,
}

impl Caller {
    /// The single-user compatibility caller: today's behavior exactly.
    pub fn local_operator(source: PrincipalSource) -> Self {
        Self {
            principal: Principal::local_operator(source),
            restriction: Restriction::default(),
        }
    }
}

/// Why a request was refused. A fixed vocabulary, safe to log and return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denial {
    /// No valid credential.
    Unauthenticated,
    /// The credential was valid once and has been revoked.
    Revoked,
    /// The role or the credential does not allow this capability.
    MissingCapability,
    /// The credential is limited to other profiles.
    ProfileOutsideCredential,
    /// The profile has no recorded owner. Only an operator may act on it.
    ProfileUnowned,
    /// Another principal owns the profile and granted nothing for this.
    ProfileNotGranted,
}

impl Denial {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Revoked => "revoked",
            Self::MissingCapability => "missing_capability",
            Self::ProfileOutsideCredential => "profile_outside_credential",
            Self::ProfileUnowned => "profile_unowned",
            Self::ProfileNotGranted => "profile_not_granted",
        }
    }

    /// The HTTP status an API returns for it: 401 when there is no
    /// identity, 403 otherwise. A refusal never reveals whether a profile
    /// exists beyond what the caller may already see.
    pub fn http_status(self) -> u16 {
        match self {
            Self::Unauthenticated | Self::Revoked => 401,
            _ => 403,
        }
    }
}

/// Proof that a caller may take `capability` on `profile`. Only
/// [`authorize`] constructs one; the fields are private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedProfile {
    profile: ProfileId,
    principal: PrincipalId,
    capability: Capability,
}

impl AuthorizedProfile {
    pub fn profile(&self) -> &ProfileId {
        &self.profile
    }

    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    pub fn capability(&self) -> Capability {
        self.capability
    }
}

/// Whether `caller` holds `capability` at all, before any profile. For
/// installation-wide actions (settings, provisioning, the tool catalog).
pub fn authorize_capability(caller: &Caller, capability: Capability) -> Result<(), Denial> {
    let role_allows = match caller.principal.role {
        Role::Operator => true,
        Role::User => !capability.operator_only(),
    };
    let credential_allows = caller
        .restriction
        .capabilities
        .as_ref()
        .is_none_or(|allowed| allowed.contains(&capability));
    if role_allows && credential_allows {
        Ok(())
    } else {
        Err(Denial::MissingCapability)
    }
}

/// Whether `caller` may take `capability` on `profile`, the server-resolved
/// profile the request would act on. The order is fixed: capability, then the
/// credential's profile limit, then ownership.
///
/// - An operator may act on every profile, owned or not: the single-user
///   installation, where the local operator is the only caller, keeps working
///   unchanged.
/// - A user may act only on a profile it owns, or one whose owner granted it
///   this capability. A profile with no recorded owner is refused: new
///   multi-user installations fail closed.
/// - Nothing else is consulted. In particular a Jev permission, a profile's
///   name, or the caller knowing the profile id authorizes nothing.
pub fn authorize(
    caller: &Caller,
    capability: Capability,
    profile: &ProfileId,
    ownership: &OwnershipRegistry,
) -> Result<AuthorizedProfile, Denial> {
    authorize_capability(caller, capability)?;
    if caller
        .restriction
        .profiles
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(profile))
    {
        return Err(Denial::ProfileOutsideCredential);
    }
    if caller.principal.role == Role::User {
        let Some(record) = ownership.get(profile) else {
            return Err(Denial::ProfileUnowned);
        };
        if !record.permits(&caller.principal.id, capability) {
            return Err(Denial::ProfileNotGranted);
        }
    }
    Ok(AuthorizedProfile {
        profile: profile.clone(),
        principal: caller.principal.id.clone(),
        capability,
    })
}

#[cfg(test)]
mod tests;
