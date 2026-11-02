//! Extensions through one shared implementation (D6-1): `lightagent
//! extensions …` and the desktop runtime's API act on the same roots and the
//! same configuration, with the same checks. An install over the API never
//! switches MCP on unasked, a failed install or removal leaves what was
//! installed as it was, and a profile's own extensions stay with that
//! profile. Every process here is separate: the runtime, the CLI and this
//! test.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const EXTENSIONS: &str = "/api/lightagent/v1/extensions";

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
        "lightagent-desktop-extensions-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
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
        .set_read_timeout(Some(Duration::from_secs(120)))
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

fn run_cli(home: &Path, args: &[&str]) -> Output {
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
    output
}

fn cli_ok(home: &Path, args: &[&str]) {
    run_cli(home, args);
}

/// The CLI's `--json` output.
fn cli_json(home: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = run_cli(home, &all);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "lightagent {args:?} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn parsed(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("not JSON ({error}): {raw}"))
}

fn config(home: &Path) -> Value {
    parsed(&std::fs::read_to_string(home.join("config.json")).expect("config.json"))
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

/// A bundle named `name` with one skill, and, with `mcp`, one stdio MCP
/// server whose environment holds a value that must never be shown.
fn bundle(dir: &Path, name: &str, mcp: bool) -> PathBuf {
    let source = dir.join(format!("src-{name}"));
    std::fs::create_dir_all(source.join("skills").join("greet")).expect("bundle");
    let servers = if mcp {
        r#","mcp_servers":[{"transport":"stdio","name":"local","command":"echo-server","args":["--flag"],"env":{"TOKEN":"secret-value"}}]"#
    } else {
        ""
    };
    std::fs::write(
        source.join("extension.json"),
        format!(r#"{{"name":"{name}","version":"1.0.0","description":"A test bundle"{servers}}}"#),
    )
    .expect("manifest");
    std::fs::write(
        source.join("skills").join("greet").join("SKILL.md"),
        "---\nname: greet\ndescription: Say hello\n---\nSay hello.\n",
    )
    .expect("skill");
    source
}

/// Every file under `dir`, by relative path, with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(dir)
                    .expect("relative")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(relative, std::fs::read(&path).expect("read"));
            }
        }
    }
    found
}

/// Hidden entries in an extension root: staging or removal leftovers.
fn leftovers(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default()
}

/// A listing's extensions without their home-specific folders.
fn summaries(listing: &Value) -> Vec<Value> {
    listing["extensions"]
        .as_array()
        .expect("extensions")
        .iter()
        .map(|ext| {
            let mut ext = ext.clone();
            if let Some(fields) = ext.as_object_mut() {
                fields.remove("skills_dir");
            }
            ext
        })
        .collect()
}

