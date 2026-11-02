//! Provider credentials through the desktop runtime are the CLI's: the same
//! write-only key file and the same config reference, saved, replaced and
//! removed through one shared operation, with the key never in a response or
//! in CLI output. A configured key that cannot be used fails closed in the
//! terminal and in the desktop runtime, before any request; no key
//! configured is not that, and still reaches the endpoint.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KEY: &str = "3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b";
const FIRST: &str = "credential-test-first-6b1f02-not-a-real-key";
const SECOND: &str = "credential-test-second-c83a51-not-a-real-key";
const UNSET: &str = "LIGHTAGENT_TEST_CREDENTIAL_UNSET";

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
        "lightagent-desktop-credentials-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).expect("home");
    home
}

fn start(home: &Path) -> Runtime {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .env_remove(UNSET)
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
        .set_read_timeout(Some(Duration::from_secs(20)))
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
        .env_remove(UNSET)
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

const CREDENTIAL: &str = "/api/lightagent/v1/provider/credential";

fn save(runtime: &Runtime, key: &str, revision: &serde_json::Value) -> (u16, String) {
    let body = serde_json::json!({ "key": key, "expected_revision": revision }).to_string();
    let (status, raw, _) = call(runtime, "PUT", CREDENTIAL, Some(&body));
    (status, raw)
}

#[test]
fn a_key_is_saved_replaced_and_removed_write_only_and_the_cli_reads_the_same() {
    let home = scratch_home("lifecycle");
    let runtime = start(&home);
    let key_file = home.join("secrets").join("provider-lightweight-local.key");

    let (status, _, absent) = call(&runtime, "GET", CREDENTIAL, None);
    assert_eq!(status, 200, "{absent}");
    assert_eq!(absent["state"], "absent");
    assert_eq!(absent["source"], serde_json::Value::Null);
    assert_eq!(absent["provider_key_id"], "lightweight-local");

    // Saved: the shared store's file, the config's reference, never echoed.
    let (status, raw) = save(&runtime, FIRST, &absent["revision"]);
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(FIRST), "{raw}");
    let saved: serde_json::Value = serde_json::from_str(&raw).expect("status JSON");
    assert_eq!(
        (saved["state"].as_str(), saved["source"].as_str()),
        (Some("ready"), Some("file"))
    );
    assert_eq!(std::fs::read_to_string(&key_file).expect("key file"), FIRST);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key_file)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the key file must be owner-only");
    }
    let reference = cli_ok(&home, &["config", "get", "inference.api_key"]);
    assert!(reference.starts_with("${file:"), "{reference}");
    assert!(
        reference.contains("provider-lightweight-local.key"),
        "{reference}"
    );
    for args in [
        vec!["config", "show"],
        vec!["config", "get", "inference.api_key"],
    ] {
        let output = cli(&home, &args);
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(FIRST),
            "{args:?}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(FIRST),
            "{args:?}"
        );
    }
    for path in [
        CREDENTIAL,
        "/api/lightagent/v1/config",
        "/api/lightagent/v1/settings",
        "/api/lightagent/v1/config/inference.api_key",
    ] {
        let (_, raw, _) = call(&runtime, "GET", path, None);
        assert!(!raw.contains(FIRST), "{path}: {raw}");
    }

    // A save against the revision read before it is refused and writes nothing.
    let (status, raw) = save(&runtime, SECOND, &absent["revision"]);
    assert_eq!(status, 409, "{raw}");
    assert_eq!(std::fs::read_to_string(&key_file).expect("key file"), FIRST);

    // Entries that cannot be sent are refused with nothing written.
    for bad in ["   ", "two\nlines"] {
        let (status, raw) = save(&runtime, bad, &saved["revision"]);
        assert_eq!(status, 400, "{raw}");
        assert!(!raw.contains("two"), "{raw}");
        assert_eq!(std::fs::read_to_string(&key_file).expect("key file"), FIRST);
    }

    // Replaced in place.
    let (status, raw) = save(&runtime, SECOND, &saved["revision"]);
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(SECOND), "{raw}");
    assert_eq!(
        std::fs::read_to_string(&key_file).expect("key file"),
        SECOND
    );

    // Removed: no reference, no file, and the CLI agrees.
    let (status, raw, removed) = call(&runtime, "DELETE", CREDENTIAL, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(removed["state"], "absent");
    assert!(!key_file.exists());
    assert_eq!(cli_ok(&home, &["config", "get", "inference.api_key"]), "");
}

