//! Provider operations shared by the terminal CLI and the HTTP API (and so
//! the desktop's Settings): one explicit way to check an endpoint.

use std::time::{Duration, Instant};

use lightagent_core::{Config, SecretRef};
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};
use serde::Serialize;

/// How long an endpoint check waits when the caller names no bound.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// What an explicit endpoint check found. It never holds a key.
#[derive(Debug, Serialize)]
pub(crate) struct EndpointCheck {
    pub base_url: String,
    pub ok: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
    /// Whether the configured key was sent (to this endpoint only).
    pub key_sent: bool,
    pub elapsed_ms: u64,
}

/// Check the active endpoint (`inference.base_url`) by listing its models,
/// with its own configured key. A configured key that cannot be used fails
/// closed: nothing is sent. A redirect is not followed, so the key reaches
/// this endpoint only, and the whole check ends within `timeout`.
pub(crate) async fn check_active(config: &Config, timeout: Duration) -> EndpointCheck {
    let base_url = config.inference.base_url.clone();
    let started = Instant::now();
    let finish = |models: Vec<String>, error: Option<String>, key_sent: bool| EndpointCheck {
        base_url: base_url.clone(),
        ok: error.is_none(),
        models,
        error,
        key_sent,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let key = match lightagent_core::provider_keys::bound_key(config, &base_url)
        .map_or(Ok(None), SecretRef::for_request)
    {
        Ok(key) => key,
        Err(problem) => {
            return finish(
                Vec::new(),
                Some(format!("inference: {problem}; nothing was sent")),
                false,
            );
        }
    };
    let key_sent = key.is_some();
    let mut provider_config = ProviderConfig::new(base_url.clone(), "default");
    if let Some(key) = key {
        provider_config = provider_config.with_api_key(key);
    }
    let provider = match LightweightProvider::for_check(provider_config, timeout) {
        Ok(provider) => provider,
        Err(error) => return finish(Vec::new(), Some(error.to_string()), false),
    };
    match tokio::time::timeout(timeout, provider.models()).await {
        Ok(Ok(models)) => finish(models, None, key_sent),
        Ok(Err(error)) => finish(Vec::new(), Some(error.to_string()), key_sent),
        Err(_) => finish(
            Vec::new(),
            Some(format!(
                "the provider did not answer within {} s",
                timeout.as_secs()
            )),
            key_sent,
        ),
    }
}
