//! Conversations through one store (D7): a conversation the desktop
//! runtime holds continues in `lightagent chat --session` with its earlier
//! turn, and one started in `lightagent chat` continues over the runtime's
//! API. Search finds a conversation by what was said and by a title set over
//! the API, which `lightagent sessions show|list` read too. An attachment is
//! saved byte for byte, owner-only, beside the profile's sessions; a name
//! that is not plain is refused. Every process here is separate: the
//! runtime, the provider, the CLI and this test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"}]}"#;
const API: &str = "/api/lightagent/v1";

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
        "lightagent-desktop-conversations-{label}-{}-{nanos}",
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

fn connect(runtime: &Runtime) -> TcpStream {
    let stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    stream
}

/// The status and raw body of the response on `stream`.
fn response(mut stream: TcpStream) -> (u16, String) {
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

/// One authenticated JSON request.
fn call(runtime: &Runtime, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = connect(runtime);
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        runtime.addr,
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("send");
    response(stream)
}

/// An attachment as the WebUI uploads one: raw bytes, named by a header.
fn upload(runtime: &Runtime, session: &str, filename: &str, bytes: &[u8]) -> (u16, String) {
    let mut stream = connect(runtime);
    let head = format!(
        "POST {API}/sessions/{session}/attachments HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {KEY}\r\nX-Lightagent-Filename: {filename}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        runtime.addr,
        bytes.len()
    );
    stream.write_all(head.as_bytes()).expect("send");
    stream.write_all(bytes).expect("body");
    response(stream)
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

fn setup(label: &str) -> (PathBuf, Runtime, Arc<Mutex<Vec<Value>>>) {
    let home = scratch_home(label);
    let (url, bodies) = provider();
    let output = cli(&home, &["config", "set", "inference.base_url", &url]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = start(&home);
    (home, runtime, bodies)
}

fn new_session(runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "POST", &format!("{API}/sessions"), Some("{}"));
    assert_eq!(status, 201, "{raw}");
    parsed(&raw)["id"].as_str().expect("session id").to_owned()
}

/// One turn in `session` over the API, waited for until it completes.
fn turn(runtime: &Runtime, session: &str, message: &str) {
    let body = json!({ "message": message, "session_id": session }).to_string();
    let (status, raw) = call(runtime, "POST", &format!("{API}/runs"), Some(&body));
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

fn session(runtime: &Runtime, id: &str) -> Value {
    let (status, raw) = call(runtime, "GET", &format!("{API}/sessions/{id}"), None);
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)
}

fn search(runtime: &Runtime, query: &str) -> Vec<Value> {
    let (status, raw) = call(
        runtime,
        "GET",
        &format!("{API}/sessions/search?q={query}"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn ids(listed: &Value) -> Vec<String> {
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_conversation_continues_between_the_desktop_and_lightagent_chat() {
    let (home, runtime, bodies) = setup("continue");
    let id = new_session(&runtime);
    turn(&runtime, &id, "The heron nests by the quarry.");

    // `chat --session` resumes the desktop's conversation with its turn.
    chat(&home, &["--session", &id], "And the egret?\n/exit\n");
    let saved = session(&runtime, &id);
    assert_eq!(
        said_by(&saved, "user"),
        ["The heron nests by the quarry.", "And the egret?"],
        "{saved}"
    );
    assert_eq!(
        said_by(&saved, "assistant"),
        ["Noted.", "Noted."],
        "{saved}"
    );
    assert!(
        carried(&bodies, "And the egret?", "The heron nests by the quarry."),
        "the CLI's turn carried the desktop's"
    );

    // A conversation started in `chat` continues over the API.
    let before = ids(&cli_json(&home, &["sessions", "list"]));
    chat(&home, &[], "Started in the terminal.\n/exit\n");
    let started: Vec<String> = ids(&cli_json(&home, &["sessions", "list"]))
        .into_iter()
        .filter(|listed| !before.contains(listed))
        .collect();
    assert_eq!(started.len(), 1, "{started:?}");
    turn(&runtime, &started[0], "Continued on the desktop.");
    let saved = session(&runtime, &started[0]);
    assert_eq!(
        said_by(&saved, "user"),
        ["Started in the terminal.", "Continued on the desktop."],
        "{saved}"
    );
    assert!(
        carried(
            &bodies,
            "Continued on the desktop.",
            "Started in the terminal."
        ),
        "the desktop's turn carried the CLI's"
    );
}

#[test]
fn search_rename_and_attachments_share_the_clis_store() {
    let (home, runtime, _bodies) = setup("store");
    let id = new_session(&runtime);
    turn(&runtime, &id, "The heron nests by the quarry.");

    // Search: by what was said, in any case; nothing for what was not.
    let found = search(&runtime, "QUARRY");
    let row = found
        .iter()
        .find(|row| row["id"] == id.as_str())
        .unwrap_or_else(|| panic!("not found: {found:?}"));
    assert_eq!(row["matched_role"], "user", "{row}");
    assert!(
        row["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.to_lowercase().contains("quarry")),
        "{row}"
    );
    assert!(search(&runtime, "absentword").is_empty());

    // Rename: trimmed, read by the CLI, found by search; an empty title is
    // refused and changes nothing.
    let path = format!("{API}/sessions/{id}");
    let (status, raw) = call(
        &runtime,
        "PATCH",
        &path,
        Some(r#"{"title":"  Heronry log  "}"#),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["title"], "Heronry log", "{raw}");
    let shown = cli_json(&home, &["sessions", "show", &id]);
    assert_eq!(shown["title"], "Heronry log", "{shown}");
    let listed = cli_json(&home, &["sessions", "list"]);
    let row = listed
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id.as_str()))
        .unwrap_or_else(|| panic!("not listed: {listed}"));
    assert_eq!(row["title"], "Heronry log", "{row}");
    assert!(
        search(&runtime, "HERONRY")
            .iter()
            .any(|row| row["id"] == id.as_str()),
        "search finds the new title"
    );
    let (status, raw) = call(&runtime, "PATCH", &path, Some(r#"{"title":"   "}"#));
    assert_eq!(status, 400, "{raw}");
    assert_eq!(
        cli_json(&home, &["sessions", "show", &id])["title"],
        "Heronry log"
    );

    // An attachment: saved byte for byte, owner-only, beside the profile's
    // sessions. A name that is not plain is refused.
    let bytes = b"field notes\n\x00\xffheron";
    let (status, raw) = upload(&runtime, &id, "notes 1.txt", bytes);
    assert_eq!(status, 200, "{raw}");
    let saved = parsed(&raw);
    assert_eq!(saved["name"], "notes 1.txt", "{saved}");
    let file = PathBuf::from(saved["path"].as_str().expect("path"));
    assert_eq!(std::fs::read(&file).expect("saved"), bytes);
    assert!(
        file.parent()
            .is_some_and(|dir| dir.ends_with(Path::new("attachments").join(&id))),
        "{}",
        file.display()
    );
    assert!(
        file.canonicalize()
            .expect("canonical")
            .starts_with(home.canonicalize().expect("canonical home")),
        "{}",
        file.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
    for name in ["../escape.txt", "", "a/b.txt"] {
        let (status, raw) = upload(&runtime, &id, name, b"x");
        assert_eq!(status, 400, "{name:?}: {raw}");
    }
    assert!(!file.with_file_name("b.txt").exists());
}
