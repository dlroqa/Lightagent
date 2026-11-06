//! Session writes from separate processes: the desktop runtime and the CLI
//! take one lock on a profile's session store and change a session as it is
//! saved at that moment, so neither loses the other's change. A deletion is
//! ordered the same way, and no later write brings a deleted session back.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEY: &str = "5e4f30211203f4e5d6c7b8a99a8b7c6d5e4f30211203f4e5d6c7b8a99a8b7c6d";

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
        "lightagent-session-writes-{label}-{}-{nanos}",
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

/// One authenticated request to the runtime at `addr`: the status and the
/// body's JSON.
fn call(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    let body = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    let json = response
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Where the runtime saved session `id`.
fn session_file(home: &Path, id: &str) -> PathBuf {
    std::fs::read_dir(home.join("profiles"))
        .expect("profiles")
        .flatten()
        .map(|entry| entry.path().join("sessions").join(format!("{id}.json")))
        .find(|path| path.is_file())
        .expect("the session file")
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).expect("read")).expect("session JSON")
}

#[test]
fn a_rename_in_the_runtime_and_a_cli_delete_wait_for_another_processs_lock() {
    let home = scratch_home("lock");
    let runtime = start(&home);
    let (status, created) = call(
        &runtime.addr,
        "POST",
        "/api/lightagent/v1/sessions",
        Some("{}"),
    );
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().expect("id").to_owned();
    let path = session_file(&home, &id);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_file_name(".lock"))
        .expect("lock file");

    // A rename from the desktop runtime waits while another process holds the
    // lock, then keeps the turn that process wrote inside it.
    lock.lock().expect("hold the session store's lock");
    let rename = {
        let addr = runtime.addr.clone();
        let route = format!("/api/lightagent/v1/sessions/{id}");
        std::thread::spawn(move || {
            call(
                &addr,
                "PATCH",
                &route,
                Some(r#"{"title":"Renamed in Settings"}"#),
            )
        })
    };
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !rename.is_finished(),
        "the runtime wrote while another process held the lock"
    );
    let mut value = read_json(&path);
    value["messages"] = serde_json::json!([
        { "role": "user", "content": "from the terminal" },
        { "role": "assistant", "content": "kept" }
    ]);
    std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("JSON")).expect("write");
    lock.unlock().expect("release");
    let (status, renamed) = rename.join().expect("join");
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["message_count"], 2);
    let saved = read_json(&path);
    assert_eq!(saved["title"], "Renamed in Settings");
    assert_eq!(saved["messages"][1]["content"], "kept");

    // A deletion from the CLI waits the same way, and a later write from the
    // runtime does not bring the session back.
    lock.lock().expect("hold the lock again");
    let delete = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["sessions", "delete", &id])
        .env("LIGHTAGENT_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI deletion");
    let mut delete = delete;
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        delete.try_wait().expect("poll").is_none(),
        "the CLI deleted while another process held the lock"
    );
    assert!(path.is_file());
    lock.unlock().expect("release");
    let output = delete.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!path.exists());
    let (status, _) = call(
        &runtime.addr,
        "PATCH",
        &format!("/api/lightagent/v1/sessions/{id}"),
        Some(r#"{"title":"Too late"}"#),
    );
    assert_eq!(status, 404);
    assert!(
        !path.exists(),
        "a write after the deletion re-created the session"
    );
}
