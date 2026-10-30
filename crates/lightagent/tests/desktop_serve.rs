//! `lightagent serve --desktop` driven as the desktop host drives it: the key
//! on standard input, one handshake line, the guard on every route (the event
//! stream and `/health` included), and exit once standard input closes
//! (docs/desktop/DESKTOP_ARCHITECTURE.md §4, §6). Raw HTTP over TCP, so the
//! test controls every header a browser or another process could send.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KEY: &str = "4f1c2b3a5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7a8";

fn scratch_home() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "lightagent-desktop-serve-{}-{nanos}",
        std::process::id()
    ))
}

/// Send one raw request with `Connection: close`; return the status and the
/// whole response.
fn request(addr: &str, head: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect to the runtime");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    stream
        .write_all(format!("{head}Connection: close\r\n\r\n").as_bytes())
        .expect("send");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, response)
}

#[test]
fn the_desktop_runtime_answers_only_its_host_and_exits_with_it() {
    let home = scratch_home();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        .env("LIGHTAGENT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start lightagent serve --desktop");
    let mut lifeline = child.stdin.take().expect("stdin");
    writeln!(lifeline, "{KEY}").expect("hand over the key");
    lifeline.flush().expect("flush the key");

    let stdout = child.stdout.take().expect("stdout");
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = ready_tx.send(line);
    });
    let line = ready_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("a handshake line within 60 s");
    let handshake: serde_json::Value = serde_json::from_str(&line).expect("the handshake is JSON");
    assert_eq!(handshake["lightagent"], "serve");
    assert_eq!(handshake["protocol"], 1);
    assert_eq!(handshake["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(handshake["profile"], "default");
    let addr = handshake["addr"].as_str().expect("addr").to_owned();
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    assert!(!line.contains(KEY), "the handshake must not repeat the key");

    let auth = format!("Authorization: Bearer {KEY}\r\n");
    let get = |path: &str, headers: &str| {
        request(
            &addr,
            &format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{headers}"),
        )
    };

    // No key, a wrong key: refused, on ordinary routes, /health and the stream.
    for path in [
        "/health",
        "/api/lightagent/v1/sessions",
        "/api/lightagent/v1/runs/none/events",
    ] {
        let (status, body) = get(path, "");
        assert_eq!(status, 401, "{path}: {body}");
        assert!(body.contains("unauthenticated"), "{body}");
    }
    assert_eq!(
        get(
            "/api/lightagent/v1/sessions",
            "Authorization: Bearer wrong\r\n"
        )
        .0,
        401
    );
    assert_eq!(get("/api/lightagent/v1/sessions?key=1", "").0, 401);

    // Another Host (DNS rebinding), even with the key: refused first.
    for host in ["localhost", "evil.example", "127.0.0.1"] {
        let (status, body) = request(
            &addr,
            &format!("GET /api/lightagent/v1/sessions HTTP/1.1\r\nHost: {host}\r\n{auth}"),
        );
        assert_eq!(status, 403, "{host}: {body}");
        assert!(body.contains("host_rejected"), "{body}");
    }

    // A browser request (Origin, cross-site fetch metadata), even with the key.
    for headers in [
        format!("{auth}Origin: http://evil.example\r\n"),
        format!("{auth}Origin: http://{addr}\r\n"),
        format!("{auth}Sec-Fetch-Site: cross-site\r\n"),
    ] {
        let (status, body) = get("/api/lightagent/v1/sessions", &headers);
        assert_eq!(status, 403, "{headers}: {body}");
        assert!(body.contains("origin_rejected"), "{body}");
    }

    // The desktop host itself is admitted.
    assert_eq!(get("/health", &auth).0, 200);
    let (status, body) = get("/api/lightagent/v1/sessions", &auth);
    assert_eq!(status, 200, "{body}");
    let status = get("/api/lightagent/v1/runs/none/events", &auth).0;
    assert!(
        status != 401 && status != 403,
        "the stream route must pass the guard: {status}"
    );
    // No Web UI is served in desktop mode.
    assert_eq!(get("/", &auth).0, 404);

    // Closing standard input is the host going away: the runtime must follow.
    drop(lifeline);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().expect("poll the runtime") {
            assert!(status.success(), "the runtime exited with {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the runtime must exit within 15 s of its host going away"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn desktop_mode_refuses_a_missing_key_and_other_binds() {
    for (args, stdin) in [
        (vec!["serve", "--desktop", "--port", "0"], ""),
        (
            vec!["serve", "--desktop", "--host", "0.0.0.0", "--port", "0"],
            KEY,
        ),
    ] {
        let home = scratch_home();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lightagent"))
            .args(&args)
            .env("LIGHTAGENT_HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start");
        if let Some(mut input) = child.stdin.take() {
            let _ = writeln!(input, "{stdin}");
        }
        let output = child.wait_with_output().expect("wait");
        assert!(!output.status.success(), "{args:?} must be refused");
        assert!(output.stdout.is_empty(), "no handshake for {args:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
