//! MCP server configuration through the shared operations (D6-3): what the
//! desktop runtime's API writes is the `mcp` section the CLI loads and saves
//! unchanged; no environment or header value or secret is ever returned; an
//! edit keeps a withheld value it leaves `null`; invalid entries,
//! duplicates, unknown names and a stale revision are refused with the file
//! unchanged byte for byte. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const MCP: &str = "/api/lightagent/v1/mcp";
const STDIO_SECRET: &str = "mcp-stdio-env-value-3b7e";
const HEADER_SECRET: &str = "mcp-header-value-91c2";

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
        "lightagent-desktop-mcp-{label}-{}-{nanos}",
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

fn config_text(home: &Path) -> String {
    std::fs::read_to_string(home.join("config.json")).expect("config.json")
}

fn mcp_section(home: &Path) -> Value {
    parsed(&config_text(home))["mcp"].clone()
}

fn setup(label: &str) -> (PathBuf, Runtime) {
    let home = scratch_home(label);
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    let runtime = start(&home);
    (home, runtime)
}

fn revision(runtime: &Runtime) -> String {
    let (status, raw) = call(runtime, "GET", MCP, None);
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)["revision"]
        .as_str()
        .expect("revision")
        .to_owned()
}

fn stdio_server() -> Value {
    json!({
        "transport": "stdio", "name": "files", "command": "mcp-files",
        "args": ["--root", "/srv"], "env": { "FILES_TOKEN": STDIO_SECRET }
    })
}

fn http_server() -> Value {
    json!({
        "transport": "http", "name": "docs", "url": "https://mcp.example.com/mcp",
        "headers": { "X-Api-Key": HEADER_SECRET },
        "auth": { "kind": "env", "var": "DOCS_TOKEN" },
        "untrusted_profiles": ["research"], "credential_profiles": ["research"]
    })
}

fn add(runtime: &Runtime, server: Value) -> (u16, String) {
    let body = json!({ "server": server }).to_string();
    call(runtime, "POST", &format!("{MCP}/servers"), Some(&body))
}

#[test]
fn what_the_api_writes_is_what_the_cli_loads_and_no_value_comes_back() {
    let (home, runtime) = setup("written");
    let current = revision(&runtime);
    let body = json!({ "server": stdio_server(), "expected_revision": current }).to_string();
    let (status, raw) = call(&runtime, "POST", &format!("{MCP}/servers"), Some(&body));
    assert_eq!(status, 201, "{raw}");
    let (status, raw) = add(&runtime, http_server());
    assert_eq!(status, 201, "{raw}");
    let (status, raw) = call(
        &runtime,
        "PUT",
        MCP,
        Some(r#"{"enabled":true,"timeout_secs":45}"#),
    );
    assert_eq!(status, 200, "{raw}");

    // Shown with names only.
    let (status, raw) = call(&runtime, "GET", MCP, None);
    assert_eq!(status, 200, "{raw}");
    assert!(
        !raw.contains(STDIO_SECRET) && !raw.contains(HEADER_SECRET),
        "{raw}"
    );
    let shown = parsed(&raw);
    assert_eq!(shown["mcp"]["enabled"], true, "{shown}");
    assert_eq!(shown["mcp"]["timeout_secs"], 45, "{shown}");
    assert_eq!(shown["mcp"]["servers"][0]["env"], json!(["FILES_TOKEN"]));
    assert_eq!(shown["mcp"]["servers"][1]["headers"], json!(["X-Api-Key"]));
    assert_eq!(
        shown["mcp"]["servers"][1]["auth"],
        json!({ "kind": "env", "var": "DOCS_TOKEN" })
    );

    // Saved with their values, as a hand-written section would be.
    let saved = mcp_section(&home);
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["timeout_secs"], 45);
    assert_eq!(saved["servers"][0]["command"], "mcp-files");
    assert_eq!(saved["servers"][0]["args"], json!(["--root", "/srv"]));
    assert_eq!(saved["servers"][0]["env"]["FILES_TOKEN"], STDIO_SECRET);
    assert_eq!(saved["servers"][1]["url"], "https://mcp.example.com/mcp");
    assert_eq!(saved["servers"][1]["headers"]["X-Api-Key"], HEADER_SECRET);
    assert!(
        saved["servers"][1]["auth"]
            .to_string()
            .contains("DOCS_TOKEN"),
        "{saved}"
    );
    assert_eq!(
        saved["servers"][1]["untrusted_profiles"],
        json!(["research"])
    );

    // The CLI loads the section, validates it (MCP is on) and saves it
    // unchanged.
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    assert_eq!(mcp_section(&home), saved);
}

