//! Sessions through one store (D7-5): `lightagent sessions list|show|delete`
//! and the desktop runtime's session routes act on the active profile's
//! saved conversations alike. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const SESSIONS: &str = "/api/lightagent/v1/sessions";

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
        "lightagent-desktop-sessions-{label}-{}-{nanos}",
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

fn cli_output(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .arg("--json")
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let output = cli_output(home, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn create(runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "POST", SESSIONS, Some("{}"));
    assert_eq!(status, 201, "{raw}");
    parsed(&raw)["id"].as_str().expect("id").to_owned()
}

#[test]
fn the_cli_and_the_api_keep_one_set_of_sessions() {
    let home = scratch_home("one");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);
    let first = create(&runtime);
    let second = create(&runtime);

    // The same list, and the same saved session.
    let (status, raw) = call(&runtime, "GET", SESSIONS, None);
    assert_eq!(status, 200, "{raw}");
    let listed = parsed(&raw)["sessions"].clone();
    assert_eq!(listed, cli_json(&home, &["sessions", "list"]));
    assert_eq!(listed.as_array().map(Vec::len), Some(2), "{listed}");
    let (status, raw) = call(&runtime, "GET", &format!("{SESSIONS}/{first}"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw), cli_json(&home, &["sessions", "show", &first]));

    // Deleted by the CLI: gone for the API.
    cli_json(&home, &["sessions", "delete", &first]);
    assert_eq!(
        call(&runtime, "GET", &format!("{SESSIONS}/{first}"), None).0,
        404
    );

    // Deleted over the API: the CLI cannot show it, and lists nothing.
    let (status, raw) = call(&runtime, "DELETE", &format!("{SESSIONS}/{second}"), None);
    assert!(status == 200 || status == 204, "{status}: {raw}");
    assert!(
        !cli_output(&home, &["sessions", "show", &second])
            .status
            .success()
    );
    assert_eq!(
        cli_json(&home, &["sessions", "list"])
            .as_array()
            .map(Vec::len),
        Some(0)
    );
}
