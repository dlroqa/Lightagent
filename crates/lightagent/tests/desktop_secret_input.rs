//! The desktop's `remote-credential add|rotate --secret-stdin` (D8): a
//! secret carried in the request, accepted only from the desktop host,
//! which read it from a file the person chose natively. The window cannot
//! send one; that half is the host's (desktop/src-tauri/src/secret.rs).

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KEY: &str = "c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
const CREDENTIALS: &str = "/api/lightagent/v1/remote-credentials";

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
        "lightagent-desktop-secret-input-{label}-{}-{nanos}",
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

fn command(home: &Path, key_file: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .env("LIGHTAGENT_HOME", home)
        .env_remove("LIGHTAGENT_CREDENTIAL_KEY")
        .env("LIGHTAGENT_CREDENTIAL_KEY_FILE", key_file);
    command
}

fn start_desktop(home: &Path, key_file: &Path) -> Runtime {
    let mut child = command(home, key_file)
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent serve --desktop");
    let mut lifeline = child.stdin.take().expect("stdin");
    writeln!(lifeline, "{KEY}").expect("key");
    lifeline.flush().expect("flush");
    let handshake: Value = serde_json::from_str(&first_line(&mut child)).expect("handshake JSON");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    Runtime {
        child,
        _lifeline: Some(lifeline),
        addr,
    }
}

/// `lightagent serve` on loopback without a key: a local operator, but not
/// the desktop host.
fn start_open(home: &Path, key_file: &Path) -> Runtime {
    let mut child = command(home, key_file)
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .env_remove("LIGHTAGENT_API_KEY")
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
        .trim()
        .to_owned();
    Runtime {
        child,
        _lifeline: None,
        addr,
    }
}

fn call(runtime: &Runtime, method: &str, path: &str, body: &Value) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let body = body.to_string();
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

fn cli(home: &Path, key_file: &Path, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let output = command(home, key_file)
        .args(&all)
        .stdin(Stdio::null())
        .output()
        .expect("run lightagent");
    assert!(
        output.status.success(),
        "lightagent {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
}

/// Fail if any file under `dir` holds `secret` in plaintext.
fn assert_nowhere(dir: &Path, secret: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            assert_nowhere(&path, secret);
        } else if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(secret),
                "{} holds a secret in plaintext",
                path.display()
            );
        }
    }
}

fn grants(home: &Path, key_file: &Path) -> Vec<Value> {
    cli(home, key_file, &["remote-credential", "list"])["credentials"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_secret_in_the_request_is_taken_only_from_the_desktop_host() {
    let home = scratch("home");
    let outside = scratch("outside");
    let key_file = outside.join("vault.key");
    cli(&home, &key_file, &["profile", "create", "work"]);
    let key_arg = key_file.to_str().expect("UTF-8 path").to_owned();
    cli(&home, &key_file, &["remote-credential", "keygen", &key_arg]);
    let fields = json!({
        "profile": "work",
        "service": "web-search",
        "destinations": ["https://search.example.com"],
    });
    let with = |extra: Value| {
        let mut body = fields.clone();
        for (name, value) in extra.as_object().expect("object") {
            body[name] = value.clone();
        }
        body
    };

    let desktop = start_desktop(&home, &key_file);
    // Added and rotated from the secret itself, as `--secret-stdin` does;
    // no answer carries it.
    let secret = "inline-secret-5c6d7e8f9a0b1c2d";
    let (code, raw) = call(
        &desktop,
        "POST",
        CREDENTIALS,
        &with(json!({"secret": format!("{secret}\n")})),
    );
    assert_eq!(code, 200, "{raw}");
    assert!(
        !raw.contains(secret),
        "the answer carries the secret: {raw}"
    );
    let added: Value = serde_json::from_str(&raw).expect("JSON");
    let id = added["added"].as_str().expect("grant id").to_owned();
    let rotated = "inline-rotated-1a2b3c4d5e6f";
    let rotate = format!("{CREDENTIALS}/{id}/rotate");
    let (code, raw) = call(&desktop, "POST", &rotate, &json!({"secret": rotated}));
    assert_eq!(code, 200, "{raw}");
    assert!(!raw.contains(rotated), "{raw}");
    let listed = grants(&home, &key_file);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["version"], 2, "{listed:?}");

    // A file and a secret together, an invalid secret, and neither: refused,
    // without echoing the secret, and nothing stored.
    let file = outside.join("web.secret");
    std::fs::write(&file, "file-secret-0a1b2c3d\n").expect("secret file");
    let (code, raw) = call(
        &desktop,
        "POST",
        CREDENTIALS,
        &with(json!({"secret": "both-secret-9f8e7d", "secret_file": file})),
    );
    assert_eq!(code, 400, "{raw}");
    assert!(raw.contains("not both"), "{raw}");
    assert!(!raw.contains("both-secret-9f8e7d"), "{raw}");
    let (code, raw) = call(
        &desktop,
        "POST",
        CREDENTIALS,
        &with(json!({"secret": "line-one-secret\nline-two-secret"})),
    );
    assert_eq!(code, 400, "{raw}");
    assert!(
        !raw.contains("line-one-secret") && !raw.contains("line-two-secret"),
        "{raw}"
    );
    let (code, raw) = call(&desktop, "POST", &rotate, &json!({"secret": ""}));
    assert_eq!(code, 400, "{raw}");
    let (code, raw) = call(&desktop, "POST", CREDENTIALS, &fields);
    assert_eq!(code, 400, "{raw}");
    assert_eq!(grants(&home, &key_file).len(), 1);
    drop(desktop);

    // Another local operator is not the desktop host: it must name a file.
    let open = start_open(&home, &key_file);
    let (code, raw) = call(
        &open,
        "POST",
        CREDENTIALS,
        &with(json!({"secret": "open-secret-6a7b8c9d"})),
    );
    assert_eq!(code, 400, "{raw}");
    assert!(raw.contains("only from the desktop host"), "{raw}");
    assert!(!raw.contains("open-secret-6a7b8c9d"), "{raw}");
    let (code, raw) = call(
        &open,
        "POST",
        &rotate,
        &json!({"secret": "open-rotate-6a7b8c9d"}),
    );
    assert_eq!(code, 400, "{raw}");
    let (code, raw) = call(
        &open,
        "POST",
        CREDENTIALS,
        &with(json!({"secret_file": file})),
    );
    assert_eq!(
        code, 200,
        "a secret file still works over the open API: {raw}"
    );
    drop(open);

    assert_eq!(grants(&home, &key_file).len(), 2);
    assert!(
        file.exists(),
        "a secret file is never deleted by the runtime"
    );
    for value in [
        secret,
        rotated,
        "open-secret-6a7b8c9d",
        "both-secret-9f8e7d",
    ] {
        assert_nowhere(&home, value);
    }
}
