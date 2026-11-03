//! Knowledge (RAG) through one shared operation set (D7-3): the desktop
//! runtime's API and `lightagent rag` act on the active profile's index
//! alike, and adds from both at the same time keep both documents. Every
//! process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const KNOWLEDGE: &str = "/api/lightagent/v1/knowledge";

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
        "lightagent-desktop-knowledge-{label}-{}-{nanos}",
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

fn arg(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

fn sources(listing: &Value) -> Vec<String> {
    let mut names: Vec<String> = listing["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .filter_map(|source| source["source"].as_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

#[test]
fn the_cli_and_the_api_keep_one_index() {
    let home = scratch_home("one");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let docs = home.join("docs");
    std::fs::create_dir_all(docs.join("folder")).expect("docs");
    std::fs::write(
        docs.join("rivers.md"),
        "Rivers carry water from mountains to the sea.\n",
    )
    .expect("doc");
    std::fs::write(
        docs.join("folder").join("a.md"),
        "Glaciers carve valleys over centuries.\n",
    )
    .expect("doc");
    std::fs::write(
        docs.join("folder").join("b.md"),
        "Deltas form where rivers slow down.\n",
    )
    .expect("doc");
    let runtime = start(&home);

    // A file indexed over the API, listed by the CLI; the listings are equal.
    let body = json!({ "path": docs.join("rivers.md") }).to_string();
    let (status, raw) = call(&runtime, "POST", KNOWLEDGE, Some(&body));
    assert_eq!(status, 200, "{raw}");
    let added = parsed(&raw);
    assert_eq!(added["indexed"][0]["source"], "rivers.md", "{added}");
    assert_eq!(added["skipped"], json!([]), "{added}");
    let (_, raw) = call(&runtime, "GET", KNOWLEDGE, None);
    assert_eq!(parsed(&raw), cli_json(&home, &["rag", "list"]));

    // A folder indexed by the CLI, listed over the API.
    cli_json(&home, &["rag", "add", arg(&docs.join("folder"))]);
    let (_, raw) = call(&runtime, "GET", KNOWLEDGE, None);
    assert_eq!(
        sources(&parsed(&raw)),
        vec!["a.md".to_owned(), "b.md".to_owned(), "rivers.md".to_owned()]
    );

    // Search: the API answers what the CLI prints.
    let (status, raw) = call(
        &runtime,
        "GET",
        &format!("{KNOWLEDGE}/search?q=rivers%20sea&top_k=2"),
        None,
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        parsed(&raw),
        cli_json(&home, &["rag", "search", "rivers sea", "--top-k", "2"])
    );

    // A relative path is refused; a missing one is reported as skipped by both.
    let body = json!({ "path": "docs/rivers.md" }).to_string();
    assert_eq!(call(&runtime, "POST", KNOWLEDGE, Some(&body)).0, 400);
    let missing = docs.join("missing.md");
    let body = json!({ "path": missing }).to_string();
    let (status, raw) = call(&runtime, "POST", KNOWLEDGE, Some(&body));
    assert_eq!(status, 200, "{raw}");
    let by_api = parsed(&raw);
    let by_cli = cli_json(&home, &["rag", "add", arg(&missing)]);
    assert_eq!(
        by_api["skipped"].as_array().map(Vec::len),
        Some(1),
        "{by_api}"
    );
    assert_eq!(by_api, by_cli);

    // Adds from both at once keep both documents (the index is written
    // under its lock).
    for round in 0..4 {
        let api_doc = docs.join(format!("api-{round}.md"));
        let cli_doc = docs.join(format!("cli-{round}.md"));
        std::fs::write(&api_doc, format!("API document {round} about lakes.\n")).expect("doc");
        std::fs::write(&cli_doc, format!("CLI document {round} about hills.\n")).expect("doc");
        let body = json!({ "path": api_doc }).to_string();
        std::thread::scope(|scope| {
            let api = scope.spawn(|| call(&runtime, "POST", KNOWLEDGE, Some(&body)).0);
            let cli = scope.spawn(|| cli_json(&home, &["rag", "add", arg(&cli_doc)]));
            assert_eq!(api.join().expect("api add"), 200);
            cli.join().expect("cli add");
        });
        let names = sources(&cli_json(&home, &["rag", "list"]));
        assert!(
            names.contains(&format!("api-{round}.md"))
                && names.contains(&format!("cli-{round}.md")),
            "round {round}: {names:?}"
        );
    }

    // Cleared over the API: the CLI lists nothing.
    let (status, raw) = call(&runtime, "DELETE", KNOWLEDGE, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(cli_json(&home, &["rag", "list"])["sources"], json!([]));
}

/// `rag add --source`: the API's `source` names a document exactly as the
/// CLI's flag does, and both list it under that name.
#[test]
fn a_source_name_names_the_document_for_the_cli_and_the_api() {
    let home = scratch_home("source");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let docs = home.join("docs");
    std::fs::create_dir_all(&docs).expect("docs");
    let api_doc = docs.join("lakes.md");
    let cli_doc = docs.join("canals.md");
    std::fs::write(&api_doc, "Lakes hold still water between hills.\n").expect("doc");
    std::fs::write(&cli_doc, "Canals carry boats between towns.\n").expect("doc");
    let runtime = start(&home);

    let body = json!({ "path": api_doc, "source": "named-by-api" }).to_string();
    let (status, raw) = call(&runtime, "POST", KNOWLEDGE, Some(&body));
    assert_eq!(status, 200, "{raw}");
    let by_api = parsed(&raw);
    assert_eq!(by_api["indexed"][0]["source"], "named-by-api", "{by_api}");
    let by_cli = cli_json(
        &home,
        &["rag", "add", arg(&cli_doc), "--source", "named-by-cli"],
    );
    assert_eq!(by_cli["indexed"][0]["source"], "named-by-cli", "{by_cli}");
    assert!(by_cli["indexed"][0]["chunks"].is_u64(), "{by_cli}");
    assert_eq!(
        by_api["indexed"][0]["chunks"],
        by_cli["indexed"][0]["chunks"]
    );

    let (status, raw) = call(&runtime, "GET", KNOWLEDGE, None);
    assert_eq!(status, 200, "{raw}");
    let listed = parsed(&raw);
    assert_eq!(listed, cli_json(&home, &["rag", "list"]));
    assert_eq!(
        sources(&listed),
        vec!["named-by-api".to_owned(), "named-by-cli".to_owned()]
    );
}
