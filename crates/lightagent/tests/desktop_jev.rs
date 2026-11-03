//! Jev status and the connection check on the desktop runtime (D7, row 56):
//! `GET /jev` and `POST /jev/verify` on `lightagent serve --desktop`, the
//! operations the Settings and Tools screens use, against a deterministic
//! local stub of TypeSafe's SystemOne endpoint. This is stub evidence, not
//! live Jev evidence (that is `jev-live.yml`, with a real key). The stub
//! checks the request the client sends (one POST to `/v1/systemone`, the
//! configured bearer key, one synthetic noul question) and answers as
//! TypeSafe does, or as a broken service would: a success, a rejected key,
//! a rejected request, a failing status, an unavailable service and
//! malformed answers are each the check's result, and the status reports a
//! check only while the settings it checked are in force. Neither answer
//! holds the key. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";
/// The variable the Jev key is read from; set for the runtime only.
const JEV_KEY_VAR: &str = "LIGHTAGENT_TEST_JEV_STUB_KEY";
/// A placeholder the stub expects as the bearer key; it is no real key.
const JEV_KEY: &str = "jev-stub-placeholder-3c4d5e";
const STATUS: &str = "/api/lightagent/v1/jev";
const VERIFY: &str = "/api/lightagent/v1/jev/verify";

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
        "lightagent-desktop-jev-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).expect("home");
    home
}

fn start(home: &Path) -> Runtime {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .env(JEV_KEY_VAR, JEV_KEY)
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
fn call(runtime: &Runtime, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
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
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

/// `GET /jev`, which must answer, and its raw body.
fn status_now(runtime: &Runtime) -> (Value, String) {
    let (status, raw) = call(runtime, "GET", STATUS, "");
    assert_eq!(status, 200, "{raw}");
    (parsed(&raw), raw)
}

/// `POST /jev/verify`, which must answer, and its raw body.
fn verify(runtime: &Runtime) -> (Value, String) {
    let (status, raw) = call(runtime, "POST", VERIFY, "{}");
    assert_eq!(status, 200, "{raw}");
    (parsed(&raw), raw)
}

fn edit_config(home: &Path, change: impl FnOnce(&mut Value)) {
    let path = home.join("config.json");
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("config JSON");
    change(&mut value);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
}

/// Jev at `base_url`, its key read from [`JEV_KEY_VAR`].
fn configure(home: &Path, base_url: &str) {
    edit_config(home, |config| {
        config["platform"]["jev"]["base_url"] = json!(base_url);
        config["platform"]["jev"]["api_key"] = json!({ "kind": "env", "var": JEV_KEY_VAR });
        config["platform"]["jev"]["timeout_secs"] = json!(5);
    });
}

/// What the stub received: the path, the bearer key and the JSON body.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    bearer: Option<String>,
    body: Value,
}

/// A local stand-in for TypeSafe's SystemOne endpoint. The first path
/// segment picks its answer; every request it receives is recorded before
/// it answers.
struct Stub {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub listener");
        let url = format!("http://{}", listener.local_addr().expect("stub address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve_one(stream, &record);
            }
        });
        Self { url, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("stub record").clone()
    }
}

fn serve_one(mut stream: TcpStream, record: &Mutex<Vec<Seen>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reading);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut length = 0_usize;
    let mut bearer = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                bearer = value.strip_prefix("Bearer ").map(str::to_owned);
            }
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (status, reply) = answer(&path, &body);
    record
        .lock()
        .expect("stub record")
        .push(Seen { path, bearer, body });
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.as_bytes());
}

/// The stub's answer: TypeSafe's envelope, or what a broken service sends.
fn answer(path: &str, body: &Value) -> (&'static str, String) {
    let envelope = |answers: Value| {
        json!({
            "model": "jev-1.13.0",
            "answers": answers,
            "usage": { "input_tokens": 12, "output_tokens": 2 },
        })
        .to_string()
    };
    match path.trim_start_matches('/').split('/').next() {
        Some("ok") => {
            // Every question asked, answered with its own (noul) type.
            let answers: serde_json::Map<String, Value> = body["questions"]
                .as_object()
                .map(|questions| {
                    questions
                        .keys()
                        .map(|id| (id.clone(), json!({ "type": "noul", "noul": 0.9 })))
                        .collect()
                })
                .unwrap_or_default();
            ("200 OK", envelope(Value::Object(answers)))
        }
        Some("auth") => (
            "401 Unauthorized",
            json!({ "error": "invalid API key" }).to_string(),
        ),
        Some("reject") => (
            "422 Unprocessable Entity",
            json!({ "error": "invalid request" }).to_string(),
        ),
        Some("busy") => (
            "503 Service Unavailable",
            json!({ "error": "unavailable" }).to_string(),
        ),
        Some("text") => ("200 OK", "this is not JSON".to_owned()),
        Some("empty") => ("200 OK", "{}".to_owned()),
        Some("unanswered") => ("200 OK", envelope(json!({}))),
        Some("wrong-type") => (
            "200 OK",
            envelope(json!({
                "reachable": {
                    "type": "choice",
                    "choice": "yes",
                    "probabilities": { "yes": 1.0 },
                    "confidence": 1.0,
                },
            })),
        ),
        _ => ("404 Not Found", "{}".to_owned()),
    }
}

