//! The chat's `/help` and `/reload` on the desktop (D7). The commands the
//! desktop runtime lists are the ones the chat's `/help` prints, in its
//! order. A desktop run is built from the current settings: a web setting
//! the CLI changes while the runtime runs is offered to the model in the
//! next run, with no restart, and listed by the tool catalog. Every process
//! here is separate: the runtime, the provider, the CLI and this test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5";
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
        "lightagent-desktop-commands-{label}-{}-{nanos}",
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

fn cli_ok(home: &Path, args: &[&str]) {
    let output = cli(home, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `lightagent chat` with `lines` on its standard input, which is not a
/// terminal; what it printed. Two minutes at most.
fn chat(home: &Path, lines: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .arg("chat")
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start lightagent chat");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(lines.as_bytes())
        .expect("lines");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let output = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("lightagent chat did not finish")
        .expect("chat output");
    assert!(
        output.status.success(),
        "lightagent chat {lines:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
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

/// A provider that answers every turn "Done." and keeps each request body.
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
                    json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"Done."}}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                ]);
                let _ = stream.write_all(answer.as_bytes());
            });
        }
    });
    (url, bodies)
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

/// The tool names the provider was offered in its latest request.
fn offered(bodies: &Mutex<Vec<Value>>) -> Vec<String> {
    bodies
        .lock()
        .expect("bodies")
        .last()
        .and_then(|body| body["tools"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn web(names: &[String]) -> bool {
    names.iter().any(|name| name.to_lowercase().contains("web"))
}

fn setup(label: &str) -> (PathBuf, Runtime, Arc<Mutex<Vec<Value>>>) {
    let home = scratch_home(label);
    let (url, bodies) = provider();
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    let runtime = start(&home);
    (home, runtime, bodies)
}

#[test]
fn the_commands_listed_are_the_chats_help() {
    let (home, runtime, _bodies) = setup("help");
    let (status, raw) = call(&runtime, "GET", &format!("{API}/commands"), None);
    assert_eq!(status, 200, "{raw}");
    let listed = parsed(&raw);
    let names: Vec<String> = listed["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .map(|command| {
            assert!(
                command["summary"]
                    .as_str()
                    .is_some_and(|summary| !summary.is_empty()),
                "{command}"
            );
            format!("/{}", command["name"].as_str().expect("name"))
        })
        .collect();
    assert!(names.len() >= 10, "{listed}");
    let printed = chat(&home, "/help\n/exit\n");
    let line = printed
        .lines()
        .find(|line| line.starts_with("Commands:"))
        .unwrap_or_else(|| panic!("no /help line in {printed:?}"));
    let help: Vec<&str> = line.split_whitespace().skip(1).collect();
    assert_eq!(help, names, "{line}");
}

#[test]
fn the_next_desktop_run_uses_a_setting_the_cli_changed() {
    let (home, runtime, bodies) = setup("reload");
    cli_ok(&home, &["config", "set", "web.enabled", "false"]);
    turn(&runtime, "One.");
    let before = offered(&bodies);
    assert!(!web(&before), "{before:?}");

    // Changed by the CLI while the runtime runs: no restart, no reload.
    cli_ok(&home, &["config", "set", "web.enabled", "true"]);
    let (status, raw) = call(&runtime, "GET", &format!("{API}/tools"), None);
    assert_eq!(status, 200, "{raw}");
    let catalog: Vec<String> = parsed(&raw)["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    assert!(web(&catalog), "{catalog:?}");
    turn(&runtime, "Two.");
    let after = offered(&bodies);
    assert!(web(&after), "{after:?}");
}
