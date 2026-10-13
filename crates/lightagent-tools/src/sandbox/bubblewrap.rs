//! An **experimental** Linux backend that runs programs under bubblewrap.
//!
//! Its assurance is [`Assurance::Experimental`]: it really launches programs
//! behind namespaces, but its guarantees have not been independently
//! verified, so no [`SandboxGate`](super::SandboxGate) ever routes a run to
//! it, and it refuses untrusted contexts itself. Nothing in Lightagent
//! constructs it outside tests, and no setting selects it.
//!
//! How a program is launched:
//!
//! - `bwrap` is spawned directly with an argument vector. No shell ever
//!   builds or runs the command, and request values are only ever single
//!   arguments after the options or after `--`.
//! - The binary must be a root-owned, non-writable, **non-setuid** regular
//!   file at a canonical absolute path. A setuid `bwrap` is refused, not used
//!   as a fallback. The backend refuses to run as root.
//! - New user, mount, PID, network, IPC, UTS and cgroup namespaces; a new
//!   session; `--die-with-parent`; every capability dropped; nested user
//!   namespaces disabled where `bwrap` supports it.
//! - The filesystem is built from nothing: `/usr` (and the merged-`/usr`
//!   links or directories) read-only, the loader cache, a fresh `/proc`,
//!   `/dev` and `/tmp`, the per-run `/scratch`, and each granted root of the
//!   run's own profile under `/work/<label>`, read-only unless granted
//!   read-write. No home directory, `/etc`, `/run`, `/var` or socket.
//! - The child's environment is exactly `PATH`, `HOME=/scratch` and the
//!   run's granted secrets, passed through the process environment (never
//!   the command line, where other host processes could read them).
//! - Wall clock and output are bounded here; on timeout or cancellation the
//!   outer `bwrap` is killed, which kills the namespace's init and with it
//!   every process in the sandbox.
//!
//! What it does **not** provide is listed by [`unsupported`]: the sandbox
//! runs as the server's own host user id (an unprivileged user namespace
//! maps to it), there are no kernel CPU, memory or process limits, the
//! per-run scratch is not size-bounded, and a hard link placed in a granted
//! root *on the host* reaches its target.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::ProfileId;
use lightagent_core::identity::RunTrust;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{
    Access, Assurance, BackendFault, Capabilities, ExecOutput, ExecRequest, ExecutionContext,
    Guarantee, SandboxBackend, SandboxLease,
};

/// The trusted location of `bwrap` on Debian, Ubuntu and Fedora.
pub const DEFAULT_BWRAP: &str = "/usr/bin/bwrap";

/// Where granted roots, the per-run scratch and the program search path sit
/// inside the sandbox.
const WORK: &str = "/work";
const SCRATCH: &str = "/scratch";
const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Size of the sandbox's `/tmp`, when `bwrap` can bound a tmpfs.
pub const TMPFS_BYTES: u64 = 16 * 1024 * 1024;

/// How long the host waits for the output pipes after the process is gone.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The guarantees this backend implements and its live tests exercise. Not
/// verified: they are relied on by nothing.
pub fn implemented() -> BTreeSet<Guarantee> {
    [
        Guarantee::FilesystemRoots,
        Guarantee::ReadOnlyRoots,
        Guarantee::HostCredentialIsolation,
        Guarantee::CleanEnvironment,
        Guarantee::ProcessTreeCleanup,
        Guarantee::EgressDenyByDefault,
        Guarantee::PrivateNetworkBlock,
        Guarantee::DnsRebindingProtection,
        Guarantee::SecretGrantsOnly,
    ]
    .into_iter()
    .collect()
}

/// The guarantees this backend does not provide, and so never claims.
pub fn unsupported() -> BTreeSet<Guarantee> {
    let implemented = implemented();
    Guarantee::ALL
        .into_iter()
        .filter(|guarantee| !implemented.contains(guarantee))
        .collect()
}

/// Why the backend is unavailable on this host. Codes only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// The server runs as root; the backend refuses.
    RunningAsRoot,
    /// `bwrap` is missing, not root-owned, writable by others, setuid, or not
    /// at a canonical absolute path.
    UntrustedBinary,
    /// The kernel offers no user namespaces.
    NoUserNamespaces,
    /// A probe sandbox could not be launched.
    LaunchFailed,
    /// The scratch base is not an absolute, private directory.
    ScratchUnavailable,
}

