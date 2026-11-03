//! Setup (D7-11): `lightagent init` and `lightagent import hermes` through
//! the desktop runtime's API do what the CLI does, and the API answers
//! with the JSON the CLI prints. Every process here is separate.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5";
const API: &str = "/api/lightagent/v1";

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

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lightagent-desktop-setup-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn start(home: &Path, env: &[(&str, &Path)]) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", home)
        .env_remove("HERMES_HOME")
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

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn post(runtime: &Runtime, path: &str, body: Value) -> (u16, Value) {
    let (status, raw) = call(runtime, "POST", path, Some(&body.to_string()));
    (status, parsed(&raw))
}

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .env_remove("HERMES_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent")
}

fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = cli(home, &all);
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

/// The configuration without what differs by home.
fn settings(home: &Path) -> Value {
    let shown = cli_json(home, &["config", "show"]);
    assert!(shown["inference"].is_object(), "{shown}");
    shown
}

fn active(home: &Path) -> Value {
    cli_json(home, &["profile", "list"])
}

const BASE_URL: &str = "http://127.0.0.1:9/v1";

#[test]
fn init_over_the_api_does_what_the_cli_does() {
    // The CLI's home.
    let by_cli = scratch("cli");
    cli_json(&by_cli, &["init"]);
    let cli_report = cli_json(
        &by_cli,
        &[
            "init",
            "--force",
            "--profile",
            "fresh",
            "--base-url",
            BASE_URL,
            "--model",
            "m-test",
        ],
    );

    // The desktop's home: scaffolded by its runtime, initialised over the
    // API, then refused until forced.
    let by_api = scratch("api");
    let runtime = start(&by_api, &[]);
    let (status, first) = post(&runtime, &format!("{API}/setup/init"), json!({}));
    assert_eq!(status, 200, "{first}");
    let (status, refused) = post(&runtime, &format!("{API}/setup/init"), json!({}));
    assert_eq!(status, 409, "{refused}");
    let (status, refused) = post(
        &runtime,
        &format!("{API}/setup/init"),
        json!({"force": true, "profile": "Not Valid"}),
    );
    assert_eq!(status, 400, "{refused}");
    let (status, api_report) = post(
        &runtime,
        &format!("{API}/setup/init"),
        json!({"force": true, "profile": "fresh", "base_url": BASE_URL, "model": "m-test"}),
    );
    assert_eq!(status, 200, "{api_report}");
    for field in ["active_profile", "base_url"] {
        assert!(cli_report[field].is_string(), "{field}: {cli_report}");
        assert_eq!(api_report[field], cli_report[field], "{field}");
    }
    assert_eq!(api_report["home"], arg(&by_api));
    assert_eq!(settings(&by_api), settings(&by_cli));
    let (cli_active, api_active) = (active(&by_cli), active(&by_api));
    assert_eq!(api_active["active"], "fresh", "{api_active}");
    assert_eq!(api_active["active"], cli_active["active"]);
}

/// A Hermes home with one profile and one skill.
fn hermes_home(label: &str) -> PathBuf {
    let hermes = scratch(label);
    let profile = hermes.join("profiles").join("Research Bot");
    std::fs::create_dir_all(&profile).expect("profile dir");
    std::fs::write(profile.join("profile.yaml"), "description: Reads papers\n").expect("profile");
    std::fs::write(profile.join("SOUL.md"), "You read papers carefully.\n").expect("persona");
    std::fs::write(
        profile.join("config.yaml"),
        format!("model:\n  default: paper-model\n  base_url: {BASE_URL}\n"),
    )
    .expect("config");
    let skill = hermes.join("skills").join("summarize");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: summarize\ndescription: Summarize a text\n---\nSummarize the text.\n",
    )
    .expect("skill");
    hermes
}

#[test]
fn a_hermes_import_over_the_api_is_the_clis() {
    let hermes = hermes_home("hermes");
    let home = scratch("import");
    let runtime = start(&home, &[]);
    let import = format!("{API}/import/hermes");
    let from = arg(&hermes);

    // Refused: a relative or a missing home.
    let (status, _) = post(&runtime, &import, json!({"from": "relative/hermes"}));
    assert_eq!(status, 400);
    let missing = hermes.join("missing");
    let (status, _) = post(&runtime, &import, json!({"from": missing}));
    assert_eq!(status, 400);

    // A dry run writes nothing and equals the CLI's.
    let (status, dry) = post(&runtime, &import, json!({"from": from, "dry_run": true}));
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["profiles"], json!(["research-bot"]), "{dry}");
    assert_eq!(dry["skills"], json!(["summarize"]), "{dry}");
    assert_eq!(
        dry,
        cli_json(&home, &["import", "hermes", "--from", from, "--dry-run"])
    );
    let listed = cli_json(&home, &["profile", "list"]).to_string();
    assert!(
        !listed.contains("research-bot"),
        "a dry run wrote: {listed}"
    );

    // Applied over the API: the CLI sees the profile with its persona.
    let (status, applied) = post(&runtime, &import, json!({"from": from}));
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["profiles"], json!(["research-bot"]), "{applied}");
    // The persona lives in SOUL.md and only the text form shows it.
    let output = cli(&home, &["profile", "show", "research-bot"]);
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{shown}");
    assert!(shown.contains("You read papers carefully."), "{shown}");
    assert_eq!(
        cli_json(&home, &["profile", "show", "research-bot"])["description"],
        "Reads papers"
    );

    // Again: skipped, alike for the API and the CLI; forced: imported.
    let (_, again) = post(&runtime, &import, json!({"from": from}));
    assert_eq!(
        again["skipped"].as_array().map(Vec::len),
        Some(2),
        "{again}"
    );
    assert_eq!(
        again,
        cli_json(&home, &["import", "hermes", "--from", from])
    );
    let (_, forced) = post(&runtime, &import, json!({"from": from, "force": true}));
    assert_eq!(forced["profiles"], json!(["research-bot"]), "{forced}");
    assert_eq!(forced["skipped"], json!([]), "{forced}");
    drop(runtime);

    // HERMES_HOME is the default Hermes home, for the runtime as for the CLI.
    let runtime = start(&home, &[("HERMES_HOME", hermes.as_path())]);
    let (status, by_default) = post(&runtime, &import, json!({"dry_run": true}));
    assert_eq!(status, 200, "{by_default}");
    assert_eq!(by_default["hermes_home"], from, "{by_default}");
}

