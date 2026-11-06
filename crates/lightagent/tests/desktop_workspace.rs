//! The tools workspace is one validated, shared setting (D4e): the desktop
//! runtime's GET|PUT /workspace and `lightagent setup tools` take the same
//! rule (an absolute folder, existing or inside an existing folder), under
//! the config's revision check, and the effective folder follows the active
//! profile when none is set. The runtime and the CLI run as separate
//! processes.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "8192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f70";
const WORKSPACE: &str = "/api/lightagent/v1/workspace";

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
        "lightagent-desktop-workspace-{label}-{}-{nanos}",
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

/// One authenticated request: the status, the raw body and its JSON.
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
    let json = serde_json::from_str(&raw).unwrap_or(Value::Null);
    (status, raw, json)
}

fn put(runtime: &Runtime, body: Value) -> (u16, String, Value) {
    call(runtime, "PUT", WORKSPACE, Some(&body.to_string()))
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

fn configured(home: &Path) -> Value {
    let config: Value =
        serde_json::from_slice(&std::fs::read(home.join("config.json")).expect("read"))
            .expect("config JSON");
    config["tools"]["workspace"].clone()
}

fn profile_workspace(home: &Path, id: &str) -> String {
    home.join("profiles")
        .join(id)
        .join("workspace")
        .display()
        .to_string()
}

#[test]
fn the_workspace_is_one_validated_shared_setting() {
    let home = scratch_home("set");
    cli_ok(&home, &["config", "set", "inference.model", "m1"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "create", "other"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);

    // None set: the active profile's own folder, and it follows a switch.
    let (status, raw, view) = call(&runtime, "GET", WORKSPACE, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["workspace"]["source"], "profile", "{view}");
    assert_eq!(
        view["workspace"]["effective"],
        profile_workspace(&home, "work")
    );
    assert!(
        view["effect"]
            .as_str()
            .unwrap_or_default()
            .contains("next run"),
        "{view}"
    );
    cli_ok(&home, &["profile", "use", "other"]);
    let (_, _, view) = call(&runtime, "GET", WORKSPACE, None);
    assert_eq!(
        view["workspace"]["effective"],
        profile_workspace(&home, "other")
    );
    assert_eq!(view["workspace"]["profile"], "other");

    // Refused folders write nothing.
    let file = home.join("a-file.txt");
    std::fs::write(&file, "x").expect("file");
    let before = std::fs::read(home.join("config.json")).expect("config");
    for folder in [
        "relative/folder".to_owned(),
        file.display().to_string(),
        home.join("missing").join("deeper").display().to_string(),
    ] {
        let (status, raw, _) = put(&runtime, json!({ "path": folder }));
        assert_eq!(status, 400, "{folder}: {raw}");
    }
    assert_eq!(
        std::fs::read(home.join("config.json")).expect("config"),
        before,
        "a refused folder wrote the config"
    );

    // A change on a revision read before a CLI write is refused.
    let folder = home.join("project").display().to_string();
    let read_at = view["revision"].clone();
    cli_ok(&home, &["config", "set", "inference.model", "m2"]);
    let (status, raw, _) = put(
        &runtime,
        json!({ "path": folder, "expected_revision": read_at }),
    );
    assert_eq!(status, 409, "{raw}");
    assert!(configured(&home).is_null());

    // On the current revision it is saved: a new folder inside an
    // existing one, created by the next run.
    let (_, _, view) = call(&runtime, "GET", WORKSPACE, None);
    let (status, raw, view) = put(
        &runtime,
        json!({ "path": folder, "expected_revision": view["revision"] }),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["workspace"]["source"], "config");
    assert_eq!(view["workspace"]["configured"], folder);
    assert_eq!(view["workspace"]["exists"], false);
    assert_eq!(configured(&home), folder);

    // Cleared: back to the active profile's own folder.
    let (status, raw, view) = put(&runtime, json!({ "path": null }));
    assert_eq!(status, 200, "{raw}");
    assert_eq!(view["workspace"]["source"], "profile");
    assert!(configured(&home).is_null());
}
