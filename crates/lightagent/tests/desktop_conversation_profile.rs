//! A conversation's own profile (D7, `lightagent chat --profile`): the
//! desktop runtime runs a conversation as the profile it was created for,
//! as `chat --profile <p>` runs a chat, and the active profile stays as it
//! was. The conversation is kept in that profile's own store; its runs are
//! built from that profile (its persona and model here); `chat --profile
//! <p> --session <id>` continues it and the desktop continues after. A
//! conversation is found only under its own profile, and a profile that
//! does not exist, or is not a valid id, finds and creates nothing. Every
//! process here is separate: the runtime, the provider, the CLI and this
//! test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"},{"id":"research-model"}]}"#;
const API: &str = "/api/lightagent/v1";
const PERSONA: &str = "You answer as the research desk, citing sources.";

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
        "lightagent-desktop-conversation-profile-{label}-{}-{nanos}",
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

/// One authenticated JSON request: the status and the raw body.
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
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("{error}: {raw}"))
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
        "lightagent {all:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI JSON")
}

/// `lightagent chat` with `lines` on its standard input, which is not a
/// terminal. A chat still running after two minutes fails the test.
fn chat(home: &Path, args: &[&str], lines: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .arg("chat")
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start lightagent chat");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(lines.as_bytes())
        .expect("lines");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("chat status") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("lightagent chat {args:?} did not finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(status.success(), "lightagent chat {args:?}: {stderr}");
}

/// Read one request; return its head and body.
fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                let body = String::from_utf8_lossy(&bytes[end + 4..end + 4 + length]).into_owned();
                return Some((head, body));
            }
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
        }
    }
}

fn sse(chunks: &[Value]) -> String {
    let mut response =
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
            .to_owned();
    for chunk in chunks {
        response.push_str(&format!("data: {chunk}\n\n"));
    }
    response.push_str("data: [DONE]\n\n");
    response
}

/// A provider that answers every turn "Noted." and keeps each request body.
fn provider() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let kept = Arc::clone(&kept);
            std::thread::spawn(move || {
                let Some((head, body)) = read_request(&mut stream) else {
                    return;
                };
                if head.starts_with("GET") {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{MODELS}",
                        MODELS.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    return;
                }
                kept.lock()
                    .expect("bodies")
                    .push(serde_json::from_str(&body).unwrap_or(Value::Null));
                let answer = sse(&[
                    json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"Noted."}}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                ]);
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    (url, bodies)
}

/// A message's text, whether a string or a list of parts.
fn text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        _ => String::new(),
    }
}

/// The system text and the model of the latest request whose newest user
/// turn says `said`.
fn sent_for(bodies: &Mutex<Vec<Value>>, said: &str) -> (String, String) {
    let bodies = bodies.lock().expect("bodies");
    let body = bodies
        .iter()
        .rev()
        .find(|body| {
            body["messages"]
                .as_array()
                .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
                .is_some_and(|message| text(message).contains(said))
        })
        .unwrap_or_else(|| panic!("no request carried {said:?}"));
    let system = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "system" || message["role"] == "developer")
        .map(text)
        .collect::<Vec<_>>()
        .join("\n");
    (
        system,
        body["model"].as_str().unwrap_or_default().to_owned(),
    )
}

/// Whether the provider was sent `latest` as the newest user turn, after
/// `earlier` and an answer.
fn carried(bodies: &Mutex<Vec<Value>>, latest: &str, earlier: &str) -> bool {
    bodies.lock().expect("bodies").iter().any(|body| {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let Some(last) = messages
            .iter()
            .rposition(|message| message["role"] == "user")
        else {
            return false;
        };
        let before = &messages[..last];
        text(&messages[last]).contains(latest)
            && before
                .iter()
                .any(|message| message["role"] == "user" && text(message).contains(earlier))
            && before.iter().any(|message| message["role"] == "assistant")
    })
}

