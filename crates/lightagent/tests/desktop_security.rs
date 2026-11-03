//! The [security] configuration section (D7-13): it has no key `config
//! set` accepts, so the desktop shows it read-only. GET /config reports it
//! as `lightagent config show` does, and the approval policy the Settings
//! screen saves (PUT /settings, the field `lightagent setup` → approvals
//! writes) is what the CLI then reports, with the section's other fields
//! untouched. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const KEY: &str = "091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8";

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
        "lightagent-desktop-security-{label}-{}-{nanos}",
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

fn call(runtime: &Runtime, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
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
    let value =
        serde_json::from_str(&raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"));
    (status, value)
}

/// `lightagent --json config show`: the configuration itself.
fn shown(home: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["--json", "config", "show"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("config show JSON")
}

#[test]
fn the_security_section_is_the_clis_and_the_approval_policy_saves_alike() {
    let home = scratch_home("security");
    let runtime = start(&home);

    let (status, view) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
    assert_eq!(status, 200, "{view}");
    let by_api = view["config"]["security"].clone();
    let by_cli = shown(&home)["security"].clone();
    assert_eq!(by_api, by_cli);
    assert!(by_cli["approval_policy"].is_string(), "{by_cli}");
    assert!(by_cli["remote_tools_default"].is_boolean(), "{by_cli}");
    assert!(by_cli["privacy_mode"].is_boolean(), "{by_cli}");

    // The Settings screen's save: the policy changes for the CLI, nothing else.
    let (status, mut settings) = call(&runtime, "GET", "/api/lightagent/v1/settings", None);
    assert_eq!(status, 200, "{settings}");
    for policy in ["strict", "permissive", "balanced"] {
        settings["approval_policy"] = Value::from(policy);
        let body = settings.to_string();
        let (status, saved) = call(&runtime, "PUT", "/api/lightagent/v1/settings", Some(&body));
        assert_eq!(status, 200, "{policy}: {saved}");
        assert_eq!(saved["approval_policy"], policy, "{saved}");
        settings = saved;
        let after = shown(&home)["security"].clone();
        assert_eq!(after["approval_policy"], policy, "{after}");
        assert_eq!(
            after["remote_tools_default"],
            by_cli["remote_tools_default"]
        );
        assert_eq!(after["privacy_mode"], by_cli["privacy_mode"]);
        let (_, view) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
        assert_eq!(view["config"]["security"], after);
    }
}
