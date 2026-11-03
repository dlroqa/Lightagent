//! `lightagent update --check` (D7-12) against a controlled release API on
//! loopback: the CLI and the desktop runtime's API report the same thing
//! for newer, equal and older releases, and the same reason for an HTTP
//! error, an exhausted rate limit, a malformed or tagless answer and a
//! timeout. Only the release metadata is ever requested, never an asset,
//! and the binary is unchanged. This is fixture evidence; the live check
//! against GitHub is .github/workflows/update-check-live.yml. Every
//! process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7";
const CHECK: &str = "/api/lightagent/v1/update/check";
const RELEASE_PATH: &str = "/repos/dlroqa/Lightagent/releases/latest";

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

/// What the fixture answers the next request with.
#[derive(Clone)]
enum Answer {
    Respond(String),
    /// Accept and say nothing, for longer than any check waits.
    Hold,
}

struct Fixture {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    answer: Arc<Mutex<Answer>>,
}

fn fixture() -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fixture");
    let url = format!(
        "http://{}{RELEASE_PATH}",
        listener.local_addr().expect("fixture address")
    );
    let requests = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(Answer::Hold));
    let (seen, current) = (Arc::clone(&requests), Arc::clone(&answer));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (seen, current) = (Arc::clone(&seen), Arc::clone(&current));
            std::thread::spawn(move || serve(stream, &seen, &current));
        }
    });
    Fixture {
        url,
        requests,
        answer,
    }
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<String>>, current: &Mutex<Answer>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    seen.lock().expect("requests").push(line);
    let answer = current.lock().expect("answer").clone();
    match answer {
        Answer::Respond(raw) => {
            let _ = stream.write_all(raw.as_bytes());
        }
        Answer::Hold => std::thread::sleep(Duration::from_secs(5)),
    }
}

/// An HTTP response; each of `headers` ends with CRLF.
fn http(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lightagent-desktop-update-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn quiet(command: &mut Command, home: &Path, env: &[(&str, &str)]) {
    command
        .env("LIGHTAGENT_HOME", home)
        .env_remove("LIGHTAGENT_TEST_RELEASE_API")
        .env_remove("LIGHTAGENT_TEST_RELEASE_TIMEOUT_MS")
        .env_remove("LIGHTAGENT_INSTALL_ROOT");
    for (name, value) in env {
        command.env(name, value);
    }
}

fn start(home: &Path, env: &[(&str, &str)]) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    quiet(&mut command, home, env);
    let mut child = command.spawn().expect("start lightagent serve --desktop");
    let mut lifeline = child.stdin.take().expect("stdin");
    writeln!(lifeline, "{KEY}").expect("key");
    lifeline.flush().expect("flush");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        let mut rest = String::new();
        while reader.read_line(&mut rest).is_ok_and(|read| read > 0) {
            rest.clear();
        }
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

fn call(runtime: &Runtime, method: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {KEY}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        runtime.addr
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

fn cli(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command.args(args).stdin(Stdio::null());
    quiet(&mut command, home, env);
    command.output().expect("run lightagent")
}

fn cli_json(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli(home, env, &all);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

/// The binary's size and modification time: unchanged if nothing replaced it.
fn stamp() -> (u64, Option<SystemTime>) {
    let meta = std::fs::metadata(env!("CARGO_BIN_EXE_lightagent")).expect("binary");
    (meta.len(), meta.modified().ok())
}

#[test]
fn the_cli_and_the_api_check_alike_against_controlled_releases() {
    let home = scratch("check");
    let fx = fixture();
    let env = [
        ("LIGHTAGENT_TEST_RELEASE_API", fx.url.as_str()),
        ("LIGHTAGENT_TEST_RELEASE_TIMEOUT_MS", "1500"),
    ];
    let before = stamp();
    let current = cli_json(&home, &[], &["doctor"])["version"]
        .as_str()
        .expect("version")
        .to_owned();
    let runtime = start(&home, &env);
    let release = |tag: &str| {
        let body = json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/dlroqa/Lightagent/releases/tag/{tag}"),
            "assets": [{
                "name": "lightagent-never-downloaded.tar.gz",
                "browser_download_url": format!("{}/never-downloaded", fx.url),
                "size": 1,
            }],
        });
        http("200 OK", "", &body.to_string())
    };

    let cases = [
        ("v99.0.0".to_owned(), true),
        (format!("v{current}"), false),
        ("v0.0.1".to_owned(), false),
    ];
    for (tag, available) in cases {
        *fx.answer.lock().expect("answer") = Answer::Respond(release(&tag));
        let by_cli = cli_json(&home, &env, &["update", "--check"]);
        let (status, raw) = call(&runtime, "GET", CHECK);
        assert_eq!(status, 200, "{tag}: {raw}");
        let by_api: Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(by_api, by_cli, "{tag}");
        assert_eq!(by_cli["latest"], tag.trim_start_matches('v'), "{by_cli}");
        assert_eq!(by_cli["update_available"], available, "{tag}: {by_cli}");
        assert_eq!(by_cli["current"], current.as_str(), "{by_cli}");
        assert_eq!(
            by_cli["release"],
            format!("https://github.com/dlroqa/Lightagent/releases/tag/{tag}"),
            "{by_cli}"
        );
    }

    let failures = [
        (
            Answer::Respond(http("500 Internal Server Error", "", "{}")),
            "GitHub answered HTTP 500",
        ),
        (
            Answer::Respond(http(
                "403 Forbidden",
                "x-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1700000000\r\n",
                r#"{"message":"API rate limit exceeded"}"#,
            )),
            "rate limit is used up (HTTP 403); it resets at Unix time 1700000000",
        ),
        (
            Answer::Respond(http("200 OK", "", "{not json")),
            "could not read the latest release",
        ),
        (
            Answer::Respond(http("200 OK", "", r#"{"html_url":"https://example.org"}"#)),
            "could not read the latest release",
        ),
        (Answer::Hold, "timed out after 1500 ms"),
    ];
    for (answer, words) in failures {
        *fx.answer.lock().expect("answer") = answer;
        let output = cli(&home, &env, &["--json", "update", "--check"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{words}: the CLI succeeded");
        assert!(stderr.contains(words), "CLI, {words}: {stderr}");
        let (status, raw) = call(&runtime, "GET", CHECK);
        assert_eq!(status, 503, "{words}: {raw}");
        assert!(raw.contains(words), "API, {words}: {raw}");
    }

    // Only the metadata was asked for, never an asset; nothing was replaced.
    let seen = fx.requests.lock().expect("requests").clone();
    assert!(seen.len() >= 16, "{seen:?}");
    assert!(
        seen.iter()
            .all(|line| line.starts_with(&format!("GET {RELEASE_PATH} "))),
        "{seen:?}"
    );
    assert_eq!(stamp(), before, "the binary changed");
}

#[test]
fn a_release_api_off_loopback_is_refused() {
    let home = scratch("refused");
    for url in [
        "https://example.com/releases/latest",
        "http://10.0.0.1:80/latest",
    ] {
        let output = cli(
            &home,
            &[("LIGHTAGENT_TEST_RELEASE_API", url)],
            &["update", "--check"],
        );
        assert!(!output.status.success(), "{url}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("must be a loopback http:// URL"),
            "{url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