#[test]
fn the_cli_and_the_api_install_list_toggle_and_remove_alike() {
    let work = scratch("alike");
    let source = bundle(&work, "greeter", false);
    let (cli_home, api_home) = (work.join("cli"), work.join("api"));
    for home in [&cli_home, &api_home] {
        std::fs::create_dir_all(home).expect("home");
        cli_ok(home, &["config", "set", "extensions.enabled", "true"]);
    }
    let runtime = start(&api_home);

    let by_cli = cli_json(&cli_home, &["extensions", "install", arg(&source)]);
    assert_eq!(by_cli["name"], "greeter", "{by_cli}");
    let body = json!({ "source": source }).to_string();
    let (status, raw) = call(&runtime, "POST", EXTENSIONS, Some(&body));
    assert_eq!(status, 201, "{raw}");
    let by_api = parsed(&raw);
    assert_eq!(by_api["name"], "greeter", "{by_api}");
    assert_eq!(by_api["scope"], "global", "{by_api}");
    assert_eq!(by_api["source"]["kind"], "local", "{by_api}");
    assert_eq!(by_api["mcp_enabled"], false, "{by_api}");

    // The same files (the source record included), settings and listing.
    assert_eq!(
        files(&cli_home.join("extensions").join("greeter")),
        files(&api_home.join("extensions").join("greeter"))
    );
    assert_eq!(
        config(&cli_home)["extensions"],
        config(&api_home)["extensions"]
    );
    assert_eq!(config(&cli_home)["mcp"], config(&api_home)["mcp"]);
    let (status, raw) = call(&runtime, "GET", EXTENSIONS, None);
    assert_eq!(status, 200, "{raw}");
    let listed = parsed(&raw);
    assert_eq!(listed, cli_json(&api_home, &["extensions", "list"]));
    assert_eq!(
        summaries(&cli_json(&cli_home, &["extensions", "list"])),
        summaries(&listed)
    );
    let (status, raw) = call(&runtime, "GET", &format!("{EXTENSIONS}/greeter"), None);
    assert_eq!(status, 200, "{raw}");
    let shown = parsed(&raw);
    let shown_by_cli = cli_json(&api_home, &["extensions", "show", "greeter"]);
    for field in [
        "name",
        "version",
        "description",
        "active",
        "scope",
        "dir",
        "instructions",
        "mcp_servers",
        "mcp",
        "skills",
        "source",
    ] {
        assert_eq!(shown[field], shown_by_cli[field], "{field}");
    }
    assert_eq!(shown["skills"], json!(["greet"]), "{shown}");

    // Switched off through the API, on through the CLI: each sees the other.
    let (status, raw) = call(
        &runtime,
        "PUT",
        &format!("{EXTENSIONS}/greeter/enabled"),
        Some(r#"{"enabled":false}"#),
    );
    assert_eq!(status, 200, "{raw}");
    assert_eq!(parsed(&raw)["active"], false, "{raw}");
    assert_eq!(
        cli_json(&api_home, &["extensions", "list"])["extensions"][0]["active"],
        false
    );
    assert_eq!(
        config(&api_home)["extensions"]["disabled"],
        json!(["greeter"])
    );
    cli_json(&api_home, &["extensions", "enable", "greeter"]);
    let (_, raw) = call(&runtime, "GET", &format!("{EXTENSIONS}/greeter"), None);
    assert_eq!(parsed(&raw)["active"], true, "{raw}");

    // Removed through the API: gone for the CLI too, with nothing left over.
    let (status, raw) = call(&runtime, "DELETE", &format!("{EXTENSIONS}/greeter"), None);
    assert_eq!(status, 200, "{raw}");
    assert_eq!(
        cli_json(&api_home, &["extensions", "list"])["extensions"],
        json!([])
    );
    assert!(!api_home.join("extensions").join("greeter").exists());
    assert!(leftovers(&api_home.join("extensions")).is_empty());
}

#[test]
fn failed_installs_and_removals_leave_what_was_there() {
    let work = scratch("failures");
    let home = work.join("home");
    std::fs::create_dir_all(&home).expect("home");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    let runtime = start(&home);
    let install = |source: &str| {
        let body = json!({ "source": source }).to_string();
        call(&runtime, "POST", EXTENSIONS, Some(&body))
    };
    let installed_dir = home.join("extensions").join("keeper");

    let source = bundle(&work, "keeper", false);
    let (status, raw) = install(arg(&source));
    assert_eq!(status, 201, "{raw}");
    let before = files(&installed_dir);

    // Installing it again, even a newer version, is refused; the installed
    // copy stays exactly as it was.
    std::fs::write(
        source.join("extension.json"),
        r#"{"name":"keeper","version":"2.0.0"}"#,
    )
    .expect("manifest");
    let (status, raw) = install(arg(&source));
    assert_eq!(status, 409, "{raw}");
    assert_eq!(files(&installed_dir), before);

    // Unusable sources are refused before anything is fetched or copied.
    let missing = work.join("no-such-folder");
    for source in [
        "relative/folder",
        arg(&missing),
        "http://example.invalid/tools.git",
        "https://user:secret@example.invalid/tools.git",
        "https://example.invalid/tools.git#../outside",
    ] {
        let (status, raw) = install(source);
        assert_eq!(status, 400, "{source}: {raw}");
        assert!(!raw.contains("secret"), "{raw}");
    }

    // A host that cannot be reached is reported as such.
    let (status, raw) = install("https://127.0.0.1:9/tools.git");
    assert_eq!(status, 502, "{raw}");

    // A bundle with a link that leads out of it is refused.
    #[cfg(unix)]
    {
        let linked = bundle(&work, "linked", false);
        std::os::unix::fs::symlink(&home, linked.join("skills").join("outside")).expect("link");
        let (status, raw) = install(arg(&linked));
        assert_eq!(status, 400, "{raw}");
        assert!(!home.join("extensions").join("linked").exists());
    }

    // Removing, showing or toggling what is not installed, or a bad name.
    assert_eq!(
        call(&runtime, "DELETE", &format!("{EXTENSIONS}/nothing"), None).0,
        404
    );
    assert_eq!(
        call(&runtime, "DELETE", &format!("{EXTENSIONS}/bad.name"), None).0,
        400
    );
    assert_eq!(
        call(&runtime, "GET", &format!("{EXTENSIONS}/nothing"), None).0,
        404
    );
    assert_eq!(
        call(
            &runtime,
            "PUT",
            &format!("{EXTENSIONS}/nothing/enabled"),
            Some(r#"{"enabled":true}"#)
        )
        .0,
        404
    );

    // After all of it: exactly the one extension, as installed, and no
    // staging or removal leftovers.
    let (_, raw) = call(&runtime, "GET", EXTENSIONS, None);
    let listed = parsed(&raw);
    assert_eq!(
        listed["extensions"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    assert_eq!(listed["extensions"][0]["version"], "1.0.0", "{listed}");
    assert_eq!(files(&installed_dir), before);
    assert!(leftovers(&home.join("extensions")).is_empty());
}

#[test]
fn a_profile_install_stays_with_its_profile_and_mcp_waits_to_be_asked() {
    let work = scratch("profile");
    let home = work.join("home");
    std::fs::create_dir_all(&home).expect("home");
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    cli_ok(&home, &["profile", "create", "work"]);
    cli_ok(&home, &["profile", "create", "other"]);
    cli_ok(&home, &["profile", "use", "work"]);
    let runtime = start(&home);
    let source = bundle(&work, "tooled", true);

    let body = json!({ "source": source, "scope": "profile" }).to_string();
    let (status, raw) = call(&runtime, "POST", EXTENSIONS, Some(&body));
    assert_eq!(status, 201, "{raw}");
    let installed = parsed(&raw);
    assert_eq!(installed["scope"], "profile", "{installed}");
    assert_eq!(
        installed["mcp_enabled"], false,
        "an install does not switch MCP on unasked"
    );
    assert_eq!(config(&home)["mcp"]["enabled"], false);
    assert!(!home.join("extensions").join("tooled").exists());

    // Shown before it is trusted: what the server would run, and only the
    // names of its environment variables.
    let (status, raw) = call(&runtime, "GET", &format!("{EXTENSIONS}/tooled"), None);
    assert_eq!(status, 200, "{raw}");
    let shown = parsed(&raw);
    assert_eq!(shown["mcp"][0]["command"], "echo-server", "{shown}");
    assert_eq!(shown["mcp"][0]["env"], json!(["TOKEN"]), "{shown}");
    assert_eq!(shown["mcp_enabled"], false, "{shown}");
    assert!(!raw.contains("secret-value"), "{raw}");
    let shown_by_cli = cli_json(&home, &["extensions", "show", "tooled"]);
    assert!(!shown_by_cli.to_string().contains("secret-value"));

    // Another profile does not load it.
    cli_ok(&home, &["profile", "use", "other"]);
    assert_eq!(
        cli_json(&home, &["extensions", "list"])["extensions"],
        json!([])
    );
    cli_ok(&home, &["profile", "use", "work"]);
    assert_eq!(
        cli_json(&home, &["extensions", "list"])["extensions"][0]["scope"],
        "profile"
    );

    // The global root does not hold it; the profile's does.
    assert_eq!(
        call(
            &runtime,
            "DELETE",
            &format!("{EXTENSIONS}/tooled?scope=global"),
            None
        )
        .0,
        404
    );
    let (status, raw) = call(
        &runtime,
        "DELETE",
        &format!("{EXTENSIONS}/tooled?scope=profile"),
        None,
    );
    assert_eq!(status, 200, "{raw}");

    // MCP is switched on only when asked.
    let body = json!({ "source": source, "activate_mcp": true }).to_string();
    let (status, raw) = call(&runtime, "POST", EXTENSIONS, Some(&body));
    assert_eq!(status, 201, "{raw}");
    assert_eq!(parsed(&raw)["mcp_enabled"], true, "{raw}");
    assert_eq!(config(&home)["mcp"]["enabled"], true);
}
