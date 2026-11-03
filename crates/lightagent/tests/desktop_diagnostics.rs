//! Diagnostics (D7-10): the desktop runtime's doctor and architecture
//! reports equal what `lightagent doctor --json` and `lightagent
//! architecture --json` print, and the diagnostics bundle, built from a
//! home seeded with every kind of secret and private content, holds
//! exactly the allowlisted sections and none of them. Every process here
//! is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
const API: &str = "/api/lightagent/v1";
const SECTIONS: [&str; 10] = [
    "schema",
    "about",
    "included",
    "excluded",
    "lightagent",
    "doctor",
    "architecture",
    "settings",
    "counts",
    "stores",
];

struct Runtime {
    child: Child,
    _lifeline: Option<ChildStdin>,
    addr: String,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lightagent-desktop-diagnostics-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn first_line(child: &mut Child) -> String {
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
    rx.recv_timeout(Duration::from_secs(60))
        .expect("first line")
}

fn start_desktop(home: &Path, env: &[(&str, &str)]) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("start lightagent serve --desktop");
    let mut lifeline = child.stdin.take().expect("stdin");
    writeln!(lifeline, "{KEY}").expect("key");
    lifeline.flush().expect("flush");
    let line = first_line(&mut child);
    let handshake: Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: Some(lifeline),
        addr,
    }
}

