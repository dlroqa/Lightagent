//! Launching and owning the bundled runtime.
//!
//! The host starts exactly one binary: the `lightagent-sidecar` shipped next
//! to its own executable (never one found on `PATH`). The per-launch key is a
//! fresh 256-bit value from the OS generator, written once as the first line
//! of the child's stdin; it never appears in argv, the environment, a URL, a
//! file, a log line, or the UI. The host then holds stdin open as the child's
//! lifeline: however the host ends, even killed, the pipe closes and the
//! runtime exits. The child answers with one JSON handshake line on stdout,
//! and is refused unless its protocol and version match this build.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

/// How long a starting runtime may take to report ready.
pub const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a stopping runtime may take after its lifeline closes.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Lines of runtime output kept in memory for diagnostics.
const LOG_LINES: usize = 2_000;

/// The one line a runtime prints when it is ready.
#[derive(Clone, Debug, Deserialize)]
pub struct Handshake {
    pub lightagent: String,
    pub protocol: u32,
    pub version: String,
    pub addr: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Why a runtime did not become usable.
#[derive(Debug)]
pub enum LaunchError {
    Failed(String),
    Mismatch { expected: String, found: String },
}

/// A running, owned runtime.
pub struct Sidecar {
    child: Child,
    lifeline: Option<ChildStdin>,
    key: String,
    pub handshake: Handshake,
    pub logs: Arc<Mutex<VecDeque<String>>>,
}

impl Sidecar {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.handshake.addr)
    }

    pub fn authorization(&self) -> String {
        format!("Bearer {}", self.key)
    }

    /// How the runtime ended, if it has: the host's crash detection.
    pub fn exit_status(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            Ok(None) => None,
            Err(error) => Some(format!("unknown ({error})")),
        }
    }

    /// Stop the runtime this host started, and nothing else.
    pub fn stop(&mut self) {
        // Closing the lifeline asks the runtime to finish; kill only if it
        // has not within the grace period.
        drop(self.lifeline.take());
        let deadline = std::time::Instant::now() + STOP_GRACE;
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop();
        // Overwrite the key before its memory is released.
        let len = self.key.len();
        self.key.clear();
        self.key.extend(std::iter::repeat_n('0', len));
    }
}

/// The bundled runtime: next to the host's own executable.
pub fn bundled_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("cannot locate the app: {error}"))?;
    let dir = exe.parent().ok_or("the app has no directory")?;
    let name = if cfg!(windows) {
        "lightagent-sidecar.exe"
    } else {
        "lightagent-sidecar"
    };
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "the bundled runtime is missing ({name} next to the app)"
        ))
    }
}

fn new_key() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "the OS random source is unavailable".to_owned())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The environment changes that keep an AppImage's own search paths out of
/// the runtime and every program it starts. An AppImage's AppRun points
/// variables such as PYTHONHOME, PYTHONPATH, LD_LIBRARY_PATH and PATH at the
/// image (`appdir`); a Python MCP server that inherits them cannot start.
/// Each variable naming a path under `appdir` loses those entries, and is
/// removed (`None`) when none is left; PATH then gets the standard search
/// path instead.
#[cfg(target_os = "linux")]
fn appimage_env_changes(
    appdir: &Path,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, Option<String>)> {
    let under = appdir.to_string_lossy().into_owned();
    let mut changes = Vec::new();
    if under.is_empty() {
        return changes;
    }
    for (name, value) in vars {
        let Some(text) = value.to_str() else {
            continue;
        };
        if !text.contains(under.as_str()) {
            continue;
        }
        let kept: Vec<&str> = text
            .split(':')
            .filter(|entry| !entry.is_empty() && !Path::new(entry).starts_with(appdir))
            .collect();
        let value = if !kept.is_empty() {
            Some(kept.join(":"))
        } else if name == "PATH" {
            Some("/usr/local/bin:/usr/bin:/bin".to_owned())
        } else {
            None
        };
        changes.push((name, value));
    }
    changes
}

#[cfg(all(test, target_os = "linux"))]
mod appimage_env {
    use super::appimage_env_changes;
    use std::ffi::OsString;
    use std::path::Path;

