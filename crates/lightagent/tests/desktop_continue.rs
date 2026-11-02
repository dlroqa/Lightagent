//! A run paused on its time budget continues over the API (D5a-3): a run
//! started with `on_time_budget: "pause"` pauses when its budget runs out
//! with a tool result the model has not read, sends nothing while paused,
//! and `continue` resumes the same execution: the next model turn carries
//! that unread tool result. A paused run can be cancelled, which frees the
//! session, and a run that did not ask to pause wraps up as before. Every
//! process here is separate: the runtime, the provider and this test.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"}]}"#;

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
        "lightagent-desktop-continue-{label}-{}-{nanos}",
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

/// A provider that, until the conversation holds a tool result, asks for
/// `datetime.now` and takes longer than the run's one-second time budget;
/// once it does, it answers "Found it.". Every request body is kept.
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
                let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                kept.lock().expect("bodies").push(request.clone());
                let has_result = request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| message["role"] == "tool")
                });
                if has_result {
                    let answer = sse(&[
                        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"Found it."}}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    ]);
                    let _ = stream.write_all(answer.as_bytes());
                    return;
                }
                let tool = request["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|tool| tool["function"]["name"].as_str())
                    .find(|name| {
                        name.chars()
                            .filter(char::is_ascii_alphanumeric)
                            .collect::<String>()
                            .eq_ignore_ascii_case("datetimenow")
                    })
                    .unwrap_or("datetime.now")
                    .to_owned();
                std::thread::sleep(Duration::from_millis(1500));
                let call = sse(&[
                    json!({"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":tool,"arguments":"{}"}}]}}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                ]);
                let _ = stream.write_all(call.as_bytes());
            });
        }
    });
    (url, bodies)
}

fn wait_until(what: &str, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !probe() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A runtime whose runs have one second and the tools enabled.
fn setup(label: &str) -> (Runtime, Arc<Mutex<Vec<Value>>>) {
    let home = scratch_home(label);
    let (url, bodies) = provider();
    cli_ok(&home, &["config", "set", "inference.base_url", &url]);
    cli_ok(&home, &["config", "set", "agent.wall_clock_secs", "1"]);
    let path = home.join("config.json");
    let mut config: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("config")).expect("JSON");
    config["tools"]["enabled"] = json!(true);
    std::fs::write(&path, serde_json::to_string_pretty(&config).expect("JSON")).expect("write");
    (start(&home), bodies)
}

fn new_session(runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "POST", "/api/lightagent/v1/sessions", Some("{}"));
    assert_eq!(status, 201, "{raw}");
    let created: Value = serde_json::from_str(&raw).expect("session JSON");
    created["id"].as_str().expect("session id").to_owned()
}

fn start_run(runtime: &Runtime, session: &str, pause: bool) -> String {
    let mut body = json!({ "message": "What time is it?", "session_id": session });
    if pause {
        body["on_time_budget"] = json!("pause");
    }
    let (status, raw) = call(
        runtime,
        "POST",
        "/api/lightagent/v1/runs",
        Some(&body.to_string()),
    );
    assert_eq!(status, 202, "{raw}");
    let created: Value = serde_json::from_str(&raw).expect("run JSON");
    created["id"].as_str().expect("run id").to_owned()
}

fn run_info(runtime: &Runtime, run: &str) -> Value {
    let (status, raw) = call(
        runtime,
        "GET",
        &format!("/api/lightagent/v1/runs/{run}"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    serde_json::from_str(&raw).expect("run JSON")
}

fn continue_run(runtime: &Runtime, run: &str, mode: &str) -> (u16, String) {
    call(
        runtime,
        "POST",
        &format!("/api/lightagent/v1/runs/{run}/continue"),
        Some(&json!({ "mode": mode }).to_string()),
    )
}

#[test]
fn a_run_paused_on_its_time_budget_continues_the_same_execution() {
    let (runtime, bodies) = setup("extend");
    let session = new_session(&runtime);
    let run = start_run(&runtime, &session, true);
    wait_until("the run to pause", || {
        run_info(&runtime, &run)["status"] == "awaiting_continue"
    });
    assert_eq!(
        bodies.lock().expect("bodies").len(),
        1,
        "nothing is sent while the run is paused"
    );
    let (status, raw) = continue_run(&runtime, &run, "extend");
    assert_eq!(status, 200, "{raw}");
    wait_until("the run to finish", || {
        run_info(&runtime, &run)["status"] == "completed"
    });
    let bodies = bodies.lock().expect("bodies").clone();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    let messages = bodies[1]["messages"].as_array().expect("messages");
    assert_eq!(
        messages.last().map(|message| message["role"].clone()),
        Some(json!("tool")),
        "the continued turn carries the tool result the model had not read"
    );
    assert!(
        bodies[1]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "extend carries on with the tools"
    );
    assert_eq!(run_info(&runtime, &run)["stop_reason"], "EndTurn");
    let (status, raw) = continue_run(&runtime, &run, "extend");
    assert_eq!(status, 409, "a finished run cannot be continued: {raw}");
}

#[test]
fn a_paused_run_can_be_cancelled_and_runs_that_did_not_ask_wrap_up() {
    let (runtime, bodies) = setup("cancel");
    let session = new_session(&runtime);
    let run = start_run(&runtime, &session, true);
    wait_until("the run to pause", || {
        run_info(&runtime, &run)["status"] == "awaiting_continue"
    });
    let (status, raw) = call(
        &runtime,
        "POST",
        &format!("/api/lightagent/v1/runs/{run}/cancel"),
        Some("{}"),
    );
    assert_eq!(status, 200, "{raw}");
    wait_until("the run to be cancelled", || {
        run_info(&runtime, &run)["status"] == "cancelled"
    });
    assert_eq!(bodies.lock().expect("bodies").len(), 1);

    // The session is free again; a run that did not ask to pause answers
    // from what it has, without tools, once its budget runs out.
    let deadline = Instant::now() + Duration::from_secs(60);
    let next = loop {
        let body = json!({ "message": "What time is it?", "session_id": session }).to_string();
        let (status, raw) = call(&runtime, "POST", "/api/lightagent/v1/runs", Some(&body));
        if status == 202 {
            let created: Value = serde_json::from_str(&raw).expect("run JSON");
            break created["id"].as_str().expect("run id").to_owned();
        }
        assert!(
            status == 409 && Instant::now() < deadline,
            "{status}: {raw}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    wait_until("the run to finish", || {
        run_info(&runtime, &next)["status"] == "completed"
    });
    let info = run_info(&runtime, &next);
    assert_eq!(info["stop_reason"], "WallClockExceeded", "{info}");
    let bodies = bodies.lock().expect("bodies").clone();
    assert_eq!(bodies.len(), 3, "{bodies:?}");
    assert!(
        bodies[2]["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty()),
        "the wrap-up turn offers no tools"
    );
}
