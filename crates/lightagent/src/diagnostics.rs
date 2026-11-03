//! The diagnostics bundle (D7): what a person can save and hand to someone
//! helping them. It is built field by field from an explicit allowlist;
//! nothing is copied wholesale. It never holds a credential value, a
//! conversation or its title, memory or knowledge contents, identity
//! records, the desktop launch key, an environment variable, a profile id
//! or a path: paths are shown as `<home>`, and URLs as their origin only.
//! Nothing is uploaded: the bundle is returned to the caller, who decides
//! where to save it.

use std::path::Path;

use lightagent_api::ops::{
    IdentityRequest, MemoryRequest, OpCategory, OpError, RagRequest, RemoteCredentialRequest,
};
use lightagent_core::{ConfigStore, LightagentPaths, PlatformEndpointConfig, ProfileStore};
use lightagent_store::SessionStore;
use serde_json::{Value, json};

/// The bundle's top-level fields, in order: the only ones it has.
pub(crate) const SECTIONS: [&str; 10] = [
    "schema",
    "about",
    "included",
    "excluded",
    "lightagent",
    "doctor",
    "architecture",
    "settings",
    "counts",
    "stores",
];

const INCLUDED: [&str; 6] = [
    "the Lightagent version, operating system and architecture",
    "doctor: whether the home exists, the provider's kind and origin, whether a profile is active, and how many profiles and built-in tools there are",
    "architecture: which platform components are configured, by origin (scheme, host and port) only",
    "settings: on/off switches and limits from the configuration, never a value that can hold a secret",
    "counts: profiles, saved providers, MCP servers, and the active profile's sessions, memories and indexed documents",
    "stores: whether the identity store and the credential vault exist, whether a vault key is configured and where it comes from, and how many records they hold",
];

const EXCLUDED: [&str; 7] = [
    "credential values: provider keys, MCP headers, remote credentials, identity credentials and the vault key",
    "conversations, their titles and transcripts",
    "memory and knowledge contents",
    "identity records: users, credentials, ownership and grants",
    "the desktop app's launch key and environment variables",
    "profile names and ids",
    "file paths, shown as <home>; URLs are reduced to scheme, host and port",
];

