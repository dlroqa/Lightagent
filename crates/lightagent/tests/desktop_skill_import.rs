//! Skills imported from a folder (D6-6, new scope): each supported layout
//! installs as an inactive extension until it is enabled; refused imports
//! install nothing; a replace that fails keeps the installed copy. Every
//! process here is separate. (Fetching from a real Git host is not
//! exercised here: CI takes no network dependency on one.)

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "92a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";
const IMPORTS: &str = "/api/lightagent/v1/skills/imports";

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
        "lightagent-desktop-skill-import-{label}-{}-{nanos}",
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn write_skill(dir: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nDo it.\n"),
    )
    .expect("SKILL.md");
}

fn import(runtime: &Runtime, body: Value) -> (u16, String) {
    call(runtime, "POST", IMPORTS, Some(&body.to_string()))
}

fn setup(label: &str) -> (PathBuf, Runtime) {
    let home = scratch_home(label);
    cli_ok(&home, &["config", "set", "extensions.enabled", "true"]);
    let runtime = start(&home);
    (home, runtime)
}

fn skill_names(runtime: &Runtime) -> Vec<String> {
    let (status, raw) = call(runtime, "GET", "/api/lightagent/v1/skills", None);
    assert_eq!(status, 200, "{raw}");
    parsed(&raw)["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .filter_map(|skill| skill["name"].as_str().map(str::to_owned))
        .collect()
}

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

#[test]
fn each_supported_layout_imports_inactive_until_enabled() {
    let (home, runtime) = setup("layouts");
    let sources = home.join("sources");
    write_skill(&sources.join("solo"), "solo-skill", "One skill");
    write_skill(&sources.join("pack").join("skills").join("a"), "a", "A");
    write_skill(&sources.join("pack").join("skills").join("b"), "b", "B");
    write_skill(&sources.join("flat").join("x"), "x", "X");
    write_skill(&sources.join("flat").join("y"), "y", "Y");
    write_skill(&sources.join("ext").join("skills").join("z"), "z", "Z");
    std::fs::write(
        sources.join("ext").join("extension.json"),
        r#"{"name":"packed"}"#,
    )
    .expect("manifest");

    let (status, raw) = import(&runtime, json!({ "source": sources.join("solo") }));
    assert_eq!(status, 201, "{raw}");
    let imported = parsed(&raw);
    assert_eq!(imported["name"], "solo", "{imported}");
    assert_eq!(imported["layout"], "single_skill", "{imported}");
    assert_eq!(imported["skills"], json!(["solo-skill"]), "{imported}");
    assert_eq!(imported["active"], false, "installed inactive unless asked");
    assert_eq!(imported["source"]["kind"], "local", "{imported}");
    assert!(!skill_names(&runtime).contains(&"solo-skill".to_owned()));
    let listed = cli_json(&home, &["extensions", "list"]);
    assert_eq!(listed["extensions"][0]["name"], "solo", "{listed}");
    assert_eq!(listed["extensions"][0]["active"], false, "{listed}");
    cli_ok(&home, &["extensions", "enable", "solo"]);
    assert!(skill_names(&runtime).contains(&"solo-skill".to_owned()));

    let (status, raw) = import(
        &runtime,
        json!({ "source": sources.join("pack"), "activate": true }),
    );
    assert_eq!(status, 201, "{raw}");
    let imported = parsed(&raw);
    assert_eq!(imported["layout"], "skills_folder", "{imported}");
    assert_eq!(imported["skills"], json!(["a", "b"]), "{imported}");
    assert_eq!(imported["active"], true, "{imported}");
    let names = skill_names(&runtime);
    assert!(
        names.contains(&"a".to_owned()) && names.contains(&"b".to_owned()),
        "{names:?}"
    );

    let (status, raw) = import(&runtime, json!({ "source": sources.join("flat") }));
    assert_eq!(status, 201, "{raw}");
    assert_eq!(parsed(&raw)["layout"], "skill_folders", "{raw}");

    let (status, raw) = import(&runtime, json!({ "source": sources.join("ext") }));
    assert_eq!(status, 201, "{raw}");
    let imported = parsed(&raw);
    assert_eq!(imported["layout"], "extension", "{imported}");
    assert_eq!(imported["name"], "packed", "{imported}");
    assert_eq!(
        parsed(&std::fs::read_to_string(home.join("config.json")).expect("config"))["mcp"]["enabled"],
        false
    );
    assert!(leftovers(&home.join("extensions")).is_empty());
}

#[test]
fn refused_imports_install_nothing() {
    let (home, runtime) = setup("refused");
    let sources = home.join("sources");
    std::fs::create_dir_all(sources.join("readme")).expect("dir");
    std::fs::write(sources.join("readme").join("README.md"), "no skills").expect("readme");
    std::fs::create_dir_all(sources.join("badname")).expect("dir");
    std::fs::write(
        sources.join("badname").join("SKILL.md"),
        "---\nname: bad name\ndescription: x\n---\nx\n",
    )
    .expect("SKILL.md");
    write_skill(&sources.join("twins").join("one"), "same", "1");
    write_skill(&sources.join("twins").join("two"), "same", "2");
    write_skill(&sources.join("fine"), "fine", "Fine");

    let refused = [
        json!({ "source": sources.join("readme") }),
        json!({ "source": sources.join("badname") }),
        json!({ "source": sources.join("twins") }),
        json!({ "source": sources.join("fine"), "revision": "main" }),
        json!({ "source": sources.join("fine"), "name": "bad.name" }),
        json!({ "source": "relative/folder" }),
        json!({ "source": sources.join("missing") }),
    ];
    for body in refused {
        let (status, raw) = import(&runtime, body.clone());
        assert_eq!(status, 400, "{body}: {raw}");
    }
    let (_, raw) = import(&runtime, json!({ "source": sources.join("readme") }));
    assert!(
        raw.contains("SKILL.md"),
        "the refusal says what was looked for: {raw}"
    );

    #[cfg(unix)]
    {
        write_skill(&sources.join("linked"), "linked", "Linked");
        std::os::unix::fs::symlink(&home, sources.join("linked").join("outside")).expect("link");
        let (status, raw) = import(&runtime, json!({ "source": sources.join("linked") }));
        assert_eq!(status, 400, "{raw}");
    }

    assert_eq!(
        cli_json(&home, &["extensions", "list"])["extensions"],
        json!([])
    );
    assert!(leftovers(&home.join("extensions")).is_empty());
}

#[test]
fn a_replace_keeps_the_installed_copy_when_it_fails() {
    let (home, runtime) = setup("replace");
    let sources = home.join("sources");
    write_skill(&sources.join("kit").join("skills").join("k"), "k", "one");
    std::fs::create_dir_all(sources.join("broken")).expect("dir");
    std::fs::write(sources.join("broken").join("README.md"), "nothing").expect("readme");
    let installed = home.join("extensions").join("kit");

    assert_eq!(
        import(&runtime, json!({ "source": sources.join("kit") })).0,
        201
    );
    let before = files(&installed);
    let (status, raw) = import(&runtime, json!({ "source": sources.join("kit") }));
    assert_eq!(status, 409, "without replace: {raw}");
    let (status, raw) = import(
        &runtime,
        json!({ "source": sources.join("broken"), "name": "kit", "replace": true }),
    );
    assert_eq!(status, 400, "{raw}");
    assert_eq!(
        files(&installed),
        before,
        "a failed replace changed the installed copy"
    );

    write_skill(&sources.join("kit").join("skills").join("k"), "k", "two");
    let (status, raw) = import(
        &runtime,
        json!({ "source": sources.join("kit"), "replace": true }),
    );
    assert_eq!(status, 201, "{raw}");
    assert_eq!(parsed(&raw)["replaced"], true, "{raw}");
    let text = std::fs::read_to_string(installed.join("skills").join("k").join("SKILL.md"))
        .expect("SKILL.md");
    assert!(text.contains("two"), "{text}");
    assert!(leftovers(&home.join("extensions")).is_empty());
}
