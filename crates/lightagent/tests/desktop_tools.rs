//! The tool catalog and skills through the shared operations (D6-5): the
//! API's catalog, with each tool's permission, is what `lightagent tools
//! list` reports; `?mcp=skip` builds it without starting any MCP server;
//! skills are listed as a run loads them, each with its source. Every
//! process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";

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
        "lightagent-desktop-tools-{label}-{}-{nanos}",
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

fn cli_ok(home: &Path, args: &[&str]) {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output: Output = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(&all)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

/// (name, risk, permission, description), sorted.
fn rows(tools: &Value) -> Vec<(String, String, String, String)> {
    let mut rows: Vec<_> = tools
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| {
            let field = |key: &str| tool[key].as_str().unwrap_or_default().to_owned();
            (
                field("name"),
                field("risk"),
                field("permission"),
                field("description"),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn skill_dir(root: &Path, name: &str, description: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nDo it.\n"),
    )
    .expect("SKILL.md");
}

#[test]
fn the_catalog_is_what_tools_list_reports() {
    let home = scratch_home("catalog");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    let runtime = start(&home);
    let (status, raw) = call(&runtime, "GET", "/api/lightagent/v1/tools?mcp=skip", None);
    assert_eq!(status, 200, "{raw}");
    let catalog = parsed(&raw);
    assert_eq!(catalog["mcp"], "off", "{catalog}");
    let by_api = rows(&catalog["tools"]);
    assert!(!by_api.is_empty());
    assert!(
        by_api
            .iter()
            .all(|row| ["auto", "ask", "block"].contains(&row.2.as_str())),
        "{by_api:?}"
    );
    assert_eq!(rows(&cli_json(&home, &["tools", "list"])), by_api);
}

#[cfg(unix)]
#[test]
fn a_catalog_without_mcp_starts_no_server() {
    let home = scratch_home("skip");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    let runtime = start(&home);
    let marker = home.join("started");
    let server = json!({ "server": {
        "transport": "stdio", "name": "probe", "command": "sh",
        "args": ["-c", "echo started >> \"$1\"; exit 3", "probe", marker]
    }})
    .to_string();
    let (status, raw) = call(
        &runtime,
        "POST",
        "/api/lightagent/v1/mcp/servers",
        Some(&server),
    );
    assert_eq!(status, 201, "{raw}");
    let (status, raw) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/mcp",
        Some(r#"{"enabled":true,"timeout_secs":2}"#),
    );
    assert_eq!(status, 200, "{raw}");

    let (status, raw) = call(&runtime, "GET", "/api/lightagent/v1/tools?mcp=skip", None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["mcp"], "skipped", "{raw}");
    assert!(!marker.exists(), "?mcp=skip started the MCP server");

    // Unchanged for other clients: the default connects, as tools list does.
    let (status, raw) = call(&runtime, "GET", "/api/lightagent/v1/tools", None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["mcp"], "connected", "{raw}");
    assert!(
        marker.exists(),
        "the default catalog did not connect the server"
    );
}

#[test]
fn skills_are_listed_as_a_run_loads_them_with_their_source() {
    let home = scratch_home("skills");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    skill_dir(&home.join("skills"), "alpha", "Global alpha");
    let source = home.join("bundle-src");
    skill_dir(&source.join("skills"), "alpha", "Extension alpha");
    skill_dir(&source.join("skills"), "beta", "Extension beta");
    std::fs::write(
        source.join("extension.json"),
        r#"{"name":"tooling","version":"1.0.0"}"#,
    )
    .expect("manifest");
    cli_ok(
        &home,
        &[
            "extensions",
            "install",
            source.to_str().expect("UTF-8 path"),
        ],
    );
    let runtime = start(&home);
    let skills = |runtime: &Runtime| {
        let (status, raw) = call(runtime, "GET", "/api/lightagent/v1/skills", None);
        assert_eq!(status, 200, "{raw}");
        parsed(&raw)["skills"].clone()
    };

    // The active extension's alpha overrides the global one, as in a run.
    let listed = skills(&runtime);
    let by_name = |name: &str| {
        listed
            .as_array()
            .expect("skills")
            .iter()
            .find(|skill| skill["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(
        by_name("alpha")["description"],
        "Extension alpha",
        "{listed}"
    );
    assert_eq!(
        by_name("alpha")["source"],
        json!({ "scope": "extension", "extension": "tooling" })
    );
    assert_eq!(by_name("beta")["source"]["scope"], "extension", "{listed}");

    // Disabled, it contributes nothing: alpha is the global one again.
    cli_ok(&home, &["extensions", "disable", "tooling"]);
    let listed = skills(&runtime);
    let names: Vec<&str> = listed
        .as_array()
        .expect("skills")
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect();
    assert_eq!(names, vec!["alpha"], "{listed}");
    assert_eq!(listed[0]["source"], json!({ "scope": "global" }));
    assert_eq!(listed[0]["description"], "Global alpha");
}
