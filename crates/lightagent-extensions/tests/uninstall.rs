//! Uninstalling is repeatable and never removes what it has not verified:
//! an extension still in use is left installed and intact, an interrupted
//! removal can be finished, and missing metadata never permits deleting
//! anything else.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use lightagent_extensions::{ExtensionStore, install, uninstall};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-uninstall-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Install an extension named `fixture` with an MCP server declaration,
/// returning the extension root.
fn installed(scratch: &Path) -> PathBuf {
    let source = scratch.join("source");
    let root = scratch.join("extensions");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("extension.json"),
        r#"{"name":"fixture","mcp_servers":[{"transport":"stdio","name":"fixture","command":"python3","args":["server.py"]}]}"#,
    )
    .unwrap();
    std::fs::write(source.join("server.py"), "import sys\n").unwrap();
    std::fs::write(source.join("marker.txt"), "installed directory").unwrap();
    install(&source, &root).unwrap();
    root
}

fn loaded(root: &Path) -> usize {
    ExtensionStore::load(&[root.to_path_buf()]).len()
}

/// A process whose working directory is `dir`, as an extension's MCP server
/// runs from its bundle. It is a real process holding the directory until it
/// is killed.
fn holding(dir: &Path) -> Child {
    // Started directly, so killing it ends the only process holding `dir`.
    let mut command = if cfg!(windows) {
        let mut command = Command::new("ping");
        command.args(["-n", "60", "127.0.0.1"]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("60");
        command
    };
    command
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

// The defect seen on Windows: uninstalling while the extension's MCP server
// was still exiting deleted every file, then failed on the directory, and no
// later uninstall could finish. Now nothing is removed while the directory is
// in use, and the extension uninstalls once it is released.
#[test]
fn an_extension_in_use_is_left_intact_and_uninstalls_once_released() {
    let scratch = scratch("in-use");
    let root = installed(&scratch);
    let dir = root.join("fixture");
    let mut holder = holding(&dir);
    let first = uninstall("fixture", &root);
    if cfg!(windows) {
        let error = first.expect_err("a directory in use cannot be removed on Windows");
        assert!(error.contains("left installed"), "{error}");
        for file in ["extension.json", "server.py", "marker.txt"] {
            assert!(dir.join(file).is_file(), "{file} was removed: {error}");
        }
        assert_eq!(loaded(&root), 1, "the extension is still installed");
    } else {
        first.expect("a directory in use can be removed here");
    }
    holder.kill().unwrap();
    holder.wait().unwrap();
    if cfg!(windows) {
        uninstall("fixture", &root).expect("uninstalled once released");
    }
    assert!(!dir.exists());
    assert_eq!(loaded(&root), 0);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0, "nothing is left");
    std::fs::remove_dir_all(&scratch).ok();
}

// An earlier, interrupted removal left the directory empty and without its
// manifest: uninstalling the name again finishes it.
#[test]
fn an_empty_directory_left_without_its_manifest_is_finished() {
    let scratch = scratch("empty");
    let root = installed(&scratch);
    for file in ["extension.json", "server.py", "marker.txt"] {
        std::fs::remove_file(root.join("fixture").join(file)).unwrap();
    }
    uninstall("fixture", &root).expect("the interrupted removal is finished");
    assert!(!root.join("fixture").exists());
    std::fs::remove_dir_all(&scratch).ok();
}

// Missing metadata never permits deleting anything: entries left without a
// manifest are reported as a partial removal and kept.
#[test]
fn entries_left_without_a_manifest_are_reported_and_kept() {
    let scratch = scratch("partial");
    let root = installed(&scratch);
    std::fs::remove_file(root.join("fixture/extension.json")).unwrap();
    let error = uninstall("fixture", &root).unwrap_err();
    assert!(error.contains("partially removed"), "{error}");
    assert!(root.join("fixture/server.py").is_file());
    assert!(root.join("fixture/marker.txt").is_file());
    std::fs::remove_dir_all(&scratch).ok();
}

// A removal interrupted after its extension was renamed aside is finished by
// uninstalling the name again, and the renamed directory is never loaded.
#[test]
fn a_removal_interrupted_after_the_rename_is_finished_and_never_loaded() {
    let scratch = scratch("renamed");
    let root = installed(&scratch);
    let aside = root.join(".fixture.uninstalling");
    std::fs::rename(root.join("fixture"), &aside).unwrap();
    assert_eq!(loaded(&root), 0, "a directory being removed is not loaded");
    uninstall("fixture", &root).expect("the interrupted removal is finished");
    assert!(!aside.exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir_all(&scratch).ok();
}

// Once removed, the name is reported as not installed, however often it is
// asked.
#[test]
fn uninstalling_twice_reports_not_installed() {
    let scratch = scratch("twice");
    let root = installed(&scratch);
    uninstall("fixture", &root).unwrap();
    for _ in 0..2 {
        let error = uninstall("fixture", &root).unwrap_err();
        assert!(error.contains("not installed"), "{error}");
    }
    std::fs::remove_dir_all(&scratch).ok();
}

// Names never reach outside the root, and links (or junctions) are never
// followed or removed, whether in place of the extension or of the directory
// an interrupted removal left.
#[test]
fn nothing_outside_the_root_is_removed() {
    let scratch = scratch("confined");
    let root = installed(&scratch);
    let outside = scratch.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("extension.json"),
        r#"{"name":"elsewhere"}"#,
    )
    .unwrap();
    for name in ["../outside", "..", "", "a/b", "a\\b", ".fixture.uninstalling"] {
        assert!(uninstall(name, &root).is_err(), "{name:?} was accepted");
    }
    let linked = |link: &Path| -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, link).is_ok()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(&outside)
                .stdout(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        }
    };
    if linked(&root.join("elsewhere")) {
        assert!(uninstall("elsewhere", &root).is_err());
    }
    if linked(&root.join(".gone.uninstalling")) {
        assert!(uninstall("gone", &root).is_err());
    }
    assert!(outside.join("extension.json").is_file(), "the outside directory was touched");
    std::fs::remove_dir_all(&scratch).ok();
}
