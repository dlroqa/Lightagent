//! Remote mode (D8): the window served by a Lightagent server elsewhere,
//! started as `lightagent serve --key-env LIGHTAGENT_API_KEY`, instead of
//! the bundled runtime.
//!
//! - The key is the host's. It comes from this app's own
//!   `LIGHTAGENT_API_KEY` (the variable the server reads) or from a key file
//!   the person chooses in the native picker, read by the host as a
//!   credential's secret is (secret.rs). It is kept in memory for the
//!   connection and never written, logged or shown, and the bundled runtime
//!   never receives it (sidecar.rs removes the variable from its
//!   environment).
//! - It is sent only to the origin the person named: HTTPS, or plain HTTP to
//!   this computer. The client uses no proxy and follows no redirect; a
//!   redirect is an error.
//! - The server must describe itself (`GET /api/lightagent/v1/server`) with
//!   this app's version and protocol, or the connection is refused, naming
//!   both.
//! - While connected the bundled runtime is stopped: nothing runs on this
//!   computer, and when the server fails its requests fail with that error.
//!   Nothing falls back to local execution. Disconnecting starts the bundled
//!   runtime again.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Host, Runtime, lock};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const SHAPE: &str = "give the server's address only, like https://agent.example.com or https://agent.example.com:8787";

/// A connected remote server. Its key is overwritten when it is dropped.
pub struct Endpoint {
    pub origin: String,
    key: String,
    pub version: String,
    pub profile: Option<String>,
    pub capabilities: Vec<String>,
}

impl Endpoint {
    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.key)
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let len = self.key.len();
        self.key.clear();
        self.key.extend(std::iter::repeat_n('0', len));
    }
}

/// What a connection attempt did. Never carries the key.
#[derive(Debug, Serialize)]
pub struct Outcome {
    /// `connected`, or `cancelled` (no key file was chosen; nothing sent).
    pub state: &'static str,
    pub origin: Option<String>,
    pub version: Option<String>,
}

#[derive(Deserialize)]
struct ServerInfo {
    service: String,
    version: String,
    protocol: u32,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    profile: Option<String>,
}

/// The origin a key may be sent to: `https://host[:port]`, or `http://` to
/// this computer only. Anything else is refused before a connection.
pub fn origin_of(address: &str) -> Result<String, String> {
    let address = address.trim();
    let (scheme, rest) = address.split_once("://").ok_or(SHAPE)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(
            "the address must start with https:// (or http:// for this computer)".to_owned(),
        );
    }
    let authority = rest.strip_suffix('/').unwrap_or(rest).to_ascii_lowercase();
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@', '\\'])
        || authority.chars().any(char::is_whitespace)
    {
        return Err(SHAPE.to_owned());
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(SHAPE)?;
        let after = &authority[end + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':').ok_or(SHAPE)?)
        };
        (&authority[..=end], port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority.as_str(), None),
        }
    };
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || !matches!(port.parse::<u16>(), Ok(number) if number != 0))
    {
        return Err(SHAPE.to_owned());
    }
    let valid = if let Some(inside) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        !inside.is_empty()
            && inside
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b':' || byte == b'.')
    } else {
        !host.is_empty()
            && !host.starts_with(['.', '-'])
            && !host.ends_with('.')
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
    };
    if !valid {
        return Err(SHAPE.to_owned());
    }
    if scheme == "http" && !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(
            "plain http:// is only for this computer (127.0.0.1, localhost or [::1]); over a network the key is sent only by https://"
                .to_owned(),
        );
    }
    Ok(format!("{scheme}://{authority}"))
}

/// A key the server can be sent in a header: one token of printable
/// characters.
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 4096 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("the key must be one line of printable characters, without spaces".to_owned());
    }
    Ok(())
}

/// Connect the window to the server at `address`, with the key from
/// `source` (`environment` or `file`).
pub async fn connect(
    host: &Arc<Host>,
    app: &tauri::AppHandle,
    address: String,
    source: String,
) -> Result<Outcome, String> {
    let origin = origin_of(&address)?;
    let key = match source.as_str() {
        "environment" => std::env::var("LIGHTAGENT_API_KEY")
            .ok()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())
            .ok_or("LIGHTAGENT_API_KEY is not set in this app's environment")?,
        "file" => {
            let Some(path) = crate::secret::pick(app).await? else {
                return Ok(Outcome {
                    state: "cancelled",
                    origin: None,
                    version: None,
                });
            };
            crate::secret::read_secret(&path)
                .map_err(|reason| format!("the key file cannot be used: {reason}"))?
                .trim()
                .to_owned()
        }
        _ => return Err("the key comes from the environment or from a file".to_owned()),
    };
    check_key(&key)?;
    let info = probe(&host.http, &origin, &key).await?;
    let _lifecycle = host.lifecycle.lock().await;
    if host.switching.load(Ordering::Acquire) {
        return Err("a profile switch is in progress; try again when it has finished".to_owned());
    }
    if !host.is_remote() {
        let runs = crate::switch::active_runs(host).await.unwrap_or_default();
        if !runs.is_empty() {
            return Err(format!(
                "{} run(s) are working on this computer; stop them before using another server",
                runs.len()
            ));
        }
    }
    let version = info.version.clone();
    let previous = std::mem::replace(
        &mut *lock(&host.runtime),
        Runtime::Remote(Endpoint {
            origin: origin.clone(),
            key,
            version: info.version,
            profile: info.profile,
            capabilities: info.capabilities,
        }),
    );
    // Stopping the bundled runtime may take its grace period.
    let _ = tauri::async_runtime::spawn_blocking(move || drop(previous)).await;
    host.e2e.note_once(
        "REMOTE_CONNECTED",
        &format!("origin={origin} version={version}"),
    );
    Ok(Outcome {
        state: "connected",
        origin: Some(origin),
        version: Some(version),
    })
}

