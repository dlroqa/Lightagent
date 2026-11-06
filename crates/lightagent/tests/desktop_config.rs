//! Configuration writes through the desktop runtime's API are the CLI's
//! `lightagent config set`: the same keys, values and errors on the same file,
//! one lock that every process takes, and a stale write refused rather than
//! lost. A secret written through Settings never appears in what either shows.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEY: &str = "9a8b7c6d5e4f30211203f4e5d6c7b8a99a8b7c6d5e4f30211203f4e5d6c7b8a9";
const SECRET: &str = "sk-desktop-config-secret-7f3a91";

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
        "lightagent-desktop-config-{label}-{}-{nanos}",
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
    let handshake: serde_json::Value = serde_json::from_str(&line).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: lifeline,
        addr,
    }
}

/// One authenticated request: the status, the raw body and its JSON.
fn call(
    runtime: &Runtime,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String, serde_json::Value) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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
    let json = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    (status, raw, json)
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

/// What a successful CLI command printed.
fn cli_ok(home: &Path, args: &[&str]) -> String {
    let output = cli(home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn key_path(key: &str) -> String {
    format!("/api/lightagent/v1/config/{key}")
}

#[test]
fn api_config_reads_and_writes_are_the_cli_config_reads_and_writes() {
    let home = scratch_home("same");
    let runtime = start(&home);

    // A CLI write is what the API reads, and the other way round; neither
    // loses the other's change.
    cli_ok(&home, &["config", "set", "agent.max_turns", "7"]);
    let (status, _, read) = call(&runtime, "GET", &key_path("agent.max_turns"), None);
    assert_eq!((status, read["value"].as_str()), (200, Some("7")), "{read}");
    let body = serde_json::json!({ "value": "9" }).to_string();
    let (status, _, written) = call(&runtime, "PUT", &key_path("memory.top_k"), Some(&body));
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["value"], "9");
    assert_eq!(cli_ok(&home, &["config", "get", "memory.top_k"]), "9");
    assert_eq!(cli_ok(&home, &["config", "get", "agent.max_turns"]), "7");

    // `config show` and GET /config show the same redacted configuration.
    let (status, _, shown) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
    assert_eq!(status, 200, "{shown}");
    let cli_shown: serde_json::Value =
        serde_json::from_str(&cli_ok(&home, &["config", "show"])).expect("config show JSON");
    assert_eq!(shown["config"], cli_shown);
    assert_eq!(
        shown["path"].as_str().map(PathBuf::from),
        Some(PathBuf::from(cli_ok(&home, &["config", "path"])))
    );

    // The same invalid value and unknown key are refused with the same words.
    let output = cli(&home, &["config", "set", "memory.top_k", "lots"]);
    assert!(!output.status.success());
    let body = serde_json::json!({ "value": "lots" }).to_string();
    let (status, _, refused) = call(&runtime, "PUT", &key_path("memory.top_k"), Some(&body));
    assert_eq!(status, 400, "{refused}");
    let api_error = refused["error"].as_str().expect("error");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(api_error),
        "CLI: {} API: {api_error}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = cli(&home, &["config", "get", "no.such.key"]);
    assert!(!output.status.success());
    let (status, _, missing) = call(&runtime, "GET", &key_path("no.such.key"), None);
    assert_eq!(status, 404, "{missing}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(missing["error"].as_str().expect("error")),
        "{missing}"
    );
    assert_eq!(cli_ok(&home, &["config", "get", "memory.top_k"]), "9");
}

#[test]
fn a_stale_write_is_refused_with_409_and_writes_nothing() {
    let home = scratch_home("stale");
    let runtime = start(&home);
    let (_, _, first) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
    let read_at = first["revision"].as_str().expect("revision").to_owned();

    // Another writer, after the revision was read.
    cli_ok(&home, &["config", "set", "agent.max_turns", "11"]);
    let config_file = home.join("config.json");
    let before = std::fs::read(&config_file).expect("config");
    let body = serde_json::json!({ "value": "3", "expected_revision": read_at }).to_string();
    let (status, _, refused) = call(&runtime, "PUT", &key_path("memory.top_k"), Some(&body));
    assert_eq!(status, 409, "{refused}");
    let (_, _, now) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
    assert_eq!(refused["revision"], now["revision"]);
    assert_eq!(std::fs::read(&config_file).expect("config"), before);

    // With the current revision it lands, and the other writer's change stays.
    let body =
        serde_json::json!({ "value": "3", "expected_revision": now["revision"] }).to_string();
    let (status, _, written) = call(&runtime, "PUT", &key_path("memory.top_k"), Some(&body));
    assert_eq!(status, 200, "{written}");
    assert_eq!(cli_ok(&home, &["config", "get", "agent.max_turns"]), "11");
    assert_eq!(cli_ok(&home, &["config", "get", "memory.top_k"]), "3");

    // The Settings screen's save is checked the same way.
    let (status, _, settings) = call(&runtime, "GET", "/api/lightagent/v1/settings", None);
    assert_eq!(status, 200, "{settings}");
    cli_ok(&home, &["config", "set", "memory.top_k", "4"]);
    let mut stale = settings.clone();
    stale["expected_revision"] = settings["revision"].clone();
    stale["max_turns"] = 5.into();
    let (status, _, refused) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/settings",
        Some(&stale.to_string()),
    );
    assert_eq!(status, 409, "{refused}");
    assert_eq!(cli_ok(&home, &["config", "get", "agent.max_turns"]), "11");

    // Without a revision, the save applies to the config as it is now and
    // keeps the CLI's change made since the screen was loaded.
    let mut fresh = settings.clone();
    fresh["max_turns"] = 5.into();
    let (status, _, saved) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/settings",
        Some(&fresh.to_string()),
    );
    assert_eq!(status, 200, "{saved}");
    assert_eq!(cli_ok(&home, &["config", "get", "agent.max_turns"]), "5");
    assert_eq!(cli_ok(&home, &["config", "get", "memory.top_k"]), "4");
}

