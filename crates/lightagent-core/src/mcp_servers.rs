//! MCP server configuration through one shared operation set (D6-3): the
//! servers in `mcp.servers`, the `mcp.enabled` switch and
//! `mcp.timeout_secs`, for the HTTP API and the desktop app, over the same
//! configuration file the terminal reads.
//!
//! A view never carries an environment or header value or a secret: only
//! their names, and the kind of an auth reference. An edit may leave a value
//! out (`null`, or auth `keep`) to keep the one already saved, so a client
//! that never saw a value can still edit the server.
//!
//! Every write is checked here, whether MCP is on or off, by the rules
//! [`Config::validate`] applies once it is on, plus a server name usable as
//! the tool namespace (`mcp.<server>.<tool>`). The operations change the
//! configuration they are given; [`crate::ConfigStore::update`] saves it only
//! when the operation succeeds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Config, McpServerEntry, ProfileId, SecretRef};

/// When a change applies.
pub const EFFECT: &str =
    "Runs started after this change use it; a running turn keeps the servers it connected.";

/// The longest per-request timeout a write may set, in seconds.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// Why an MCP change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpConfigError {
    /// No configured server has the name.
    NotFound(String),
    /// The entry cannot be used as given.
    Invalid(String),
    /// Another configured server already has the name.
    Conflict(String),
}

impl std::fmt::Display for McpConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Invalid(message) | Self::Conflict(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for McpConfigError {}

/// The `mcp` section as a client may see it.
#[derive(Clone, Debug, Serialize)]
pub struct McpView {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub servers: Vec<ServerView>,
}

/// An auth reference's kind; an environment variable's name is shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthView {
    Env { var: String },
    File,
}

/// One configured server, its environment and header values withheld.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ServerView {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: Vec<String>,
    },
    Http {
        name: String,
        url: String,
        headers: Vec<String>,
        auth: Option<AuthView>,
        untrusted_profiles: Vec<String>,
        credential_profiles: Vec<String>,
    },
}

/// How an HTTP server authenticates, as a write states it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthInput {
    /// Connect without a bearer key.
    #[default]
    None,
    /// Keep the auth reference already saved.
    Keep,
    /// Read the bearer key from this environment variable when connecting.
    Env { var: String },
}

/// One server as a write states it. An environment or header value of
/// `null` keeps the value saved under that name.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ServerInput {
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, Option<String>>,
    },
    Http {
        name: String,
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, Option<String>>,
        #[serde(default)]
        auth: AuthInput,
        #[serde(default)]
        untrusted_profiles: Vec<String>,
        #[serde(default)]
        credential_profiles: Vec<String>,
    },
}

/// The `mcp` section with every value withheld.
pub fn view(config: &Config) -> McpView {
    McpView {
        enabled: config.mcp.enabled,
        timeout_secs: config.mcp.timeout_secs,
        servers: config.mcp.servers.iter().map(server_view).collect(),
    }
}

fn server_view(entry: &McpServerEntry) -> ServerView {
    match entry {
        McpServerEntry::Stdio {
            name,
            command,
            args,
            env,
        } => ServerView::Stdio {
            name: name.clone(),
            command: command.clone(),
            args: args.clone(),
            env: env.keys().cloned().collect(),
        },
        McpServerEntry::Http {
            name,
            url,
            headers,
            auth,
            untrusted_profiles,
            credential_profiles,
        } => ServerView::Http {
            name: name.clone(),
            url: url.clone(),
            headers: headers.keys().cloned().collect(),
            auth: auth.as_ref().map(|auth| match auth {
                SecretRef::Env { var } => AuthView::Env { var: var.clone() },
                SecretRef::File { .. } => AuthView::File,
            }),
            untrusted_profiles: untrusted_profiles.clone(),
            credential_profiles: credential_profiles.clone(),
        },
    }
}