/// What the host offers, for diagnostics. Holds no host path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HostReport {
    pub kernel: Option<String>,
    pub max_user_namespaces: Option<u64>,
    /// Debian's switch; absent on other kernels.
    pub unprivileged_userns_clone: Option<u64>,
    /// Ubuntu's AppArmor restriction of unprivileged user namespaces.
    pub apparmor_restrict_unprivileged_userns: Option<u64>,
    pub running_as_root: bool,
    pub bwrap_trusted: bool,
    pub bwrap_version: Option<String>,
    /// `bwrap --size` (a bounded tmpfs).
    pub tmpfs_size_supported: bool,
    /// `bwrap --disable-userns` (no nested user namespaces).
    pub disable_userns_supported: bool,
    pub cgroup_v2: bool,
    /// Whether this process could create a child cgroup to account a
    /// sandbox's CPU, memory and processes. Reported only: the backend uses
    /// no cgroup, so those limits are unsupported either way.
    pub cgroup_delegated: bool,
    /// `None` when a probe sandbox launched.
    pub launch: Option<Unavailable>,
    /// A classification of a failed launch, never its text.
    pub launch_detail: Option<&'static str>,
}

/// Configuration supplied by the operator, never by a request.
#[derive(Clone)]
pub struct BubblewrapConfig {
    pub bwrap: PathBuf,
    /// A private directory per-run scratch directories are created in.
    pub scratch_base: PathBuf,
    /// The host directory behind each root label, per profile. A run can
    /// only ever mount its own profile's entries.
    pub roots: BTreeMap<(ProfileId, String), PathBuf>,
    /// Absolute paths, inside the sandbox, of the only programs it runs.
    /// There is no production list; tests supply fixed synthetic ones.
    pub programs: BTreeSet<String>,
    /// Resolves a granted secret name to its value.
    pub secrets: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

/// The read-only system layout, as found on this host.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SystemEntry {
    /// `/name` is a symbolic link to `target` (merged `/usr`).
    Link { name: String, target: PathBuf },
    /// `/name` is a real directory, bound read-only.
    Dir { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Layout {
    system: Vec<SystemEntry>,
    loader_cache: bool,
    tmpfs_size: bool,
    disable_userns: bool,
}

impl Layout {
    fn detect(report: &HostReport) -> Self {
        let mut system = Vec::new();
        for name in ["bin", "sbin", "lib", "lib32", "lib64", "libx32"] {
            let host = Path::new("/").join(name);
            match std::fs::read_link(&host) {
                Ok(target) => system.push(SystemEntry::Link {
                    name: name.to_owned(),
                    target,
                }),
                Err(_) if host.is_dir() => system.push(SystemEntry::Dir {
                    name: name.to_owned(),
                }),
                Err(_) => {}
            }
        }
        Self {
            system,
            loader_cache: Path::new("/etc/ld.so.cache").is_file(),
            tmpfs_size: report.tmpfs_size_supported,
            disable_userns: report.disable_userns_supported,
        }
    }
}

fn arg(value: impl Into<OsString>) -> OsString {
    value.into()
}

/// The fixed part of every `bwrap` command line: namespaces, privileges and
/// the read-only system. Nothing here comes from a request.
fn base_args(layout: &Layout) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--unshare-user",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-uts",
        "--unshare-cgroup",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--hostname",
        "sandbox",
        "--ro-bind",
        "/usr",
        "/usr",
    ]
    .into_iter()
    .map(arg)
    .collect();
    if layout.disable_userns {
        args.push(arg("--disable-userns"));
    }
    for entry in &layout.system {
        match entry {
            SystemEntry::Link { name, target } => {
                args.extend([
                    arg("--symlink"),
                    arg(target.as_os_str()),
                    arg(format!("/{name}")),
                ]);
            }
            SystemEntry::Dir { name } => {
                args.extend([
                    arg("--ro-bind"),
                    arg(format!("/{name}")),
                    arg(format!("/{name}")),
                ]);
            }
        }
    }
    if layout.loader_cache {
        args.extend(["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"].map(arg));
    }
    args.extend(["--proc", "/proc", "--dev", "/dev"].map(arg));
    if layout.tmpfs_size {
        args.extend([arg("--size"), arg(TMPFS_BYTES.to_string())]);
    }
    args.extend(["--tmpfs", "/tmp", "--dir", WORK].map(arg));
    args
}