/// Leave remote mode and start the bundled runtime again.
pub async fn disconnect(host: &Arc<Host>) -> Result<(), String> {
    let host = Arc::clone(host);
    tauri::async_runtime::spawn_blocking(move || {
        let _lifecycle = host.lifecycle.blocking_lock();
        if !host.is_remote() {
            return;
        }
        // Dropping the endpoint overwrites its key.
        *lock(&host.runtime) = Runtime::Starting;
        host.e2e.note_once("REMOTE_DISCONNECTED", "");
        crate::start_runtime(&host, false);
    })
    .await
    .map_err(|error| error.to_string())
}

/// Ask the server what it is, with the key, and check it can serve this app.
async fn probe(http: &reqwest::Client, origin: &str, key: &str) -> Result<ServerInfo, String> {
    let response = http
        .get(format!("{origin}/api/lightagent/v1/server"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("could not reach {origin}: {}", reason(&error)))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(format!(
            "{origin} answered with a redirect ({}); the key is sent only to the address you gave, so it was not followed",
            status.as_u16()
        ));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "{origin} refused the key ({}): check LIGHTAGENT_API_KEY, or the key file, against the key the server was started with",
            status.as_u16()
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "{origin} does not describe itself: it is not a Lightagent server, or one older than {}",
            crate::VERSION
        ));
    }
    if !status.is_success() {
        return Err(format!("{origin} answered {}", status.as_u16()));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("{origin}'s answer was cut off: {}", reason(&error)))?;
    let info: ServerInfo = serde_json::from_str(&body)
        .map_err(|_| format!("{origin} answered, but not as a Lightagent server"))?;
    if info.service != "lightagent" {
        return Err(format!("{origin} is not a Lightagent server"));
    }
    if info.protocol != crate::contract::PROTOCOL {
        return Err(format!(
            "{origin} speaks protocol {}, and this app speaks {}",
            info.protocol,
            crate::contract::PROTOCOL
        ));
    }
    if info.version != crate::VERSION {
        return Err(format!(
            "{origin} runs Lightagent {}, and this app is {}: they must be the same version",
            info.version,
            crate::VERSION
        ));
    }
    Ok(info)
}

fn reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "it did not answer in time".to_owned()
    } else if error.is_connect() {
        "the connection failed (nothing is listening there, or there is no network)".to_owned()
    } else {
        "the request failed".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_only_to_https_or_to_this_computer() {
        for (address, origin) in [
            ("https://agent.example.com", "https://agent.example.com"),
            (
                " HTTPS://Agent.Example.com:8787/ ",
                "https://agent.example.com:8787",
            ),
            ("http://127.0.0.1:8787", "http://127.0.0.1:8787"),
            ("http://localhost", "http://localhost"),
            ("http://[::1]:9", "http://[::1]:9"),
            ("https://[2001:db8::1]:443", "https://[2001:db8::1]:443"),
        ] {
            assert_eq!(origin_of(address).as_deref(), Ok(origin), "{address}");
        }
        for address in [
            "http://192.0.2.10:8787",
            "http://agent.example.com",
            "ftp://agent.example.com",
            "agent.example.com",
            "https://user@agent.example.com",
            "https://agent.example.com/api",
            "https://agent.example.com?x=1",
            "https://agent.example.com#x",
            "https://agent.example.com:0",
            "https://agent.example.com:99999",
            "https://agent.example.com:",
            "https://agent example.com",
            "https://",
            "https://[::1",
            "https://.example.com",
            "https://-example.com",
        ] {
            assert!(origin_of(address).is_err(), "{address}");
        }
    }

    #[test]
    fn a_key_is_one_printable_token() {
        assert!(check_key("la-remote-key_123.ABC").is_ok());
        for key in ["", "two words", "line\nbreak", "tab\tkey", "\u{e9}t\u{e9}"] {
            assert!(check_key(key).is_err(), "{key:?}");
        }
        assert!(check_key(&"k".repeat(4097)).is_err());
    }
}