#[test]
fn a_secret_saved_through_settings_never_appears_in_cli_or_api_output() {
    let home = scratch_home("secret");
    let runtime = start(&home);
    let (_, _, mut settings) = call(&runtime, "GET", "/api/lightagent/v1/settings", None);
    settings["jev"]["api_key"] = SECRET.into();
    let (status, raw, _) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/settings",
        Some(&settings.to_string()),
    );
    assert_eq!(status, 200, "{raw}");
    assert!(!raw.contains(SECRET));

    let (_, raw, shown) = call(&runtime, "GET", "/api/lightagent/v1/config", None);
    assert!(!raw.contains(SECRET), "GET /config: {raw}");
    // The secret was stored: the config refers to it.
    assert!(
        !shown["config"]["platform"]["jev"]["api_key"].is_null(),
        "{shown}"
    );
    for path in [
        "/api/lightagent/v1/settings".to_owned(),
        key_path("inference.api_key"),
    ] {
        let (_, raw, _) = call(&runtime, "GET", &path, None);
        assert!(!raw.contains(SECRET), "{path}: {raw}");
    }
    for args in [
        vec!["config", "show"],
        vec!["config", "get", "inference.api_key"],
    ] {
        let output = cli(&home, &args);
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(SECRET)
                && !String::from_utf8_lossy(&output.stderr).contains(SECRET),
            "{args:?}"
        );
    }
}

#[test]
fn one_lock_holds_every_process_and_a_waiting_writer_keeps_the_holders_change() {
    let home = scratch_home("lock");
    cli_ok(&home, &["config", "set", "agent.max_turns", "7"]);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join("config.json.lock"))
        .expect("lock file");
    lock.lock().expect("hold the config lock");
    let writer = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["config", "set", "agent.max_tool_calls", "11"])
        .env("LIGHTAGENT_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI writer");
    let mut writer = writer;
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        writer.try_wait().expect("poll").is_none(),
        "the CLI wrote while another process held the config lock"
    );
    // Another writer, inside the lock: a change the waiting CLI must keep.
    let path = home.join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("config JSON");
    value["memory"]["top_k"] = 9.into();
    std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
    lock.unlock().expect("release");
    let output = writer.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (key, expected) in [
        ("agent.max_turns", "7"),
        ("agent.max_tool_calls", "11"),
        ("memory.top_k", "9"),
    ] {
        assert_eq!(cli_ok(&home, &["config", "get", key]), expected, "{key}");
    }
}

/// Every file under `root`, by `/`-separated relative path, with its text.
/// The one normalization: the home's own path, which differs between two
/// homes by construction, reads as `<HOME>` wherever a file records it
/// (raw, or escaped inside JSON).
fn files(root: &Path) -> std::collections::BTreeMap<String, String> {
    let home = root.display().to_string();
    let escaped = serde_json::to_string(&home).expect("JSON");
    let escaped = escaped.trim_matches('"').to_owned();
    let mut found = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("under the home")
                .to_string_lossy()
                .replace('\\', "/");
            let text = String::from_utf8_lossy(&std::fs::read(&path).expect("read"))
                .replace(&escaped, "<HOME>")
                .replace(&home, "<HOME>");
            found.insert(relative, text);
        }
    }
    found
}

