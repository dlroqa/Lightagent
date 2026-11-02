//! Saved providers are one shared operation set (D4d-2b). The desktop
//! runtime's routes create, rename, move, select and remove them and
//! select the model, under the config's revision check, with each key
//! bound to its endpoint; the CLI uses what the desktop wrote. Every
//! endpoint is a local fake that records each request.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e";
const LAB_KEY: &str = "saved-test-lab-key-91e4d0-not-a-real-key";
const PROVIDERS: &str = "/api/lightagent/v1/providers";
const ACTIVE: &str = "/api/lightagent/v1/provider/active";
const MODEL: &str = "/api/lightagent/v1/provider/model";
const CREDENTIAL: &str = "/api/lightagent/v1/provider/credential";
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
        "lightagent-desktop-saved-{label}-{}-{nanos}",
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

/// One authenticated request: the status, the raw body and its JSON. No
/// response may carry the provider key.
fn call(runtime: &Runtime, method: &str, path: &str, body: Option<&str>) -> (u16, String, Value) {
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
    assert!(!raw.contains(LAB_KEY), "{method} {path} returned the key");
    let json = serde_json::from_str(&raw).unwrap_or(Value::Null);
    (status, raw, json)
}

fn send(runtime: &Runtime, method: &str, path: &str, body: Value) -> (u16, String, Value) {
    call(runtime, method, path, Some(&body.to_string()))
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

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read")).expect("JSON")
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

/// Save `key` for the active endpoint through the shared credential
/// operation.
fn save_key(runtime: &Runtime, key: &str) {
    let (_, _, status) = call(runtime, "GET", CREDENTIAL, None);
    let body = json!({ "key": key, "expected_revision": status["revision"] });
    let (status, raw, _) = send(runtime, "PUT", CREDENTIAL, body);
    assert_eq!(status, 200, "{raw}");
}

fn provider<'a>(view: &'a Value, id: &str) -> Option<&'a Value> {
    view["providers"]
        .as_array()?
        .iter()
        .find(|provider| provider["id"] == id)
}

fn key_file(home: &Path, id: &str) -> PathBuf {
    home.join("secrets").join(format!("provider-{id}.key"))
}

fn revision(runtime: &Runtime) -> Value {
    let (_, _, view) = call(runtime, "GET", PROVIDERS, None);
    view["revision"].clone()
}

