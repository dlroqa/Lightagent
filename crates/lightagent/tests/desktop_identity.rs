//! Identity through one shared operation set (D7-7): `lightagent identity`
//! and the desktop runtime's API manage one identity store as the local
//! operator. What one writes the other lists, and the API answers with the
//! JSON the CLI prints. A credential's secret reaches only a new owner-only
//! file: no answer, audit record or file under the home carries it. A
//! server that is not the local operator's (keyed, so reachable from
//! elsewhere) refuses every identity route. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192";
const IDENTITY: &str = "/api/lightagent/v1/identity";

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

fn scratch_home(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let home = std::env::temp_dir().join(format!(
        "lightagent-desktop-identity-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).expect("home");
    home
}

/// The first line `child` prints, within a minute. The rest of its output
/// is read and dropped, so a later line never meets a closed pipe.
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

/// The desktop app's runtime: `serve --desktop`, behind the desktop guard.
fn start_desktop(home: &Path) -> Runtime {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent serve --desktop");
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

/// `lightagent serve` on loopback: open, or keyed when `key` is given.
fn start_plain(home: &Path, key: Option<&str>) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(key) = key {
        command
            .args(["--key-env", "LIGHTAGENT_TEST_IDENTITY_KEY"])
            .env("LIGHTAGENT_TEST_IDENTITY_KEY", key);
    }
    let mut child = command.spawn().expect("start lightagent serve");
    // "Lightagent API listening on http://<addr>/api/lightagent/v1  (profile '…')"
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

/// One request, with the key when given: the status and the raw body.
fn send(
    runtime: &Runtime,
    key: Option<&str>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let body = body.unwrap_or("");
    let auth = key
        .map(|key| format!("Authorization: Bearer {key}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn call(runtime: &Runtime, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    send(runtime, Some(KEY), method, path, body)
}

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

/// A request that must succeed; its JSON answer.
fn ok(runtime: &Runtime, method: &str, path: &str, body: Option<Value>) -> Value {
    let body = body.map(|body| body.to_string());
    let (status, raw) = call(runtime, method, path, body.as_deref());
    assert_eq!(status, 200, "{method} {path}: {raw}");
    parsed(&raw)
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_ok(home: &Path, args: &[&str]) {
    let output = cli(home, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli(home, &all);
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

/// The secret part of the credential in `file` (`la_<key id>_<secret>`).
fn secret_in(file: &Path) -> String {
    let token = std::fs::read_to_string(file).expect("secret file");
    let secret = token
        .trim()
        .rsplit('_')
        .next()
        .unwrap_or_default()
        .to_owned();
    assert!(secret.len() >= 32, "no secret in {}", file.display());
    secret
}

/// Fail if any file under `dir`, except those under `skip`, holds `secret`.
fn assert_nowhere(dir: &Path, skip: &Path, secret: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.starts_with(skip) {
            continue;
        }
        if path.is_dir() {
            assert_nowhere(&path, skip, secret);
        } else if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(secret),
                "{} holds a credential's secret",
                path.display()
            );
        }
    }
}

fn row<'a>(rows: &'a Value, key: &str, id: &str) -> &'a Value {
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row[key] == id))
        .unwrap_or(&Value::Null)
}

#[test]
fn the_cli_and_the_api_manage_one_identity_store() {
    let home = scratch_home("one");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    let handed = home.join("handed-out");
    std::fs::create_dir_all(&handed).expect("dir");
    let runtime = start_desktop(&home);

    // Reads never create the store; the API reports what the CLI does.
    assert_eq!(
        ok(&runtime, "GET", IDENTITY, None),
        cli_json(&home, &["identity", "status"])
    );
    assert_eq!(ok(&runtime, "GET", IDENTITY, None)["store"], "absent");

    // A user created over the API and one by the CLI: listed alike.
    let users = format!("{IDENTITY}/users");
    let created = ok(&runtime, "POST", &users, Some(json!({"id": "alice"})));
    assert_eq!(created["created"], "alice", "{created}");
    cli_ok(&home, &["identity", "user", "create", "bob"]);
    assert_eq!(
        ok(&runtime, "GET", &users, None),
        cli_json(&home, &["identity", "user", "list"])
    );
    assert_eq!(
        ok(&runtime, "GET", IDENTITY, None),
        cli_json(&home, &["identity", "status"])
    );

    // Issued over the API: the secret goes only to the new owner-only file.
    let credentials = format!("{IDENTITY}/credentials");
    let alice_key = handed.join("alice.key");
    let body = json!({
        "principal": "alice",
        "expires_in": 3600,
        "capabilities": ["runs_read", "sessions_read"],
        "profiles": ["work"],
        "secret_file": alice_key,
    })
    .to_string();
    let (status, issued_raw) = call(&runtime, "POST", &credentials, Some(&body));
    assert_eq!(status, 200, "{issued_raw}");
    let issued = parsed(&issued_raw);
    let key_id = issued["issued"].as_str().expect("key id").to_owned();
    assert_eq!(issued["secret_delivered_to"], "file", "{issued}");
    assert_eq!(
        issued["restriction"],
        cli_json(&home, &["identity", "credential", "list"])["credentials"][0]["restriction"]
    );
    assert_eq!(
        issued["restriction"]["profiles"],
        json!(["work"]),
        "{issued}"
    );
    assert_eq!(
        issued["restriction"]["capabilities"],
        json!(["runs_read", "sessions_read"]),
        "{issued}"
    );
    let token = std::fs::read_to_string(&alice_key).expect("secret file");
    assert!(
        token.starts_with(&format!("la_{key_id}_")),
        "the file holds the issued credential"
    );
    let secret = secret_in(&alice_key);
    assert!(
        !issued_raw.contains(&secret),
        "the answer carries the secret"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&alice_key)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the secret file is owner-only");
    }

    // Refused without a new, absolute file, or for an unknown user: nothing
    // is issued, no file is overwritten or left behind, no path is named.
    let before = ok(&runtime, "GET", &credentials, None);
    let stray = handed.join("nobody.key");
    for body in [
        json!({"principal": "alice"}),
        json!({"principal": "alice", "secret_file": "relative.key"}),
        json!({"principal": "alice", "secret_file": alice_key}),
        json!({"principal": "nobody", "secret_file": stray}),
        json!({"principal": "alice", "capabilities": ["fly"], "secret_file": stray}),
    ] {
        let (status, raw) = call(&runtime, "POST", &credentials, Some(&body.to_string()));
        assert_eq!(status, 400, "{body}: {raw}");
        assert!(
            !raw.contains(arg(&handed)),
            "the error names the file: {raw}"
        );
        assert!(!raw.contains(&secret), "the error carries a secret: {raw}");
    }
    assert_eq!(
        std::fs::read_to_string(&alice_key).expect("secret file"),
        token
    );
    assert!(!stray.exists(), "a refused issue left its file");
    assert_eq!(ok(&runtime, "GET", &credentials, None), before);

    // Listed alike; a rotation by the CLI is listed over the API, one over
    // the API by the CLI.
    let alice_only = format!("{credentials}?principal=alice");
    let cli_alice = ["identity", "credential", "list", "--principal", "alice"];
    assert_eq!(
        ok(&runtime, "GET", &alice_only, None),
        cli_json(&home, &cli_alice)
    );
    let rotated_key = handed.join("alice-rotated.key");
    let rotated = cli_json(
        &home,
        &[
            "identity",
            "credential",
            "rotate",
            &key_id,
            "--secret-file",
            arg(&rotated_key),
        ],
    );
    let second = rotated["issued"].as_str().expect("new key id").to_owned();
    let api_rotated_key = handed.join("alice-api-rotated.key");
    let api_rotated = ok(
        &runtime,
        "POST",
        &format!("{credentials}/{second}/rotate"),
        Some(json!({"expires_in": 600, "secret_file": api_rotated_key})),
    );
    assert_eq!(api_rotated["replaces"], second.as_str(), "{api_rotated}");
    assert_eq!(api_rotated["previous_still_valid"], true, "{api_rotated}");
    assert_eq!(api_rotated["secret_delivered_to"], "file", "{api_rotated}");
    let listed = ok(&runtime, "GET", &alice_only, None);
    assert_eq!(listed, cli_json(&home, &cli_alice));
    assert_eq!(
        listed["credentials"].as_array().map(Vec::len),
        Some(3),
        "{listed}"
    );

    // Revoked over the API: revoked for the CLI.
    let revoked = ok(
        &runtime,
        "POST",
        &format!("{credentials}/{key_id}/revoke"),
        None,
    );
    assert_eq!(revoked["revoked"], key_id.as_str(), "{revoked}");
    let listed = cli_json(&home, &cli_alice);
    assert_eq!(
        row(&listed["credentials"], "key_id", &key_id)["state"],
        "revoked"
    );

    // Ownership: assigned over the API, reassigned by the CLI, moved back
    // over the API, removed over the API; the listings agree throughout.
    let owner = format!("{IDENTITY}/owners/work");
    let grants = format!("{IDENTITY}/grants");
    let work_grants = format!("{grants}?profile=work");
    let cli_work = ["identity", "grants", "list", "--profile", "work"];
    let assigned = ok(&runtime, "PUT", &owner, Some(json!({"principal": "alice"})));
    assert_eq!(assigned["owner"], "alice", "{assigned}");
    assert_eq!(assigned["profile"], "work", "{assigned}");
    let listed = ok(&runtime, "GET", &work_grants, None);
    assert_eq!(listed, cli_json(&home, &cli_work));
    assert_eq!(listed["profiles"][0]["owner"], "alice", "{listed}");
    let (status, raw) = call(
        &runtime,
        "PUT",
        &owner,
        Some(&json!({"principal": "bob"}).to_string()),
    );
    assert_eq!(status, 400, "an owned profile is not assigned again: {raw}");
    cli_ok(&home, &["identity", "owner", "reassign", "work", "bob"]);
    assert_eq!(
        ok(&runtime, "GET", &grants, None),
        cli_json(&home, &["identity", "grants"])
    );
    let moved = ok(
        &runtime,
        "POST",
        &format!("{owner}/reassign"),
        Some(json!({"principal": "alice"})),
    );
    assert_eq!(moved["owner"], "alice", "{moved}");
    let revoked = ok(
        &runtime,
        "POST",
        &format!("{grants}/revoke"),
        Some(json!({"profile": "work", "principal": "bob", "capability": "sessions_read"})),
    );
    assert_eq!(revoked["revoked"], "sessions_read", "{revoked}");
    let (status, raw) = call(
        &runtime,
        "POST",
        &format!("{grants}/revoke"),
        Some(&json!({"profile": "work", "principal": "bob", "capability": "fly"}).to_string()),
    );
    assert_eq!(status, 400, "{raw}");
    assert_eq!(
        ok(&runtime, "GET", &work_grants, None),
        cli_json(&home, &cli_work)
    );
    let removed = ok(&runtime, "DELETE", &owner, None);
    assert_eq!(removed["owner"], Value::Null, "{removed}");
    assert_eq!(cli_json(&home, &cli_work)["profiles"], json!([]));

    // Suspended over the API: suspended for the CLI, its credentials revoked.
    let suspended = ok(&runtime, "POST", &format!("{users}/alice/suspend"), None);
    assert_eq!(suspended["suspended"], "alice", "{suspended}");
    let listed = cli_json(&home, &["identity", "user", "list"]);
    let alice = row(&listed["principals"], "id", "alice");
    assert_eq!(alice["status"], "suspended", "{listed}");
    assert_eq!(alice["unrevoked_credentials"], 0, "{listed}");

    // The audit over the API is the CLI's.
    let audit = ok(&runtime, "GET", &format!("{IDENTITY}/audit?limit=5"), None);
    assert_eq!(
        audit,
        cli_json(&home, &["identity", "audit", "--limit", "5"])
    );
    assert_eq!(
        audit["records"].as_array().map(Vec::len),
        Some(5),
        "{audit}"
    );
    let full = ok(
        &runtime,
        "GET",
        &format!("{IDENTITY}/audit?limit=1000"),
        None,
    );

    // No secret anywhere but its own file.
    for file in [&alice_key, &rotated_key, &api_rotated_key] {
        let secret = secret_in(file);
        assert_nowhere(&home, &handed, &secret);
        assert!(
            !full.to_string().contains(&secret),
            "the audit holds a secret"
        );
    }
}