/// A path relative to a granted root, with no `..`, root or prefix.
fn relative_ok(path: &str) -> bool {
    !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// The per-execution part: the run's scratch, its granted roots, the working
/// directory and the program, each a separate argument.
fn exec_args(
    layout: &Layout,
    scratch: &Path,
    mounts: &[(String, Access, PathBuf)],
    request: &ExecRequest,
) -> Vec<OsString> {
    let mut args = base_args(layout);
    args.extend([arg("--bind"), arg(scratch.as_os_str()), arg(SCRATCH)]);
    for (label, access, host) in mounts {
        let flag = match access {
            Access::ReadOnly => "--ro-bind",
            Access::ReadWrite => "--bind",
        };
        args.extend([
            arg(flag),
            arg(host.as_os_str()),
            arg(format!("{WORK}/{label}")),
        ]);
    }
    let mut cwd = PathBuf::from(WORK).join(&request.root);
    if let Some(relative) = &request.cwd {
        cwd.push(relative);
    }
    args.extend([arg("--chdir"), arg(cwd.into_os_string()), arg("--")]);
    args.push(arg(&request.program));
    args.extend(request.args.iter().map(arg));
    args
}

fn read_number(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The effective user id of this process, from the owner of `/proc/self`.
fn effective_uid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|meta| meta.uid())
}

/// A root-owned, non-writable, non-setuid executable at a canonical path,
/// in a root-owned directory nobody else can write.
fn trusted_binary(path: &Path) -> bool {
    let canonical =
        path.is_absolute() && std::fs::canonicalize(path).is_ok_and(|resolved| resolved == path);
    let safe = |meta: &std::fs::Metadata| meta.uid() == 0 && meta.mode() & 0o022 == 0;
    canonical
        && std::fs::symlink_metadata(path).is_ok_and(|meta| {
            meta.file_type().is_file()
                && safe(&meta)
                && meta.mode() & 0o6000 == 0
                && meta.mode() & 0o111 != 0
        })
        && path
            .parent()
            .and_then(|dir| std::fs::metadata(dir).ok())
            .is_some_and(|meta| safe(&meta))
}

fn classify_launch(stderr: &str) -> &'static str {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("uid map") || lower.contains("setting up uid") {
        "uid_map_denied"
    } else if lower.contains("creating new namespace") || lower.contains("operation not permitted")
    {
        "namespace_denied"
    } else if lower.contains("permission denied") {
        "permission_denied"
    } else {
        "other"
    }
}

async fn output_of(bwrap: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(bwrap)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

/// Inspect the host and try to launch a probe sandbox.
pub async fn probe(bwrap: &Path) -> HostReport {
    let mut report = HostReport {
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|text| text.trim().to_owned()),
        max_user_namespaces: read_number("/proc/sys/user/max_user_namespaces"),
        unprivileged_userns_clone: read_number("/proc/sys/kernel/unprivileged_userns_clone"),
        apparmor_restrict_unprivileged_userns: read_number(
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        ),
        running_as_root: effective_uid().is_none_or(|uid| uid == 0),
        bwrap_trusted: trusted_binary(bwrap),
        cgroup_v2: Path::new("/sys/fs/cgroup/cgroup.controllers").is_file(),
        ..HostReport::default()
    };
    report.cgroup_delegated = report.cgroup_v2
        && std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
            })
            .and_then(|path| {
                let dir = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
                std::fs::metadata(dir).ok()
            })
            .zip(effective_uid())
            .is_some_and(|(meta, uid)| meta.uid() == uid);
    if report.running_as_root {
        report.launch = Some(Unavailable::RunningAsRoot);
        return report;
    }
    if !report.bwrap_trusted {
        report.launch = Some(Unavailable::UntrustedBinary);
        return report;
    }
    if report.max_user_namespaces.is_none_or(|max| max == 0)
        || report.unprivileged_userns_clone == Some(0)
    {
        report.launch = Some(Unavailable::NoUserNamespaces);
        return report;
    }
    report.bwrap_version = output_of(bwrap, &["--version"])
        .await
        .map(|text| text.trim().to_owned());
    let help = output_of(bwrap, &["--help"]).await.unwrap_or_default();
    report.tmpfs_size_supported = help.contains("--size");
    report.disable_userns_supported = help.contains("--disable-userns");

    let mut args = base_args(&Layout::detect(&report));
    args.extend(["--chdir", "/", "--", "/usr/bin/true"].map(arg));
    let launched = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(bwrap)
            .args(&args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match launched {
        Ok(Ok(output)) if output.status.success() => {}
        Ok(Ok(output)) => {
            report.launch = Some(Unavailable::LaunchFailed);
            report.launch_detail = Some(classify_launch(&String::from_utf8_lossy(&output.stderr)));
        }
        _ => {
            report.launch = Some(Unavailable::LaunchFailed);
            report.launch_detail = Some("no_result");
        }
    }
    report
}

