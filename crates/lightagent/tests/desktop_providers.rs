//! An endpoint check is the same for the CLI (`lightagent models`) and the
//! desktop runtime (POST /provider/check): the configured key goes to the
//! configured endpoint and nowhere else, a redirect is reported rather than
//! followed, a silent endpoint is cut off at the requested bound, and a
//! configured key that cannot be used sends nothing. Every endpoint here is
//! a local fake.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KEY: &str = "4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c";
const PROVIDER_KEY: &str = "check-test-provider-key-a41c7e-not-a-real-key";
const CREDENTIAL: &str = "/api/lightagent/v1/provider/credential";
const CHECK: &str = "/api/lightagent/v1/provider/check";
const MODELS: &str = r#"{"object":"list","data":[{"id":"m1"},{"id":"m2"}]}"#;

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
        "lightagent-desktop-providers-{label}-{}-{nanos}",
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

fn http_response(status: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{extra_headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A fake endpoint that answers one request with `response` (or, with
/// `None`, holds it unanswered for a while) and returns the request it got.
fn fake(response: Option<String>) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        match response {
            Some(response) => {
                let _ = stream.write_all(response.as_bytes());
            }
            None => std::thread::sleep(Duration::from_secs(15)),
        }
        String::from_utf8_lossy(&request).into_owned()
    });
    (url, handle)
}

/// An endpoint that only records whether anything connected to it.
fn untouched() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(true).expect("non-blocking");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    (listener, url)
}

fn contacted(listener: &TcpListener) -> bool {
    match listener.accept() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("accept: {error}"),
    }
}

fn use_endpoint(home: &Path, url: &str) {
    cli_ok(home, &["config", "set", "inference.base_url", url]);
}

fn sends_key(request: &str) -> bool {
    request.to_ascii_lowercase().contains(&format!(
        "authorization: bearer {}",
        PROVIDER_KEY.to_ascii_lowercase()
    ))
}

#[test]
fn an_endpoint_check_is_explicit_bounded_and_sends_the_key_to_that_endpoint_only() {
    let home = scratch_home("check");
    let runtime = start(&home);
    // The provider key, saved through the shared operation.
    let (_, _, status) = call(&runtime, "GET", CREDENTIAL, None);
    let body = serde_json::json!({ "key": PROVIDER_KEY, "expected_revision": status["revision"] })
        .to_string();
    let (status, raw, _) = call(&runtime, "PUT", CREDENTIAL, Some(&body));
    assert_eq!(status, 200, "{raw}");

    // Reachable: the key goes to this endpoint, and the API's models are the CLI's.
    let (url, endpoint) = fake(Some(http_response("200 OK", "", MODELS)));
    use_endpoint(&home, &url);
    let (status, raw, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(PROVIDER_KEY), "{raw}");
    assert_eq!(check["ok"], true, "{check}");
    assert_eq!(check["models"], serde_json::json!(["m1", "m2"]));
    assert_eq!(check["key_sent"], true);
    let request = endpoint.join().expect("endpoint");
    assert!(request.starts_with("GET /v1/models "), "{request}");
    assert!(sends_key(&request), "the check did not send the saved key");
    let (url, endpoint) = fake(Some(http_response("200 OK", "", MODELS)));
    use_endpoint(&home, &url);
    assert_eq!(cli_ok(&home, &["--json", "models"]), r#"["m1","m2"]"#);
    assert!(sends_key(&endpoint.join().expect("endpoint")));

    // A provider error is reported, by the API and the CLI alike.
    for runner in ["api", "cli"] {
        let (url, endpoint) = fake(Some(http_response(
            "401 Unauthorized",
            "",
            r#"{"error":"no"}"#,
        )));
        use_endpoint(&home, &url);
        let text = if runner == "api" {
            let (_, _, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
            assert_eq!(check["ok"], false, "{check}");
            check["error"].as_str().unwrap_or_default().to_owned()
        } else {
            let output = cli(&home, &["models"]);
            assert!(!output.status.success());
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        assert!(text.contains("401"), "{runner}: {text}");
        assert!(!text.contains(PROVIDER_KEY), "{runner}: {text}");
        endpoint.join().expect("endpoint");
    }

    // A redirect is reported and not followed: its target gets nothing.
    for runner in ["api", "cli"] {
        let (target, target_url) = untouched();
        let (url, endpoint) = fake(Some(http_response(
            "302 Found",
            &format!("Location: {target_url}/v1/models\r\n"),
            "",
        )));
        use_endpoint(&home, &url);
        let text = if runner == "api" {
            let (_, _, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
            assert_eq!(check["ok"], false, "{check}");
            check["error"].as_str().unwrap_or_default().to_owned()
        } else {
            let output = cli(&home, &["models"]);
            assert!(!output.status.success());
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        assert!(text.contains("302"), "{runner}: {text}");
        assert!(sends_key(&endpoint.join().expect("endpoint")));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!contacted(&target), "{runner}: the redirect was followed");
    }

    // A silent endpoint is cut off at the requested bound.
    let (url, endpoint) = fake(None);
    use_endpoint(&home, &url);
    let started = Instant::now();
    let (status, raw, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":2}"#));
    let took = started.elapsed();
    assert_eq!(status, 200, "{raw}");
    assert_eq!(check["ok"], false, "{check}");
    assert!(took < Duration::from_secs(10), "the check took {took:?}");
    let text = check["error"].as_str().unwrap_or_default();
    assert!(
        text.contains("within 2 s") || text.to_ascii_lowercase().contains("timed out"),
        "{text}"
    );
    drop(endpoint);

    // A configured key that cannot be used sends nothing.
    let (silent, url) = untouched();
    use_endpoint(&home, &url);
    let config_file = home.join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_file).expect("read")).expect("config JSON");
    value["inference"]["api_key"] =
        serde_json::json!({ "kind": "file", "path": home.join("missing-check-key") });
    std::fs::write(
        &config_file,
        serde_json::to_vec_pretty(&value).expect("JSON"),
    )
    .expect("write");
    let (_, _, check) = call(&runtime, "POST", CHECK, Some(r#"{"timeout_secs":5}"#));
    assert_eq!(check["ok"], false, "{check}");
    assert_eq!(check["key_sent"], false);
    assert!(
        check["error"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot be read"),
        "{check}"
    );
    assert!(
        !contacted(&silent),
        "an unusable key still contacted the endpoint"
    );
}
