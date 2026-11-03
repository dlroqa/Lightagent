//! Memory through one shared operation set (D7-1): the desktop runtime's
//! API and `lightagent memory` act on the active profile's memories alike.
//! What one writes the other lists, and the API answers each read with the
//! JSON the CLI prints. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const MEMORIES: &str = "/api/lightagent/v1/memories";

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
        "lightagent-desktop-memory-{label}-{}-{nanos}",
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
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli_output(home, &all);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn get(runtime: &Runtime, path: &str) -> Value {
    let (status, raw) = call(runtime, "GET", path, None);
    assert_eq!(status, 200, "{path}: {raw}");
    parsed(&raw)
}

#[test]
fn the_cli_and_the_api_keep_one_memory() {
    let home = scratch_home("one");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);

    // Written over the API: the CLI lists it, and the API lists the same.
    let (status, raw) = call(
        &runtime,
        "POST",
        MEMORIES,
        Some(r#"{"text":"Prefers dark mode","kind":"preference","tags":["ui"]}"#),
    );
    assert_eq!(status, 200, "{raw}");
    let id = parsed(&raw)["id"].as_str().expect("id").to_owned();
    let by_cli = cli_json(&home, &["memory", "list"]);
    assert_eq!(get(&runtime, MEMORIES), by_cli);
    assert_eq!(by_cli["memories"][0]["id"], id.as_str(), "{by_cli}");
    assert_eq!(by_cli["memories"][0]["kind"], "preference");
    assert_eq!(by_cli["memories"][0]["tags"], json!(["ui"]));

    // Written by the CLI: the API lists it.
    cli_json(&home, &["memory", "add", "Uses Rust for the backend"]);
    assert_eq!(
        get(&runtime, MEMORIES)["memories"].as_array().map(Vec::len),
        Some(2)
    );

    // Search and reflect: the API answers what the CLI prints.
    assert_eq!(
        get(
            &runtime,
            &format!("{MEMORIES}/search?q=dark%20mode&top_k=3")
        ),
        cli_json(&home, &["memory", "search", "dark mode", "--top-k", "3"])
    );
    assert_eq!(
        get(&runtime, &format!("{MEMORIES}/reflect")),
        cli_json(&home, &["memory", "reflect"])
    );

    // Updated over the API, forgotten by the CLI.
    let (status, raw) = call(
        &runtime,
        "PUT",
        &format!("{MEMORIES}/items/{id}"),
        Some(r#"{"text":"Prefers dark mode everywhere"}"#),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["updated"], true, "{raw}");
    assert!(
        cli_json(&home, &["memory", "list"])
            .to_string()
            .contains("Prefers dark mode everywhere")
    );
    let (_, raw) = call(
        &runtime,
        "PUT",
        &format!("{MEMORIES}/items/no-such-memory"),
        Some(r#"{"text":"x"}"#),
    );
    assert_eq!(parsed(&raw)["updated"], false, "{raw}");
    assert_eq!(
        call(&runtime, "POST", MEMORIES, Some(r#"{"text":"   "}"#)).0,
        400,
        "an empty memory is refused"
    );
    assert_eq!(
        call(
            &runtime,
            "PUT",
            &format!("{MEMORIES}/items/{id}"),
            Some(r#"{"text":""}"#)
        )
        .0,
        400
    );
    assert_eq!(cli_json(&home, &["memory", "forget", &id])["removed"], true);
    assert!(!get(&runtime, MEMORIES).to_string().contains(&id));

    // A saved session: the same candidates; promoting a message it lacks is
    // refused by both.
    let (status, raw) = call(&runtime, "POST", "/api/lightagent/v1/sessions", Some("{}"));
    assert_eq!(status, 201, "{raw}");
    let session = parsed(&raw)["id"].as_str().expect("session id").to_owned();
    assert_eq!(
        get(
            &runtime,
            &format!("{MEMORIES}/candidates?session={session}")
        ),
        cli_json(&home, &["memory", "candidates", &session])
    );
    let body = json!({ "session": session, "message": 1 }).to_string();
    assert_eq!(
        call(
            &runtime,
            "POST",
            &format!("{MEMORIES}/promote"),
            Some(&body)
        )
        .0,
        400
    );
    assert!(
        !cli_output(&home, &["memory", "promote", &session, "1"])
            .status
            .success()
    );
    let (status, raw) = call(
        &runtime,
        "DELETE",
        &format!("/api/lightagent/v1/sessions/{session}"),
        None,
    );
    assert!(status == 200 || status == 204, "{status}: {raw}");
    assert_eq!(
        call(
            &runtime,
            "GET",
            &format!("{MEMORIES}/candidates?session={session}"),
            None
        )
        .0,
        404,
        "a session that is gone"
    );

    // Cleared over the API: the CLI lists nothing.
    let (status, raw) = call(&runtime, "DELETE", MEMORIES, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(cli_json(&home, &["memory", "list"])["memories"], json!([]));
}