#[test]
fn edits_keep_withheld_values_and_refusals_write_nothing() {
    let (home, runtime) = setup("edits");
    assert_eq!(add(&runtime, http_server()).0, 201);
    assert_eq!(add(&runtime, stdio_server()).0, 201);

    // An edit that never saw the values keeps them.
    let body = json!({ "server": {
        "transport": "http", "name": "docs", "url": "https://mcp.example.com/v2",
        "headers": { "X-Api-Key": null }, "auth": { "kind": "keep" }
    }})
    .to_string();
    let (status, raw) = call(&runtime, "PUT", &format!("{MCP}/servers/docs"), Some(&body));
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(HEADER_SECRET), "{raw}");
    let saved = mcp_section(&home);
    assert_eq!(saved["servers"][0]["url"], "https://mcp.example.com/v2");
    assert_eq!(saved["servers"][0]["headers"]["X-Api-Key"], HEADER_SECRET);
    assert!(
        saved["servers"][0]["auth"]
            .to_string()
            .contains("DOCS_TOKEN")
    );

    // A given value replaces it; a rename onto another server is refused.
    let body = json!({ "server": {
        "transport": "http", "name": "docs", "url": "https://mcp.example.com/v2",
        "headers": { "X-Api-Key": "replaced" }
    }})
    .to_string();
    assert_eq!(
        call(&runtime, "PUT", &format!("{MCP}/servers/docs"), Some(&body)).0,
        200
    );
    let saved = mcp_section(&home);
    assert_eq!(saved["servers"][0]["headers"]["X-Api-Key"], "replaced");
    assert!(saved["servers"][0]["auth"].is_null(), "{saved}");
    let body = json!({ "server": {
        "transport": "stdio", "name": "docs", "command": "mcp-files"
    }})
    .to_string();
    assert_eq!(
        call(
            &runtime,
            "PUT",
            &format!("{MCP}/servers/files"),
            Some(&body)
        )
        .0,
        409
    );

    // Every refusal leaves the file exactly as it was.
    let before = config_text(&home);
    let refused = [
        json!({ "transport": "stdio", "name": "empty", "command": "  " }),
        json!({ "transport": "http", "name": "ftp", "url": "ftp://mcp.example.com/" }),
        json!({ "transport": "http", "name": "userinfo", "url": "https://u:p@mcp.example.com/" }),
        json!({ "transport": "http", "name": "profiles", "url": "https://mcp.example.com/", "untrusted_profiles": ["bad id"] }),
        json!({ "transport": "stdio", "name": "has.dot", "command": "x" }),
        json!({ "transport": "stdio", "name": "nothing-kept", "command": "x", "env": { "TOKEN": null } }),
        json!({ "transport": "http", "name": "no-auth-kept", "url": "https://mcp.example.com/", "auth": { "kind": "keep" } }),
        json!({ "transport": "http", "name": "bad-var", "url": "https://mcp.example.com/", "auth": { "kind": "env", "var": "1BAD" } }),
    ];
    for server in refused {
        let (status, raw) = add(&runtime, server.clone());
        assert_eq!(status, 400, "{server}: {raw}");
    }
    let (status, raw) = add(&runtime, stdio_server());
    assert_eq!(status, 409, "a duplicate name: {raw}");
    let (status, raw) = call(&runtime, "PUT", MCP, Some(r#"{"timeout_secs":0}"#));
    assert_eq!(status, 400, "{raw}");
    let body = json!({ "server": { "transport": "http", "name": "files", "url": "https://mcp.example.com/", "headers": { "X-Api-Key": null } } }).to_string();
    let (status, raw) = call(
        &runtime,
        "PUT",
        &format!("{MCP}/servers/files"),
        Some(&body),
    );
    assert_eq!(status, 400, "a stdio server has no header to keep: {raw}");
    assert_eq!(
        call(
            &runtime,
            "PUT",
            &format!("{MCP}/servers/missing"),
            Some(&body)
        )
        .0,
        404
    );
    assert_eq!(
        call(&runtime, "DELETE", &format!("{MCP}/servers/missing"), None).0,
        404
    );
    assert_eq!(config_text(&home), before, "a refusal changed config.json");

    // A write on a revision read before another write is refused.
    let stale = revision(&runtime);
    cli_ok(&home, &["config", "set", "extensions.enabled", "false"]);
    let after_cli = config_text(&home);
    let body = json!({ "server": { "transport": "stdio", "name": "late", "command": "x" }, "expected_revision": stale }).to_string();
    let (status, raw) = call(&runtime, "POST", &format!("{MCP}/servers"), Some(&body));
    assert_eq!(status, 409, "{raw}");
    assert_eq!(config_text(&home), after_cli);

    // Removed.
    let (status, raw) = call(&runtime, "DELETE", &format!("{MCP}/servers/docs"), None);
    assert_eq!(status, 200, "{raw}");
    let names: Vec<Value> = mcp_section(&home)["servers"]
        .as_array()
        .expect("servers")
        .iter()
        .map(|server| server["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("files")]);
}

/// A minimal streamable-HTTP MCP server with one tool, `ping`.
fn spawn_http_mcp() -> u16 {
    fn reply(body: &str) -> Option<String> {
        let message: Value = serde_json::from_str(body).ok()?;
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => Some(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}}).to_string()),
            "notifications/initialized" => None,
            "tools/list" => Some(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}}).to_string()),
            _ => Some(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"no"}}).to_string()),
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 2048];
            let mut body = String::new();
            loop {
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length {
                        body = String::from_utf8_lossy(&buffer[end + 4..end + 4 + length])
                            .into_owned();
                        break;
                    }
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                }
            }
            let response = match reply(&body) {
                Some(json) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                ),
                None => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

