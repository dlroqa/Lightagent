//! Provider API keys in Lightagent's owner-only secrets store, the one
//! credential store (DESKTOP_ARCHITECTURE.md §8). `lightagent setup`, the
//! HTTP API and the desktop store, replace and remove a key through these
//! functions, so all of them write the same file and the same reference.
//! Nothing here returns a stored key.

use crate::config::{Config, SecretRef};
use crate::paths::{self, LightagentPaths};

/// The local Lightweight endpoint's provider id.
pub const LOCAL_PROVIDER_ID: &str = "lightweight-local";

const LOCAL_URL: &str = "http://127.0.0.1:11434";

/// A filename-safe provider id from a provider's name: lowercase ASCII
/// letters and digits, with anything else as `-`.
pub fn key_id(name: &str) -> String {
    let id = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let id = id.trim_matches('-');
    if id.is_empty() {
        "provider".to_owned()
    } else {
        id.to_owned()
    }
}

fn same_url(left: &str, right: &str) -> bool {
    left.trim_end_matches('/') == right.trim_end_matches('/')
}

/// The provider id whose key the active endpoint (`inference.base_url`)
/// uses: its saved provider's, the local endpoint's, or `custom`.
pub fn active_key_id(config: &Config) -> String {
    let base_url = &config.inference.base_url;
    if let Some(saved) = config
        .inference
        .saved_providers
        .iter()
        .find(|saved| same_url(&saved.base_url, base_url))
    {
        return key_id(&saved.name);
    }
    if same_url(base_url, LOCAL_URL) {
        LOCAL_PROVIDER_ID.to_owned()
    } else {
        "custom".to_owned()
    }
}

/// A key typed by a person, checked before it is stored: trimmed, not
/// empty, and something an HTTP header can carry. The error never repeats
/// the key.
pub fn validate_key(key: &str) -> Result<&str, &'static str> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the provider API key cannot be empty");
    }
    if key.chars().any(char::is_control) {
        return Err("the provider API key contains a line break or control character");
    }
    Ok(key)
}

/// Store `key` for `provider_id`, owner-only and atomically, replacing any
/// key stored for it before. Returns the reference to put in the config.
pub fn store_key(
    paths: &LightagentPaths,
    provider_id: &str,
    key: &str,
) -> Result<SecretRef, String> {
    let key = validate_key(key).map_err(str::to_owned)?;
    let path = paths.provider_key_file(&key_id(provider_id));
    paths::write_private(&path, key.as_bytes())
        .map_err(|error| format!("the provider API key could not be saved: {}", error.kind()))?;
    Ok(SecretRef::file(path))
}

/// The reference `store_key` gives for `provider_id`.
pub fn stored_reference(paths: &LightagentPaths, provider_id: &str) -> SecretRef {
    SecretRef::file(paths.provider_key_file(&key_id(provider_id)))
}

/// Point the active endpoint, and its saved provider if it has one, at
/// `secret`; `None` sends no key.
pub fn use_key(config: &mut Config, secret: Option<SecretRef>) {
    let base_url = config.inference.base_url.clone();
    for saved in config
        .inference
        .saved_providers
        .iter_mut()
        .filter(|saved| same_url(&saved.base_url, &base_url))
    {
        saved.api_key = secret.clone();
    }
    config.inference.api_key = secret;
}

/// The key reference bound to `base_url`, the only key a request to that
/// endpoint may carry: the active endpoint's (`inference.api_key`) when
/// `base_url` is the active endpoint, a saved provider's when it is that
/// provider's endpoint, and otherwise none. A profile that overrides the
/// endpoint therefore never receives another endpoint's key.
pub fn bound_key<'a>(config: &'a Config, base_url: &str) -> Option<&'a SecretRef> {
    if same_url(&config.inference.base_url, base_url) {
        return config.inference.api_key.as_ref();
    }
    config
        .inference
        .saved_providers
        .iter()
        .find(|saved| same_url(&saved.base_url, base_url))
        .and_then(|saved| saved.api_key.as_ref())
}

/// Make `base_url` the active endpoint, with the key bound to it: the same
/// endpoint keeps its key, a saved provider's endpoint takes that
/// provider's key, and any other endpoint has none. The previous
/// endpoint's key is never carried to a different endpoint.
pub fn select_endpoint(config: &mut Config, base_url: &str) {
    if !same_url(&config.inference.base_url, base_url) {
        config.inference.api_key = config
            .inference
            .saved_providers
            .iter()
            .find(|saved| same_url(&saved.base_url, base_url))
            .and_then(|saved| saved.api_key.clone());
    }
    config.inference.base_url = base_url.to_owned();
}