#[test]
fn users_created_by_the_cli_and_the_api_at_once_are_all_kept() {
    let home = scratch_home("concurrent");
    let runtime = start_desktop(&home);
    let users = format!("{IDENTITY}/users");
    std::thread::scope(|scope| {
        let api = scope.spawn(|| {
            (0..4)
                .map(|n| {
                    let body = json!({"id": format!("api-{n}")}).to_string();
                    call(&runtime, "POST", &users, Some(&body)).0
                })
                .collect::<Vec<_>>()
        });
        let by_cli = scope.spawn(|| {
            for n in 0..4 {
                cli_ok(&home, &["identity", "user", "create", &format!("cli-{n}")]);
            }
        });
        assert_eq!(api.join().expect("api"), vec![200; 4]);
        by_cli.join().expect("cli");
    });
    let listed = cli_json(&home, &["identity", "user", "list"]);
    let mut ids: Vec<String> = listed["principals"]
        .as_array()
        .expect("principals")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    let expected: Vec<String> = [
        "api-0", "api-1", "api-2", "api-3", "cli-0", "cli-1", "cli-2", "cli-3",
    ]
    .iter()
    .map(|id| (*id).to_owned())
    .collect();
    assert_eq!(ids, expected);
}

#[test]
fn only_the_local_operator_reaches_identity() {
    let home = scratch_home("gate");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "use", "work"]);

    // A keyed server may be reached from elsewhere: every identity route
    // refuses, even with the key, and nothing is created.
    let keyed = start_plain(&home, Some(KEY));
    let routes: [(&str, String, Option<&str>); 6] = [
        ("GET", IDENTITY.to_owned(), None),
        ("GET", format!("{IDENTITY}/users"), None),
        (
            "POST",
            format!("{IDENTITY}/users"),
            Some(r#"{"id":"mallory"}"#),
        ),
        ("GET", format!("{IDENTITY}/credentials"), None),
        ("DELETE", format!("{IDENTITY}/owners/work"), None),
        ("GET", format!("{IDENTITY}/audit"), None),
    ];
    for (method, path, body) in &routes {
        let (status, raw) = send(&keyed, Some(KEY), method, path, *body);
        assert_eq!(status, 403, "{method} {path}: {raw}");
        let (status, _) = send(&keyed, None, method, path, *body);
        assert!(
            matches!(status, 401 | 403),
            "{method} {path} without the key: {status}"
        );
    }
    drop(keyed);
    assert_eq!(cli_json(&home, &["identity", "status"])["store"], "absent");

    // An open server is reachable only on loopback: it answers.
    let open = start_plain(&home, None);
    let (status, raw) = send(&open, None, "GET", IDENTITY, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw), cli_json(&home, &["identity", "status"]));
}