/// Add a server. Its name must be new; nothing can be kept, so every value
/// is given.
pub fn add(config: &mut Config, input: ServerInput) -> Result<(), McpConfigError> {
    let entry = build(input, None)?;
    if config.mcp.servers.iter().any(|s| s.name() == entry.name()) {
        return Err(McpConfigError::Conflict(format!(
            "an MCP server named {:?} already exists",
            entry.name()
        )));
    }
    config.mcp.servers.push(entry);
    Ok(())
}

/// Replace server `name` (it may be renamed to a name no other server
/// has), keeping a withheld value wherever the input leaves it `null`.
pub fn update(config: &mut Config, name: &str, input: ServerInput) -> Result<(), McpConfigError> {
    let position = position(config, name)?;
    let saved = config.mcp.servers.remove(position);
    let entry = build(input, Some(&saved))?;
    if config.mcp.servers.iter().any(|s| s.name() == entry.name()) {
        return Err(McpConfigError::Conflict(format!(
            "an MCP server named {:?} already exists",
            entry.name()
        )));
    }
    config.mcp.servers.insert(position, entry);
    Ok(())
}

/// Remove server `name`.
pub fn remove(config: &mut Config, name: &str) -> Result<(), McpConfigError> {
    let position = position(config, name)?;
    config.mcp.servers.remove(position);
    Ok(())
}

/// Switch MCP on or off, and set its per-request timeout.
pub fn set(
    config: &mut Config,
    enabled: Option<bool>,
    timeout_secs: Option<u64>,
) -> Result<(), McpConfigError> {
    if let Some(timeout) = timeout_secs {
        if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
            return Err(McpConfigError::Invalid(format!(
                "the MCP timeout must be 1 to {MAX_TIMEOUT_SECS} seconds"
            )));
        }
        config.mcp.timeout_secs = timeout;
    }
    if let Some(enabled) = enabled {
        config.mcp.enabled = enabled;
    }
    Ok(())
}

fn position(config: &Config, name: &str) -> Result<usize, McpConfigError> {
    config
        .mcp
        .servers
        .iter()
        .position(|s| s.name() == name)
        .ok_or_else(|| McpConfigError::NotFound(format!("no configured MCP server named {name:?}")))
}

fn invalid(message: String) -> McpConfigError {
    McpConfigError::Invalid(message)
}