fn start_keyed(home: &Path) -> Runtime {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .args(["--key-env", "LIGHTAGENT_TEST_DIAGNOSTICS_KEY"])
        .env("LIGHTAGENT_TEST_DIAGNOSTICS_KEY", KEY)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent serve");
    let line = first_line(&mut child);
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_else(|| panic!("no address in {line:?}"))
        .to_owned();
    Runtime {
        child,
        _lifeline: None,
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

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn ok(runtime: &Runtime, method: &str, path: &str, body: Option<Value>) -> (Value, String) {
    let body = body.map(|body| body.to_string());
    let (status, raw) = call(runtime, method, path, body.as_deref());
    assert_eq!(status, 200, "{method} {path}: {raw}");
    (parsed(&raw), raw)
}

fn cli(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("run lightagent")
}

fn cli_ok(home: &Path, env: &[(&str, &str)], args: &[&str]) {
    let output = cli(home, env, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli(home, &[], &all);
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

#[test]
fn the_bundle_holds_the_allowlist_and_no_secret_or_private_content() {
    let home = scratch("home");
    let outside = scratch("outside");
    cli_ok(&home, &[], &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &[], &["profile", "create", "diag-private-profile"]);
    cli_ok(&home, &[], &["profile", "use", "diag-private-profile"]);
    let base_url = "https://diag-user:DIAG-URL-PASSWORD@llm.diag.example:8443/v1/private-path?token=DIAG-URL-TOKEN";
    cli_ok(
        &home,
        &[],
        &["config", "set", "inference.base_url", base_url],
    );
    let vault_key = outside.join("vault.key");
    cli_ok(
        &home,
        &[],
        &["remote-credential", "keygen", arg(&vault_key)],
    );
    let key_env = ("LIGHTAGENT_CREDENTIAL_KEY_FILE", arg(&vault_key));
    let env = [
        key_env,
        ("LIGHTAGENT_OPEN_TERMINAL_TOKEN", "DIAG-ENV-TERMINAL-TOKEN"),
        ("LIGHTAGENT_PROVIDER_KEY", "DIAG-ENV-PROVIDER-KEY"),
        ("LIGHTAGENT_API_KEY", "DIAG-ENV-API-KEY"),
    ];
    let runtime = start_desktop(&home, &env);

    // Doctor and architecture: what the CLI prints.
    let (doctor, _) = ok(&runtime, "GET", &format!("{API}/diagnostics/doctor"), None);
    assert_eq!(doctor, cli_json(&home, &["doctor"]));
    // The home is the one LIGHTAGENT_HOME names, for the runtime and the CLI.
    let reported = PathBuf::from(doctor["home"].as_str().expect("doctor home"));
    assert_eq!(
        reported.canonicalize().expect("canonical reported home"),
        home.canonicalize().expect("canonical home"),
        "{doctor}"
    );
    let (architecture, _) = ok(
        &runtime,
        "GET",
        &format!("{API}/diagnostics/architecture"),
        None,
    );
    assert_eq!(architecture, cli_json(&home, &["architecture"]));

    // Seed: a saved provider key, a memory, a session title, a document, an
    // identity credential and a remote credential.
    ok(
        &runtime,
        "PUT",
        &format!("{API}/provider/credential"),
        Some(json!({"key": "DIAG-SAVED-PROVIDER-KEY"})),
    );
    ok(
        &runtime,
        "POST",
        &format!("{API}/memories"),
        Some(json!({"text": "DIAG-PRIVATE-MEMORY about a diagnosis"})),
    );
    let (status, raw) = call(&runtime, "POST", &format!("{API}/sessions"), Some("{}"));
    assert!(matches!(status, 200 | 201), "POST /sessions: {raw}");
    let session = parsed(&raw)["id"].as_str().expect("session id").to_owned();
    ok(
        &runtime,
        "PATCH",
        &format!("{API}/sessions/{session}"),
        Some(json!({"title": "DIAG-PRIVATE-TITLE"})),
    );
    let document = outside.join("diag-private-document.md");
    std::fs::write(&document, "DIAG-PRIVATE-DOCUMENT text about rivers.\n").expect("document");
    ok(
        &runtime,
        "POST",
        &format!("{API}/knowledge"),
        Some(json!({"path": document, "source": "diag-private-source"})),
    );
    ok(
        &runtime,
        "POST",
        &format!("{API}/identity/users"),
        Some(json!({"id": "diag-private-user"})),
    );
    let token_file = outside.join("identity.key");
    ok(
        &runtime,
        "POST",
        &format!("{API}/identity/credentials"),
        Some(json!({"principal": "diag-private-user", "secret_file": token_file})),
    );
    let token = std::fs::read_to_string(&token_file).expect("token");
    let identity_secret = token
        .trim()
        .rsplit('_')
        .next()
        .unwrap_or_default()
        .to_owned();
    assert!(identity_secret.len() >= 32);
    let remote_secret_file = outside.join("remote.secret");
    std::fs::write(&remote_secret_file, "DIAG-REMOTE-SECRET\n").expect("secret");
    ok(
        &runtime,
        "POST",
        &format!("{API}/remote-credentials"),
        Some(json!({
            "profile": "diag-private-profile",
            "service": "web-search",
            "destinations": ["https://remote.diag.example"],
            "secret_file": remote_secret_file,
        })),
    );

    // The bundle: exactly the allowlisted sections.
    let (bundle, raw) = ok(&runtime, "GET", &format!("{API}/diagnostics/bundle"), None);
    let mut sections: Vec<&str> = bundle
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    sections.sort_unstable();
    let mut expected = SECTIONS.to_vec();
    expected.sort_unstable();
    assert_eq!(sections, expected);
    assert_eq!(bundle["schema"], "lightagent-diagnostics/1");

    // None of the seeded secrets or private content; no launch key, path,
    // user or password from the URL.
    let forbidden = [
        "DIAG-SAVED-PROVIDER-KEY",
        "DIAG-URL-PASSWORD",
        "DIAG-URL-TOKEN",
        "diag-user",
        "private-path",
        "DIAG-ENV-TERMINAL-TOKEN",
        "DIAG-ENV-PROVIDER-KEY",
        "DIAG-ENV-API-KEY",
        "DIAG-PRIVATE-MEMORY",
        "DIAG-PRIVATE-TITLE",
        "DIAG-PRIVATE-DOCUMENT",
        "diag-private-source",
        "diag-private-user",
        "diag-private-profile",
        "DIAG-REMOTE-SECRET",
        "remote.diag.example",
        identity_secret.as_str(),
        KEY,
        arg(&home),
        arg(&outside),
    ];
    for text in forbidden {
        assert!(!raw.contains(text), "the bundle holds {text:?}: {raw}");
    }

    // What it does hold: the origin, the counts, the stores' state.
    assert_eq!(
        bundle["doctor"]["provider_origin"],
        "https://llm.diag.example:8443"
    );
    assert_eq!(bundle["doctor"]["home"], "<home>");
    assert_eq!(bundle["counts"]["active_profile_memories"], 1, "{bundle}");
    assert_eq!(bundle["counts"]["active_profile_sessions"], 1, "{bundle}");
    assert_eq!(bundle["counts"]["active_profile_documents"], 1, "{bundle}");
    assert_eq!(bundle["stores"]["identity"]["store"], "present", "{bundle}");
    assert_eq!(
        bundle["stores"]["remote_credentials"]["key_source"], "file",
        "{bundle}"
    );
    assert_eq!(
        bundle["stores"]["remote_credentials"]["grants"]["active"], 1,
        "{bundle}"
    );
}

#[test]
fn a_keyed_server_refuses_diagnostics() {
    let home = scratch("gate");
    cli_ok(&home, &[], &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &[], &["profile", "create", "work"]);
    cli_ok(&home, &[], &["profile", "use", "work"]);
    let keyed = start_keyed(&home);
    for path in ["doctor", "architecture", "bundle"] {
        let (status, raw) = call(&keyed, "GET", &format!("{API}/diagnostics/{path}"), None);
        assert_eq!(status, 403, "{path}: {raw}");
    }
}
