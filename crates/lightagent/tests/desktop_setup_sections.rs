//! The setup sections a desktop Settings control mirrors (D7): for each
//! choice `lightagent setup tools|web|approvals|tui` offers, Settings writes
//! the same configuration through the desktop runtime. Two homes start
//! alike; one takes each choice through `lightagent setup` (its prompts read
//! from stdin), the other through PUT /settings, and the sections setup
//! writes stay equal after every step. The runtime reports the web mode
//! setup wrote. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3";
const SETTINGS: &str = "/api/lightagent/v1/settings";
const CUSTOM: &str = "http://127.0.0.1:9/search?format=json";

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
        "lightagent-desktop-setup-sections-{label}-{}-{nanos}",
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
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("{error}: {raw}"))
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_ok(home: &Path, args: &[&str]) -> Output {
    let output = cli(home, args);
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// `lightagent setup <section>` with `input` on its standard input, which is
/// not a terminal, so setup reads numbered choices. Two minutes at most.
fn setup(home: &Path, section: &str, input: &str) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["setup", section])
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start lightagent setup");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("input");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("setup status") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("lightagent setup {section} did not finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "lightagent setup {section} {input:?}: {stderr}"
    );
}

/// What the four setup sections write, from `lightagent --json config show`.
/// Every field must be there with its type: a missing one never compares
/// equal to another missing one.
fn sections(home: &Path) -> Value {
    let output = cli_ok(home, &["--json", "config", "show"]);
    let shown: Value = serde_json::from_slice(&output.stdout).expect("config JSON");
    let picked = json!({
        "tools_enabled": shown["tools"]["enabled"],
        "allow_terminal": shown["tools"]["allow_terminal"],
        "web": shown["web"],
        "realtime": shown["rag"]["realtime_enabled"],
        "approval_policy": shown["security"]["approval_policy"],
        "show_reasoning": shown["tui"]["show_reasoning"],
    });
    for field in [
        "tools_enabled",
        "allow_terminal",
        "realtime",
        "show_reasoning",
    ] {
        assert!(picked[field].is_boolean(), "{field}: {picked}");
    }
    assert!(picked["approval_policy"].is_string(), "{picked}");
    assert!(
        picked["web"]["enabled"].is_boolean() && picked["web"]["search"].is_object(),
        "{picked}"
    );
    picked
}

/// Each choice setup offers, in steps that change something: the section,
/// setup's input, the Settings values, and the web mode GET /settings reports.
fn steps() -> Vec<(&'static str, String, Value, Option<&'static str>)> {
    vec![
        (
            "tools",
            "3\n\n".to_owned(),
            json!({"filesystem_tools_enabled": true, "terminal_enabled": true}),
            None,
        ),
        (
            "tools",
            "2\n\n".to_owned(),
            json!({"filesystem_tools_enabled": true, "terminal_enabled": false}),
            None,
        ),
        (
            "tools",
            "1\n".to_owned(),
            json!({"filesystem_tools_enabled": false, "terminal_enabled": false}),
            None,
        ),
        (
            "web",
            "3\n".to_owned(),
            json!({"web_mode": "duckduckgo", "web_enabled": true}),
            Some("duckduckgo"),
        ),
        (
            "web",
            format!("4\n{CUSTOM}\n"),
            json!({"web_mode": "custom", "web_enabled": true, "web_search_endpoint": CUSTOM}),
            Some("custom"),
        ),
        (
            "web",
            "2\n".to_owned(),
            json!({"web_mode": "fetch", "web_enabled": true}),
            Some("fetch"),
        ),
        (
            "web",
            "1\n".to_owned(),
            json!({"web_mode": "off", "web_enabled": false}),
            Some("off"),
        ),
        (
            "approvals",
            "2\n".to_owned(),
            json!({"approval_policy": "strict"}),
            None,
        ),
        (
            "approvals",
            "3\n".to_owned(),
            json!({"approval_policy": "permissive"}),
            None,
        ),
        (
            "approvals",
            "1\n".to_owned(),
            json!({"approval_policy": "balanced"}),
            None,
        ),
        (
            "tui",
            "2\n".to_owned(),
            json!({"show_reasoning_in_tui": false}),
            None,
        ),
        (
            "tui",
            "1\n".to_owned(),
            json!({"show_reasoning_in_tui": true}),
            None,
        ),
    ]
}

#[test]
fn settings_write_what_each_setup_choice_writes() {
    let by_setup = scratch_home("setup");
    let by_settings = scratch_home("settings");
    for home in [&by_setup, &by_settings] {
        cli_ok(
            home,
            &["config", "set", "inference.base_url", "http://127.0.0.1:9"],
        );
    }
    let desktop = start(&by_settings);
    assert_eq!(
        sections(&by_setup),
        sections(&by_settings),
        "they start alike"
    );
    for (section, input, patch, mode) in steps() {
        let before = sections(&by_setup);
        setup(&by_setup, section, &input);
        let (status, raw) = call(&desktop, "GET", SETTINGS, None);
        assert_eq!(status, 200, "{raw}");
        let mut settings = parsed(&raw);
        for (field, value) in patch.as_object().expect("patch") {
            settings[field] = value.clone();
        }
        let (status, raw) = call(&desktop, "PUT", SETTINGS, Some(&settings.to_string()));
        assert_eq!(status, 200, "{section} {input:?}: {raw}");
        let after = sections(&by_setup);
        assert_ne!(before, after, "{section} {input:?} changed nothing");
        assert_eq!(after, sections(&by_settings), "{section} {input:?}");
        if let Some(mode) = mode {
            assert_eq!(parsed(&raw)["web_mode"], mode, "{raw}");
        }
    }

    // The searches as setup writes them.
    setup(&by_setup, "web", "3\n");
    let shown = sections(&by_setup);
    assert_eq!(
        shown["web"]["search"]["endpoint"],
        "https://html.duckduckgo.com/html/"
    );
    assert_eq!(shown["realtime"], true);

    // The runtime reads the web mode the CLI's setup wrote.
    let reader = start(&by_setup);
    let (status, raw) = call(&reader, "GET", SETTINGS, None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["web_mode"], "duckduckgo", "{raw}");

    // A custom search without its URL is refused, and nothing is written.
    let before = sections(&by_settings);
    let (_, raw) = call(&desktop, "GET", SETTINGS, None);
    let mut settings = parsed(&raw);
    settings["web_mode"] = json!("custom");
    settings["web_search_endpoint"] = json!("  ");
    let (status, raw) = call(&desktop, "PUT", SETTINGS, Some(&settings.to_string()));
    assert_eq!(status, 400, "{raw}");
    assert_eq!(sections(&by_settings), before);

    // A save that leaves the web mode alone keeps what the mode implies:
    // realtime knowledge turned on with `config set` stays on through an
    // unrelated change.
    cli_ok(
        &by_settings,
        &["config", "set", "rag.realtime_enabled", "true"],
    );
    let (_, raw) = call(&desktop, "GET", SETTINGS, None);
    let mut settings = parsed(&raw);
    assert_eq!(settings["web_mode"], "off", "{settings}");
    let turns = settings["max_turns"].as_u64().expect("max_turns");
    settings["max_turns"] = json!(turns + 1);
    let (status, raw) = call(&desktop, "PUT", SETTINGS, Some(&settings.to_string()));
    assert_eq!(status, 200, "{raw}");
    assert_eq!(sections(&by_settings)["realtime"], true);
}
