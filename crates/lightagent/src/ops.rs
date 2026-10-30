//! Typed operations shared by the CLI and `lightagent serve`.
//!
//! Each returns data instead of printing it, so `lightagent profile …`, the
//! Web UI and the desktop app perform exactly the same change to the same
//! stores (docs/desktop/DESKTOP_ARCHITECTURE.md §12). Errors carry a stable
//! category from [`OpCategory`] and the message the CLI has always printed.

use lightagent_api::ops::{CreateProfile, OpCategory, OpError, ProfileRequest};
use lightagent_core::{
    AgentProfile, ConfigStore, LightagentPaths, ProfileError, ProfileId, ProfileStore,
};
use serde_json::{Value, json};

fn profile_error(error: ProfileError) -> OpError {
    let category = match &error {
        ProfileError::InvalidId { .. } => OpCategory::Invalid,
        ProfileError::AlreadyExists { .. } | ProfileError::ActiveProfile { .. } => {
            OpCategory::Conflict
        }
        ProfileError::NotFound { .. } => OpCategory::NotFound,
        ProfileError::DefaultProfile => OpCategory::Denied,
        ProfileError::Corrupt { .. } | ProfileError::Io { .. } => OpCategory::Internal,
    };
    OpError::new(category, error.to_string())
}

fn profile_id(id: &str) -> Result<ProfileId, OpError> {
    ProfileId::new(id).map_err(profile_error)
}

/// The profiles and which one is active.
pub(crate) struct ProfileList {
    pub(crate) active: Option<String>,
    pub(crate) profiles: Vec<String>,
}

/// The profile store, with the protected `default` profile ensured.
pub(crate) struct Profiles {
    store: ProfileStore,
    default: ProfileId,
}

impl Profiles {
    pub(crate) fn open(paths: &LightagentPaths) -> Result<Self, OpError> {
        let config = ConfigStore::at(paths)
            .load()
            .map_err(|error| OpError::new(OpCategory::Internal, error.to_string()))?;
        let store = ProfileStore::new(paths.root());
        let default = crate::ensure_default_profile(&store, &config)
            .map_err(|message| OpError::new(OpCategory::Internal, message))?;
        Ok(Self { store, default })
    }

    pub(crate) fn list(&self) -> Result<ProfileList, OpError> {
        let active = self.store.active().map_err(profile_error)?;
        let profiles = self.store.list().map_err(profile_error)?;
        Ok(ProfileList {
            active: active.map(|id| id.as_str().to_owned()),
            profiles: profiles.iter().map(|id| id.as_str().to_owned()).collect(),
        })
    }

    pub(crate) fn show(&self, id: &str) -> Result<AgentProfile, OpError> {
        self.store.load(&profile_id(id)?).map_err(profile_error)
    }

    pub(crate) fn create(&self, request: CreateProfile) -> Result<ProfileId, OpError> {
        let id = profile_id(&request.id)?;
        let profile = AgentProfile::new(
            id.clone(),
            request.name.unwrap_or_else(|| id.as_str().to_string()),
            request
                .persona
                .unwrap_or_else(|| "You are a helpful local agent.".to_string()),
            request.model.unwrap_or_else(|| "default".to_string()),
        );
        self.store.create(&profile).map_err(profile_error)?;
        Ok(id)
    }

    pub(crate) fn preset(&self, id: &str, preset: &str) -> Result<ProfileId, OpError> {
        let id = profile_id(id)?;
        let mut profile = self.store.load(&id).map_err(profile_error)?;
        lightagent_core::provisioning::apply_preset(&mut profile, preset).map_err(|error| {
            OpError::new(
                OpCategory::Invalid,
                format!(
                    "{error}: known presets are {:?}",
                    lightagent_core::provisioning::PRESETS
                ),
            )
        })?;
        self.store.save(&profile).map_err(profile_error)?;
        Ok(id)
    }

    /// Make `id` the active profile. The write is atomic (temporary file and
    /// rename), so a concurrent reader sees the old or the new choice.
    pub(crate) fn use_profile(&self, id: &str) -> Result<ProfileId, OpError> {
        let id = profile_id(id)?;
        self.store.set_active(&id).map_err(profile_error)?;
        Ok(id)
    }

    /// Delete `id` and its data; an active profile hands over to `default`
    /// first, and `default` itself is protected.
    pub(crate) fn delete(&self, id: &str) -> Result<ProfileId, OpError> {
        let id = profile_id(id)?;
        if self.store.active().map_err(profile_error)?.as_ref() == Some(&id) {
            self.store
                .set_active(&self.default)
                .map_err(profile_error)?;
        }
        self.store.delete(&id).map_err(profile_error)?;
        Ok(id)
    }

    /// One API request, answered with the same JSON `--json` prints.
    pub(crate) fn perform(&self, request: ProfileRequest) -> Result<Value, OpError> {
        match request {
            ProfileRequest::Show(id) => serde_json::to_value(self.show(&id)?)
                .map_err(|error| OpError::new(OpCategory::Internal, error.to_string())),
            ProfileRequest::Create(request) => {
                Ok(json!({ "created": self.create(request)?.as_str() }))
            }
            ProfileRequest::Use(id) => Ok(json!({ "active": self.use_profile(&id)?.as_str() })),
            ProfileRequest::Preset { id, preset } => {
                let id = self.preset(&id, &preset)?;
                Ok(json!({ "profile": id.as_str(), "preset": preset }))
            }
            ProfileRequest::Delete(id) => Ok(json!({ "deleted": self.delete(&id)?.as_str() })),
        }
    }
}
