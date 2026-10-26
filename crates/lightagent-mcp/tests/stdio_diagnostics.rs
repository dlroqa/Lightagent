//! A stdio server that exits is told apart from one that is slow. A request
//! to a server that has closed its output fails at once with why, its exit
//! status and the end of its stderr (sanitized); a server that stays silent
//! still times out exactly as before. Skipped when `python3` is unavailable.

use std::time::{Duration, Instant};

use lightagent_mcp::McpError;
use lightagent_mcp::connection::{Connection, StdioConnection};
use serde_json::json;

fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

// The server writes to stderr and exits before answering: the request fails
// well inside its timeout, says the server closed its output, carries the end
// of its stderr, and shows neither the variable it was given nor a token.
#[tokio::test]
async fn a_server_that_exits_is_reported_at_once_with_its_stderr() {
    if !python_available() {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let script = r#"import os, sys; sys.stderr.write("starting\n"); sys.stderr.write("could not connect with token=abc123 using " + os.environ["FIXTURE_SECRET"] + "\n"); sys.stderr.flush(); sys.exit(3)"#;
    let connection = StdioConnection::spawn(
        "python3",
        &["-c".to_owned(), script.to_owned()],
        &[("FIXTURE_SECRET".to_owned(), "s3cr3t-value".to_owned())],
        Duration::from_secs(20),
    )
    .await
    .expect("spawn");
    let started = Instant::now();
    let error = connection
        .request("initialize", json!({}))
        .await
        .expect_err("the server exited without answering");
    let elapsed = started.elapsed();
    let text = error.to_string();
    println!("reported after {elapsed:?}: {text}");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}: {text}");
    assert!(text.contains("closed its output"), "{text}");
    assert!(text.contains("starting"), "{text}");
    assert!(text.contains("could not connect"), "{text}");
    assert!(!text.contains("abc123"), "{text}");
    assert!(!text.contains("s3cr3t-value"), "{text}");
}

// A server that is alive but never answers still times out, as before.
#[tokio::test]
async fn a_silent_server_still_times_out() {
    if !python_available() {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let connection = StdioConnection::spawn(
        "python3",
        &["-c".to_owned(), "import time; time.sleep(60)".to_owned()],
        &[],
        Duration::from_secs(2),
    )
    .await
    .expect("spawn");
    let started = Instant::now();
    let error = connection
        .request("initialize", json!({}))
        .await
        .expect_err("a silent server never answers");
    assert!(matches!(error, McpError::Timeout), "{error}");
    assert!(started.elapsed() >= Duration::from_secs(2));
}

/// Run `script` as a server that writes to stderr and exits, and return how
/// its first request failed.
async fn reported(script: &str, env: &[(String, String)]) -> String {
    let connection = StdioConnection::spawn(
        "python3",
        &["-c".to_owned(), script.to_owned()],
        env,
        Duration::from_secs(20),
    )
    .await
    .expect("spawn");
    let error = connection
        .request("initialize", json!({}))
        .await
        .expect_err("the server exited without answering");
    let text = error.to_string();
    println!("reported: {text}");
    text
}

// Output that reaches the client in pieces is put back together before it
// is sanitized: a credential written in two flushed parts, and a variable's
// value split across writes, are both redacted.
#[tokio::test]
async fn output_split_across_writes_is_redacted_whole() {
    if !python_available() {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let script = r#"import os, sys, time; s = os.environ["FIXTURE_SECRET"]; sys.stderr.write("auth tok"); sys.stderr.flush(); time.sleep(0.3); sys.stderr.write("en=abc123 rest\nusing " + s[:4]); sys.stderr.flush(); time.sleep(0.3); sys.stderr.write(s[4:] + " ok\n"); sys.stderr.flush(); sys.exit(5)"#;
    let text = reported(
        script,
        &[("FIXTURE_SECRET".to_owned(), "s3cr3t-value".to_owned())],
    )
    .await;
    assert!(text.contains("auth token=[redacted]"), "{text}");
    assert!(text.contains("using [redacted] ok"), "{text}");
    assert!(!text.contains("abc123"), "{text}");
    assert!(!text.contains("s3cr3t-value"), "{text}");
    assert!(!text.contains("s3cr"), "{text}");
}

// Only the last 20 lines are kept, and a line longer than 4 KiB is omitted
// whole: no part of it is shown.
#[tokio::test]
async fn stderr_is_bounded_to_its_last_lines_and_long_lines_are_omitted() {
    if !python_available() {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let script = r#"import sys; [sys.stderr.write("L%02d\n" % i) for i in range(30)]; sys.stderr.write("x" * 5000 + " TAILMARK\n"); sys.stderr.flush(); sys.exit(4)"#;
    let text = reported(script, &[]).await;
    for kept in ["L11", "L29", "[a longer line was omitted]"] {
        assert!(text.contains(kept), "{kept} missing: {text}");
    }
    for dropped in ["L00", "L10", "TAILMARK", "xxxxxxxxxx"] {
        assert!(!text.contains(dropped), "{dropped} shown: {text}");
    }
}

// A credential that reaches the client across a read boundary (after a long
// line) is still redacted.
#[tokio::test]
async fn a_credential_across_a_read_boundary_is_redacted() {
    if !python_available() {
        eprintln!("python3 unavailable; skipping");
        return;
    }
    let script = r#"import sys; sys.stderr.write("z" * 1015 + "\n" + "login password=hunter22 failed\n"); sys.stderr.flush(); sys.exit(6)"#;
    let text = reported(script, &[]).await;
    assert!(text.contains("login password=[redacted]"), "{text}");
    assert!(!text.contains("hunter22"), "{text}");
}