/// What `role` said in a saved session, in order.
fn said_by(session: &Value, role: &str) -> Vec<String> {
    session["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == role)
        .map(text)
        .collect()
}

fn ids(listed: &Value) -> Vec<String> {
    listed["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str())
        .map(str::to_owned)
        .collect()
}

/// `path` for the conversations of `profile`.
fn scoped(path: &str, profile: &str) -> String {
    format!("{path}?profile={profile}")
}

/// A new conversation, for `profile` when one is named.
fn new_session(runtime: &Runtime, profile: Option<&str>) -> String {
    let path = match profile {
        Some(profile) => scoped(&format!("{API}/sessions"), profile),
        None => format!("{API}/sessions"),
    };
    let (status, raw) = call(runtime, "POST", &path, Some("{}"));
    assert_eq!(status, 201, "{raw}");
    parsed(&raw)["id"].as_str().expect("session id").to_owned()
}

/// One turn over the API in `session`, run as `profile` when one is named,
/// waited for until it completes.
fn turn(runtime: &Runtime, session: &str, profile: Option<&str>, message: &str) {
    let mut body = json!({ "message": message, "session_id": session });
    if let Some(profile) = profile {
        body["profile"] = json!(profile);
    }
    let (status, raw) = call(
        runtime,
        "POST",
        &format!("{API}/runs"),
        Some(&body.to_string()),
    );
    assert_eq!(status, 202, "{raw}");
    let run = parsed(&raw)["id"].as_str().expect("run id").to_owned();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, raw) = call(runtime, "GET", &format!("{API}/runs/{run}"), None);
        assert_eq!(status, 200, "{raw}");
        let state = parsed(&raw)["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if state == "completed" {
            return;
        }
        assert!(
            !matches!(state.as_str(), "failed" | "cancelled"),
            "the turn ended {state}: {raw}"
        );
        assert!(
            Instant::now() < deadline,
            "the turn did not complete: {raw}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn setup(label: &str) -> (PathBuf, Runtime, Arc<Mutex<Vec<Value>>>) {
    let home = scratch_home(label);
    let (url, bodies) = provider();
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    cli_ok(&home, &["config", "set", "inference.model", "m1"]);
    cli_ok(
        &home,
        &[
            "profile",
            "create",
            "research",
            "--persona",
            PERSONA,
            "--model",
            "research-model",
        ],
    );
    let runtime = start(&home);
    (home, runtime, bodies)
}

/// The active profile, as the runtime and the CLI report it (they agree).
fn active(home: &Path, runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "GET", &format!("{API}/profiles"), None);
    assert_eq!(status, 200, "{raw}");
    let served = parsed(&raw)["active_profile"].clone();
    assert_eq!(served, cli_json(home, &["profile", "list"])["active"]);
    served.as_str().expect("an active profile").to_owned()
}

#[test]
fn a_conversation_runs_as_its_own_profile_and_the_active_profile_stays() {
    let (home, runtime, bodies) = setup("own");
    let default = active(&home, &runtime);
    assert_ne!(default, "research");

    // Created for research: listed under research only, bound to it.
    let id = new_session(&runtime, Some("research"));
    let (_, raw) = call(
        &runtime,
        "GET",
        &scoped(&format!("{API}/sessions"), "research"),
        None,
    );
    assert!(ids(&parsed(&raw)).contains(&id), "{raw}");
    let (_, raw) = call(&runtime, "GET", &format!("{API}/sessions"), None);
    assert!(
        !ids(&parsed(&raw)).contains(&id),
        "listed under the active profile: {raw}"
    );

    // Its run is built from research: its persona and its model.
    turn(&runtime, &id, Some("research"), "Where do herons nest?");
    let (system, model) = sent_for(&bodies, "Where do herons nest?");
    assert!(system.contains(PERSONA), "{system}");
    assert_eq!(model, "research-model");
    let (status, raw) = call(
        &runtime,
        "GET",
        &scoped(&format!("{API}/sessions/{id}"), "research"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    let saved = parsed(&raw);
    assert_eq!(saved["profile"], "research", "{saved}");
    assert_eq!(
        said_by(&saved, "user"),
        ["Where do herons nest?"],
        "{saved}"
    );

    // `chat --profile research` sends what the desktop sent for research.
    chat(&home, &["--profile", "research"], "And egrets?\n/exit\n");
    let (by_cli, cli_model) = sent_for(&bodies, "And egrets?");
    assert!(by_cli.contains(PERSONA), "{by_cli}");
    assert_eq!(cli_model, model);

    // A conversation of the active profile is not research's.
    let plain = new_session(&runtime, None);
    turn(&runtime, &plain, None, "A plain question.");
    let (system, model) = sent_for(&bodies, "A plain question.");
    assert!(!system.contains(PERSONA), "{system}");
    assert_ne!(model, "research-model");

    // `chat --profile research --session <id>` continues the conversation,
    // and the desktop continues after it.
    chat(
        &home,
        &["--profile", "research", "--session", &id],
        "Which ones winter here?\n/exit\n",
    );
    assert!(
        carried(&bodies, "Which ones winter here?", "Where do herons nest?"),
        "the CLI's turn carried the desktop's"
    );
    turn(&runtime, &id, Some("research"), "Thanks.");
    assert!(
        carried(&bodies, "Thanks.", "Which ones winter here?"),
        "the desktop's turn carried the CLI's"
    );
    let (_, raw) = call(
        &runtime,
        "GET",
        &scoped(&format!("{API}/sessions/{id}"), "research"),
        None,
    );
    assert_eq!(
        said_by(&parsed(&raw), "user"),
        [
            "Where do herons nest?",
            "Which ones winter here?",
            "Thanks."
        ],
        "{raw}"
    );

    // The active profile is what it was, for the runtime and the CLI.
    assert_eq!(active(&home, &runtime), default);
}

#[test]
fn a_conversation_is_found_only_under_its_own_profile() {
    let (_home, runtime, bodies) = setup("found");
    let id = new_session(&runtime, Some("research"));
    let session = format!("{API}/sessions/{id}");

    // Under the active profile it is not there: not read, not renamed, not
    // deleted, not run.
    let (status, raw) = call(&runtime, "GET", &session, None);
    assert_eq!(status, 404, "{raw}");
    let (status, raw) = call(
        &runtime,
        "PATCH",
        &session,
        Some(r#"{"title":"Elsewhere"}"#),
    );
    assert!(
        status >= 400,
        "renamed from the active profile: {status} {raw}"
    );
    let (status, raw) = call(&runtime, "DELETE", &session, None);
    assert!(
        status >= 400 || parsed(&raw)["deleted"] == false,
        "deleted from the active profile: {status} {raw}"
    );
    let body = json!({ "message": "Run me elsewhere.", "session_id": id }).to_string();
    let (status, raw) = call(&runtime, "POST", &format!("{API}/runs"), Some(&body));
    assert!(status >= 400, "run from the active profile: {status} {raw}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !bodies
            .lock()
            .expect("bodies")
            .iter()
            .any(|body| body.to_string().contains("Run me elsewhere.")),
        "the refused run reached the model"
    );
    let (status, raw) = call(&runtime, "GET", &scoped(&session, "research"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["title"], "agent session", "{raw}");

    // A profile that does not exist, or is not a valid id, finds and makes
    // nothing.
    for path in [
        scoped(&format!("{API}/sessions"), "nobody"),
        scoped(&session, "nobody"),
    ] {
        let (status, raw) = call(&runtime, "GET", &path, None);
        assert_eq!(status, 404, "{path}: {raw}");
        assert!(raw.contains("no such profile"), "{raw}");
    }
    let (status, raw) = call(
        &runtime,
        "POST",
        &scoped(&format!("{API}/sessions"), "nobody"),
        Some("{}"),
    );
    assert_eq!(status, 404, "{raw}");
    let (status, raw) = call(
        &runtime,
        "POST",
        &scoped(&format!("{API}/sessions"), "..%2Fescape"),
        Some("{}"),
    );
    assert_eq!(status, 400, "{raw}");
    let body = json!({ "message": "Nobody's.", "session_id": id, "profile": "nobody" }).to_string();
    let (status, raw) = call(&runtime, "POST", &format!("{API}/runs"), Some(&body));
    assert_eq!(status, 404, "{raw}");
}
