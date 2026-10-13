//! Live tests of the **experimental** bubblewrap backend against the real
//! kernel. They are `#[ignore]`d and run in the "Linux sandbox (bubblewrap,
//! experimental)" CI job with `--ignored`. If the host cannot launch the
//! backend they **fail** with an infrastructure report; they never skip.
//!
//! Every fixture is synthetic: temporary directories, fake secret files and
//! fake credentials set by the CI job. A passing run demonstrates specific
//! controls on one runner. It does not verify the backend: its assurance stays
//! `Experimental` and no gate routes a run to it.

#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::net::TcpListener;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lightagent_core::identity::{
    Caller, Capability, OwnershipRegistry, Principal, PrincipalId, PrincipalSource, Restriction,
    Role, authorize,
};
use lightagent_core::{ProfileId, RunId};
use lightagent_tools::sandbox::bubblewrap::{
    BubblewrapBackend, BubblewrapConfig, DEFAULT_BWRAP, TMPFS_BYTES, implemented, unsupported,
};
use lightagent_tools::sandbox::{
    Access, Assurance, BackendFault, DenialReason, ExecOutput, ExecRequest, ExecutionContext,
    ExecutionLimits, Grants, Guarantee, NetworkGrant, Route, SandboxBackend, SandboxError,
    SandboxGate,
};
use tokio_util::sync::CancellationToken;

const PYTHON: &str = "/usr/bin/python3";
const HOST_SECRET: &str = "synthetic-host-secret-value";
const GRANTED: &str = "granted-synthetic-value";

/// Synthetic credentials the CI job puts in the server's environment, which
/// the sandbox must never see.
const SERVER_SECRETS: [&str; 4] = [
    "OPENAI_API_KEY",
    "TYPESAFE_API_KEY",
    "LIGHTAGENT_API_KEY",
    "SSH_AUTH_SOCK",
];

fn profile(id: &str) -> ProfileId {
    ProfileId::new(id).unwrap()
}

fn summary(line: &str) {
    println!("{line}");
    let file = std::env::var("GITHUB_STEP_SUMMARY")
        .ok()
        .and_then(|path| std::fs::OpenOptions::new().append(true).open(path).ok());
    if let Some(mut file) = file {
        writeln!(file, "{line}").ok();
    }
}

struct Fixture {
    base: PathBuf,
    research_workspace: PathBuf,
    research_reference: PathBuf,
    billing_workspace: PathBuf,
    host_secret: PathBuf,
    scratch: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.base).ok();
    }
}

