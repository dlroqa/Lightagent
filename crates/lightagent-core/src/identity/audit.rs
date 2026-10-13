//! Authorization audit records.
//!
//! A record says who asked to do what to which profile, and what was decided.
//! Every field is an identifier, a fixed-vocabulary code or a timestamp: there
//! is no free-text field, so a record cannot carry a message, a prompt, a tool
//! output or a token.

use serde::{Deserialize, Serialize};

use super::{Caller, Capability, Denial, PrincipalId, PrincipalSource};
use crate::profile::ProfileId;

/// What was attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Presenting a credential, recorded when it is refused.
    Authenticate,
    /// A profile-scoped or installation-wide request, by capability.
    Request(Capability),
    CredentialIssued,
    CredentialRevoked,
    OwnerAssigned,
    OwnerReassigned,
    GrantAdded,
    GrantRevoked,
    CredentialRotated,
    PrincipalCreated,
    PrincipalSuspended,
    PrincipalReactivated,
    IssuerTrusted,
    /// A profile's ownership record was removed (profile deleted or released).
    OwnerRemoved,
    /// A repeated provisioning request matched an earlier one.
    IdempotentReplay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Allowed,
    Denied(Denial),
}

/// One audit line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub at_unix: u64,
    /// `None` when no identity could be established.
    pub principal: Option<PrincipalId>,
    /// The credential's public key id, when one was used. Never the secret.
    pub key_id: Option<String>,
    pub action: AuditAction,
    pub profile: Option<ProfileId>,
    pub outcome: AuditOutcome,
}

impl AuditRecord {
    /// The record for one authorization decision.
    pub fn decision(
        at_unix: u64,
        caller: Option<&Caller>,
        action: AuditAction,
        profile: Option<&ProfileId>,
        outcome: Result<(), Denial>,
    ) -> Self {
        Self {
            at_unix,
            principal: caller.map(|caller| caller.principal.id.clone()),
            key_id: caller.and_then(|caller| match &caller.principal.source {
                PrincipalSource::Credential { key_id } => Some(key_id.clone()),
                _ => None,
            }),
            action,
            profile: profile.cloned(),
            outcome: match outcome {
                Ok(()) => AuditOutcome::Allowed,
                Err(denial) => AuditOutcome::Denied(denial),
            },
        }
    }
}