#[test]
fn a_check_connects_only_on_request_lists_tools_and_changes_nothing() {
    let (home, runtime) = setup("check");
    let port = spawn_http_mcp();
    let server = json!({
        "transport": "http", "name": "docs", "url": format!("http://127.0.0.1:{port}/mcp"),
        "headers": { "X-Api-Key": HEADER_SECRET }
    });
    assert_eq!(add(&runtime, server).0, 201);
    let before = config_text(&home);

    let (status, raw) = call(&runtime, "POST", &format!("{MCP}/servers/docs/check"), None);
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(HEADER_SECRET), "{raw}");
    let checked = parsed(&raw);
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(checked["transport"], "http", "{checked}");
    assert_eq!(checked["declared_by"], "configuration", "{checked}");
    assert_eq!(checked["mcp_enabled"], false, "{checked}");
    let tools = checked["tools"].as_array().expect("tools");
    assert!(
        tools
            .iter()
            .any(|tool| tool.as_str().is_some_and(|name| name.ends_with("ping"))),
        "{checked}"
    );
    assert_eq!(config_text(&home), before, "a check changed config.json");
    assert_eq!(
        call(
            &runtime,
            "POST",
            &format!("{MCP}/servers/missing/check"),
            None
        )
        .0,
        404
    );

    // A stdio server starts only when checked; its stderr secret is
    // redacted, and its process is gone afterwards.
    #[cfg(unix)]
    {
        let pidfile = home.join("probe.pid");
        let server = json!({
            "transport": "stdio", "name": "probe", "command": "sh",
            "args": ["-c", "echo $$ > \"$1\"; echo \"token $TOKEN\" >&2; exec sleep 300", "probe", pidfile],
            "env": { "TOKEN": STDIO_SECRET }
        });
        assert_eq!(add(&runtime, server).0, 201);
        assert_eq!(
            call(&runtime, "PUT", MCP, Some(r#"{"timeout_secs":2}"#)).0,
            200
        );
        assert_eq!(call(&runtime, "GET", MCP, None).0, 200);
        assert!(!pidfile.exists(), "listing the servers started one");
        let (status, raw) = call(
            &runtime,
            "POST",
            &format!("{MCP}/servers/probe/check"),
            None,
        );
        assert_eq!(status, 200, "{raw}");
        assert!(!raw.contains(STDIO_SECRET), "{raw}");
        let checked = parsed(&raw);
        assert_eq!(checked["ok"], false, "{checked}");
        assert!(checked["error"].is_string(), "{checked}");
        let pid = std::fs::read_to_string(&pidfile).expect("the check started the server");
        let pid = pid.trim().to_owned();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let state = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()
                .expect("ps");
            let state = String::from_utf8_lossy(&state.stdout).trim().to_owned();
            if state.is_empty() || state.starts_with('Z') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the server process {pid} is still running ({state})"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            mcp_section(&home)["enabled"],
            false,
            "a check switched MCP on"
        );
    }
}
