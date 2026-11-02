//! Every configured credential fails closed (D4d-3): a key that is
//! configured but cannot be used (an unreadable file, an unset variable,
//! an empty value, a value with a control character) stops the operation
//! before anything is sent, with an answer that names neither the file nor
//! the variable. Shown here for the desktop runtime's Jev connection check,
//! in a separate process, against an endpoint that records any connection.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f";
const UNSET: &str = "LIGHTAGENT_TEST_FAIL_CLOSED_UNSET";

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
        "lightagent-desktop-fail-closed-{label}-{}-{nanos}",
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

fn edit_config(home: &Path, change: impl FnOnce(&mut Value)) {
    let path = home.join("config.json");
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("config JSON");
    change(&mut value);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
}

/// Each kind of configured reference that cannot be used.
fn unusable(home: &Path) -> Vec<Value> {
    let empty = home.join("empty-fail-closed-key");
    std::fs::write(&empty, " \n").expect("empty key");
    let control = home.join("control-fail-closed-key");
    std::fs::write(&control, "abc\u{7}def").expect("control key");
    vec![
        json!({ "kind": "file", "path": home.join("missing-fail-closed-key") }),
        json!({ "kind": "env", "var": UNSET }),
        json!({ "kind": "file", "path": empty }),
        json!({ "kind": "file", "path": control }),
    ]
}

#[test]
fn a_jev_check_with_an_unusable_key_sends_nothing() {
    let home = scratch_home("jev");
    let (listener, url) = untouched();
    let runtime = start(&home);
    for reference in unusable(&home) {
        edit_config(&home, |config| {
            config["platform"]["jev"]["base_url"] = json!(url);
            config["platform"]["jev"]["api_key"] = reference.clone();
        });
        let (status, raw) = call(&runtime, "POST", "/api/lightagent/v1/jev/verify", "{}");
        assert_eq!(status, 200, "{reference}: {raw}");
        assert!(raw.contains("credential_unavailable"), "{reference}: {raw}");
        assert!(
            !raw.contains("fail-closed-key") && !raw.contains(UNSET),
            "the answer names the key's location: {raw}"
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !contacted(&listener),
            "{reference}: Jev was contacted without its key"
        );
    }
}