/// The experimental bubblewrap backend.
pub struct BubblewrapBackend {
    config: BubblewrapConfig,
    layout: Layout,
    report: HostReport,
    next: AtomicU64,
    leases: Mutex<HashMap<u64, PathBuf>>,
}

impl BubblewrapBackend {
    /// Probe the host and build the backend, or say why it is unavailable.
    pub async fn detect(config: BubblewrapConfig) -> Result<Self, (Unavailable, HostReport)> {
        let report = probe(&config.bwrap).await;
        if let Some(reason) = report.launch {
            return Err((reason, report));
        }
        let private_base = config.scratch_base.is_absolute()
            && std::fs::canonicalize(&config.scratch_base)
                .is_ok_and(|resolved| resolved == config.scratch_base)
            && std::fs::metadata(&config.scratch_base)
                .is_ok_and(|meta| meta.is_dir() && meta.mode() & 0o077 == 0);
        if !private_base {
            return Err((Unavailable::ScratchUnavailable, report));
        }
        Ok(Self {
            layout: Layout::detect(&report),
            config,
            report,
            next: AtomicU64::new(1),
            leases: Mutex::new(HashMap::new()),
        })
    }

    pub fn report(&self) -> &HostReport {
        &self.report
    }

    /// The host directories to mount for `ctx`: only its own profile's
    /// entries, each a canonical directory. Any granted label without a safe
    /// entry fails the whole start.
    fn mounts(&self, ctx: &ExecutionContext) -> Option<Vec<(String, Access, PathBuf)>> {
        ctx.grants()
            .roots
            .iter()
            .map(|(label, access)| {
                let host = self
                    .config
                    .roots
                    .get(&(ctx.profile().clone(), label.clone()))?;
                let canonical = host.is_absolute()
                    && host != Path::new("/")
                    && std::fs::canonicalize(host).is_ok_and(|resolved| &resolved == host)
                    && host.is_dir();
                canonical.then(|| (label.clone(), *access, host.clone()))
            })
            .collect()
    }
}

/// Read a pipe to the end, keeping at most `cap` bytes.
async fn read_capped(mut pipe: impl AsyncRead + Unpin, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                if read > room {
                    truncated = true;
                }
                kept.extend_from_slice(&buffer[..read.min(room)]);
            }
        }
    }
    (kept, truncated)
}

