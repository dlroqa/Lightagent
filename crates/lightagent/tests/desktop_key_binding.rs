//! A provider key is bound to the endpoint it was saved for (D4d-2b). The
//! active endpoint's key goes to the active endpoint only, a saved
//! provider's key to that provider's endpoint only, and any other endpoint
//! (such as a profile's own `routing.base_url`) receives no key. Changing
//! the endpoint never carries the previous endpoint's key to the new one.
//! Proved for the desktop runtime (provider view, run, check, config route)
//! and the CLI (`chat`, `models`, `config set`) in separate processes,
//! against local fakes that record every request.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KEY: &str = "5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";
const ACTIVE_KEY: &str = "binding-test-active-key-7d21c9-not-a-real-key";
const LAB_KEY: &str = "binding-test-lab-key-3b80f4-not-a-real-key";
const CREDENTIAL: &str = "/api/lightagent/v1/provider/credential";
const CHECK: &str = "/api/lightagent/v1/provider/check";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"},{"id":"m2"}]}"#;
const WAIT: Duration = Duration::from_secs(20);

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
        "lightagent-desktop-binding-{label}-{}-{nanos}",
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
    let handshake: serde_json::Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: lifeline,
        addr,
    }
}

/// One authenticated request: the status, the raw body and its JSON.
fn call(
    runtime: &Runtime,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String, serde_json::Value) {
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
    let json = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    (status, raw, json)
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_ok(home: &Path, args: &[&str]) -> String {
    let output = cli(home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

/// A fake OpenAI-compatible endpoint. It answers every request with the
/// model list and keeps each request's head until it is dropped.
struct Recorder {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Recorder {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.set_nonblocking(true).expect("non-blocking");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, done) = (Arc::clone(&requests), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let head = read_head(&mut stream);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{MODELS}",
                            MODELS.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        if let Ok(mut log) = log.lock() {
                            log.push(head);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            requests,
            stop,
        }
    }

    /// The requests received so far, removed from the record.
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().expect("log"))
    }

    /// The requests that arrive within `wait`: it waits for the first, then
    /// a little longer for any that follow it.
    fn collect(&self, wait: Duration) -> Vec<String> {
        let deadline = Instant::now() + wait;
        while self.requests.lock().expect("log").is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(500));
        self.drain()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The bearer token a request carried, if any.
fn bearer(request: &str) -> Option<String> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("authorization").then(|| {
            let value = value.trim();
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .unwrap_or(value)
                .to_owned()
        })
    })
}

/// The endpoint received requests, and every one carried `expected`, or no
/// key at all when `expected` is `None`.
fn assert_keys(requests: &[String], expected: Option<&str>, what: &str) {
    assert!(
        !requests.is_empty(),
        "{what}: the endpoint received no request"
    );
    for request in requests {
        assert_eq!(bearer(request).as_deref(), expected, "{what}: {request}");
    }
}

fn edit_json(path: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("read")).expect("JSON");
    change(&mut value);
    std::fs::write(path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
}

/// Save `key` for the active endpoint through the shared operation.
fn save_key(runtime: &Runtime, key: &str) {
    let (_, _, status) = call(runtime, "GET", CREDENTIAL, None);
    let body =
        serde_json::json!({ "key": key, "expected_revision": status["revision"] }).to_string();
    let (status, raw, _) = call(runtime, "PUT", CREDENTIAL, Some(&body));
    assert_eq!(status, 200, "{raw}");
}

fn key_state(runtime: &Runtime) -> String {
    let (_, _, status) = call(runtime, "GET", CREDENTIAL, None);
    status["state"].as_str().unwrap_or_default().to_owned()
}

/// A saved provider "Lab" at `url`, with its own key in a file.
fn save_lab(home: &Path, url: &str) {
    let lab_key = home.join("lab-provider-key");
    std::fs::write(&lab_key, LAB_KEY).expect("lab key");
    edit_json(&home.join("config.json"), |config| {
        config["inference"]["saved_providers"] = serde_json::json!([
            { "name": "Lab", "base_url": url, "api_key": { "kind": "file", "path": lab_key } }
        ]);
    });
}

