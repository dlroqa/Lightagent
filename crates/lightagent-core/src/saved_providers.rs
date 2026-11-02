//! Saved providers: named OpenAI-compatible endpoints the provider picker
//! offers, each with its own key. `lightagent setup provider`, the HTTP API
//! and the desktop's Settings list, create, edit, select and remove them
//! through these functions, inside one config update, so every surface
//! writes the same config and the same key files. A key stays bound to its
//! endpoint (`provider_keys::bound_key`); nothing here returns one.

use serde::Serialize;

use crate::ProfileStore;
use crate::config::{Config, SavedProvider, SecretRef, secret_state};
use crate::paths::{self, LightagentPaths};
use crate::provider_keys::{self, LOCAL_PROVIDER_ID, LOCAL_URL, same_url};

/// The active endpoint's id when it is neither the local endpoint nor a
/// saved provider's.
pub const CUSTOM_PROVIDER_ID: &str = "custom";

/// When a provider or model change takes effect.
pub const EFFECT: &str = "Applies to the next run. A run in progress keeps its provider and model; an open terminal chat keeps its endpoint until it is restarted.";

/// One provider as the picker shows it. Never a key, a key file's path or
/// an environment variable's name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderView {
    /// `lightweight-local`, a saved provider's id, or `custom`.
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// `local`, `saved`, or `custom` (the active endpoint, not saved).
    pub kind: &'static str,
    pub active: bool,
    /// `absent`, `ready`, or why the configured key cannot be used.
    pub key_state: &'static str,
    /// `file` or `env`, when a key is configured.
    pub key_source: Option<&'static str>,
}

/// Why a provider change wrote nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// The request is not acceptable.
    Invalid(String),
    /// No provider has this id.
    NotFound(String),
    /// Another saved provider already has this name or endpoint.
    Duplicate(String),
    /// A key file could not be read or written.
    Store(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Duplicate(message) | Self::Store(message) => {
                f.write_str(message)
            }
            Self::NotFound(id) => write!(f, "no provider '{id}'"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// What an edit changes. `None` keeps a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderEdit {
    pub name: Option<String>,
    pub base_url: Option<String>,
    /// Keep the key when the endpoint changes. Without it, a key saved for
    /// one endpoint is never sent to another: it is released.
    pub keep_key: bool,
}

/// What a change leaves for after the config is saved ([`finish`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The provider whose stored key file the change released. It is
    /// removed only after the save, so a refused save keeps every key it
    /// names.
    pub release_key: Option<String>,
    /// The model the active profile uses from now (`None`: automatic).
    pub profile_model: Option<Option<String>>,
    /// Clear the active profile's own endpoint, so it follows the active
    /// provider.
    pub clear_profile_endpoint: bool,
}

fn view(
    config: &Config,
    id: String,
    name: String,
    base_url: String,
    kind: &'static str,
) -> ProviderView {
    let key = provider_keys::bound_key(config, &base_url);
    ProviderView {
        active: same_url(&config.inference.base_url, &base_url),
        key_state: secret_state(key),
        key_source: key.map(|secret| match secret {
            SecretRef::Env { .. } => "env",
            SecretRef::File { .. } => "file",
        }),
        id,
        name,
        base_url,
        kind,
    }
}

/// Every provider the picker offers: the local endpoint, each saved
/// provider, and the active endpoint when it is neither.
pub fn list(config: &Config) -> Vec<ProviderView> {
    let mut providers = vec![view(
        config,
        LOCAL_PROVIDER_ID.to_owned(),
        "Lightweight local".to_owned(),
        LOCAL_URL.to_owned(),
        "local",
    )];
    providers.extend(config.inference.saved_providers.iter().map(|saved| {
        view(
            config,
            provider_keys::key_id(&saved.name),
            saved.name.clone(),
            saved.base_url.clone(),
            "saved",
        )
    }));
    if !providers.iter().any(|provider| provider.active) {
        providers.push(view(
            config,
            CUSTOM_PROVIDER_ID.to_owned(),
            "Custom endpoint".to_owned(),
            config.inference.base_url.clone(),
            "custom",
        ));
    }
    providers
}

