//! Diagnostic (branch diag/windows-mcp-timeout only; never merged).
//!
//! `uninstall` while a process still runs from the extension's directory, as
//! an MCP server does while it exits after `/extensions uninstall`, and then
//! again once that process is gone. The property checked: an interrupted
//! uninstall can still be completed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "diagnostic: run by the diagnostic workflow"]
fn an_interrupted_uninstall_can_still_be_completed() {
    let scratch = std::env::temp_dir().join(format!(
        "lightagent-uninstall-diag-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = scratch.join("source");
    let root = scratch.join("extensions");
    std::fs::create_dir_all(&source).unwrap();
    // The bundle of chat::model_tests::installed_mcp_tool_is_listed_callable_and_removed.
    std::fs::write(
        source.join("extension.json"),
        r#"{"name":"fixture","mcp_servers":[{"transport":"stdio","name":"fixture","command":"python3","args":["server.py"]}]}"#,
    )
    .unwrap();
    std::fs::write(source.join("marker.txt"), "installed directory").unwrap();
    std::fs::write(source.join("server.py"), "import sys\n").unwrap();
    lightagent_extensions::install(&source, &root).unwrap();
    let dir = root.join("fixture");

    // A process whose working directory is the installed bundle.
    let mut holder = std::process::Command::new("python3")
        .args(["-c", "import time; time.sleep(3)"])
        .current_dir(&dir)
        .spawn()
        .expect("python3");
    std::thread::sleep(Duration::from_millis(500));
    let first = lightagent_extensions::uninstall("fixture", &root);
    let files_after_first: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let dir_after_first = dir.exists();
    holder.wait().unwrap();

    let second = lightagent_extensions::uninstall("fixture", &root);
    let report = format!(
        "os {}: first uninstall (process running from the directory): {first:?}; \
         directory left: {dir_after_first}, files left: {files_after_first:?}; \
         second uninstall (process gone): {second:?}; directory left: {}",
        std::env::consts::OS,
        dir.exists()
    );
    println!("{report}");
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{report}").unwrap();
    }
    std::fs::remove_dir_all(&scratch).ok();
    assert!(
        first.is_ok() || second.is_ok(),
        "an interrupted uninstall cannot be completed: {report}"
    );
}