/// A URL as its origin (scheme, host and port): never a user, password,
/// path, query or fragment. Anything that is not a URL is left out.
fn origin(url: &str) -> Value {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Value::Null;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if scheme.is_empty() || host.is_empty() {
        return Value::Null;
    }
    Value::String(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

fn endpoint(endpoint: &PlatformEndpointConfig) -> Value {
    if endpoint.enabled {
        json!({
            "configured": true,
            "origin": endpoint.base_url.as_deref().map_or(Value::Null, origin),
        })
    } else {
        json!({ "configured": false })
    }
}

/// How many entries `field` of an operation's answer holds, when it worked.
fn count(answer: Result<Value, OpError>, field: &str) -> Option<usize> {
    answer
        .ok()
        .and_then(|value| value[field].as_array().map(Vec::len))
}

/// The bundle for the home at `root`.
pub(crate) async fn bundle(root: &Path) -> Result<Value, OpError> {
    let internal = |error: String| OpError::new(OpCategory::Internal, error);
    let paths = LightagentPaths::rooted_at(root);
    let config = ConfigStore::at(&paths)
        .load()
        .map_err(|error| internal(error.to_string()))?;
    let profiles = ProfileStore::new(root);
    let active = profiles
        .active()
        .map_err(|error| internal(error.to_string()))?;
    let listed = profiles
        .list()
        .map_err(|error| internal(error.to_string()))?;
    let sessions = active.as_ref().and_then(|id| {
        SessionStore::at_profile(&profiles.handle(id))
            .list()
            .ok()
            .map(|rows| rows.len())
    });
    let memories = count(
        crate::memory::perform(&paths, MemoryRequest::List).await,
        "memories",
    );
    let documents = count(
        crate::rag::perform(&paths, RagRequest::List).await,
        "sources",
    );
    let identity = crate::identity_admin::perform(root, IdentityRequest::Status)
        .map(|status| {
            json!({
                "store": status["store"],
                "multi_user": status["multi_user"],
                "principals": status["principals"],
                "credentials": status["credentials"],
            })
        })
        .unwrap_or_else(|_| json!({ "error": "the identity store could not be read" }));
    let vault = crate::remote_credentials::perform(root, RemoteCredentialRequest::Status)
        .map(|status| {
            json!({
                "vault": status["vault"],
                "key_configured": status["key_configured"],
                "key_source": status["key_source"],
                "grants": status["grants"],
            })
        })
        .unwrap_or_else(|_| json!({ "error": "the configured credential key is unusable" }));
    let platform = &config.platform;
    let searxng = config
        .web
        .search
        .endpoint
        .as_deref()
        .filter(|_| config.web.enabled)
        .map_or(Value::Null, origin);
    let bundle = json!({
        "schema": "lightagent-diagnostics/1",
        "about": "A diagnostics bundle to share with someone helping you with Lightagent. It holds only what `included` lists; everything `excluded` lists is left out. It is not sent anywhere: you choose where to save it.",
        "included": INCLUDED,
        "excluded": EXCLUDED,
        "lightagent": {
            "version": crate::VERSION,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "doctor": {
            "home": "<home>",
            "home_exists": root.exists(),
            "config_file": "<home>/config.json",
            "provider": config.inference.provider,
            "provider_origin": origin(&config.inference.base_url),
            "active_profile_set": active.is_some(),
            "profiles": listed.len(),
            "tools": lightagent_tools::ToolRegistry::builtin().names().len(),
        },
        "architecture": {
            "jev": endpoint(&platform.jev.endpoint),
            "qdrant": endpoint(&platform.qdrant.endpoint),
            "infinity": endpoint(&platform.infinity.endpoint),
            "open_terminal": endpoint(&platform.open_terminal.endpoint),
            "searxng": searxng,
        },
        "settings": {
            "tools_enabled": config.tools.enabled,
            "terminal_allowed": config.tools.allow_terminal,
            "web_enabled": config.web.enabled,
            "mcp_enabled": config.mcp.enabled,
            "extensions_enabled": config.extensions.enabled,
            "agent_max_turns": config.agent.max_turns,
            "agent_max_tool_calls": config.agent.max_tool_calls,
            "context_tokens": config.runtime.n_ctx,
        },
        "counts": {
            "profiles": listed.len(),
            "saved_providers": config.inference.saved_providers.len(),
            "mcp_servers": config.mcp.servers.len(),
            "active_profile_sessions": sessions,
            "active_profile_memories": memories,
            "active_profile_documents": documents,
        },
        "stores": {
            "identity": identity,
            "remote_credentials": vault,
        },
    });
    debug_assert!(
        bundle
            .as_object()
            .is_some_and(|fields| fields.len() == SECTIONS.len()
                && SECTIONS.iter().all(|section| fields.contains_key(*section))),
        "the bundle's fields are exactly SECTIONS"
    );
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_reduced_to_its_origin() {
        assert_eq!(
            origin("https://user:pass@Example.COM:8443/v1/chat?token=abc#x"),
            json!("https://example.com:8443")
        );
        assert_eq!(origin("http://127.0.0.1:0"), json!("http://127.0.0.1:0"));
        assert_eq!(origin("not a url"), Value::Null);
        assert_eq!(origin("https://user@/path"), Value::Null);
    }

    #[test]
    fn the_sections_are_the_allowlist() {
        let names: Vec<&str> = SECTIONS.to_vec();
        assert_eq!(names.len(), 10);
        assert!(
            EXCLUDED
                .iter()
                .any(|item| item.contains("credential values"))
        );
        assert!(EXCLUDED.iter().any(|item| item.contains("conversations")));
        assert!(
            EXCLUDED
                .iter()
                .any(|item| item.contains("memory and knowledge"))
        );
    }
}