/// One provider by id, as [`list`] shows it.
pub fn get(config: &Config, id: &str) -> Result<ProviderView, ProviderError> {
    list(config)
        .into_iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| ProviderError::NotFound(id.to_owned()))
}

fn valid_name(name: &str) -> Result<String, ProviderError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProviderError::Invalid(
            "enter a name for this provider".to_owned(),
        ));
    }
    if name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ProviderError::Invalid(
            "a provider name has at most 64 characters and no control characters".to_owned(),
        ));
    }
    let id = provider_keys::key_id(name);
    if id == LOCAL_PROVIDER_ID || id == CUSTOM_PROVIDER_ID {
        return Err(ProviderError::Invalid(format!(
            "the provider name {name:?} is reserved"
        )));
    }
    Ok(name.to_owned())
}

fn valid_url(base_url: &str) -> Result<String, ProviderError> {
    let url = base_url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    match rest {
        Some(rest)
            if !rest.is_empty()
                && !url
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()) =>
        {
            Ok(url.to_owned())
        }
        _ => Err(ProviderError::Invalid(
            "enter an http:// or https:// URL".to_owned(),
        )),
    }
}

fn valid_model(model: Option<&str>) -> Result<Option<String>, ProviderError> {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return Ok(None);
    };
    if model.chars().count() > 256 || model.chars().any(char::is_control) {
        return Err(ProviderError::Invalid(
            "a model id has at most 256 characters and no control characters".to_owned(),
        ));
    }
    Ok(Some(model.to_owned()))
}

/// Refuse a name or endpoint another saved provider (not `except`) has: two
/// providers sharing a key id would share one key file, and two sharing an
/// endpoint would make its key ambiguous.
fn refuse_duplicate(
    config: &Config,
    name: &str,
    base_url: &str,
    except: Option<usize>,
) -> Result<(), ProviderError> {
    if same_url(base_url, LOCAL_URL) {
        return Err(ProviderError::Duplicate(
            "that is the local Lightweight endpoint, which is always offered".to_owned(),
        ));
    }
    let id = provider_keys::key_id(name);
    for (index, saved) in config.inference.saved_providers.iter().enumerate() {
        if Some(index) == except {
            continue;
        }
        if provider_keys::key_id(&saved.name) == id {
            return Err(ProviderError::Duplicate(format!(
                "the saved provider {:?} already has that name",
                saved.name
            )));
        }
        if same_url(&saved.base_url, base_url) {
            return Err(ProviderError::Duplicate(format!(
                "the saved provider {:?} already uses that endpoint",
                saved.name
            )));
        }
    }
    Ok(())
}

fn position(config: &Config, id: &str) -> Result<usize, ProviderError> {
    if id == LOCAL_PROVIDER_ID {
        return Err(ProviderError::Invalid(
            "the local Lightweight endpoint cannot be edited or removed".to_owned(),
        ));
    }
    config
        .inference
        .saved_providers
        .iter()
        .position(|saved| provider_keys::key_id(&saved.name) == id)
        .ok_or_else(|| ProviderError::NotFound(id.to_owned()))
}

/// Copy the stored key of `from` to `to`'s file, owner-only. The old file
/// is released after the save.
fn move_key(paths: &LightagentPaths, from: &str, to: &str) -> Result<SecretRef, ProviderError> {
    let failed = |error: std::io::Error| {
        ProviderError::Store(format!(
            "the provider API key could not be moved: {}",
            error.kind()
        ))
    };
    let key = std::fs::read(paths.provider_key_file(from)).map_err(failed)?;
    paths::write_private(&paths.provider_key_file(to), &key).map_err(failed)?;
    Ok(provider_keys::stored_reference(paths, to))
}

/// Save a new provider at `base_url`. It has no key until one is saved for
/// it, unless it is the active endpoint, whose configured key was saved for
/// this very endpoint. The active endpoint does not change.
pub fn create(
    config: &mut Config,
    name: &str,
    base_url: &str,
) -> Result<ProviderView, ProviderError> {
    let name = valid_name(name)?;
    let base_url = valid_url(base_url)?;
    refuse_duplicate(config, &name, &base_url, None)?;
    let api_key = provider_keys::bound_key(config, &base_url).cloned();
    let id = provider_keys::key_id(&name);
    config.inference.saved_providers.push(SavedProvider {
        name,
        base_url,
        api_key,
    });
    get(config, &id)
}

