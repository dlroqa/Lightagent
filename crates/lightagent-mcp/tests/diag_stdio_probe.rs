//! Diagnostic probe (branch diag/windows-mcp-timeout only; never merged).
//!
//! The stdio MCP path of
//! `chat::model_tests::installed_mcp_tool_is_listed_callable_and_removed`,
//! instrumented: the same server script, the same `python3 server.py`
//! spawn from the server's own directory, the same 30 s request timeout and
//! the same initialize / tools/list / tools/call sequence. The script also
//! records its own phases, errors and stderr to files in its directory, and
//! the probe checks the process is gone after the client is dropped. Nothing
//! here changes the code under test.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lightagent_mcp::client::McpClient;
use lightagent_mcp::connection::StdioConnection;
use serde_json::json;

/// The original fixture's protocol answers, plus phase records.
const SERVER: &str = r#"
import json, os, sys, time, traceback, atexit
_trace = open("trace.log", "a", buffering=1)
def note(event):
    _trace.write("%.6f %d %s\n" % (time.time(), os.getpid(), event))
sys.stderr = open("stderr.log", "a", buffering=1)
note("start version=%s" % sys.version.split()[0])
atexit.register(lambda: note("exit"))
try:
    for line in sys.stdin:
        msg = json.loads(line)
        method = msg.get("method")
        note("recv " + str(method))
        if "id" not in msg:
            continue
        if method == "initialize":
            result = {"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
        elif method == "tools/list":
            result = {"tools":[{"name":"where","description":"Read extension asset","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":True}}]}
        elif method == "tools/call":
            result = {"content":[{"type":"text","text":open("marker.txt").read()}]}
        else:
            result = {}
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":result}), flush=True)
        note("sent " + str(method))
    note("stdin closed")
except BaseException:
    note("error " + traceback.format_exc().replace("\n", " | "))
    raise
"#;

fn wall() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn alive(pid: &str) -> bool {
    if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains(pid))
            .unwrap_or(false)
    } else {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn read(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.chars().take(4_000).collect()
}

/// One spawn, handshake, list, call and drop. Returns (succeeded, report).
async fn probe(label: String) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-mcp-probe-{}-{label}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("server.py"), SERVER).unwrap();
    std::fs::write(dir.join("marker.txt"), "installed directory").unwrap();

    let spawned_wall = wall();
    let started = Instant::now();
    let mut phases = Vec::new();
    let mut last = "spawn";
    let mut ok = false;
    match StdioConnection::spawn_in(
        "python3",
        &["server.py".to_owned()],
        &[],
        Some(&dir),
        Duration::from_secs(30),
    )
    .await
    {
        Err(error) => phases.push(format!("spawn error {error}")),
        Ok(connection) => {
            phases.push(format!("spawned {:?}", started.elapsed()));
            let client = McpClient::new(Box::new(connection));
            last = "initialize";
            match client.initialize().await {
                Err(error) => phases.push(format!("initialize error {error} at {:?}", started.elapsed())),
                Ok(()) => {
                    phases.push(format!("initialized {:?}", started.elapsed()));
                    last = "tools/list";
                    match client.list_tools().await {
                        Err(error) => phases.push(format!("list error {error} at {:?}", started.elapsed())),
                        Ok(defs) => {
                            phases.push(format!("listed {} at {:?}", defs.len(), started.elapsed()));
                            last = "tools/call";
                            match client.call_tool("where", json!({})).await {
                                Err(error) => phases.push(format!("call error {error} at {:?}", started.elapsed())),
                                Ok(_) => {
                                    phases.push(format!("called {:?}", started.elapsed()));
                                    last = "done";
                                    ok = true;
                                }
                            }
                        }
                    }
                }
            }
            drop(client);
        }
    }
    let dropped = Instant::now();
    let trace = read(&dir.join("trace.log"));
    let stderr = read(&dir.join("stderr.log"));
    // Python start: from our spawn call to the script's first record.
    let start_line = trace.lines().find(|line| line.contains(" start "));
    let python_start = start_line
        .and_then(|line| line.split(' ').next())
        .and_then(|stamp| stamp.parse::<f64>().ok())
        .map(|stamp| format!("{:.3}s", stamp - spawned_wall))
        .unwrap_or_else(|| "never recorded".to_owned());
    let pid = start_line
        .and_then(|line| line.split(' ').nth(1))
        .map(str::to_owned);
    // Cleanup: is the server gone within 5 s of the drop?
    let mut cleanup = "no pid".to_owned();
    if let Some(pid) = &pid {
        cleanup = "still running after 5s".to_owned();
        for _ in 0..50 {
            if !alive(pid) {
                cleanup = format!("gone after {:?}", dropped.elapsed());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let mut report = format!(
        "probe {label}: {} last phase {last}; python start {python_start}; {}; cleanup {cleanup}",
        if ok { "OK" } else { "FAILED" },
        phases.join(", ")
    );
    if !ok {
        report.push_str(&format!(
            "\n  server trace:\n    {}\n  server stderr:\n    {}",
            trace.trim().replace('\n', "\n    "),
            if stderr.trim().is_empty() { "(empty)".to_owned() } else { stderr.trim().replace('\n', "\n    ") }
        ));
    }
    std::fs::remove_dir_all(&dir).ok();
    (ok, report)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "diagnostic: run by the diagnostic workflow"]
async fn stdio_probe() {
    let setting = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let rounds = setting("DIAG_ROUNDS", 10);
    let parallel = setting("DIAG_PARALLEL", 4);
    let mode = std::env::var("DIAG_MODE").unwrap_or_else(|_| "idle".to_owned());
    let mut failures = 0;
    let mut total = 0;
    for round in 0..rounds {
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..parallel {
            tasks.spawn(probe(format!("{mode}-{round}-{index}")));
        }
        while let Some(result) = tasks.join_next().await {
            let (ok, report) = result.unwrap();
            total += 1;
            if !ok {
                failures += 1;
            }
            println!("{report}");
        }
    }
    let summary = format!("### stdio probe ({mode}): {failures} failed of {total}\n");
    println!("{summary}");
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(summary.as_bytes()).unwrap();
    }
    assert_eq!(failures, 0, "{failures} of {total} probes failed");
}