#[test]
fn the_check_and_the_status_report_what_the_service_answered() {
    let home = scratch_home("checks");
    // The config file exists once something saved it: the CLI, here.
    let created = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["config", "set", "inference.model", "m1"])
        .env("LIGHTAGENT_HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let stub = Stub::start();
    let runtime = start(&home);

    // Nothing configured: the status says so, and the check sends nothing.
    let (shown, _) = status_now(&runtime);
    assert_eq!(shown["name"], "Jev Task Classification", "{shown}");
    assert_eq!(shown["endpoint_configured"], false, "{shown}");
    assert_eq!(shown["configured"], false, "{shown}");
    assert!(
        shown["profile"]
            .as_str()
            .is_some_and(|profile| !profile.is_empty()),
        "{shown}"
    );
    assert!(shown["verification"].is_null(), "{shown}");
    let (unconfigured, _) = verify(&runtime);
    assert_eq!(unconfigured["ok"], false, "{unconfigured}");
    assert_eq!(unconfigured["error"], "no_endpoint", "{unconfigured}");
    assert!(stub.seen().is_empty(), "{:?}", stub.seen());

    // A success: one request, as the client sends it, and its model.
    configure(&home, &format!("{}/ok", stub.url));
    let (verified, raw_verified) = verify(&runtime);
    assert_eq!(verified["ok"], true, "{verified}");
    assert_eq!(verified["model_version"], "jev-1.13.0", "{verified}");
    assert!(verified["error"].is_null(), "{verified}");
    assert!(verified["latency_ms"].is_u64(), "{verified}");
    assert!(
        verified["checked_at_unix"]
            .as_u64()
            .is_some_and(|at| at > 0),
        "{verified}"
    );
    let seen = stub.seen();
    assert_eq!(seen.len(), 1, "one request per check: {seen:?}");
    assert_eq!(seen[0].path, "/ok/v1/systemone", "{seen:?}");
    assert_eq!(seen[0].bearer.as_deref(), Some(JEV_KEY), "{seen:?}");
    let questions = seen[0].body["questions"]
        .as_object()
        .unwrap_or_else(|| panic!("no questions: {}", seen[0].body));
    assert_eq!(questions.len(), 1, "{}", seen[0].body);
    assert_eq!(questions["reachable"]["type"], "noul", "{}", seen[0].body);
    assert!(
        seen[0]
            .body
            .to_string()
            .contains("Lightagent connection check."),
        "{}",
        seen[0].body
    );
    let (shown, raw_shown) = status_now(&runtime);
    assert_eq!(
        shown["verification"], verified,
        "the status reports the check just made"
    );
    assert_eq!(shown["endpoint_configured"], true, "{shown}");
    assert_eq!(shown["api_key_configured"], true, "{shown}");
    assert_eq!(shown["api_key_resolvable"], true, "{shown}");
    assert_eq!(shown["configured"], true, "{shown}");
    for raw in [&raw_verified, &raw_shown] {
        assert!(
            !raw.contains(JEV_KEY) && !raw.contains(JEV_KEY_VAR),
            "an answer holds the key or its variable: {raw}"
        );
    }

    // Other settings: the earlier check is not reported for them.
    configure(&home, &format!("{}/auth", stub.url));
    let (shown, _) = status_now(&runtime);
    assert!(
        shown["verification"].is_null(),
        "a check of other settings is reported: {shown}"
    );

    // Each failure is the check's result, and the status reports it.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("port");
        format!("http://{}", listener.local_addr().expect("address"))
    };
    for (base_url, error, requests) in [
        (format!("{}/auth", stub.url), "unauthorized", 1),
        (format!("{}/reject", stub.url), "rejected", 1),
        (format!("{}/busy", stub.url), "status", 1),
        (closed, "connect", 0),
        (format!("{}/text", stub.url), "malformed", 1),
        (format!("{}/empty", stub.url), "malformed", 1),
        (format!("{}/unanswered", stub.url), "malformed", 1),
        (format!("{}/wrong-type", stub.url), "malformed", 1),
    ] {
        configure(&home, &base_url);
        let before = stub.seen().len();
        let (checked, raw) = verify(&runtime);
        assert_eq!(checked["ok"], false, "{base_url}: {checked}");
        assert_eq!(checked["error"], error, "{base_url}: {checked}");
        assert!(checked["model_version"].is_null(), "{base_url}: {checked}");
        assert!(!raw.contains(JEV_KEY), "{base_url}: the key is in {raw}");
        assert_eq!(
            stub.seen().len() - before,
            requests,
            "{base_url}: requests sent"
        );
        let (shown, _) = status_now(&runtime);
        assert_eq!(
            shown["verification"], checked,
            "{base_url}: the status reports the failed check"
        );
    }

    // And the service answering again verifies again.
    configure(&home, &format!("{}/ok", stub.url));
    let (verified, _) = verify(&runtime);
    assert_eq!(verified["ok"], true, "{verified}");
}