/// Rename provider `id` or move it to another endpoint.
///
/// - A rename keeps its key: a key in the shared store moves to the new
///   name's file.
/// - A new endpoint releases the key, unless `keep_key` says to keep it.
/// - When it is the active endpoint, the active endpoint follows it.
///
/// Profiles whose own `routing.base_url` names the old endpoint keep it and
/// send it no key from then on, unless another saved provider is there.
pub fn update(
    config: &mut Config,
    paths: &LightagentPaths,
    id: &str,
    edit: &ProviderEdit,
) -> Result<(ProviderView, Outcome), ProviderError> {
    let index = position(config, id)?;
    let current = config.inference.saved_providers[index].clone();
    let name = edit
        .name
        .as_deref()
        .map(valid_name)
        .transpose()?
        .unwrap_or_else(|| current.name.clone());
    let base_url = edit
        .base_url
        .as_deref()
        .map(valid_url)
        .transpose()?
        .unwrap_or_else(|| current.base_url.clone());
    refuse_duplicate(config, &name, &base_url, Some(index))?;
    let was_active = same_url(&config.inference.base_url, &current.base_url);
    let (old_id, new_id) = (
        provider_keys::key_id(&current.name),
        provider_keys::key_id(&name),
    );
    let mut api_key = provider_keys::bound_key(config, &current.base_url).cloned();
    let stored = api_key.as_ref() == Some(&provider_keys::stored_reference(paths, &old_id));
    let mut outcome = Outcome::default();
    if !same_url(&base_url, &current.base_url) && !edit.keep_key {
        api_key = None;
        if stored {
            outcome.release_key = Some(old_id.clone());
        }
    } else if stored && new_id != old_id {
        api_key = Some(move_key(paths, &old_id, &new_id)?);
        outcome.release_key = Some(old_id.clone());
    }
    let saved = &mut config.inference.saved_providers[index];
    saved.name = name;
    saved.base_url = base_url.clone();
    saved.api_key = api_key.clone();
    if was_active {
        config.inference.base_url = base_url;
        config.inference.api_key = api_key;
    }
    Ok((get(config, &new_id)?, outcome))
}

/// Remove provider `id`. Its stored key is released after the save. When
/// it was the active endpoint, the local endpoint becomes active, with no
/// key and automatic model selection, and the active profile follows.
pub fn remove(
    config: &mut Config,
    paths: &LightagentPaths,
    id: &str,
) -> Result<Outcome, ProviderError> {
    let index = position(config, id)?;
    let removed = config.inference.saved_providers.remove(index);
    let was_active = same_url(&config.inference.base_url, &removed.base_url);
    let key = if was_active {
        config.inference.api_key.clone()
    } else {
        removed.api_key.clone()
    };
    let mut outcome = Outcome::default();
    if key.as_ref() == Some(&provider_keys::stored_reference(paths, id)) {
        outcome.release_key = Some(id.to_owned());
    }
    if was_active {
        provider_keys::select_endpoint(config, LOCAL_URL);
        config.inference.model = None;
        outcome.profile_model = Some(None);
        outcome.clear_profile_endpoint = true;
    }
    Ok(outcome)
}

/// Make provider `id` the active endpoint with `model` (`None`: automatic,
/// following the model the endpoint has loaded). The endpoint takes only
/// the key bound to it (`provider_keys::select_endpoint`). The active
/// profile follows: its own endpoint is cleared and it uses `model`.
/// Applies to the next run ([`EFFECT`]).
pub fn select(
    config: &mut Config,
    id: &str,
    model: Option<&str>,
) -> Result<(ProviderView, Outcome), ProviderError> {
    let base_url = get(config, id)?.base_url;
    let model = valid_model(model)?;
    provider_keys::select_endpoint(config, &base_url);
    config.inference.model = model.clone();
    let outcome = Outcome {
        release_key: None,
        profile_model: Some(model),
        clear_profile_endpoint: true,
    };
    Ok((get(config, id)?, outcome))
}

