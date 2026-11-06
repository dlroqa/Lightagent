//! Profile guidance (D7, the chat's `/onboard`): the desktop runtime
//! installs, shows, replaces and removes the onboarding guidance that
//! `/onboard <file.md>` and `/onboard remove` manage in the terminal chat,
//! on the same files, and the next run carries it in its instructions.
//! Every process here is separate: the runtime, the provider, the CLI and
//! this test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
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
        "lightagent-desktop-onboarding-{label}-{}-{nanos}",
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
fn chat(home: &Path, lines: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .arg("chat")
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
            panic!("lightagent chat did not finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(status.success(), "lightagent chat {lines:?}: {stderr}");
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

/// Whether any request the provider received carried `text` in a message.
fn sent_with(bodies: &Mutex<Vec<Value>>, text: &str) -> bool {
    bodies.lock().expect("bodies").iter().any(|body| {
        body["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|message| match &message["content"] {
                Value::String(content) => content.contains(text),
                Value::Array(parts) => parts
                    .iter()
                    .any(|part| part["text"].as_str().is_some_and(|t| t.contains(text))),
                _ => false,
            })
        })
    })
}

fn turn(runtime: &Runtime, message: &str) {
    let (status, raw) = call(runtime, "POST", &format!("{API}/sessions"), Some("{}"));
    assert_eq!(status, 201, "{raw}");
    let session = parsed(&raw)["id"].as_str().expect("session id").to_owned();
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

fn shown(runtime: &Runtime) -> Value {
    let (status, raw) = call(runtime, "GET", &format!("{API}/onboarding"), None);
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)
}

fn install(runtime: &Runtime, path: &str) -> (u16, String) {
    let body = json!({ "path": path }).to_string();
    call(runtime, "PUT", &format!("{API}/onboarding"), Some(&body))
}

#[test]
fn guidance_is_one_bundle_for_the_chat_and_the_desktop() {
    let home = scratch_home("guidance");
    let (url, bodies) = provider();
    let output = cli(&home, &["config", "set", "inference.base_url", &url]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = start(&home);
    assert_eq!(shown(&runtime)["installed"], false);
    assert_eq!(shown(&runtime)["text"], Value::Null);

    // Refused, and nothing installed: a relative path, a missing file.
    let missing = home.join("missing.md");
    for path in ["guide.md", missing.to_str().expect("path")] {
        let (status, raw) = install(&runtime, path);
        assert_eq!(status, 400, "{path}: {raw}");
    }
    assert_eq!(shown(&runtime)["installed"], false);

    // Installed over the API: shown, turned on, and in the next run.
    let first = home.join("first.md");
    std::fs::write(&first, "Answer as a heron would.").expect("first");
    let (status, raw) = install(&runtime, first.to_str().expect("path"));
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["installed"], true, "{raw}");
    assert_eq!(parsed(&raw)["text"], "Answer as a heron would.", "{raw}");
    let config = cli_json(&home, &["config", "show"]);
    assert_eq!(config["extensions"]["enabled"], true, "{config}");
    turn(&runtime, "Who are you?");
    assert!(
        sent_with(&bodies, "Answer as a heron would."),
        "the next run carries the guidance"
    );

    // The chat's `/onboard remove` removes what the desktop installed, and
    // what its `/onboard <file>` installs is what the desktop shows.
    chat(&home, "/onboard remove\n/exit\n");
    assert_eq!(shown(&runtime)["installed"], false);
    let second = home.join("second.md");
    std::fs::write(&second, "Answer as an egret would.").expect("second");
    chat(&home, &format!("/onboard {}\n/exit\n", second.display()));
    assert_eq!(shown(&runtime)["text"], "Answer as an egret would.");

    // Replaced, then removed over the API; removing nothing is refused.
    let (status, raw) = install(&runtime, first.to_str().expect("path"));
    assert_eq!(status, 200, "{raw}");
    assert_eq!(shown(&runtime)["text"], "Answer as a heron would.");
    let (status, raw) = call(&runtime, "DELETE", &format!("{API}/onboarding"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["installed"], false, "{raw}");
    let (status, raw) = call(&runtime, "DELETE", &format!("{API}/onboarding"), None);
    assert_eq!(status, 404, "{raw}");
    assert!(first.exists(), "the source file is left where it is");
}
