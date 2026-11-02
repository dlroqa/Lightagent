//! A dropped run lease is free at once, whatever child processes exist: one
//! started while the lease was held never inherits it across exec, and one
//! still starting (Unix: between fork and exec, sharing its parent's open
//! files) does not keep it either, because the lease unlocks before it
//! closes. The short write locks (`paths::lock_exclusive`, as the config and
//! session stores take them) are released the same way. A held lease is
//! refused within the store's documented bound, and different sessions take
//! their leases at the same time.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lightagent_core::paths;
use lightagent_store::{SessionId, SessionStore, StoreError};

/// The store's 500 ms bound for refusing a held lease, plus scheduling slack.
const REFUSAL_BOUND: Duration = Duration::from_millis(2000);

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightagent-run-lease-{}",
        SessionId::generate().as_str()
    ))
}

fn lease_file(dir: &Path, id: &SessionId) -> PathBuf {
    dir.join(".runs").join(format!("{}.lock", id.as_str()))
}

/// Whether the lock file at `path` can be taken right now, with no wait.
fn free_now(path: &Path) -> bool {
    match paths::lock_exclusive(path, Duration::ZERO) {
        Ok(_file) => true,
        Err(paths::LockError::Busy) => false,
        Err(paths::LockError::Io(error)) => panic!("lock file {}: {error}", path.display()),
    }
}

/// A child that keeps running for several seconds.
fn long_child() -> Child {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", "ping -n 6 127.0.0.1 >NUL"]);
        command
    } else {
        let mut command = Command::new("sleep");
        command.arg("5");
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a child")
}

#[test]
fn a_child_started_while_the_lease_is_held_does_not_keep_it() {
    let dir = scratch_dir();
    let store = SessionStore::new(dir.clone());
    let id = SessionId::generate();
    let lease = store.lease_run(&id).expect("lease");
    let mut child = long_child();
    assert!(!free_now(&lease_file(&dir, &id)), "the lease is held");
    drop(lease);
    let free = free_now(&lease_file(&dir, &id));
    let running = child.try_wait().expect("the child").is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(running, "the child must still run during the check");
    assert!(
        free,
        "a child started while the lease was held kept it after the lease was dropped"
    );
}

#[cfg(unix)]
#[test]
fn a_lease_dropped_while_a_child_is_starting_is_free_at_once() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::CommandExt as _;

    let dir = scratch_dir();
    let store = SessionStore::new(dir.clone());
    let id = SessionId::generate();
    let lease = store.lease_run(&id).expect("lease");
    // The control: a lock file closed without unlocking.
    let control = dir.join("control.lock");
    let held = paths::lock_exclusive(&control, Duration::ZERO)
        .expect("control lock")
        .into_file()
        .expect("the control lock's file");
    // A short write lock, as the config and session stores take it.
    let write = dir.join("write.lock");
    let writing = paths::lock_exclusive(&write, Duration::ZERO).expect("write lock");

    let (mut forked, mut tell) = std::io::pipe().expect("pipe");
    let starter = std::thread::spawn(move || {
        let mut command = Command::new("true");
        // SAFETY: the closure runs in the child between fork and exec and
        // only writes one byte to a pipe and sleeps; it allocates nothing.
        unsafe {
            command.pre_exec(move || {
                tell.write_all(b"x")?;
                std::thread::sleep(Duration::from_secs(3));
                Ok(())
            });
        }
        command.status()
    });
    // The child exists and holds copies of every file open above.
    let mut byte = [0u8; 1];
    forked.read_exact(&mut byte).expect("the child forked");

    drop(lease);
    drop(held);
    drop(writing);
    let lease_free = free_now(&lease_file(&dir, &id));
    let write_free = free_now(&write);
    let control_free = free_now(&control);
    let starting = !starter.is_finished();
    starter.join().expect("starter").expect("the child ran");
    assert!(
        starting,
        "the child must still be starting during the check"
    );
    assert!(
        lease_free,
        "a dropped lease stayed held by a child that was still starting"
    );
    assert!(
        write_free,
        "a dropped write lock stayed held by a child that was still starting"
    );
    if cfg!(target_os = "linux") {
        assert!(
            !control_free,
            "control: a lock file closed without unlocking should stay held by the forked child"
        );
    }
}

#[test]
fn a_held_lease_is_refused_within_the_bound_and_other_sessions_still_run() {
    let store = SessionStore::new(scratch_dir());
    let (one, two) = (SessionId::generate(), SessionId::generate());
    let held = store.lease_run(&one).expect("lease");
    let asked = Instant::now();
    let refused = store.lease_run(&one);
    let waited = asked.elapsed();
    assert!(
        matches!(refused, Err(StoreError::Busy { .. })),
        "{refused:?}"
    );
    assert!(waited < REFUSAL_BOUND, "refused only after {waited:?}");
    let other = store
        .lease_run(&two)
        .expect("another session runs at the same time");
    drop((held, other));
}