/// Use `model` on the active endpoint (`None`: automatic). The active
/// profile uses it too; its own endpoint is left as it is. Applies to the
/// next run ([`EFFECT`]).
pub fn select_model(config: &mut Config, model: Option<&str>) -> Result<Outcome, ProviderError> {
    let model = valid_model(model)?;
    config.inference.model = model.clone();
    Ok(Outcome {
        release_key: None,
        profile_model: Some(model),
        clear_profile_endpoint: false,
    })
}

/// Finish `outcome` once the config is saved: release a key file, then
/// point the active profile at the new endpoint and model.
pub fn finish(paths: &LightagentPaths, outcome: &Outcome) -> Result<(), String> {
    if let Some(id) = &outcome.release_key {
        provider_keys::remove_key(paths, id)?;
    }
    if outcome.profile_model.is_none() && !outcome.clear_profile_endpoint {
        return Ok(());
    }
    let profiles = ProfileStore::new(paths.root());
    let Some(active) = profiles.active().map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let mut profile = profiles.load(&active).map_err(|error| error.to_string())?;
    if outcome.clear_profile_endpoint {
        profile.routing.base_url = None;
    }
    if let Some(model) = &outcome.profile_model {
        profile.routing.model = model.clone().unwrap_or_else(|| "default".to_owned());
    }
    profiles.save(&profile).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> LightagentPaths {
        LightagentPaths::rooted_at(std::env::temp_dir().join(format!(
            "lightagent-saved-providers-{}",
            crate::ids::RunId::new().as_str()
        )))
    }

    #[test]
    fn providers_are_listed_without_keys_and_saved_once() {
        let mut config = Config::default();
        let local = list(&config);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].id, LOCAL_PROVIDER_ID);
        assert!(local[0].active);
        let lab = create(&mut config, " Lab ", "https://lab.example/v1/").expect("create");
        assert_eq!(
            (lab.id.as_str(), lab.name.as_str(), lab.base_url.as_str()),
            ("lab", "Lab", "https://lab.example/v1")
        );
        assert!(!lab.active);
        assert_eq!(lab.key_state, "absent");
        for (name, url) in [
            ("lab", "https://other.example"),
            ("LAB!", "https://other.example"),
            ("Other", "https://lab.example/v1/"),
            ("Local", LOCAL_URL),
        ] {
            assert!(
                matches!(
                    create(&mut config, name, url),
                    Err(ProviderError::Duplicate(_))
                ),
                "{name} {url}"
            );
        }
        for (name, url) in [
            ("", "https://x.example"),
            ("Custom", "https://x.example"),
            ("lightweight local", "https://x.example"),
            ("X", "ftp://x.example"),
            ("X", "https://"),
            ("X", "https://a b.example"),
        ] {
            assert!(
                matches!(
                    create(&mut config, name, url),
                    Err(ProviderError::Invalid(_))
                ),
                "{name} {url}"
            );
        }
        assert_eq!(config.inference.saved_providers.len(), 1);
        config.inference.saved_providers[0].api_key = Some(SecretRef::env("LAB_KEY"));
        let text = serde_json::to_string(&list(&config)).expect("JSON");
        assert!(!text.contains("LAB_KEY"), "{text}");
        // An unsaved active endpoint is listed as custom.
        config.inference.base_url = "https://elsewhere.example".to_owned();
        let custom = get(&config, CUSTOM_PROVIDER_ID).expect("custom");
        assert!(custom.active);
        assert_eq!(custom.kind, "custom");
    }

    #[test]
    fn selecting_binds_the_endpoint_key_and_the_profile_follows() {
        let mut config = Config::default();
        config.inference.api_key = Some(SecretRef::env("LOCAL_KEY"));
        create(&mut config, "Lab", "https://lab.example/v1").expect("create");
        config.inference.saved_providers[0].api_key = Some(SecretRef::env("LAB_KEY"));
        let (lab, outcome) = select(&mut config, "lab", Some(" m2 ")).expect("select");
        assert!(lab.active);
        assert_eq!(config.inference.base_url, "https://lab.example/v1");
        assert_eq!(config.inference.api_key, Some(SecretRef::env("LAB_KEY")));
        assert_eq!(config.inference.model.as_deref(), Some("m2"));
        assert_eq!(outcome.profile_model, Some(Some("m2".to_owned())));
        assert!(outcome.clear_profile_endpoint);
        // Back to the local endpoint: the lab key does not follow.
        let (_, outcome) = select(&mut config, LOCAL_PROVIDER_ID, None).expect("local");
        assert_eq!(config.inference.api_key, None);
        assert_eq!(config.inference.model, None);
        assert_eq!(outcome.profile_model, Some(None));
        assert!(matches!(
            select(&mut config, "nope", None),
            Err(ProviderError::NotFound(_))
        ));
        let outcome = select_model(&mut config, Some("m1")).expect("model");
        assert_eq!(config.inference.model.as_deref(), Some("m1"));
        assert!(!outcome.clear_profile_endpoint);
        assert!(select_model(&mut config, Some("bad\nmodel")).is_err());
        assert_eq!(config.inference.model.as_deref(), Some("m1"));
    }

    #[test]
    fn a_rename_moves_the_stored_key_and_a_new_endpoint_releases_it() {
        let paths = scratch();
        let mut config = Config::default();
        create(&mut config, "Lab", "https://lab.example/v1").expect("create");
        select(&mut config, "lab", None).expect("select");
        let secret = provider_keys::store_key(&paths, "lab", "lab-key-value").expect("store");
        provider_keys::use_key(&mut config, Some(secret));

        // A rename: the key moves to the new name's file.
        let edit = ProviderEdit {
            name: Some("Lab Two".to_owned()),
            ..ProviderEdit::default()
        };
        let (view, outcome) = update(&mut config, &paths, "lab", &edit).expect("rename");
        assert_eq!((view.id.as_str(), view.key_state), ("lab-two", "ready"));
        assert_eq!(outcome.release_key.as_deref(), Some("lab"));
        assert_eq!(
            config.inference.api_key,
            Some(provider_keys::stored_reference(&paths, "lab-two"))
        );
        finish(&paths, &outcome).expect("finish");
        assert!(!paths.provider_key_file("lab").exists());

        // A new endpoint without keep_key: the key is released.
        let edit = ProviderEdit {
            base_url: Some("https://new.example/v1".to_owned()),
            ..ProviderEdit::default()
        };
        let (view, outcome) = update(&mut config, &paths, "lab-two", &edit).expect("move");
        assert_eq!(view.key_state, "absent");
        assert!(view.active);
        assert_eq!(config.inference.base_url, "https://new.example/v1");
        assert_eq!(config.inference.api_key, None);
        assert_eq!(outcome.release_key.as_deref(), Some("lab-two"));
        finish(&paths, &outcome).expect("finish");
        assert!(!paths.provider_key_file("lab-two").exists());

        // keep_key carries the key to a new endpoint, only when asked.
        let secret = provider_keys::store_key(&paths, "lab-two", "lab-key-value").expect("store");
        provider_keys::use_key(&mut config, Some(secret.clone()));
        let edit = ProviderEdit {
            base_url: Some("https://newer.example/v1".to_owned()),
            keep_key: true,
            ..ProviderEdit::default()
        };
        let (view, outcome) = update(&mut config, &paths, "lab-two", &edit).expect("keep");
        assert_eq!(view.key_state, "ready");
        assert_eq!(config.inference.api_key, Some(secret));
        assert_eq!(outcome.release_key, None);

        // Removing the active provider releases its key and leaves the local
        // endpoint active, keyless.
        let outcome = remove(&mut config, &paths, "lab-two").expect("remove");
        assert!(config.inference.saved_providers.is_empty());
        assert_eq!(config.inference.base_url, LOCAL_URL);
        assert_eq!(config.inference.api_key, None);
        assert_eq!(outcome.release_key.as_deref(), Some("lab-two"));
        assert!(outcome.clear_profile_endpoint);
        finish(&paths, &outcome).expect("finish");
        assert!(!paths.provider_key_file("lab-two").exists());
        assert!(matches!(
            remove(&mut config, &paths, LOCAL_PROVIDER_ID),
            Err(ProviderError::Invalid(_))
        ));
        assert!(matches!(
            remove(&mut config, &paths, "lab-two"),
            Err(ProviderError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(paths.root());
    }
}
