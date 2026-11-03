//! Every configuration section the CLI can set (inventory rows
//! config.<section>, and setup → tui): one key of each, written over the
//! desktop runtime's API, is what `lightagent config get` reads, and a CLI
//! write is what the API reads. [security] has no settable key. Every
//! process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6";
const CONFIG: &str = "/api/lightagent/v1/config";

/// One key per section: the value the API writes, then the one the CLI writes.
const SECTIONS: [(&str, &str, &str); 11] = [
    ("inference.model", "sections-model-a", "sections-model-b"),
    ("agent.max_turns", "17", "19"),
    ("web.timeout_secs", "21", "23"),
    ("tools.profile_policy", "report_only", "enforce"),
    ("rag.realtime_enabled", "true", "false"),
    ("memory.top_k", "6", "8"),
    ("runtime.threads", "3", "5"),
    ("generation.max_output_tokens", "1234", "2345"),
    ("platform.infinity.timeout_secs", "11", "13"),
    ("subagents.selection_profiles", "alpha", "alpha,beta"),
    ("tui.show_reasoning", "false", "true"),
];

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
        "lightagent-desktop-config-sections-{label}-{}-{nanos}",
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
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn one_key_of_every_settable_section_reads_and_writes_alike() {
    let home = scratch_home("sections");
    let runtime = start(&home);
    for (key, by_api, by_cli) in SECTIONS {
        let body = json!({ "value": by_api }).to_string();
        let (status, raw) = call(&runtime, "PUT", &format!("{CONFIG}/{key}"), Some(&body));
        assert_eq!(status, 200, "{key}: {raw}");
        assert_eq!(cli_ok(&home, &["config", "get", key]), by_api, "{key}");
        cli_ok(&home, &["config", "set", key, by_cli]);
        let (status, raw) = call(&runtime, "GET", &format!("{CONFIG}/{key}"), None);
        assert_eq!(status, 200, "{key}: {raw}");
        let read: Value = serde_json::from_str(&raw).expect("JSON");
        assert_eq!(read["value"], by_cli, "{key}: {raw}");
    }
}