/// `bwrap`'s own messages can name host paths; the program's cannot, since
/// it sees none. Withhold the former.
fn scrub_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .map(|line| {
            if line.starts_with("bwrap:") {
                "[sandbox setup message withheld]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy)]
enum Waited {
    Exited(std::process::ExitStatus),
    Cancelled,
    TimedOut,
    Failed,
}

#[async_trait]
impl SandboxBackend for BubblewrapBackend {
    fn name(&self) -> &'static str {
        "bubblewrap-experimental"
    }

    fn assurance(&self) -> Assurance {
        Assurance::Experimental
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            enforced: implemented(),
            demonstrated_on: BTreeSet::new(),
        }
    }

    /// Synchronous inside: no await, so dropping it leaves nothing behind.
    async fn start(&self, ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault> {
        if ctx.trust() == RunTrust::RemoteUser {
            // Experimental: never for an untrusted run, whatever the caller.
            return Err(BackendFault::Unavailable);
        }
        if ctx.cancel().is_cancelled() {
            return Err(BackendFault::Cancelled);
        }
        if self.mounts(ctx).is_none() {
            return Err(BackendFault::StartFailed);
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let dir = self
            .config
            .scratch_base
            .join(format!("run-{}-{id}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|_| BackendFault::StartFailed)?;
        match self.leases.lock() {
            Ok(mut leases) => {
                leases.insert(id, dir);
                Ok(SandboxLease::new(id))
            }
            Err(_) => {
                std::fs::remove_dir_all(&dir).ok();
                Err(BackendFault::StartFailed)
            }
        }
    }

    async fn execute(
        &self,
        lease: &SandboxLease,
        ctx: &ExecutionContext,
        request: &ExecRequest,
    ) -> Result<ExecOutput, BackendFault> {
        if ctx.cancel().is_cancelled() {
            return Err(BackendFault::Cancelled);
        }
        if !self.config.programs.contains(&request.program)
            || !ctx.grants().roots.contains_key(&request.root)
            || request.cwd.as_deref().is_some_and(|cwd| !relative_ok(cwd))
            || request.args.iter().any(|value| value.contains('\0'))
        {
            return Err(BackendFault::ExecutionFailed);
        }
        let scratch = self
            .leases
            .lock()
            .ok()
            .and_then(|leases| leases.get(&lease.id()).cloned())
            .ok_or(BackendFault::ExecutionFailed)?;
        let mounts = self.mounts(ctx).ok_or(BackendFault::StartFailed)?;
        let args = exec_args(&self.layout, &scratch, &mounts, request);
        let limits = ctx.grants().limits;

        let mut child = tokio::process::Command::new(&self.config.bwrap)
            .args(&args)
            .env_clear()
            .envs(ctx.child_environment(&*self.config.secrets))
            .env("PATH", SANDBOX_PATH)
            .env("HOME", SCRATCH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| BackendFault::StartFailed)?;
        let cap = limits.max_output_bytes;
        let stdout = child
            .stdout
            .take()
            .map(|pipe| tokio::spawn(read_capped(pipe, cap)));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| tokio::spawn(read_capped(pipe, cap)));

        let waited = tokio::select! {
            biased;
            () = ctx.cancel().cancelled() => Waited::Cancelled,
            waited = tokio::time::timeout(limits.wall_clock, child.wait()) => match waited {
                Ok(Ok(status)) => Waited::Exited(status),
                Ok(Err(_)) => Waited::Failed,
                Err(_) => Waited::TimedOut,
            },
        };
        let status = match waited {
            Waited::Exited(status) => Some(status),
            _ => {
                // Killing the outer bwrap kills the namespace's init
                // (`--die-with-parent`), and the kernel then kills every
                // process left in the PID namespace.
                child.start_kill().ok();
                child.wait().await.ok();
                None
            }
        };
        let mut collected = Vec::new();
        for reader in [stdout, stderr] {
            let Some(mut reader) = reader else {
                collected.push((Vec::new(), false));
                continue;
            };
            match tokio::time::timeout(DRAIN_GRACE, &mut reader).await {
                Ok(Ok(result)) => collected.push(result),
                _ => {
                    reader.abort();
                    collected.push((Vec::new(), true));
                }
            }
        }
        let status = match (waited, status) {
            (Waited::Cancelled, _) => return Err(BackendFault::Cancelled),
            (Waited::TimedOut, _) => return Err(BackendFault::TimedOut),
            (Waited::Failed, _) | (_, None) => return Err(BackendFault::ExecutionFailed),
            (Waited::Exited(_), Some(status)) => status,
        };
        let (out, out_cut) = collected.first().cloned().unwrap_or_default();
        let (err, err_cut) = collected.get(1).cloned().unwrap_or_default();
        Ok(ExecOutput {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: scrub_stderr(&String::from_utf8_lossy(&err)),
            truncated: out_cut || err_cut,
        })
    }

    async fn release(&self, lease: SandboxLease) {
        let dir = self
            .leases
            .lock()
            .ok()
            .and_then(|mut leases| leases.remove(&lease.id()));
        if let Some(dir) = dir {
            // Does not follow symbolic links the sandbox may have left.
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            system: vec![
                SystemEntry::Link {
                    name: "bin".to_owned(),
                    target: PathBuf::from("usr/bin"),
                },
                SystemEntry::Dir {
                    name: "lib64".to_owned(),
                },
            ],
            loader_cache: true,
            tmpfs_size: true,
            disable_userns: true,
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_namespace_and_privilege_control_is_always_requested() {
        let args = strings(&base_args(&layout()));
        for flag in [
            "--unshare-user",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-uts",
            "--unshare-cgroup",
            "--die-with-parent",
            "--new-session",
            "--disable-userns",
        ] {
            assert!(args.contains(&flag.to_owned()), "{flag}");
        }
        let cap = args.iter().position(|value| value == "--cap-drop").unwrap();
        assert_eq!(args[cap + 1], "ALL");
        // Never a shared network, a host home, /etc as a whole, /run or /var.
        for forbidden in ["--share-net", "/home", "/root", "/run", "/var", "/etc"] {
            assert!(!args.contains(&forbidden.to_owned()), "{forbidden}");
        }
        assert!(
            !args
                .iter()
                .any(|value| value.contains("bash") || value == "sh")
        );
    }

    #[test]
    fn request_values_are_single_arguments_after_the_options() {
        let request = ExecRequest {
            program: "/usr/bin/python3".to_owned(),
            args: vec![
                "--bind".to_owned(),
                "/".to_owned(),
                "/".to_owned(),
                "; rm -rf /".to_owned(),
            ],
            root: "workspace".to_owned(),
            cwd: Some("sub dir".to_owned()),
        };
        let mounts = vec![
            (
                "workspace".to_owned(),
                Access::ReadWrite,
                PathBuf::from("/srv/w"),
            ),
            (
                "reference".to_owned(),
                Access::ReadOnly,
                PathBuf::from("/srv/r"),
            ),
        ];
        let args = strings(&exec_args(
            &layout(),
            Path::new("/srv/scratch/run-1"),
            &mounts,
            &request,
        ));
        let separator = args.iter().position(|value| value == "--").unwrap();
        assert_eq!(
            &args[separator + 1..],
            ["/usr/bin/python3", "--bind", "/", "/", "; rm -rf /"]
        );
        let options = &args[..separator];
        let chdir = options.iter().position(|value| value == "--chdir").unwrap();
        assert_eq!(options[chdir + 1], "/work/workspace/sub dir");
        let window = |flag: &str, host: &str, inside: &str| {
            options
                .windows(3)
                .any(|w| w[0] == flag && w[1] == host && w[2] == inside)
        };
        assert!(window("--bind", "/srv/w", "/work/workspace"));
        assert!(window("--ro-bind", "/srv/r", "/work/reference"));
        assert!(window("--bind", "/srv/scratch/run-1", "/scratch"));
        // Request arguments never appear among the options.
        assert!(!options.contains(&"; rm -rf /".to_owned()));
    }

    #[test]
    fn relative_paths_never_climb_or_root() {
        assert!(relative_ok("a/b"));
        assert!(relative_ok("./a"));
        for bad in ["../a", "a/../../b", "/etc", "a\0b"] {
            assert!(!relative_ok(bad), "{bad}");
        }
    }

    #[test]
    fn bwrap_messages_are_withheld_and_program_output_kept() {
        assert_eq!(
            scrub_stderr("bwrap: Can't find source path /srv/secret: No such file\nprogram line"),
            "[sandbox setup message withheld]\nprogram line"
        );
    }

    #[test]
    fn the_backend_claims_only_what_it_implements_and_never_identity_or_limits() {
        let unsupported = unsupported();
        for guarantee in [
            Guarantee::SeparateIdentity,
            Guarantee::LinkEscape,
            Guarantee::CpuLimit,
            Guarantee::MemoryLimit,
            Guarantee::ProcessLimit,
        ] {
            assert!(unsupported.contains(&guarantee), "{guarantee:?}");
            assert!(!implemented().contains(&guarantee), "{guarantee:?}");
        }
        assert_eq!(
            implemented().len() + unsupported.len(),
            Guarantee::ALL.len()
        );
    }

    #[test]
    fn a_relative_or_missing_binary_is_untrusted() {
        assert!(!trusted_binary(Path::new("bwrap")));
        assert!(!trusted_binary(Path::new("/nonexistent/bwrap")));
    }

    #[test]
    fn launch_failures_are_classified_without_their_text() {
        assert_eq!(
            classify_launch("bwrap: setting up uid map: Permission denied"),
            "uid_map_denied"
        );
        assert_eq!(classify_launch("bwrap: something /srv/x"), "other");
    }
}