/// Every file under `dir`, by relative path, with its bytes.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("read file");
                let relative = path.strip_prefix(dir).expect("under dir").to_path_buf();
                found.push((relative, bytes));
            }
        }
    }
    found.sort();
    found
}

/// `lightagent --json import hermes <args>` with `env` set and HERMES_HOME
/// otherwise unset: whether it succeeded, and what it printed.
fn cli_import(home: &Path, env: &[(&str, &Path)], args: &[&str]) -> (bool, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .args(["--json", "import", "hermes"])
        .args(args)
        .env("LIGHTAGENT_HOME", home)
        .env_remove("HERMES_HOME")
        .stdin(Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().expect("run lightagent");
    let printed = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    (output.status.success(), printed)
}

#[test]
fn hermes_home_names_the_source_and_lightagent_home_the_destination() {
    let named = hermes_home("named-hermes");
    let explicit = hermes_home("explicit-hermes");
    let user = scratch("user-home");
    let fallback = user.join(".hermes").join("profiles").join("Home Bot");
    std::fs::create_dir_all(&fallback).expect("fallback profile");
    std::fs::write(fallback.join("SOUL.md"), "You live in ~/.hermes.\n").expect("persona");
    let home = scratch("homes");
    let import = format!("{API}/import/hermes");
    let before = snapshot(&named);
    let env = [("HERMES_HOME", named.as_path()), ("HOME", user.as_path())];

    // With no folder, HERMES_HOME is read, before ~/.hermes; a folder given
    // comes first. The runtime and the CLI agree.
    let runtime = start(&home, &env);
    let (status, dry) = post(&runtime, &import, json!({"dry_run": true}));
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["hermes_home"], arg(&named), "{dry}");
    assert_eq!(dry["profiles"], json!(["research-bot"]), "{dry}");
    let (succeeded, by_cli) = cli_import(&home, &env, &["--dry-run"]);
    assert!(succeeded, "{by_cli}");
    assert_eq!(dry, by_cli);
    let (status, given) = post(
        &runtime,
        &import,
        json!({"from": explicit, "dry_run": true}),
    );
    assert_eq!(status, 200, "{given}");
    assert_eq!(given["hermes_home"], arg(&explicit), "{given}");

    // Applied: written into LIGHTAGENT_HOME, which stays the runtime's
    // home; the Hermes home is only read.
    let (status, applied) = post(&runtime, &import, json!({}));
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["profiles"], json!(["research-bot"]), "{applied}");
    let listed = cli_json(&home, &["profile", "list"]).to_string();
    assert!(listed.contains("research-bot"), "{listed}");
    assert!(
        snapshot(&named) == before,
        "the import changed the Hermes home"
    );
    let (status, raw) = call(&runtime, "GET", &format!("{API}/diagnostics/doctor"), None);
    assert_eq!(status, 200, "{raw}");
    let doctor = parsed(&raw);
    let shown = PathBuf::from(doctor["home"].as_str().expect("doctor home"));
    assert_eq!(
        shown.canonicalize().expect("shown home"),
        home.canonicalize().expect("home"),
        "{doctor}"
    );
    drop(runtime);

    // Without HERMES_HOME: ~/.hermes.
    let runtime = start(&home, &[("HOME", user.as_path())]);
    let (status, dry) = post(&runtime, &import, json!({"dry_run": true}));
    assert_eq!(status, 200, "{dry}");
    assert_eq!(dry["hermes_home"], arg(&user.join(".hermes")), "{dry}");
    assert_eq!(dry["profiles"], json!(["home-bot"]), "{dry}");
    drop(runtime);

    // A HERMES_HOME naming no folder is refused, with nothing written, and
    // ~/.hermes is not tried instead.
    let missing = named.join("missing");
    let unusable = [("HERMES_HOME", missing.as_path()), ("HOME", user.as_path())];
    let runtime = start(&home, &unusable);
    let (status, refused) = post(&runtime, &import, json!({"dry_run": true}));
    assert_eq!(status, 400, "{refused}");
    assert!(refused.to_string().contains("no Hermes home"), "{refused}");
    let (succeeded, _) = cli_import(&home, &unusable, &["--dry-run"]);
    assert!(!succeeded, "the CLI refuses as well");
    let listed = cli_json(&home, &["profile", "list"]).to_string();
    assert!(!listed.contains("home-bot"), "{listed}");
    drop(runtime);

    // An empty HERMES_HOME is a value, not unset: it is refused as well.
    #[cfg(unix)]
    {
        let empty = [("HERMES_HOME", Path::new("")), ("HOME", user.as_path())];
        let runtime = start(&home, &empty);
        let (status, refused) = post(&runtime, &import, json!({"dry_run": true}));
        assert_eq!(status, 400, "{refused}");
        drop(runtime);
    }
}