fn private_dir(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

fn fixture() -> Fixture {
    let base = std::env::temp_dir().join(format!(
        "lightagent-sbx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    private_dir(&base);
    let base = std::fs::canonicalize(&base).unwrap();
    let dirs = [
        "research/workspace",
        "research/reference",
        "billing/workspace",
        "host",
        "scratch",
    ];
    for dir in dirs {
        private_dir(&base.join(dir));
    }
    std::fs::write(base.join("research/workspace/notes.txt"), "research notes").unwrap();
    std::fs::write(base.join("research/reference/readme.txt"), "reference text").unwrap();
    std::fs::write(base.join("billing/workspace/ledger.txt"), "billing ledger").unwrap();
    let host_secret = base.join("host/credentials");
    std::fs::write(&host_secret, HOST_SECRET).unwrap();
    Fixture {
        research_workspace: base.join("research/workspace"),
        research_reference: base.join("research/reference"),
        billing_workspace: base.join("billing/workspace"),
        scratch: base.join("scratch"),
        host_secret,
        base,
    }
}

fn config(fixture: &Fixture) -> BubblewrapConfig {
    let roots = BTreeMap::from([
        (
            (profile("research"), "workspace".to_owned()),
            fixture.research_workspace.clone(),
        ),
        (
            (profile("research"), "reference".to_owned()),
            fixture.research_reference.clone(),
        ),
        (
            (profile("billing"), "workspace".to_owned()),
            fixture.billing_workspace.clone(),
        ),
    ]);
    BubblewrapConfig {
        bwrap: PathBuf::from(DEFAULT_BWRAP),
        scratch_base: fixture.scratch.clone(),
        roots,
        programs: [PYTHON, "/usr/bin/env", "/usr/bin/true", "/usr/bin/mount"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        secrets: Arc::new(|name: &str| match name {
            "REPORT_TOKEN" => Some(GRANTED.to_owned()),
            "PROVIDER_API_KEY" => Some("must-never-reach-a-child".to_owned()),
            _ => None,
        }),
    }
}

async fn backend(config: BubblewrapConfig) -> BubblewrapBackend {
    match BubblewrapBackend::detect(config).await {
        Ok(backend) => backend,
        Err((reason, report)) => panic!(
            "INFRASTRUCTURE LIMITATION: the bubblewrap backend cannot run on this host \
             ({reason:?}). This is reported, not skipped. Host report: {}",
            serde_json::to_string(&report).unwrap()
        ),
    }
}

fn grants() -> Grants {
    Grants {
        roots: BTreeMap::from([
            ("workspace".to_owned(), Access::ReadWrite),
            ("reference".to_owned(), Access::ReadOnly),
        ]),
        network: NetworkGrant::Deny,
        secrets: BTreeSet::from(["REPORT_TOKEN".to_owned()]),
        limits: ExecutionLimits::default(),
    }
}

fn context(profile_id: &str, grants: Grants) -> ExecutionContext {
    ExecutionContext::local_operator(
        RunId::new(),
        profile(profile_id),
        grants,
        CancellationToken::new(),
    )
    .unwrap()
}

fn request(program: &str, args: &[&str]) -> ExecRequest {
    ExecRequest {
        program: program.to_owned(),
        args: args.iter().map(|value| (*value).to_owned()).collect(),
        root: "workspace".to_owned(),
        cwd: None,
    }
}

async fn run(
    backend: &BubblewrapBackend,
    ctx: &ExecutionContext,
    request: &ExecRequest,
) -> Result<ExecOutput, BackendFault> {
    let lease = backend.start(ctx).await?;
    let outcome = backend.execute(&lease, ctx, request).await;
    backend.release(lease).await;
    outcome
}

async fn python(
    backend: &BubblewrapBackend,
    ctx: &ExecutionContext,
    script: &str,
    args: &[&str],
) -> String {
    let mut all = vec!["-c", script];
    all.extend_from_slice(args);
    let output = run(backend, ctx, &request(PYTHON, &all)).await.unwrap();
    assert_eq!(output.exit_code, Some(0), "stderr: {}", output.stderr);
    output.stdout
}

/// Each `name result [detail]` line a probe script printed.
fn results(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(' ')?;
            Some((name.to_owned(), rest.to_owned()))
        })
        .collect()
}

/// Processes on the host whose command line holds `marker`.
fn processes_with(marker: &str) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|entry| std::fs::read(entry.path().join("cmdline")).ok())
        .filter(|cmdline| String::from_utf8_lossy(cmdline).contains(marker))
        .count()
}