#[test]
fn saved_providers_are_shared_revision_checked_and_keep_keys_bound() {
    let home = scratch_home("ops");
    cli_ok(&home, &["profile", "create", "work", "--model", "m1"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);
    let (lab, moved, other) = (Recorder::start(), Recorder::start(), Recorder::start());
    let profile_file = home.join("profiles").join("work").join("profile.json");

    // The list: the local endpoint, active, keyless; a change applies to the
    // next run.
    let (status, raw, view) = call(&runtime, "GET", PROVIDERS, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["active"], "lightweight-local", "{view}");
    assert!(
        view["effect"]
            .as_str()
            .unwrap_or_default()
            .contains("next run"),
        "{view}"
    );

    // Create, on the current revision: listed, keyless, not selected.
    let read_at = view["revision"].clone();
    let body = json!({ "name": "Lab", "base_url": lab.url, "expected_revision": read_at });
    let (status, raw, view) = send(&runtime, "POST", PROVIDERS, body);
    assert_eq!(status, 201, "{raw}");
    let created = provider(&view, "lab").expect("lab listed");
    assert_eq!(created["key_state"], "absent");
    assert_eq!(created["active"], false);
    assert_eq!(view["active"], "lightweight-local");
    // A save on the revision read before that create is refused.
    let body = json!({ "name": "Other", "base_url": other.url, "expected_revision": read_at });
    let (status, raw, _) = send(&runtime, "POST", PROVIDERS, body);
    assert_eq!(status, 409, "{raw}");
    // Duplicates and invalid input are refused and write nothing.
    let before = std::fs::read(home.join("config.json")).expect("config");
    for (body, code) in [
        (json!({ "name": "LAB!", "base_url": other.url }), 409),
        (
            json!({ "name": "Other", "base_url": format!("{}/", lab.url) }),
            409,
        ),
        (json!({ "name": "Custom", "base_url": other.url }), 400),
        (
            json!({ "name": "Other", "base_url": "ftp://models.example" }),
            400,
        ),
    ] {
        let (status, raw, _) = send(&runtime, "POST", PROVIDERS, body.clone());
        assert_eq!(status, code, "{body}: {raw}");
    }
    assert_eq!(
        std::fs::read(home.join("config.json")).expect("config"),
        before,
        "a refused create wrote the config"
    );

    // Select it with a model: the active profile follows, and the CLI uses
    // it. It has no key yet, so none is sent.
    let (status, raw, view) = send(
        &runtime,
        "PUT",
        ACTIVE,
        json!({ "id": "lab", "model": "m2" }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["active"], "lab");
    assert_eq!(view["model"], "m2");
    let profile = read_json(&profile_file);
    assert_eq!(profile["routing"]["model"], "m2", "{profile}");
    assert!(profile["routing"]["base_url"].is_null(), "{profile}");
    lab.drain();
    assert_eq!(cli_ok(&home, &["--json", "models"]), r#"["m1","m2"]"#);
    assert_keys(&lab.collect(WAIT), None, "the selected provider, keyless");

    // Its key, saved through the shared credential operation, goes to its
    // endpoint only: the provider's own check and the CLI.
    save_key(&runtime, LAB_KEY);
    assert!(key_file(&home, "lab").exists());
    let (status, raw, check) = send(
        &runtime,
        "POST",
        &format!("{PROVIDERS}/lab/check"),
        json!({ "timeout_secs": 5 }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(check["key_sent"], true, "{check}");
    assert_keys(&lab.collect(WAIT), Some(LAB_KEY), "the provider's check");

    // Another saved provider, checked by id: its endpoint gets no key.
    let body = json!({ "name": "Other", "base_url": other.url });
    let (status, raw, _) = send(&runtime, "POST", PROVIDERS, body);
    assert_eq!(status, 201, "{raw}");
    let (_, raw, check) = send(
        &runtime,
        "POST",
        &format!("{PROVIDERS}/other/check"),
        json!({ "timeout_secs": 5 }),
    );
    assert_eq!(check["key_sent"], false, "{raw}");
    assert_keys(&other.collect(WAIT), None, "another provider's check");

    // A rename keeps the key: it moves to the new name's file.
    let (status, raw, view) = send(
        &runtime,
        "PATCH",
        &format!("{PROVIDERS}/lab"),
        json!({ "name": "Lab Two" }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["active"], "lab-two");
    assert_eq!(
        provider(&view, "lab-two").expect("renamed")["key_state"],
        "ready"
    );
    assert!(!key_file(&home, "lab").exists());
    assert!(key_file(&home, "lab-two").exists());
    assert_eq!(cli_ok(&home, &["--json", "models"]), r#"["m1","m2"]"#);
    assert_keys(&lab.collect(WAIT), Some(LAB_KEY), "the renamed provider");

    // A new endpoint releases the key: it is never sent there.
    let (status, raw, view) = send(
        &runtime,
        "PATCH",
        &format!("{PROVIDERS}/lab-two"),
        json!({ "base_url": moved.url }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        provider(&view, "lab-two").expect("moved")["key_state"],
        "absent"
    );
    assert!(!key_file(&home, "lab-two").exists());
    let (_, _, credential) = call(&runtime, "GET", CREDENTIAL, None);
    assert_eq!(credential["state"], "absent", "{credential}");
    moved.drain();
    assert_eq!(cli_ok(&home, &["--json", "models"]), r#"["m1","m2"]"#);
    assert_keys(&moved.collect(WAIT), None, "a moved provider");

    // keep_key carries the key to a new endpoint, only when asked.
    save_key(&runtime, LAB_KEY);
    let (status, raw, view) = send(
        &runtime,
        "PATCH",
        &format!("{PROVIDERS}/lab-two"),
        json!({ "base_url": lab.url, "keep_key": true }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        provider(&view, "lab-two").expect("kept")["key_state"],
        "ready"
    );
    lab.drain();
    assert_eq!(cli_ok(&home, &["--json", "models"]), r#"["m1","m2"]"#);
    assert_keys(&lab.collect(WAIT), Some(LAB_KEY), "keep_key");

    // Ordered concurrency: a change against a revision read before a CLI
    // write is refused and writes nothing; on the current revision it
    // applies, and the active profile follows.
    let read_at = revision(&runtime);
    cli_ok(&home, &["config", "set", "inference.model", "m1"]);
    let body = json!({ "model": "m2", "expected_revision": read_at });
    let (status, raw, _) = send(&runtime, "PUT", MODEL, body);
    assert_eq!(status, 409, "{raw}");
    assert_eq!(
        read_json(&home.join("config.json"))["inference"]["model"],
        "m1"
    );
    let body = json!({ "model": null, "expected_revision": revision(&runtime) });
    let (status, raw, view) = send(&runtime, "PUT", MODEL, body);
    assert_eq!(status, 200, "{raw}");
    assert!(view["model"].is_null(), "{view}");
    assert_eq!(read_json(&profile_file)["routing"]["model"], "default");

    // Removing the active provider releases its key and leaves the local
    // endpoint active, keyless, with automatic model selection.
    let (status, raw, view) = call(&runtime, "DELETE", &format!("{PROVIDERS}/lab-two"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["active"], "lightweight-local");
    assert!(provider(&view, "lab-two").is_none());
    assert!(!key_file(&home, "lab-two").exists());
    let (_, _, credential) = call(&runtime, "GET", CREDENTIAL, None);
    assert_eq!(credential["state"], "absent", "{credential}");

    // Unknown and reserved ids.
    let (status, raw, _) = send(
        &runtime,
        "PATCH",
        &format!("{PROVIDERS}/nope"),
        json!({ "name": "X" }),
    );
    assert_eq!(status, 404, "{raw}");
    let (status, raw, _) = call(
        &runtime,
        "DELETE",
        &format!("{PROVIDERS}/lightweight-local"),
        None,
    );
    assert_eq!(status, 400, "{raw}");
    let (status, raw, shown) = call(&runtime, "GET", &format!("{PROVIDERS}/other"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(shown["provider"]["base_url"], other.url);
}
