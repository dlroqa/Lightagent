//! Remote credentials through one shared operation set (D7-9): `lightagent
//! remote-credential` and the desktop runtime's API manage one vault as the
//! local operator, with the key each process is started with. What one
//! writes the other lists, and the API answers with the JSON the CLI
//! prints. A secret is read only from a file and is never in an answer,
//! the audit, or a file under the home in plaintext; refusals name no
//! file; a configured key that cannot be used refuses every command; a
//! keyed server refuses every route. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3";
const VAULT_HEX_KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const OTHER_HEX_KEY: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const CREDENTIALS: &str = "/api/lightagent/v1/remote-credentials";

struct Runtime {
    child: Child,
    _lifeline: Option<ChildStdin>,
    addr: String,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lightagent-desktop-credentials-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The key environment a process runs with: none, a key file, a key, or
/// both a key file and a key.
#[derive(Clone, Copy)]
enum KeyEnv<'a> {
    None,
    File(&'a Path),
    Hex(&'a str),
    Both(&'a Path, &'a str),
}

fn with_key(command: &mut Command, key: KeyEnv<'_>) {
    command
        .env_remove("LIGHTAGENT_CREDENTIAL_KEY_FILE")
        .env_remove("LIGHTAGENT_CREDENTIAL_KEY");
    match key {
        KeyEnv::None => {}
        KeyEnv::File(path) => {
            command.env("LIGHTAGENT_CREDENTIAL_KEY_FILE", path);
        }
        KeyEnv::Hex(hex) => {
            command.env("LIGHTAGENT_CREDENTIAL_KEY", hex);
        }
        KeyEnv::Both(path, hex) => {
            command.env("LIGHTAGENT_CREDENTIAL_KEY_FILE", path);
            command.env("LIGHTAGENT_CREDENTIAL_KEY", hex);
        }
    }
}

/// The first line `child` prints, within a minute; the rest is drained.
fn first_line(child: &mut Child) -> String {
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        let mut rest = String::new();
        while reader.read_line(&mut rest).is_ok_and(|read| read > 0) {
            rest.clear();
        }
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("first line")
}

fn start_desktop(home: &Path, key: KeyEnv<'_>) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    with_key(&mut command, key);
    let mut child = command.spawn().expect("start lightagent serve --desktop");
    let mut lifeline = child.stdin.take().expect("stdin");
    writeln!(lifeline, "{KEY}").expect("key");
    lifeline.flush().expect("flush");
    let line = first_line(&mut child);
    let handshake: Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: Some(lifeline),
        addr,
    }
}

/// `lightagent serve` on loopback with an API key: reachable from elsewhere.
fn start_keyed(home: &Path) -> Runtime {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .args(["--key-env", "LIGHTAGENT_TEST_CREDENTIALS_KEY"])
        .env("LIGHTAGENT_TEST_CREDENTIALS_KEY", KEY)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent serve");
    let line = first_line(&mut child);
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_else(|| panic!("no address in {line:?}"))
        .to_owned();
    Runtime {
        child,
        _lifeline: None,
        addr,
    }
}

fn call(runtime: &Runtime, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        runtime.addr,
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("send");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let raw = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, raw)
}

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn ok(runtime: &Runtime, method: &str, path: &str, body: Option<Value>) -> (Value, String) {
    let body = body.map(|body| body.to_string());
    let (status, raw) = call(runtime, method, path, body.as_deref());
    assert_eq!(status, 200, "{method} {path}: {raw}");
    (parsed(&raw), raw)
}

fn cli(home: &Path, key: KeyEnv<'_>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null());
    with_key(&mut command, key);
    command.output().expect("run lightagent")
}

fn cli_ok(home: &Path, key: KeyEnv<'_>, args: &[&str]) {
    let output = cli(home, key, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_json(home: &Path, key: KeyEnv<'_>, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli(home, key, &all);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

/// Fail if any file under `dir` holds `secret` in plaintext.
fn assert_nowhere(dir: &Path, secret: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            assert_nowhere(&path, secret);
        } else if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(secret),
                "{} holds a secret in plaintext",
                path.display()
            );
        }
    }
}

fn grant<'a>(listing: &'a Value, id: &str) -> &'a Value {
    listing["credentials"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .unwrap_or(&Value::Null)
}

