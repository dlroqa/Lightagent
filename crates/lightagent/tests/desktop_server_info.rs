//! `GET /api/lightagent/v1/server` (D8 remote mode): what a server is, for
//! the desktop host before it sends a server work. Behind the key; the
//! desktop handshake reports the same capabilities.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const KEY: &str = "server-info-key-6d7e8f9a0b1c";
const LAUNCH: &str = "d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5";

struct Runtime {
    child: Child,
    _lifeline: Option<ChildStdin>,
    addr: String,
    first: String,
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
        "lightagent-server-info-{label}-{}-{nanos}",
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

fn start(home: &Path, desktop: bool) -> Runtime {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lightagent"));
    command
        .env("LIGHTAGENT_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if desktop {
        command
            .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
            .stdin(Stdio::piped());
    } else {
        command
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--key-env",
                "LIGHTAGENT_TEST_SERVER_INFO_KEY",
            ])
            .env("LIGHTAGENT_TEST_SERVER_INFO_KEY", KEY)
            .stdin(Stdio::null());
    }
    let mut child = command.spawn().expect("start lightagent serve");
    let lifeline = if desktop {
        let mut lifeline = child.stdin.take().expect("stdin");
        writeln!(lifeline, "{LAUNCH}").expect("key");
        lifeline.flush().expect("flush");
        Some(lifeline)
    } else {
        None
    };
    let first = first_line(&mut child);
    let addr = if desktop {
        let handshake: Value = serde_json::from_str(&first).expect("handshake");
        handshake["addr"].as_str().expect("addr").to_owned()
    } else {
        first
            .split("http://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_else(|| panic!("no address in {first:?}"))
            .trim()
            .to_owned()
    };
    Runtime {
        child,
        _lifeline: lifeline,
        addr,
        first,
    }
}

fn get(runtime: &Runtime, path: &str, key: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(&runtime.addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let auth = key
        .map(|key| format!("Authorization: Bearer {key}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\n{auth}Connection: close\r\n\r\n",
        runtime.addr
    );
    stream.write_all(head.as_bytes()).expect("send");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn a_server_says_what_it_is_only_to_a_caller_with_its_key() {
    let home = scratch("home");
    // The desktop runtime first: it sets the home up, as a plain `serve`
    // needs (an active profile).
    let desktop = start(&home, true);
    let handshake: Value = serde_json::from_str(&desktop.first).expect("handshake");
    let (code, raw) = get(&desktop, "/api/lightagent/v1/server", Some(LAUNCH));
    assert_eq!(code, 200, "{raw}");
    assert_eq!(
        serde_json::from_str::<Value>(&raw).expect("JSON")["version"],
        handshake["version"]
    );
    drop(desktop);

    let keyed = start(&home, false);
    for key in [None, Some("not-the-key")] {
        let (code, raw) = get(&keyed, "/api/lightagent/v1/server", key);
        assert!(matches!(code, 401 | 403), "{key:?}: {code} {raw}");
    }
    let (code, raw) = get(&keyed, "/api/lightagent/v1/server", Some(KEY));
    assert_eq!(code, 200, "{raw}");
    assert!(!raw.contains(KEY), "{raw}");
    let info: Value = serde_json::from_str(&raw).expect("JSON");
    assert_eq!(info["service"], "lightagent", "{info}");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"), "{info}");
    assert_eq!(info["protocol"], 1, "{info}");
    assert!(info["profile"].is_string(), "{info}");
    drop(keyed);
    assert_eq!(
        handshake["capabilities"], info["capabilities"],
        "the handshake and the server agree"
    );
}
