//! Worker assignments through one shared operation (D7-6):
//! `lightagent workers status|assign|revoke` and the desktop runtime's API
//! change and report the same assignments. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const WORKERS: &str = "/api/lightagent/v1/workers";

struct Runtime {
    child: Child,
    _lifeline: ChildStdin,
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
        "lightagent-desktop-workers-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).expect("home");
    home
}

fn start(home: &Path) -> Runtime {
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
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(60)).expect("handshake");
    let handshake: Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: lifeline,
        addr,
    }
}

/// One authenticated request: the status and the raw body.
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

fn cli_ok(home: &Path, args: &[&str]) {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output: Output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(&all)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

/// The API's readiness for `profile` (the active one when `None`).
fn api_status(runtime: &Runtime, profile: Option<&str>) -> Value {
    let path = match profile {
        Some(profile) => format!("{WORKERS}?profile={profile}"),
        None => WORKERS.to_owned(),
    };
    let (status, raw) = call(runtime, "GET", &path, None);
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)["workers"].clone()
}

#[test]
fn the_cli_and_the_api_keep_one_set_of_assignments() {
    let home = scratch_home("one");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "create", "helper"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);
    assert_eq!(
        api_status(&runtime, None),
        cli_json(&home, &["workers", "status"])
    );

    // Assigned over the API to a profile: the CLI reports the same.
    let (status, raw) = call(
        &runtime,
        "PUT",
        &format!("{WORKERS}/coder"),
        Some(r#"{"worker":"helper"}"#),
    );
    assert_eq!(status, 200, "{raw}");
    assert!(parsed(&raw)["id"].is_string(), "{raw}");
    let after = api_status(&runtime, None);
    assert_eq!(after, cli_json(&home, &["workers", "status"]));
    assert!(after.to_string().contains("helper"), "{after}");

    // Revoked by the CLI: the API reports the same.
    let revoked = cli_json(&home, &["workers", "revoke", "coder"]);
    assert_eq!(revoked["revoked"], true, "{revoked}");
    assert_eq!(
        api_status(&runtime, None),
        cli_json(&home, &["workers", "status"])
    );

    // The built-in worker over the API; revoked over the API, then nothing left.
    let (status, raw) = call(&runtime, "PUT", &format!("{WORKERS}/reviewer"), Some("{}"));
    assert_eq!(status, 200, "{raw}");
    let (status, raw) = call(&runtime, "DELETE", &format!("{WORKERS}/reviewer"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["revoked"], true, "{raw}");
    let (_, raw) = call(&runtime, "DELETE", &format!("{WORKERS}/reviewer"), None);
    assert_eq!(parsed(&raw)["revoked"], false, "{raw}");

    // Another profile's workers, named.
    let body = r#"{"worker":"work","profile":"helper"}"#;
    let (status, raw) = call(&runtime, "PUT", &format!("{WORKERS}/tester"), Some(body));
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        api_status(&runtime, Some("helper")),
        cli_json(&home, &["workers", "status", "--profile", "helper"])
    );
    let (status, raw) = call(
        &runtime,
        "DELETE",
        &format!("{WORKERS}/tester?profile=helper"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["revoked"], true, "{raw}");

    // Refused as the CLI refuses them.
    for (role, body) in [
        ("wizard", "{}"),
        ("coder", r#"{"worker":"work"}"#),
        ("coder", r#"{"worker":"nobody"}"#),
    ] {
        let (status, raw) = call(&runtime, "PUT", &format!("{WORKERS}/{role}"), Some(body));
        assert_eq!(status, 400, "{role} {body}: {raw}");
    }
}