#[test]
fn each_endpoint_receives_only_the_key_bound_to_it() {
    let home = scratch_home("profiles");
    let active = Recorder::start();
    let lab = Recorder::start();
    let other = Recorder::start();
    cli_ok(&home, &["config", "set", "inference.base_url", &active.url]);
    let runtime = start(&home);
    save_key(&runtime, ACTIVE_KEY);
    assert_eq!(key_state(&runtime), "ready");
    save_lab(&home, &lab.url);

    // Profiles on the active endpoint, on the saved provider's endpoint, and
    // on an endpoint no key was saved for.
    let profiles = [
        ("plain", None, &active, Some(ACTIVE_KEY)),
        ("lab", Some(lab.url.clone()), &lab, Some(LAB_KEY)),
        ("elsewhere", Some(other.url.clone()), &other, None),
    ];
    for (id, url, _, _) in &profiles {
        let id = *id;
        cli_ok(&home, &["profile", "create", id, "--model", "m1"]);
        if let Some(url) = url {
            let manifest = home.join("profiles").join(id).join("profile.json");
            edit_json(&manifest, |profile| {
                profile["routing"]["base_url"] = serde_json::json!(url);
            });
        }
    }

    for (id, _, endpoint, expected) in profiles {
        for recorder in [&active, &lab, &other] {
            recorder.drain();
        }
        // The desktop's provider view of the active profile: its models and
        // the runtime catalog.
        cli_ok(&home, &["profile", "use", id]);
        let (status, raw, _) = call(&runtime, "GET", "/api/lightagent/v1/provider", None);
        assert!(
            !raw.contains(ACTIVE_KEY) && !raw.contains(LAB_KEY),
            "{id}: {raw}"
        );
        assert_keys(
            &endpoint.collect(WAIT),
            expected,
            &format!("{id}: provider view ({status})"),
        );

        // A desktop run of that profile.
        let body = serde_json::json!({ "message": "hello", "profile": id }).to_string();
        let (status, raw, _) = call(&runtime, "POST", "/api/lightagent/v1/runs", Some(&body));
        assert!((200..300).contains(&status), "{id}: {status} {raw}");
        assert_keys(
            &endpoint.collect(WAIT),
            expected,
            &format!("{id}: desktop run"),
        );

        // The terminal chat of that profile, in its own process.
        let output = cli(&home, &["chat", "--profile", id]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(ACTIVE_KEY) && !stderr.contains(LAB_KEY),
            "{id}: {stderr}"
        );
        assert_keys(
            &endpoint.collect(WAIT),
            expected,
            &format!("{id}: CLI chat"),
        );
    }
}

#[test]
fn changing_the_endpoint_never_carries_the_previous_key() {
    let home = scratch_home("select");
    let first = Recorder::start();
    cli_ok(&home, &["config", "set", "inference.base_url", &first.url]);
    let runtime = start(&home);
    save_key(&runtime, ACTIVE_KEY);

    // The same endpoint, spelled with a trailing slash, keeps its key.
    let same = format!("{}/", first.url);
    cli_ok(&home, &["config", "set", "inference.base_url", &same]);
    assert_eq!(key_state(&runtime), "ready");
    first.drain();
    cli_ok(&home, &["--json", "models"]);
    assert_keys(&first.collect(WAIT), Some(ACTIVE_KEY), "the same endpoint");

    // A new endpoint set by the CLI: no key is configured, and none is sent
    // by the CLI or by the desktop's check.
    let second = Recorder::start();
    cli_ok(&home, &["config", "set", "inference.base_url", &second.url]);
    assert_eq!(key_state(&runtime), "absent");
    cli_ok(&home, &["--json", "models"]);
    assert_keys(&second.collect(WAIT), None, "a new endpoint: CLI models");
    let (_, raw, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
    assert_eq!(check["key_sent"], false, "{raw}");
    assert_keys(&second.collect(WAIT), None, "a new endpoint: desktop check");

    // A new endpoint set through the desktop's config route: the same rule.
    let third = Recorder::start();
    let body = serde_json::json!({ "value": third.url }).to_string();
    let (status, raw, _) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/config/inference.base_url",
        Some(&body),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(key_state(&runtime), "absent");
    let (_, raw, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
    assert_eq!(check["key_sent"], false, "{raw}");
    assert_keys(
        &third.collect(WAIT),
        None,
        "a new endpoint set by the desktop",
    );

    // A saved provider's endpoint takes that provider's own key.
    let lab = Recorder::start();
    save_lab(&home, &lab.url);
    cli_ok(&home, &["config", "set", "inference.base_url", &lab.url]);
    assert_eq!(key_state(&runtime), "ready");
    cli_ok(&home, &["--json", "models"]);
    assert_keys(
        &lab.collect(WAIT),
        Some(LAB_KEY),
        "a saved provider's endpoint",
    );
}