#[test]
fn the_cli_and_the_api_manage_one_vault() {
    let home = scratch("home");
    let outside = scratch("outside");
    cli_ok(
        &home,
        KeyEnv::None,
        &["config", "set", "extensions.enabled", "true"],
    );
    cli_ok(&home, KeyEnv::None, &["profile", "create", "work"]);
    let vault = format!("{CREDENTIALS}/vault");

    // Without a key: the status agrees; adding is refused; a key file is
    // made outside the home only, owner-only, never twice.
    let runtime = start_desktop(&home, KeyEnv::None);
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(
        status,
        cli_json(&home, KeyEnv::None, &["remote-credential", "status"])
    );
    assert_eq!(status["key_configured"], false, "{status}");
    assert_eq!(status["key_source"], Value::Null, "{status}");
    let secret_file = outside.join("web.secret");
    let secret = "web-secret-7f3a9c1e5b2d4f6a8c0e";
    std::fs::write(&secret_file, format!("{secret}\n")).expect("secret file");
    let add = json!({
        "profile": "work",
        "service": "web-search",
        "destinations": ["https://search.example.com"],
        "max_requests_per_run": 5,
        "operations": ["search"],
        "expires_in": 3600,
        "secret_file": secret_file,
    });
    let (code, raw) = call(&runtime, "POST", CREDENTIALS, Some(&add.to_string()));
    assert_eq!(code, 400, "{raw}");
    assert!(raw.contains("no credential key is configured"), "{raw}");
    let key_a = outside.join("a.key");
    let keys = format!("{CREDENTIALS}/keys");
    let (made, _) = ok(&runtime, "POST", &keys, Some(json!({"path": key_a})));
    assert_eq!(made["key_file"], "created", "{made}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key_a)
            .expect("key file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the key file is owner-only");
    }
    for path in [key_a.clone(), home.join("inside.key")] {
        let (code, raw) = call(
            &runtime,
            "POST",
            &keys,
            Some(&json!({"path": path}).to_string()),
        );
        assert_eq!(code, 400, "{raw}");
    }
    let (code, raw) = call(&runtime, "POST", &keys, Some(r#"{"path":"relative.key"}"#));
    assert_eq!(code, 400, "{raw}");
    drop(runtime);

    // With the key file: what the API adds the CLI lists, and the reverse.
    let key = KeyEnv::File(&key_a);
    let runtime = start_desktop(&home, key);
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(
        status,
        cli_json(&home, key, &["remote-credential", "status"])
    );
    assert_eq!(status["key_source"], "file", "{status}");
    let (added, added_raw) = ok(&runtime, "POST", CREDENTIALS, Some(add.clone()));
    let id = added["added"].as_str().expect("grant id").to_owned();
    assert_eq!(added["service"], "web-search", "{added}");
    assert!(!added_raw.contains(secret), "the answer carries the secret");
    let mcp_secret_file = outside.join("mcp.secret");
    let mcp_secret = "mcp-secret-0b1c2d3e4f5a6b7c8d9e";
    std::fs::write(&mcp_secret_file, mcp_secret).expect("secret file");
    let by_cli = cli_json(
        &home,
        key,
        &[
            "remote-credential",
            "add",
            "work",
            "--service",
            "mcp:docs",
            "--destination",
            "https://docs.example.com",
            "--secret-file",
            arg(&mcp_secret_file),
        ],
    );
    let mcp_id = by_cli["added"].as_str().expect("grant id").to_owned();
    let only_work = format!("{CREDENTIALS}?profile=work");
    let (listed, listed_raw) = ok(&runtime, "GET", &only_work, None);
    assert_eq!(
        listed,
        cli_json(
            &home,
            key,
            &["remote-credential", "list", "--profile", "work"]
        )
    );
    assert_eq!(
        listed["credentials"].as_array().map(Vec::len),
        Some(2),
        "{listed}"
    );
    assert!(!listed_raw.contains(secret) && !listed_raw.contains(mcp_secret));
    assert_eq!(grant(&listed, &id)["max_requests_per_run"], 5, "{listed}");
    assert_eq!(
        grant(&listed, &id)["operations"],
        json!(["search"]),
        "{listed}"
    );
    assert!(grant(&listed, &id)["expires_unix"].is_u64(), "{listed}");

    // Refused, naming no file: no secret file, a relative one, a missing
    // one, an invalid destination, no destination, an unknown profile.
    let missing = outside.join("missing.secret");
    for body in [
        json!({"profile": "work", "service": "web-search", "destinations": ["https://a.example"]}),
        json!({"profile": "work", "service": "web-search", "destinations": ["https://a.example"], "secret_file": "relative.secret"}),
        json!({"profile": "work", "service": "web-search", "destinations": ["https://a.example"], "secret_file": missing}),
        json!({"profile": "work", "service": "web-search", "destinations": ["http://a.example/path"], "secret_file": secret_file}),
        json!({"profile": "work", "service": "web-search", "destinations": [], "secret_file": secret_file}),
        json!({"profile": "nobody", "service": "web-search", "destinations": ["https://a.example"], "secret_file": secret_file}),
    ] {
        let (code, raw) = call(&runtime, "POST", CREDENTIALS, Some(&body.to_string()));
        assert_eq!(code, 400, "{body}: {raw}");
        assert!(
            !raw.contains(arg(&outside)),
            "the error names a file: {raw}"
        );
        assert!(!raw.contains(secret), "the error carries the secret: {raw}");
    }
    assert_eq!(ok(&runtime, "GET", &only_work, None).0, listed);

    // Rotated over the API from a new file; revoked over the API.
    let rotated_file = outside.join("web-rotated.secret");
    let rotated_secret = "web-secret-rotated-9e8d7c6b5a4f3e2d";
    std::fs::write(&rotated_file, rotated_secret).expect("secret file");
    let (rotated, rotated_raw) = ok(
        &runtime,
        "POST",
        &format!("{CREDENTIALS}/{id}/rotate"),
        Some(json!({"secret_file": rotated_file})),
    );
    assert_eq!(rotated["rotated"], id.as_str(), "{rotated}");
    assert_eq!(rotated["version"], 2, "{rotated}");
    assert!(!rotated_raw.contains(rotated_secret));
    let listed = cli_json(&home, key, &["remote-credential", "list"]);
    assert_eq!(grant(&listed, &id)["version"], 2, "{listed}");
    let (revoked, _) = ok(
        &runtime,
        "POST",
        &format!("{CREDENTIALS}/{id}/revoke"),
        None,
    );
    assert_eq!(revoked["revoked"], id.as_str(), "{revoked}");
    let listed = cli_json(&home, key, &["remote-credential", "list"]);
    assert_eq!(grant(&listed, &id)["state"], "revoked", "{listed}");
    assert_eq!(grant(&listed, &mcp_id)["state"], "active", "{listed}");

    // The audit and the status agree; no secret in either, nor in plaintext
    // under the home.
    let (audit, audit_raw) = ok(
        &runtime,
        "GET",
        &format!("{CREDENTIALS}/audit?limit=3"),
        None,
    );
    assert_eq!(
        audit,
        cli_json(&home, key, &["remote-credential", "audit", "--limit", "3"])
    );
    assert_eq!(
        audit["records"].as_array().map(Vec::len),
        Some(3),
        "{audit}"
    );
    let (status, status_raw) = ok(&runtime, "GET", &vault, None);
    assert_eq!(
        status,
        cli_json(&home, key, &["remote-credential", "status"])
    );
    for text in [secret, mcp_secret, rotated_secret] {
        assert!(!audit_raw.contains(text) && !status_raw.contains(text));
        assert_nowhere(&home, text);
    }
    drop(runtime);

    // A new key: the active grant does not open with it until it is
    // re-sealed from the previous key over the API.
    let key_b = outside.join("b.key");
    cli_ok(
        &home,
        KeyEnv::None,
        &["remote-credential", "keygen", arg(&key_b)],
    );
    let key = KeyEnv::File(&key_b);
    let runtime = start_desktop(&home, key);
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(status["unopenable_with_configured_key"], 1, "{status}");
    let rekey = format!("{CREDENTIALS}/rekey");
    let (code, raw) = call(
        &runtime,
        "POST",
        &rekey,
        Some(r#"{"previous_key_file":"a.key"}"#),
    );
    assert_eq!(code, 400, "{raw}");
    let (resealed, _) = ok(
        &runtime,
        "POST",
        &rekey,
        Some(json!({"previous_key_file": key_a})),
    );
    assert_eq!(resealed["resealed"], 1, "{resealed}");
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(status["unopenable_with_configured_key"], 0, "{status}");
    assert_eq!(
        status,
        cli_json(&home, key, &["remote-credential", "status"])
    );
}

#[test]
fn a_configured_key_that_cannot_be_used_refuses_every_command() {
    let home = scratch("unusable-home");
    let outside = scratch("unusable-outside");
    let missing = outside.join("missing.key");
    let runtime = start_desktop(&home, KeyEnv::File(&missing));
    for (method, path) in [
        ("GET", format!("{CREDENTIALS}/vault")),
        ("GET", CREDENTIALS.to_owned()),
        ("GET", format!("{CREDENTIALS}/audit")),
    ] {
        let (code, raw) = call(&runtime, method, &path, None);
        assert_eq!(code, 400, "{method} {path}: {raw}");
        assert!(
            raw.contains("the configured credential key is unusable"),
            "{raw}"
        );
        assert!(
            !raw.contains(arg(&outside)),
            "the error names the key file: {raw}"
        );
    }
    assert!(
        !cli(
            &home,
            KeyEnv::File(&missing),
            &["remote-credential", "status"]
        )
        .status
        .success(),
        "the CLI refuses as well"
    );
    // A new key is still made, by the API and by the CLI; the vault stays
    // refused until the configuration names a usable key.
    let (status, raw) = call(
        &runtime,
        "POST",
        &format!("{CREDENTIALS}/keys"),
        Some(&json!({"path": outside.join("new-by-api.key")}).to_string()),
    );
    assert_eq!(status, 200, "{raw}");
    let by_cli = outside.join("new-by-cli.key");
    cli_ok(
        &home,
        KeyEnv::File(&missing),
        &["remote-credential", "keygen", arg(&by_cli)],
    );
    assert!(by_cli.is_file());
    let (status, _) = call(&runtime, "GET", &format!("{CREDENTIALS}/vault"), None);
    assert_eq!(status, 400);
    drop(runtime);

    // A key given as hex in the environment is reported as such.
    let runtime = start_desktop(&home, KeyEnv::Hex(VAULT_HEX_KEY));
    let (status, _) = ok(&runtime, "GET", &format!("{CREDENTIALS}/vault"), None);
    assert_eq!(status["key_source"], "env", "{status}");
    assert_eq!(
        status,
        cli_json(
            &home,
            KeyEnv::Hex(VAULT_HEX_KEY),
            &["remote-credential", "status"]
        )
    );
}

#[test]
fn a_keyed_server_refuses_every_credential_route() {
    let home = scratch("gate");
    cli_ok(
        &home,
        KeyEnv::None,
        &["config", "set", "extensions.enabled", "true"],
    );
    cli_ok(&home, KeyEnv::None, &["profile", "create", "work"]);
    cli_ok(&home, KeyEnv::None, &["profile", "use", "work"]);
    let keyed = start_keyed(&home);
    for (method, path) in [
        ("GET", format!("{CREDENTIALS}/vault")),
        ("GET", CREDENTIALS.to_owned()),
        ("GET", format!("{CREDENTIALS}/audit")),
        ("POST", format!("{CREDENTIALS}/keys")),
    ] {
        let (code, raw) = call(&keyed, method, &path, Some(r#"{"path":"/tmp/never.key"}"#));
        assert_eq!(code, 403, "{method} {path}: {raw}");
    }
    assert!(!Path::new("/tmp/never.key").exists());
}

#[test]
fn a_key_in_the_environment_seals_opens_and_fails_closed() {
    let home = scratch("env-home");
    let outside = scratch("env-outside");
    cli_ok(
        &home,
        KeyEnv::None,
        &["config", "set", "extensions.enabled", "true"],
    );
    cli_ok(&home, KeyEnv::None, &["profile", "create", "work"]);
    let vault = format!("{CREDENTIALS}/vault");
    let secret_file = outside.join("env.secret");
    let secret = "env-secret-1c2d3e4f5a6b7c8d9e0f";
    std::fs::write(&secret_file, format!("{secret}\n")).expect("secret file");
    let add = json!({
        "profile": "work",
        "service": "mcp:env-docs",
        "destinations": ["https://docs.example.com"],
        "secret_file": secret_file,
    });

    // The key in LIGHTAGENT_CREDENTIAL_KEY seals a grant; the CLI with the
    // same key lists it, and the status names the source.
    let key = KeyEnv::Hex(VAULT_HEX_KEY);
    let runtime = start_desktop(&home, key);
    let (added, added_raw) = ok(&runtime, "POST", CREDENTIALS, Some(add.clone()));
    let id = added["added"].as_str().expect("grant id").to_owned();
    let (listed, listed_raw) = ok(&runtime, "GET", CREDENTIALS, None);
    assert_eq!(listed, cli_json(&home, key, &["remote-credential", "list"]));
    assert_eq!(grant(&listed, &id)["version"], 1, "{listed}");
    let (status, status_raw) = ok(&runtime, "GET", &vault, None);
    assert_eq!(status["key_source"], "env", "{status}");
    assert_eq!(status["key_configured"], true, "{status}");
    assert_eq!(status["unopenable_with_configured_key"], 0, "{status}");
    assert_eq!(
        status,
        cli_json(&home, key, &["remote-credential", "status"])
    );
    for raw in [&added_raw, &listed_raw, &status_raw] {
        assert!(
            !raw.contains(secret) && !raw.contains(VAULT_HEX_KEY),
            "an answer holds the secret or the key: {raw}"
        );
    }
    assert_nowhere(&home, secret);
    assert_nowhere(&home, VAULT_HEX_KEY);
    drop(runtime);

    // Another key opens nothing this one sealed: it is counted, not opened.
    let runtime = start_desktop(&home, KeyEnv::Hex(OTHER_HEX_KEY));
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(status["key_source"], "env", "{status}");
    assert_eq!(status["unopenable_with_configured_key"], 1, "{status}");
    drop(runtime);

    // A key file and a key: the key file is used.
    let key_file = outside.join("file.key");
    cli_ok(
        &home,
        KeyEnv::None,
        &["remote-credential", "keygen", arg(&key_file)],
    );
    let both = KeyEnv::Both(&key_file, VAULT_HEX_KEY);
    let runtime = start_desktop(&home, both);
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(
        status["key_source"], "file",
        "the key file comes first: {status}"
    );
    assert_eq!(status["unopenable_with_configured_key"], 1, "{status}");
    assert_eq!(
        status,
        cli_json(&home, both, &["remote-credential", "status"])
    );
    drop(runtime);

    // Not a key: every command is refused, by the API and the CLI, and no
    // answer repeats the value.
    let invalid = "not-a-key-5e6f7a8b9c";
    let runtime = start_desktop(&home, KeyEnv::Hex(invalid));
    for (method, path, body) in [
        ("GET", vault.clone(), None),
        ("GET", CREDENTIALS.to_owned(), None),
        ("POST", CREDENTIALS.to_owned(), Some(add.to_string())),
    ] {
        let (code, raw) = call(&runtime, method, &path, body.as_deref());
        assert_eq!(code, 400, "{method} {path}: {raw}");
        assert!(
            raw.contains("the configured credential key is unusable"),
            "{raw}"
        );
        assert!(
            !raw.contains(invalid) && !raw.contains(secret),
            "{method} {path}: {raw}"
        );
    }
    let refused = cli(
        &home,
        KeyEnv::Hex(invalid),
        &["remote-credential", "status"],
    );
    assert!(!refused.status.success(), "the CLI refuses as well");
    assert!(
        !String::from_utf8_lossy(&refused.stderr).contains(invalid),
        "the CLI repeats the value"
    );
    drop(runtime);

    // An empty value is no key: nothing opens, and adding is refused.
    let runtime = start_desktop(&home, KeyEnv::Hex(""));
    let (status, _) = ok(&runtime, "GET", &vault, None);
    assert_eq!(status["key_configured"], false, "{status}");
    assert_eq!(status["key_source"], Value::Null, "{status}");
    assert_eq!(status["unopenable_with_configured_key"], 1, "{status}");
    let (code, raw) = call(&runtime, "POST", CREDENTIALS, Some(&add.to_string()));
    assert_eq!(code, 400, "{raw}");
    assert!(raw.contains("no credential key is configured"), "{raw}");
}
