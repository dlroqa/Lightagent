//! A turn interrupted by the death of the runtime running it (D5a): when a
//! runtime starts again on the same home, the session records that turn as
//! `Interrupted` and the turn is never run again (the provider gets no
//! second request for it), while the session takes the next turn. An
//! interrupted regeneration leaves the saved answer as it was. Every process
//! here is separate: the runtimes, the provider and this test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"}]}"#;
const ANSWER: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
    data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"First.\"}}]}\n\n\
    data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
    data: [DONE]\n\n";

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
        "lightagent-desktop-interrupted-{label}-{}-{nanos}",
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
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Read one request, its body included; return its head.
fn read_request(stream: &mut TcpStream) -> Option<String> {
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
                return Some(head);
            }
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return None,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
        }
    }
}

/// A provider that answers the first `answered` chat requests with "First."
/// and holds every later one open, counting them all.
fn provider(answered: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let chats = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&chats);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let counter = Arc::clone(&counter);
            std::thread::spawn(move || {
                let Some(head) = read_request(&mut stream) else {
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
                if counter.fetch_add(1, Ordering::SeqCst) < answered {
                    let _ = stream.write_all(ANSWER.as_bytes());
                } else {
                    std::thread::sleep(Duration::from_secs(300));
                }
            });
        }
    });
    (url, chats)
}

fn wait_until(what: &str, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !probe() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn new_session(runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "POST", "/api/lightagent/v1/sessions", Some("{}"));
    assert_eq!(status, 201, "{raw}");
    let created: Value = serde_json::from_str(&raw).expect("session JSON");
    created["id"].as_str().expect("session id").to_owned()
}

fn session(runtime: &Runtime, id: &str) -> Value {
    let (status, raw) = call(
        runtime,
        "GET",
        &format!("/api/lightagent/v1/sessions/{id}"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    serde_json::from_str(&raw).expect("session JSON")
}

fn run_on(runtime: &Runtime, id: &str, message: &str) -> (u16, String) {
    let body = json!({ "message": message, "session_id": id }).to_string();
    call(runtime, "POST", "/api/lightagent/v1/runs", Some(&body))
}

fn regenerate(runtime: &Runtime, id: &str, answer: &str) -> (u16, String) {
    let body = json!({ "run_id": answer }).to_string();
    call(
        runtime,
        "POST",
        &format!("/api/lightagent/v1/sessions/{id}/regenerate"),
        Some(&body),
    )
}

#[test]
fn a_turn_interrupted_by_a_crash_is_recorded_and_never_run_again() {
    let home = scratch_home("append");
    let (url, chats) = provider(0);
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    let runtime = start(&home);
    let id = new_session(&runtime);
    let (status, raw) = run_on(&runtime, &id, "hello");
    assert_eq!(status, 202, "{raw}");
    wait_until("the turn to reach the provider", || {
        chats.load(Ordering::SeqCst) == 1
    });
    // Killed mid-turn: nothing records the turn.
    drop(runtime);

    let runtime = start(&home);
    let saved = session(&runtime, &id);
    let runs = saved["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "{saved}");
    assert_eq!(runs[0]["stop_reason"], "Interrupted", "{saved}");
    assert_eq!(
        saved["messages"].as_array().map(Vec::len),
        Some(1),
        "the prompt only, no answer: {saved}"
    );
    assert_eq!(
        chats.load(Ordering::SeqCst),
        1,
        "the interrupted turn was sent again"
    );

    // The session takes the next turn, and the interruption stays recorded once.
    let (status, raw) = run_on(&runtime, &id, "again");
    assert_eq!(status, 202, "{raw}");
    wait_until("the next turn to reach the provider", || {
        chats.load(Ordering::SeqCst) == 2
    });
    let saved = session(&runtime, &id);
    assert_eq!(saved["runs"].as_array().map(Vec::len), Some(1), "{saved}");
}

#[test]
fn a_second_runtime_leaves_a_live_turn_alone() {
    let home = scratch_home("live");
    let (url, chats) = provider(0);
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    let first = start(&home);
    let id = new_session(&first);
    let (status, raw) = run_on(&first, &id, "hello");
    assert_eq!(status, 202, "{raw}");
    wait_until("the turn to reach the provider", || {
        chats.load(Ordering::SeqCst) == 1
    });
    // Another runtime starts on the same home while that turn runs: its
    // process still holds the session's lease, so the turn is not
    // interrupted, at startup or when the session is asked for a turn.
    let second = start(&home);
    let saved = session(&second, &id);
    assert_eq!(saved["runs"].as_array().map(Vec::len), Some(0), "{saved}");
    let (status, raw) = run_on(&second, &id, "again");
    assert_eq!(status, 409, "the live turn still holds the session: {raw}");
    let saved = session(&second, &id);
    assert_eq!(saved["runs"].as_array().map(Vec::len), Some(0), "{saved}");
    // Once its process is gone, the next turn records it, and only the new
    // turn is sent.
    drop(first);
    let (status, raw) = run_on(&second, &id, "again");
    assert_eq!(status, 202, "{raw}");
    let saved = session(&second, &id);
    let runs = saved["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 1, "{saved}");
    assert_eq!(runs[0]["stop_reason"], "Interrupted", "{saved}");
    wait_until("the new turn to reach the provider", || {
        chats.load(Ordering::SeqCst) == 2
    });
}

#[test]
fn an_interrupted_regeneration_keeps_the_saved_answer() {
    let home = scratch_home("regenerate");
    let (url, chats) = provider(1);
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    let runtime = start(&home);
    let id = new_session(&runtime);
    let (status, raw) = run_on(&runtime, &id, "hello");
    assert_eq!(status, 202, "{raw}");
    wait_until("the answer to be saved", || {
        session(&runtime, &id)["runs"]
            .as_array()
            .is_some_and(|runs| runs.len() == 1)
    });
    let saved = session(&runtime, &id);
    let answer = saved["runs"][0]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    // The session is free once the recorded run has released it.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (status, raw) = loop {
        let (status, raw) = regenerate(&runtime, &id, &answer);
        if status != 409 || !raw.contains("active run") || Instant::now() >= deadline {
            break (status, raw);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status, 202, "{raw}");
    wait_until("the regeneration to reach the provider", || {
        chats.load(Ordering::SeqCst) == 2
    });
    drop(runtime);

    let runtime = start(&home);
    let after = session(&runtime, &id);
    assert_eq!(
        after["messages"], saved["messages"],
        "the saved answer is kept"
    );
    assert_eq!(after["runs"], saved["runs"], "no record added or replaced");
    assert_eq!(
        chats.load(Ordering::SeqCst),
        2,
        "the regeneration was sent again"
    );
    // Its journal is gone: the answer can be regenerated again.
    let (status, raw) = regenerate(&runtime, &id, &answer);
    assert_eq!(status, 202, "{raw}");
}
