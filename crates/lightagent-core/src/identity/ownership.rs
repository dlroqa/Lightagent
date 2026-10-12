//! Who owns each profile, and what the owner granted to others.
//!
//! A value type: the caller decides where it is stored and how concurrent
//! writers are serialized. Every change is explicit. Assigning an owner is
//! idempotent for the same owner and refused for a different one, so a
//! repeated provisioning request is harmless and a conflicting one is visible.
//! Only [`OwnershipRegistry::reassign`], an operator action, moves a profile.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Capability, PrincipalId};
use crate::profile::ProfileId;

/// One profile's owner and grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileOwnership {
    pub owner: PrincipalId,
    /// Capabilities the owner granted to other principals on this profile.
    /// Operator-only capabilities are never stored here.
    #[serde(default)]
    pub grants: BTreeMap<PrincipalId, BTreeSet<Capability>>,
}

impl ProfileOwnership {
    /// Whether `principal` may take `capability` on this profile: the owner
    /// may take every capability a user may hold; anyone else needs a grant.
    pub fn permits(&self, principal: &PrincipalId, capability: Capability) -> bool {
        if capability.operator_only() {
            return false;
        }
        principal == &self.owner
            || self
                .grants
                .get(principal)
                .is_some_and(|granted| granted.contains(&capability))
    }
}

/// The outcome of an assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assignment {
    /// The profile had no owner and now has this one.
    Created,
    /// The profile already had exactly this owner: a repeated request.
    Unchanged,
    /// An operator moved the profile from another owner.
    Reassigned,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OwnershipError {
    /// The profile has a different owner. Reassignment is an explicit
    /// operator action.
    #[error("the profile already has a different owner")]
    Conflict,
    #[error("the profile has no recorded owner")]
    Unowned,
    #[error("operator-only capabilities cannot be granted on a profile")]
    OperatorOnly,
    #[error("the owner already holds every capability on its profile")]
    GrantToOwner,
}

/// Every profile's ownership record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipRegistry {
    #[serde(default)]
    profiles: BTreeMap<ProfileId, ProfileOwnership>,
}

impl OwnershipRegistry {
    pub fn get(&self, profile: &ProfileId) -> Option<&ProfileOwnership> {
        self.profiles.get(profile)
    }

    /// Profiles `principal` owns.
    pub fn owned_by<'a>(
        &'a self,
        principal: &'a PrincipalId,
    ) -> impl Iterator<Item = &'a ProfileId> {
        self.profiles
            .iter()
            .filter(move |(_, record)| &record.owner == principal)
            .map(|(profile, _)| profile)
    }

    /// Record `owner` for an unowned profile. Idempotent for the same owner;
    /// refused for a different one.
    pub fn assign(
        &mut self,
        profile: ProfileId,
        owner: PrincipalId,
    ) -> Result<Assignment, OwnershipError> {
        match self.profiles.get(&profile) {
            Some(record) if record.owner == owner => Ok(Assignment::Unchanged),
            Some(_) => Err(OwnershipError::Conflict),
            None => {
                self.profiles.insert(
                    profile,
                    ProfileOwnership {
                        owner,
                        grants: BTreeMap::new(),
                    },
                );
                Ok(Assignment::Created)
            }
        }
    }

    /// Move a profile to a new owner, dropping every grant the previous
    /// owner made: they were the previous owner's decisions.
    pub fn reassign(&mut self, profile: ProfileId, owner: PrincipalId) -> Assignment {
        match self.profiles.get_mut(&profile) {
            Some(record) if record.owner == owner => Assignment::Unchanged,
            Some(record) => {
                record.owner = owner;
                record.grants.clear();
                Assignment::Reassigned
            }
            None => {
                self.profiles.insert(
                    profile,
                    ProfileOwnership {
                        owner,
                        grants: BTreeMap::new(),
                    },
                );
                Assignment::Created
            }
        }
    }

    /// Grant `capability` on `profile` to `principal`.
    pub fn grant(
        &mut self,
        profile: &ProfileId,
        principal: PrincipalId,
        capability: Capability,
    ) -> Result<(), OwnershipError> {
        if capability.operator_only() {
            return Err(OwnershipError::OperatorOnly);
        }
        let record = self
            .profiles
            .get_mut(profile)
            .ok_or(OwnershipError::Unowned)?;
        if record.owner == principal {
            return Err(OwnershipError::GrantToOwner);
        }
        record
            .grants
            .entry(principal)
            .or_default()
            .insert(capability);
        Ok(())
    }

    /// Withdraw a grant. Takes effect at the next authorization.
    pub fn revoke(&mut self, profile: &ProfileId, principal: &PrincipalId, capability: Capability) {
        if let Some(record) = self.profiles.get_mut(profile)
            && let Some(granted) = record.grants.get_mut(principal)
        {
            granted.remove(&capability);
            if granted.is_empty() {
                record.grants.remove(principal);
            }
        }
    }

    /// Forget a deleted profile's ownership, so a new profile reusing the id
    /// does not inherit it.
    pub fn remove(&mut self, profile: &ProfileId) -> Option<ProfileOwnership> {
        self.profiles.remove(profile)
    }
}