/// Remove the key stored for `provider_id`. Returns whether a file was
/// removed. The config's references are the caller's to clear first.
pub fn remove_key(paths: &LightagentPaths, provider_id: &str) -> Result<bool, String> {
    match std::fs::remove_file(paths.provider_key_file(&key_id(provider_id))) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "the provider API key could not be removed: {}",
            error.kind()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SavedProvider;

    fn scratch() -> LightagentPaths {
        LightagentPaths::rooted_at(std::env::temp_dir().join(format!(
            "lightagent-provider-keys-{}",
            crate::ids::RunId::new().as_str()
        )))
    }

    #[test]
    fn ids_are_filename_safe() {
        assert_eq!(key_id("My Lab!"), "my-lab");
        assert_eq!(key_id("lightweight-local"), "lightweight-local");
        assert_eq!(key_id("../.."), "provider");
    }

    #[test]
    fn a_typed_key_is_trimmed_and_refused_when_empty_or_unsendable() {
        assert_eq!(validate_key("  sk-1 \n"), Ok("sk-1"));
        assert!(validate_key("   ").is_err());
        assert!(validate_key("sk\n1").is_err());
        assert!(validate_key("sk\u{0}1").is_err());
    }

    #[test]
    fn the_active_endpoint_names_its_key() {
        let mut config = Config::default();
        assert_eq!(active_key_id(&config), LOCAL_PROVIDER_ID);
        config.inference.base_url = "https://models.example/v1".to_owned();
        assert_eq!(active_key_id(&config), "custom");
        config.inference.saved_providers.push(SavedProvider {
            name: "Lab".to_owned(),
            base_url: "https://models.example/v1/".to_owned(),
            api_key: None,
        });
        assert_eq!(active_key_id(&config), "lab");
        use_key(&mut config, Some(SecretRef::env("LAB_KEY")));
        assert_eq!(
            config.inference.saved_providers[0].api_key,
            Some(SecretRef::env("LAB_KEY"))
        );
        assert_eq!(config.inference.api_key, Some(SecretRef::env("LAB_KEY")));
        use_key(&mut config, None);
        assert_eq!(config.inference.saved_providers[0].api_key, None);
        assert_eq!(config.inference.api_key, None);
    }

    #[test]
    fn a_key_is_bound_to_its_endpoint_and_never_carried_to_another() {
        let mut config = Config::default();
        config.inference.base_url = "https://a.example/v1".to_owned();
        config.inference.api_key = Some(SecretRef::env("A_KEY"));
        config.inference.saved_providers.push(SavedProvider {
            name: "Lab".to_owned(),
            base_url: "https://lab.example/v1/".to_owned(),
            api_key: Some(SecretRef::env("LAB_KEY")),
        });
        // A request takes its own endpoint's key, or none.
        assert_eq!(
            bound_key(&config, "https://a.example/v1/"),
            Some(&SecretRef::env("A_KEY"))
        );
        assert_eq!(
            bound_key(&config, "https://lab.example/v1"),
            Some(&SecretRef::env("LAB_KEY"))
        );
        assert_eq!(bound_key(&config, "https://elsewhere.example/v1"), None);
        // The same endpoint keeps its key.
        select_endpoint(&mut config, "https://a.example/v1/");
        assert_eq!(config.inference.api_key, Some(SecretRef::env("A_KEY")));
        // Another endpoint does not take the previous one's key.
        select_endpoint(&mut config, "https://elsewhere.example/v1");
        assert_eq!(config.inference.base_url, "https://elsewhere.example/v1");
        assert_eq!(config.inference.api_key, None);
        // A saved provider's endpoint takes that provider's key.
        select_endpoint(&mut config, "https://lab.example/v1");
        assert_eq!(config.inference.api_key, Some(SecretRef::env("LAB_KEY")));
        // A saved provider without a key leaves the endpoint without one.
        config.inference.saved_providers[0].api_key = None;
        select_endpoint(&mut config, "https://a.example/v1");
        assert_eq!(config.inference.api_key, None);
    }

    #[test]
    fn a_key_is_stored_owner_only_replaced_and_removed() {
        let paths = scratch();
        let first = store_key(&paths, "lab", " first-key ").expect("store");
        assert_eq!(first, stored_reference(&paths, "lab"));
        assert_eq!(first.for_request(), Ok(Some("first-key".to_owned())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(paths.provider_key_file("lab"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let error = store_key(&paths, "lab", "bad\nkey").expect_err("refused");
        assert!(!error.contains("bad"), "{error}");
        assert_eq!(first.for_request(), Ok(Some("first-key".to_owned())));
        store_key(&paths, "lab", "second-key").expect("replace");
        assert_eq!(first.for_request(), Ok(Some("second-key".to_owned())));
        assert_eq!(remove_key(&paths, "lab"), Ok(true));
        assert_eq!(remove_key(&paths, "lab"), Ok(false));
        let _ = std::fs::remove_dir_all(paths.root());
    }
}
