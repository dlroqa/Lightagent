//! Profile operations through the desktop runtime's API leave exactly the
//! state the CLI sees, because both perform the same typed operations
//! (`crates/lightagent/src/ops.rs`). Errors carry stable categories.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEY: &str = "9a8b7c6d5e4f30211203f4e5d6c7b8a99a8b7c6d5e4f30211203f4e5d6c7b8a9";

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

fn scratch_home() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "lightagent-desktop-profiles-{}-{nanos}",
        std::process::id()
    ))
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
    let handshake: serde_json::Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: lifeline,
        addr,
    }
}

/// One authenticated request; returns the status and the parsed JSON body.
fn call(
    runtime: &Runtime,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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
    let json = response
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// What `lightagent profile list --json` reports for `home`.
fn cli_profiles(home: &Path) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["profile", "list", "--json"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent profile list");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("profile list JSON")
}

#[test]
fn api_profile_operations_are_the_cli_profile_operations() {
    let home = scratch_home();
    let runtime = start(&home);

    let (status, body) = call(&runtime, "GET", "/api/lightagent/v1/runs", None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["runs"], serde_json::json!([]));

    let (status, body) = call(
        &runtime,
        "POST",
        "/api/lightagent/v1/profiles",
        Some(r#"{"id":"work","name":"Work","model":"m1"}"#),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["created"], "work");

    let (status, body) = call(&runtime, "GET", "/api/lightagent/v1/profiles/work", None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name"], "Work");

    let (status, body) = call(
        &runtime,
        "POST",
        "/api/lightagent/v1/profiles/work/preset",
        Some(r#"{"preset":"research@1"}"#),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["preset"], "research@1");

    let (status, body) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/profiles/work/active",
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["active"], "work");

    // The CLI sees exactly what the API did.
    let listed = cli_profiles(&home);
    assert_eq!(listed["active"], "work");
    assert!(
        listed["profiles"]
            .as_array()
            .expect("profiles")
            .iter()
            .any(|id| id == "work"),
        "{listed}"
    );

    // Stable error categories.
    let cases = [
        (
            "GET",
            "/api/lightagent/v1/profiles/missing",
            None,
            404,
            "not_found",
        ),
        (
            "POST",
            "/api/lightagent/v1/profiles",
            Some(r#"{"id":"work"}"#),
            409,
            "conflict",
        ),
        (
            "POST",
            "/api/lightagent/v1/profiles",
            Some(r#"{"id":"Bad Id!"}"#),
            400,
            "invalid",
        ),
        (
            "POST",
            "/api/lightagent/v1/profiles/work/preset",
            Some(r#"{"preset":"nope@9"}"#),
            400,
            "invalid",
        ),
        (
            "DELETE",
            "/api/lightagent/v1/profiles/default",
            None,
            403,
            "denied",
        ),
    ];
    for (method, path, body, want_status, want_category) in cases {
        let (status, json) = call(&runtime, method, path, body);
        assert_eq!(status, want_status, "{method} {path}: {json}");
        assert_eq!(
            json["error"]["category"], want_category,
            "{method} {path}: {json}"
        );
    }

    // Deleting the active profile hands over to `default`, as the CLI does.
    let (status, body) = call(&runtime, "DELETE", "/api/lightagent/v1/profiles/work", None);
    assert_eq!(status, 200, "{body}");
    let listed = cli_profiles(&home);
    assert_eq!(listed["active"], "default");
    assert!(
        !listed["profiles"]
            .as_array()
            .expect("profiles")
            .iter()
            .any(|id| id == "work"),
        "{listed}"
    );

    drop(runtime);
    let _ = std::fs::remove_dir_all(&home);
}