/// The values a write names: each given value, or for `null` the one saved
/// under that name.
fn resolve(
    kind: &str,
    server: &str,
    given: BTreeMap<String, Option<String>>,
    saved: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, McpConfigError> {
    let mut values = BTreeMap::new();
    for (key, value) in given {
        if key.is_empty() || key.contains(['=', ':', '\0']) || key.chars().any(char::is_whitespace)
        {
            return Err(invalid(format!(
                "MCP server {server:?} has an invalid {kind} name {key:?}"
            )));
        }
        let value = match value {
            Some(value) => value,
            None => saved
                .and_then(|saved| saved.get(&key))
                .cloned()
                .ok_or_else(|| {
                    invalid(format!(
                        "MCP server {server:?} has no saved {kind} value for {key:?} to keep"
                    ))
                })?,
        };
        values.insert(key, value);
    }
    Ok(values)
}

fn valid_variable(var: &str) -> bool {
    var.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether an http(s) URL carries userinfo (`user:pass@`) in its authority.
fn has_userinfo(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .contains('@')
}

fn build(
    input: ServerInput,
    saved: Option<&McpServerEntry>,
) -> Result<McpServerEntry, McpConfigError> {
    let entry = match input {
        ServerInput::Stdio {
            name,
            command,
            args,
            env,
        } => {
            let saved_env = match saved {
                Some(McpServerEntry::Stdio { env, .. }) => Some(env),
                _ => None,
            };
            let env = resolve("environment variable", &name, env, saved_env)?;
            McpServerEntry::Stdio {
                name,
                command: command.trim().to_owned(),
                args,
                env,
            }
        }
        ServerInput::Http {
            name,
            url,
            headers,
            auth,
            untrusted_profiles,
            credential_profiles,
        } => {
            let (saved_headers, saved_auth) = match saved {
                Some(McpServerEntry::Http { headers, auth, .. }) => (Some(headers), auth.as_ref()),
                _ => (None, None),
            };
            let headers = resolve("header", &name, headers, saved_headers)?;
            let auth = match auth {
                AuthInput::None => None,
                AuthInput::Keep => Some(saved_auth.cloned().ok_or_else(|| {
                    invalid(format!("MCP server {name:?} has no saved auth to keep"))
                })?),
                AuthInput::Env { var } => {
                    if !valid_variable(&var) {
                        return Err(invalid(format!(
                            "MCP server {name:?} auth variable {var:?} is not an environment variable name"
                        )));
                    }
                    Some(SecretRef::Env { var })
                }
            };
            McpServerEntry::Http {
                name,
                url: url.trim().to_owned(),
                headers,
                auth,
                untrusted_profiles,
                credential_profiles,
            }
        }
    };
    check(&entry)?;
    Ok(entry)
}

/// The rules [`Config::validate`] applies to an enabled `mcp` section, and a
/// server name usable as a tool namespace.
fn check(entry: &McpServerEntry) -> Result<(), McpConfigError> {
    let name = entry.name();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(invalid(format!(
            "MCP server name {name:?} must be ASCII letters, digits, '-' or '_'"
        )));
    }
    match entry {
        McpServerEntry::Stdio { command, .. } => {
            if command.is_empty() {
                return Err(invalid(format!("MCP server {name:?} has an empty command")));
            }
        }
        McpServerEntry::Http {
            url,
            untrusted_profiles,
            credential_profiles,
            ..
        } => {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(invalid(format!(
                    "MCP server {name:?} url must be an http(s) URL"
                )));
            }
            if has_userinfo(url) {
                return Err(invalid(format!(
                    "MCP server {name:?} url must not carry credentials; use auth"
                )));
            }
            for profile in untrusted_profiles.iter().chain(credential_profiles) {
                if ProfileId::new(profile.clone()).is_err() {
                    return Err(invalid(format!(
                        "MCP server {name:?} names {profile:?}, which is not a valid profile id"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(name: &str, header: Option<&str>, auth: AuthInput) -> ServerInput {
        let mut headers = BTreeMap::new();
        headers.insert("X-Key".to_owned(), header.map(str::to_owned));
        ServerInput::Http {
            name: name.to_owned(),
            url: "https://mcp.example.com/mcp".to_owned(),
            headers,
            auth,
            untrusted_profiles: Vec::new(),
            credential_profiles: Vec::new(),
        }
    }

    #[test]
    fn a_view_withholds_every_value() {
        let mut config = Config::default();
        add(
            &mut config,
            http(
                "docs",
                Some("header-value"),
                AuthInput::Env {
                    var: "DOCS_KEY".to_owned(),
                },
            ),
        )
        .unwrap();
        let shown = serde_json::to_string(&view(&config)).unwrap();
        assert!(!shown.contains("header-value"), "{shown}");
        assert!(
            shown.contains("X-Key") && shown.contains("DOCS_KEY"),
            "{shown}"
        );
    }

    #[test]
    fn an_edit_keeps_a_withheld_value_and_refuses_one_never_saved() {
        let mut config = Config::default();
        add(
            &mut config,
            http(
                "docs",
                Some("kept"),
                AuthInput::Env {
                    var: "K".to_owned(),
                },
            ),
        )
        .unwrap();
        update(&mut config, "docs", http("docs", None, AuthInput::Keep)).unwrap();
        match &config.mcp.servers[0] {
            McpServerEntry::Http { headers, auth, .. } => {
                assert_eq!(headers.get("X-Key").map(String::as_str), Some("kept"));
                assert_eq!(
                    auth,
                    &Some(SecretRef::Env {
                        var: "K".to_owned()
                    })
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            add(&mut config, http("other", None, AuthInput::None)),
            Err(McpConfigError::Invalid(_))
        ));
        assert!(matches!(
            add(&mut config, http("docs", Some("v"), AuthInput::None)),
            Err(McpConfigError::Conflict(_))
        ));
        assert!(matches!(
            remove(&mut config, "missing"),
            Err(McpConfigError::NotFound(_))
        ));
    }
}