    fn vars(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    #[test]
    fn an_appimage_s_search_paths_do_not_reach_the_runtime() {
        let changes = appimage_env_changes(
            Path::new("/tmp/.mount_la"),
            vars(&[
                ("PATH", "/tmp/.mount_la/usr/bin/:/usr/local/bin:/usr/bin"),
                ("PYTHONHOME", "/tmp/.mount_la/usr/"),
                ("PYTHONPATH", "/tmp/.mount_la/usr/share/pyshared/:"),
                ("LD_LIBRARY_PATH", "/tmp/.mount_la/usr/lib/:/opt/lib"),
                ("LANG", "C.UTF-8"),
            ]),
        );
        let find = |wanted: &str| {
            changes
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            find("PATH"),
            Some(Some("/usr/local/bin:/usr/bin".to_owned()))
        );
        assert_eq!(find("PYTHONHOME"), Some(None));
        assert_eq!(find("PYTHONPATH"), Some(None));
        assert_eq!(find("LD_LIBRARY_PATH"), Some(Some("/opt/lib".to_owned())));
        assert_eq!(find("LANG"), None);
    }

    #[test]
    fn a_path_left_with_nothing_gets_the_standard_search_path() {
        let changes = appimage_env_changes(
            Path::new("/tmp/.mount_la"),
            vars(&[("PATH", "/tmp/.mount_la/usr/bin")]),
        );
        assert_eq!(
            changes,
            vec![(
                OsString::from("PATH"),
                Some("/usr/local/bin:/usr/bin:/bin".to_owned())
            )]
        );
    }

    #[test]
    fn a_prefix_that_only_looks_alike_is_kept() {
        let changes = appimage_env_changes(
            Path::new("/tmp/.mount_la"),
            vars(&[("PATH", "/tmp/.mount_la/usr/bin:/tmp/.mount_lab/bin")]),
        );
        assert_eq!(
            changes,
            vec![(
                OsString::from("PATH"),
                Some("/tmp/.mount_lab/bin".to_owned())
            )]
        );
    }
}

/// Start `binary` as an owned runtime for `profile` and wait for its handshake.
pub fn launch(
    binary: &Path,
    profile: Option<&str>,
    expected_version: &str,
) -> Result<Sidecar, LaunchError> {
    let key = new_key().map_err(LaunchError::Failed)?;
    let mut command = Command::new(binary);
    command
        .args(["serve", "--desktop", "--host", "127.0.0.1", "--port", "0"])
        // A remote server's key is the host's (remote.rs); the bundled
        // runtime never gets it, even when this app was started with it.
        .env_remove("LIGHTAGENT_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // Inside an AppImage, its search paths stay with the host: the runtime
    // and the programs it starts get the person's own environment.
    #[cfg(target_os = "linux")]
    {
        if let Some(appdir) = std::env::var_os("APPDIR").filter(|dir| !dir.is_empty()) {
            for (name, value) in appimage_env_changes(Path::new(&appdir), std::env::vars_os()) {
                match value {
                    Some(value) => {
                        command.env(name, value);
                    }
                    None => {
                        command.env_remove(name);
                    }
                }
            }
        }
    }
    let mut child = command
        .spawn()
        .map_err(|error| LaunchError::Failed(format!("could not start the runtime: {error}")))?;

    // Deliver the key, then keep stdin open as the runtime's lifeline.
    let lifeline = child.stdin.take().and_then(|mut stdin| {
        stdin
            .write_all(format!("{key}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .ok()
            .map(|()| stdin)
    });
    let Some(lifeline) = lifeline else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(LaunchError::Failed(
            "could not hand the runtime its credential".to_owned(),
        ));
    };

    let logs = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_LINES)));
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        let logs = Arc::clone(&logs);
        let key = key.clone();
        std::thread::spawn(move || pump(stdout, &logs, &key, Some(ready_tx)));
    }
    if let Some(stderr) = child.stderr.take() {
        let logs = Arc::clone(&logs);
        let key = key.clone();
        std::thread::spawn(move || pump(stderr, &logs, &key, None));
    }

    let line = match ready_rx.recv_timeout(READY_TIMEOUT) {
        Ok(line) => line,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::Failed(match error {
                mpsc::RecvTimeoutError::Timeout => format!(
                    "the runtime did not report ready within {} s",
                    READY_TIMEOUT.as_secs()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    "the runtime exited before reporting ready".to_owned()
                }
            }));
        }
    };
    let handshake: Handshake = match serde_json::from_str(&line) {
        Ok(handshake) => handshake,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::Failed(
                "the runtime's ready line is not a handshake".to_owned(),
            ));
        }
    };
    let mut sidecar = Sidecar {
        child,
        lifeline: Some(lifeline),
        key,
        handshake,
        logs,
    };
    if sidecar.handshake.lightagent != "serve"
        || sidecar.handshake.protocol != crate::contract::PROTOCOL
    {
        let found = format!(
            "{} protocol {}",
            sidecar.handshake.lightagent, sidecar.handshake.protocol
        );
        sidecar.stop();
        return Err(LaunchError::Mismatch {
            expected: format!("serve protocol {}", crate::contract::PROTOCOL),
            found,
        });
    }
    if sidecar.handshake.version != expected_version {
        let found = sidecar.handshake.version.clone();
        sidecar.stop();
        return Err(LaunchError::Mismatch {
            expected: expected_version.to_owned(),
            found,
        });
    }
    if !is_loopback_addr(&sidecar.handshake.addr) {
        sidecar.stop();
        return Err(LaunchError::Failed(
            "the runtime is not bound to 127.0.0.1".to_owned(),
        ));
    }
    Ok(sidecar)
}

fn is_loopback_addr(addr: &str) -> bool {
    addr.strip_prefix("127.0.0.1:")
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Copy one output stream into the bounded log, redacting the key should it
/// ever appear, and hand the first stdout line to the launcher.
fn pump(
    stream: impl Read,
    logs: &Mutex<VecDeque<String>>,
    key: &str,
    mut ready: Option<mpsc::Sender<String>>,
) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let line = line.replace(key, "<redacted>");
        if let Some(sender) = ready.take() {
            let _ = sender.send(line.clone());
        }
        if let Ok(mut logs) = logs.lock() {
            if logs.len() == LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_fresh_and_256_bit() {
        let a = new_key().expect("key");
        let b = new_key().expect("key");
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn only_loopback_addresses_are_accepted() {
        assert!(is_loopback_addr("127.0.0.1:49152"));
        for addr in [
            "0.0.0.0:80",
            "127.0.0.1:",
            "localhost:80",
            "10.0.0.1:80",
            "127.0.0.1:80/x",
        ] {
            assert!(!is_loopback_addr(addr), "{addr}");
        }
    }
}