async fn none_left(marker: &str) -> bool {
    for _ in 0..30 {
        if processes_with(marker) == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn marker(name: &str) -> String {
    format!("lightagent-marker-{name}-{}", std::process::id())
}

fn ipv4(octets: [u8; 4]) -> String {
    std::net::Ipv4Addr::from(octets).to_string()
}

const PROBE: &str = r#"
import errno, os, sys
def t(name, f):
    try:
        r = f()
        print(name, 'ok', '' if r is None else str(r).strip())
    except OSError as e:
        print(name, 'denied', errno.errorcode.get(e.errno, e.errno))
"#;

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn the_host_report_is_recorded_and_the_backend_launches() {
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;
    let report = backend.report();
    summary("### Bubblewrap host report");
    summary(&format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(report).unwrap()
    ));
    assert!(!report.running_as_root);
    assert!(report.bwrap_trusted);
    assert_eq!(report.launch, None);
    assert_eq!(backend.assurance(), Assurance::Experimental);
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn namespaces_privileges_and_layout_are_isolated() {
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;
    let script = r#"
import os
status = dict(l.split(':\t', 1) for l in open('/proc/self/status').read().splitlines() if ':\t' in l)
for key in ('CapEff', 'CapPrm', 'CapBnd', 'NoNewPrivs'):
    print(key, status.get(key, '?').strip())
print('pids', len([p for p in os.listdir('/proc') if p.isdigit()]))
print('root', ','.join(sorted(os.listdir('/'))))
print('work', ','.join(sorted(os.listdir('/work'))))
print('session_leader', os.getsid(0) == os.getpid())
print('hostname', os.uname().nodename)
"#;
    let out = results(&python(&backend, &context("research", grants()), script, &[]).await);
    summary(&format!("namespaces and privileges: {out:?}"));
    assert_eq!(out["CapEff"], "0000000000000000", "no effective capability");
    assert_eq!(out["CapPrm"], "0000000000000000", "no permitted capability");
    assert_eq!(
        out["NoNewPrivs"], "1",
        "privilege escalation through exec is off"
    );
    let pids: usize = out["pids"].parse().unwrap();
    assert!(pids <= 3, "a private PID namespace, saw {pids} processes");
    assert_eq!(out["session_leader"], "True", "a new session");
    assert_eq!(out["hostname"], "sandbox");
    assert_eq!(out["work"], "reference,workspace");
    let allowed: BTreeSet<&str> = [
        "bin", "dev", "etc", "lib", "lib32", "lib64", "libx32", "proc", "sbin", "scratch", "tmp",
        "usr", "work",
    ]
    .into();
    for entry in out["root"].split(',') {
        assert!(
            allowed.contains(entry),
            "unexpected top-level entry {entry:?}"
        );
    }
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn no_server_secret_or_socket_reaches_the_sandbox() {
    for name in SERVER_SECRETS {
        assert!(
            std::env::var_os(name).is_some(),
            "precondition: the CI job sets the synthetic {name} in the server's environment"
        );
    }
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;
    let env = run(
        &backend,
        &context("research", grants()),
        &request("/usr/bin/env", &[]),
    )
    .await
    .unwrap();
    let vars: BTreeMap<&str, &str> = env
        .stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    summary(&format!(
        "child environment keys: {:?}",
        vars.keys().collect::<Vec<_>>()
    ));
    assert_eq!(
        vars.keys().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from(["HOME", "PATH", "REPORT_TOKEN"])
    );
    assert_eq!(vars["REPORT_TOKEN"], GRANTED);
    for name in SERVER_SECRETS {
        let value = std::env::var(name).unwrap();
        assert!(!env.stdout.contains(&value), "{name}'s value leaked");
    }
    assert!(!env.stdout.contains("must-never-reach-a-child"));

    // Without grants, nothing but the fixed PATH and HOME.
    let mut none = grants();
    none.secrets.clear();
    let bare = run(
        &backend,
        &context("research", none),
        &request("/usr/bin/env", &[]),
    )
    .await
    .unwrap();
    assert_eq!(bare.stdout.lines().count(), 2, "{}", bare.stdout);

    let ssh = std::env::var("SSH_AUTH_SOCK").unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    let script = format!(
        "{PROBE}\nfor p in sys.argv[1:]:\n    t('exists:' + p, lambda p=p: os.stat(p) and 'present')\n"
    );
    let probes = [
        ssh.as_str(),
        home.as_str(),
        "/var/run/docker.sock",
        "/run/docker.sock",
        "/run/dbus/system_bus_socket",
        "/run/user",
        "/etc/shadow",
        "/etc/passwd",
        "/root",
    ];
    let out = results(&python(&backend, &context("research", grants()), &script, &probes).await);
    summary(&format!("host sockets and private paths: {out:?}"));
    for path in probes.iter().filter(|path| !path.is_empty()) {
        assert_eq!(
            out[&format!("exists:{path}")],
            "denied ENOENT",
            "{path} is visible"
        );
    }
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn the_filesystem_holds_only_the_runs_own_grants() {
    let fixture = fixture();
    // Placed by the host before the run: a symbolic link and a hard link to
    // the host secret inside a granted root.
    std::os::unix::fs::symlink(
        &fixture.host_secret,
        fixture.research_workspace.join("host-link"),
    )
    .unwrap();
    std::fs::hard_link(
        &fixture.host_secret,
        fixture.research_workspace.join("host-hardlink"),
    )
    .unwrap();
    let backend = backend(config(&fixture)).await;
    let script = format!(
        r#"{PROBE}
secret, home, billing = sys.argv[1:4]
t('read_granted', lambda: open('/work/workspace/notes.txt').read())
t('write_granted', lambda: open('/work/workspace/written.txt', 'w').write('from sandbox'))
t('read_readonly', lambda: open('/work/reference/readme.txt').read())
t('write_readonly', lambda: open('/work/reference/readme.txt', 'w').write('x'))
t('create_in_readonly', lambda: open('/work/reference/new.txt', 'w').write('x'))
t('read_host_secret', lambda: open(secret).read())
t('traverse_to_secret', lambda: open('/work/workspace/../../..' + secret).read())
t('host_home', lambda: os.listdir(home))
t('other_profile', lambda: open(billing).read())
t('other_profile_by_label', lambda: os.listdir('/work/../work/billing'))
os.symlink(secret, '/work/workspace/escape-link')
t('planted_symlink', lambda: open('/work/workspace/escape-link').read())
t('host_symlink', lambda: open('/work/workspace/host-link').read())
t('host_hardlink', lambda: open('/work/workspace/host-hardlink').read())
t('hardlink_from_readonly', lambda: os.link('/work/reference/readme.txt', '/work/workspace/hl1'))
t('hardlink_from_system', lambda: os.link('/usr/bin/env', '/work/workspace/hl2'))
t('rename_mountpoint', lambda: os.rename('/work/reference', '/work/moved'))
t('write_usr', lambda: open('/usr/planted', 'w').write('x'))
t('nested_userns', lambda: os.unshare(os.CLONE_NEWUSER))
t('new_mount_ns', lambda: os.unshare(os.CLONE_NEWNS))
"#
    );
    let home = std::env::var("HOME").unwrap_or_else(|_| "/nonexistent-home".to_owned());
    let billing = fixture.billing_workspace.join("ledger.txt");
    let out = results(
        &python(
            &backend,
            &context("research", grants()),
            &script,
            &[
                fixture.host_secret.to_str().unwrap(),
                home.as_str(),
                billing.to_str().unwrap(),
            ],
        )
        .await,
    );
    summary(&format!("filesystem: {out:?}"));
    assert_eq!(out["read_granted"], "ok research notes");
    assert!(out["write_granted"].starts_with("ok"));
    assert_eq!(
        std::fs::read_to_string(fixture.research_workspace.join("written.txt")).unwrap(),
        "from sandbox"
    );
    assert_eq!(out["read_readonly"], "ok reference text");
    assert_eq!(out["write_readonly"], "denied EROFS");
    assert_eq!(out["create_in_readonly"], "denied EROFS");
    for denied in [
        "read_host_secret",
        "traverse_to_secret",
        "host_home",
        "other_profile",
        "other_profile_by_label",
        "planted_symlink",
        "host_symlink",
    ] {
        assert_eq!(out[denied], "denied ENOENT", "{denied}");
    }
    assert_eq!(out["hardlink_from_readonly"], "denied EXDEV");
    assert_eq!(out["hardlink_from_system"], "denied EXDEV");
    assert!(out["rename_mountpoint"].starts_with("denied"));
    assert!(out["write_usr"].starts_with("denied"));
    assert!(out["new_mount_ns"].starts_with("denied"));
    if backend.report().disable_userns_supported {
        assert!(out["nested_userns"].starts_with("denied"));
    } else {
        summary("UNSUPPORTED: bwrap lacks --disable-userns; nested user namespaces not blocked");
    }
    // Known risk, recorded rather than hidden: a hard link the HOST placed in
    // a granted root reaches its target. That is why `link_escape` is not
    // claimed.
    assert_eq!(out["host_hardlink"], format!("ok {HOST_SECRET}"));
    assert!(unsupported().contains(&Guarantee::LinkEscape));

    // The sandbox's own link, seen from the host, is still refused by the
    // trusted file tools' workspace confinement.
    let workspace = lightagent_tools::Workspace::new(&fixture.research_workspace).unwrap();
    assert!(workspace.resolve_existing("escape-link").is_err());
    assert_eq!(
        std::fs::read_to_string(&fixture.host_secret).unwrap(),
        HOST_SECRET
    );
    assert_eq!(
        std::fs::read_to_string(fixture.research_reference.join("readme.txt")).unwrap(),
        "reference text"
    );

    // A setuid-root `mount` on the host gains nothing inside.
    for args in [
        ["-t", "tmpfs", "none", "/work/reference"],
        ["-o", "remount,rw", "/work/reference", "/work/reference"],
    ] {
        let mounted = run(
            &backend,
            &context("research", grants()),
            &request("/usr/bin/mount", &args),
        )
        .await
        .unwrap();
        assert_ne!(mounted.exit_code, Some(0), "mount {args:?} succeeded");
    }
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn unsafe_roots_other_profiles_and_untrusted_runs_fail_closed() {
    let fixture = fixture();
    let link = fixture.base.join("linked-root");
    std::os::unix::fs::symlink(&fixture.research_workspace, &link).unwrap();
    let mut with_link = config(&fixture);
    with_link
        .roots
        .insert((profile("research"), "linked".to_owned()), link);
    let backend = backend(with_link).await;

    let mut linked = grants();
    linked.roots = BTreeMap::from([("linked".to_owned(), Access::ReadOnly)]);
    assert_eq!(
        backend.start(&context("research", linked)).await,
        Err(BackendFault::StartFailed),
        "a root behind a symbolic link is refused"
    );
    // billing has no `reference` root: research's is never substituted.
    assert_eq!(
        backend.start(&context("billing", grants())).await,
        Err(BackendFault::StartFailed)
    );
    let mut sneaky = request(PYTHON, &["-c", "print(1)"]);
    sneaky.cwd = Some("../../billing".to_owned());
    assert_eq!(
        run(&backend, &context("research", grants()), &sneaky).await,
        Err(BackendFault::ExecutionFailed)
    );
    assert_eq!(
        run(
            &backend,
            &context("research", grants()),
            &request("/usr/bin/bash", &["-c", "id"])
        )
        .await,
        Err(BackendFault::ExecutionFailed),
        "only the fixed programs run"
    );

    // Untrusted: the backend refuses it, and the production gate never
    // routes to an experimental backend.
    let alice = PrincipalId::new("alice").unwrap();
    let mut ownership = OwnershipRegistry::default();
    ownership
        .assign(profile("research"), alice.clone())
        .unwrap();
    let caller = Caller {
        principal: Principal {
            id: alice,
            role: Role::User,
            source: PrincipalSource::Credential {
                key_id: "k1".to_owned(),
            },
        },
        restriction: Restriction::default(),
    };
    let authorized = authorize(
        &caller,
        Capability::RunsWrite,
        &profile("research"),
        &ownership,
    )
    .unwrap();
    let remote = ExecutionContext::remote_user(
        RunId::new(),
        &authorized,
        grants(),
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(backend.start(&remote).await, Err(BackendFault::Unavailable));
    let backend = Arc::new(backend);
    let gate = SandboxGate::with_backend(backend.clone(), Arc::new(|_: &ExecutionContext| true));
    assert_eq!(
        gate.route(&remote),
        Err(SandboxError::Denied(DenialReason::NotVerified))
    );
    assert_eq!(
        gate.execute(&remote, &request("/usr/bin/true", &[])).await,
        Err(SandboxError::Denied(DenialReason::NotVerified))
    );
    assert_eq!(gate.route(&context("research", grants())), Ok(Route::Host));
    // Not one scratch directory left behind.
    assert_eq!(std::fs::read_dir(&fixture.scratch).unwrap().count(), 0);
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn the_whole_process_tree_ends_with_the_execution() {
    let fixture = fixture();
    let backend = Arc::new(backend(config(&fixture)).await);
    let sleeper = "import time; time.sleep(300)";

    // An orphaned, re-sessioned grandchild dies with the namespace.
    let orphan = marker("orphan");
    let spawn = r#"
import subprocess, sys
subprocess.Popen(['/usr/bin/python3', '-c', 'import time; time.sleep(300)', sys.argv[1]],
                 start_new_session=True)
print('spawned')
"#;
    let out = run(
        &backend,
        &context("research", grants()),
        &request(PYTHON, &["-c", spawn, orphan.as_str()]),
    )
    .await
    .unwrap();
    assert_eq!(out.stdout.trim(), "spawned");
    assert!(none_left(&orphan).await, "orphan survived");

    // Timeout kills the tree.
    let timed = marker("timeout");
    let mut short = grants();
    short.limits.wall_clock = Duration::from_secs(2);
    let started = Instant::now();
    assert_eq!(
        run(
            &backend,
            &context("research", short),
            &request(PYTHON, &["-c", sleeper, timed.as_str()])
        )
        .await,
        Err(BackendFault::TimedOut)
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(none_left(&timed).await, "timed-out process survived");

    // Cancellation during execution, and during startup.
    for (name, delay) in [("cancel-run", 500u64), ("cancel-startup", 1)] {
        for attempt in 0..3 {
            let mark = marker(&format!("{name}-{attempt}"));
            let ctx = context("research", grants());
            let cancel = ctx.cancel().clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                cancel.cancel();
            });
            let outcome = run(
                &backend,
                &ctx,
                &request(PYTHON, &["-c", sleeper, mark.as_str()]),
            )
            .await;
            assert_eq!(outcome, Err(BackendFault::Cancelled), "{name}");
            assert!(none_left(&mark).await, "{name}: process survived");
        }
    }
    // Already cancelled: nothing starts.
    let ctx = context("research", grants());
    ctx.cancel().cancel();
    assert_eq!(backend.start(&ctx).await, Err(BackendFault::Cancelled));

    // Concurrent sandboxes see only their own scratch.
    let mut tasks = Vec::new();
    for index in 0..8 {
        let backend = backend.clone();
        tasks.push(tokio::spawn(async move {
            let script = "import os, sys\nopen('/scratch/' + sys.argv[1], 'w').write('x')\nprint(','.join(sorted(os.listdir('/scratch'))))";
            let name = format!("run{index}");
            run(
                &backend,
                &context("research", grants()),
                &request(PYTHON, &["-c", script, name.as_str()]),
            )
            .await
            .map(|output| (name, output.stdout.trim().to_owned()))
        }));
    }
    for task in tasks {
        let (name, listing) = task.await.unwrap().unwrap();
        assert_eq!(listing, name, "a sandbox saw another's scratch");
    }
    assert_eq!(
        std::fs::read_dir(&fixture.scratch).unwrap().count(),
        0,
        "every per-run scratch was removed"
    );
    summary(
        "process tree: orphan, timeout, cancellation (run and startup), 8 concurrent: all cleaned up",
    );
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn output_and_temporary_storage_are_bounded_and_limits_reported() {
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;
    let mut small = grants();
    small.limits.max_output_bytes = 1000;
    let flood = "import sys\nsys.stdout.write('o' * 1048576)\nsys.stderr.write('e' * 1048576)";
    let out = run(
        &backend,
        &context("research", small),
        &request(PYTHON, &["-c", flood]),
    )
    .await
    .unwrap();
    assert!(out.truncated);
    assert_eq!(out.stdout.len(), 1000);
    assert!(out.stderr.len() <= 1000);

    let fill = format!(
        "{PROBE}\nt('fill_tmp', lambda: open('/tmp/fill', 'wb').write(b'0' * {}))\n",
        TMPFS_BYTES * 4
    );
    let result = results(&python(&backend, &context("research", grants()), &fill, &[]).await);
    if backend.report().tmpfs_size_supported {
        assert_eq!(result["fill_tmp"], "denied ENOSPC");
        summary(&format!(
            "/tmp bounded at {TMPFS_BYTES} bytes: write of 4x refused"
        ));
    } else {
        summary(&format!(
            "UNSUPPORTED: bwrap cannot size a tmpfs on this runner; /tmp unbounded ({:?})",
            result["fill_tmp"]
        ));
    }

    // No kernel resource accounting is used, so none is claimed, and the
    // gate's requirement of all of them can never be met by this backend.
    let report = backend.report();
    for limit in [
        Guarantee::CpuLimit,
        Guarantee::MemoryLimit,
        Guarantee::ProcessLimit,
        Guarantee::SeparateIdentity,
    ] {
        assert!(
            !backend.capabilities().enforced.contains(&limit),
            "{limit:?}"
        );
    }
    assert_eq!(backend.capabilities().enforced, implemented());
    summary(&format!(
        "UNSUPPORTED (not claimed): cpu_limit, memory_limit, process_limit, separate_identity, link_escape. cgroup v2: {}, delegated to this user: {}",
        report.cgroup_v2, report.cgroup_delegated
    ));
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn the_network_is_denied_including_host_loopback_and_dns() {
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;

    // Host-side listeners the sandbox must not reach. The host can, which
    // shows the probe is not trivially failing.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    assert!(std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).is_ok());
    let abstract_name = format!("lightagent-abstract-{}", std::process::id());
    let _abstract =
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap())
            .unwrap();

    let script = r#"
import errno, os, socket, sys
def c(name, host, port):
    s = socket.socket(socket.AF_INET6 if ':' in host else socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(3)
    try:
        s.connect((host, int(port)))
        print(name, 'ok')
    except OSError as e:
        print(name, 'denied', errno.errorcode.get(e.errno, type(e).__name__) if e.errno else type(e).__name__)
for spec in sys.argv[2:]:
    c(*spec.split(','))
try:
    socket.getaddrinfo('example.com', 443)
    print('dns ok')
except OSError as e:
    print('dns denied', type(e).__name__)
u = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    u.connect('\0' + sys.argv[1])
    print('abstract_socket ok')
except OSError as e:
    print('abstract_socket denied', errno.errorcode.get(e.errno, e.errno))
print('interfaces', ','.join(sorted(l.split(':')[0].strip() for l in open('/proc/net/dev').read().splitlines()[2:])))
print('resolv_conf', os.path.exists('/etc/resolv.conf'))
"#;
    let specs = [
        format!("public_https,{},443", ipv4([1, 1, 1, 1])),
        format!("public_dns,{},53", ipv4([8, 8, 8, 8])),
        format!("host_loopback,127.0.0.1,{port}"),
        format!("host_loopback_v6,::1,{port}"),
        format!("private,{},80", ipv4([10, 0, 0, 1])),
        format!("metadata,{},80", ipv4([169, 254, 169, 254])),
        format!("azure_wireserver,{},80", ipv4([168, 63, 129, 16])),
    ];
    let mut args = vec![abstract_name.as_str()];
    args.extend(specs.iter().map(String::as_str));
    let out = results(&python(&backend, &context("research", grants()), script, &args).await);
    summary(&format!("network: {out:?}"));
    for name in [
        "public_https",
        "public_dns",
        "private",
        "metadata",
        "azure_wireserver",
    ] {
        assert!(out[name].starts_with("denied"), "{name}: {}", out[name]);
    }
    // The sandbox's own loopback is empty: the host's listener is unreachable.
    assert_eq!(out["host_loopback"], "denied ECONNREFUSED");
    assert!(out["host_loopback_v6"].starts_with("denied"));
    assert!(out["dns"].starts_with("denied"));
    assert_eq!(out["abstract_socket"], "denied ECONNREFUSED");
    assert_eq!(out["interfaces"], "lo");
    assert_eq!(out["resolv_conf"], "False");
    listener.set_nonblocking(true).unwrap();
    // Only the host's own sanity connection was ever accepted.
    assert!(listener.accept().is_ok());
    assert!(listener.accept().is_err());
}

#[tokio::test]
#[ignore = "live: needs bubblewrap and user namespaces; run by the sandbox CI job"]
async fn startup_overhead_is_measured() {
    let fixture = fixture();
    let backend = backend(config(&fixture)).await;
    let ctx = context("research", grants());
    let true_request = request("/usr/bin/true", &[]);
    let mut sandboxed = Vec::new();
    let mut direct = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        let output = run(&backend, &ctx, &true_request).await.unwrap();
        sandboxed.push(started.elapsed());
        assert_eq!(output.exit_code, Some(0));
        let started = Instant::now();
        tokio::process::Command::new("/usr/bin/true")
            .status()
            .await
            .unwrap();
        direct.push(started.elapsed());
    }
    sandboxed.sort();
    direct.sort();
    summary(&format!(
        "startup overhead (/usr/bin/true, 20 runs): sandboxed median {:?} p90 {:?}; direct median {:?}",
        sandboxed[10], sandboxed[18], direct[10]
    ));
}