/// A configured provider endpoint that only records whether anything
/// connected to it.
fn endpoint() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(true).expect("non-blocking");
    let port = listener.local_addr().expect("address").port();
    (listener, format!("http://127.0.0.1:{port}/v1"))
}

fn contacted(listener: &TcpListener) -> bool {
    match listener.accept() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("accept: {error}"),
    }
}

fn set_reference(home: &Path, reference: serde_json::Value) {
    let path = home.join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("config JSON");
    value["inference"]["api_key"] = reference;
    std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
}

#[test]
fn a_configured_key_that_cannot_be_used_fails_closed_and_an_absent_one_does_not() {
    let home = scratch_home("fail-closed");
    let (listener, base_url) = endpoint();
    cli_ok(&home, &["config", "set", "inference.base_url", &base_url]);
    let empty = home.join("empty-provider-key");
    std::fs::write(&empty, " \n").expect("empty key");
    let control = home.join("control-provider-key");
    std::fs::write(&control, "abc\u{7}def").expect("control key");
    let runtime = start(&home);

    let cases = [
        (
            serde_json::json!({ "kind": "file", "path": home.join("missing-provider-key") }),
            "unreadable",
            "cannot be read",
        ),
        (
            serde_json::json!({ "kind": "env", "var": UNSET }),
            "unresolved",
            "environment variable is not set",
        ),
        (
            serde_json::json!({ "kind": "file", "path": empty }),
            "empty",
            "is empty",
        ),
        (
            serde_json::json!({ "kind": "file", "path": control }),
            "invalid",
            "control character",
        ),
    ];
    for (reference, state, words) in cases {
        set_reference(&home, reference.clone());
        let (_, _, status) = call(&runtime, "GET", CREDENTIAL, None);
        assert_eq!(status["state"], state, "{reference}");
        for args in [vec!["models"], vec!["chat"]] {
            let output = cli(&home, &args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{args:?} {reference}: {stderr}");
            assert!(stderr.contains(words), "{args:?} {reference}: {stderr}");
            assert!(
                !stderr.contains("provider-key"),
                "{args:?} names the key file: {stderr}"
            );
            assert!(
                !stderr.contains(UNSET),
                "{args:?} names the variable: {stderr}"
            );
        }
        // The desktop runtime's run fails the same way.
        let (status, raw, run) = call(
            &runtime,
            "POST",
            "/api/lightagent/v1/runs",
            Some(r#"{"message":"hello"}"#),
        );
        assert_eq!(status, 202, "{raw}");
        let id = run["id"].as_str().expect("run id").to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (_, _, run) = call(
                &runtime,
                "GET",
                &format!("/api/lightagent/v1/runs/{id}"),
                None,
            );
            if run["status"] == "failed" {
                break;
            }
            assert_ne!(run["status"], "completed", "{run}");
            assert!(Instant::now() < deadline, "the run never failed: {run}");
            std::thread::sleep(Duration::from_millis(50));
        }
        let (_, events, _) = call(
            &runtime,
            "GET",
            &format!("/api/lightagent/v1/runs/{id}/events"),
            None,
        );
        assert!(events.contains(words), "{reference}: {events}");
        assert!(
            !contacted(&listener),
            "the endpoint was contacted with {reference}"
        );
    }

    // No key configured is intentionally absent: the request is made, keyless.
    set_reference(&home, serde_json::Value::Null);
    let (_, _, status) = call(&runtime, "GET", CREDENTIAL, None);
    assert_eq!(status["state"], "absent");
    let mut models = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .arg("models")
        .env("LIGHTAGENT_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent models");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut reached = false;
    while Instant::now() < deadline {
        if contacted(&listener) {
            reached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = models.kill();
    let _ = models.wait();
    assert!(
        reached,
        "with no key configured, `lightagent models` never reached the endpoint"
    );
}