#[test]
fn the_cli_and_the_desktop_write_the_same_files_in_isolated_homes() {
    let by_cli = scratch_home("golden-cli");
    let by_desktop = scratch_home("golden-desktop");
    for args in [
        vec![
            "profile", "create", "work", "--name", "Work", "--model", "m1",
        ],
        vec!["profile", "use", "work"],
        vec!["config", "set", "agent.max_turns", "7"],
        vec!["config", "set", "memory.top_k", "9"],
        vec!["profile", "create", "scratch"],
        vec!["profile", "delete", "scratch"],
    ] {
        cli_ok(&by_cli, &args);
    }
    let runtime = start(&by_desktop);
    let steps = [
        (
            "POST",
            "/api/lightagent/v1/profiles".to_owned(),
            Some(r#"{"id":"work","name":"Work","model":"m1"}"#),
        ),
        (
            "PUT",
            "/api/lightagent/v1/profiles/work/active".to_owned(),
            None,
        ),
        ("PUT", key_path("agent.max_turns"), Some(r#"{"value":"7"}"#)),
        ("PUT", key_path("memory.top_k"), Some(r#"{"value":"9"}"#)),
        (
            "POST",
            "/api/lightagent/v1/profiles".to_owned(),
            Some(r#"{"id":"scratch"}"#),
        ),
        (
            "DELETE",
            "/api/lightagent/v1/profiles/scratch".to_owned(),
            None,
        ),
    ];
    for (method, path, body) in steps {
        let (status, raw, _) = call(&runtime, method, &path, body);
        assert_eq!(status, 200, "{method} {path}: {raw}");
    }
    drop(runtime);

    let (cli_files, desktop_files) = (files(&by_cli), files(&by_desktop));
    assert_eq!(
        cli_files.keys().collect::<Vec<_>>(),
        desktop_files.keys().collect::<Vec<_>>(),
        "the two homes hold different files"
    );
    for (name, text) in &cli_files {
        assert_eq!(Some(text), desktop_files.get(name), "{name} differs");
    }
}

#[test]
fn a_settings_save_then_a_cli_write_keeps_both() {
    let home = scratch_home("settings-first");
    let runtime = start(&home);
    let (_, _, settings) = call(&runtime, "GET", "/api/lightagent/v1/settings", None);
    let mut save = settings.clone();
    save["expected_revision"] = settings["revision"].clone();
    save["max_turns"] = 6.into();
    let (status, raw, _) = call(
        &runtime,
        "PUT",
        "/api/lightagent/v1/settings",
        Some(&save.to_string()),
    );
    assert_eq!(status, 200, "{raw}");
    // The CLI writes after the Settings save committed: it keeps that save.
    cli_ok(&home, &["config", "set", "memory.top_k", "5"]);
    assert_eq!(cli_ok(&home, &["config", "get", "agent.max_turns"]), "6");
    assert_eq!(cli_ok(&home, &["config", "get", "memory.top_k"]), "5");
    let (_, _, after) = call(&runtime, "GET", "/api/lightagent/v1/settings", None);
    assert_eq!(after["max_turns"], 6, "{after}");
}

#[test]
fn an_unreadable_key_file_fails_the_run_closed_before_any_request() {
    let home = scratch_home("unreadable-key");
    // The configured provider endpoint: it must receive no connection at all.
    let provider = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    provider.set_nonblocking(true).expect("non-blocking");
    let port = provider.local_addr().expect("address").port();
    cli_ok(
        &home,
        &[
            "config",
            "set",
            "inference.base_url",
            &format!("http://127.0.0.1:{port}/v1"),
        ],
    );
    let missing = home.join("missing-inference-key");
    let config_file = home.join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_file).expect("read")).expect("config JSON");
    value["inference"]["api_key"] = serde_json::json!({ "kind": "file", "path": missing });
    std::fs::write(
        &config_file,
        serde_json::to_vec_pretty(&value).expect("JSON"),
    )
    .expect("write");

    let runtime = start(&home);
    let (status, raw, run) = call(
        &runtime,
        "POST",
        "/api/lightagent/v1/runs",
        Some(r#"{"message":"hello"}"#),
    );
    assert_eq!(status, 202, "{raw}");
    let id = run["id"].as_str().expect("run id").to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (_, _, state) = call(
            &runtime,
            "GET",
            &format!("/api/lightagent/v1/runs/{id}"),
            None,
        );
        if state["status"] == "failed" {
            break;
        }
        assert_ne!(state["status"], "completed", "{state}");
        assert!(
            std::time::Instant::now() < deadline,
            "the run never failed: {state}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, events, _) = call(
        &runtime,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}/events"),
        None,
    );
    assert!(events.contains("cannot be read"), "{events}");
    assert!(
        !events.contains("missing-inference-key"),
        "the error names the key file: {events}"
    );
    match provider.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("the provider endpoint was contacted: {other:?}"),
    }
}

#[test]
fn the_api_routing_report_is_the_cli_routing_report() {
    let home = scratch_home("routing-report");
    cli_ok(&home, &["profile", "create", "work"]);
    let runtime = start(&home);
    let (status, raw, report) = call(
        &runtime,
        "GET",
        "/api/lightagent/v1/jev/routing-report",
        None,
    );
    assert_eq!(status, 200, "{raw}");
    let printed = cli_ok(&home, &["--json", "config", "jev-routing-report"]);
    let cli: serde_json::Value = serde_json::from_str(&printed).expect("report JSON");
    assert_eq!(report, cli);
    assert_eq!(report["changes_made"], false);
    assert!(
        report["profiles"]
            .as_array()
            .expect("profiles")
            .iter()
            .any(|row| row["profile"] == "work"),
        "{report}"
    );
}
