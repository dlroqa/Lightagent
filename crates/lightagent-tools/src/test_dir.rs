//! Temporary directories for this crate's unit tests.
//!
//! A [`TestDir`] is created with an exclusive `create_dir`, so two tests are
//! never handed the same directory, however close together they ask: a name
//! that is already taken (by another test, or left over from an earlier
//! process with the same id) is skipped, never shared. The directory lives
//! as long as its `TestDir`, and dropping it removes that directory only.
//!
//! This replaces helpers that named a directory after the process id and
//! the clock and created it with `create_dir_all`, which succeeds on a
//! directory that already exists. Two tests reading the same clock value
//! shared one directory, and the first to finish deleted it under the other.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes the directories of one process.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// An empty directory owned by one test, removed when dropped.
pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    /// A new, empty directory under the system temporary directory, named
    /// `lightagent-<tag>-<process>-<n>`.
    pub(crate) fn new(tag: &str) -> Self {
        let pid = std::process::id();
        Self::create_in(&std::env::temp_dir(), || {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            format!("lightagent-{tag}-{pid}-{n}")
        })
    }

    /// Create a directory in `parent` under the first name from `name` that
    /// nobody holds.
    fn create_in(parent: &Path, mut name: impl FnMut() -> String) -> Self {
        for _ in 0..1_000 {
            let path = parent.join(name());
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
        panic!("no free directory name in {}", parent.display());
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn concurrent_allocations_never_share_a_directory() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|thread| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..50)
                        .map(|i| {
                            let dir = TestDir::new("alloc");
                            let owner = format!("{thread}-{i}");
                            std::fs::write(dir.path().join("owner"), &owner).unwrap();
                            (dir, owner)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let dirs: Vec<(TestDir, String)> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        let unique: BTreeSet<&Path> = dirs.iter().map(|(dir, _)| dir.path()).collect();
        assert_eq!(unique.len(), 16 * 50);
        for (dir, owner) in &dirs {
            let written = std::fs::read_to_string(dir.path().join("owner")).unwrap();
            assert_eq!(&written, owner, "{}", dir.path().display());
        }
    }

    #[test]
    fn a_name_already_taken_is_skipped_never_shared() {
        let parent = TestDir::new("parent");
        let mut names = ["same", "same", "next"].into_iter();
        let mut name = || names.next().unwrap().to_owned();
        let first = TestDir::create_in(parent.path(), &mut name);
        let second = TestDir::create_in(parent.path(), &mut name);
        assert_eq!(first.path(), parent.path().join("same"));
        assert_eq!(second.path(), parent.path().join("next"));
    }

    #[test]
    fn dropping_one_directory_never_removes_another() {
        let kept = TestDir::new("kept");
        std::fs::write(kept.path().join("a.txt"), "mine").unwrap();
        let dropped = TestDir::new("dropped");
        let gone = dropped.path().to_path_buf();
        drop(dropped);
        assert!(!gone.exists());
        let read = std::fs::read_to_string(kept.path().join("a.txt")).unwrap();
        assert_eq!(read, "mine");
    }

    /// The replaced helpers, with the clock held still: two tests are handed
    /// one directory, and one test's cleanup deletes the file the other has
    /// just written, so its read fails with "not found", the error of the
    /// failed Windows run. Under the same collision, `TestDir` gives the
    /// second test a directory of its own.
    #[test]
    fn a_repeated_clock_value_shared_a_directory_under_the_replaced_helper() {
        let parent = TestDir::new("clock");
        let replaced = |nanos: u128| {
            let dir = parent
                .path()
                .join(format!("lightagent-fs-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let writer = replaced(1_000);
        let other = replaced(1_000);
        assert_eq!(writer, other, "both tests were handed one directory");
        std::fs::create_dir_all(writer.join("notes")).unwrap();
        std::fs::write(writer.join("notes/a.txt"), "hello").unwrap();
        std::fs::remove_dir_all(&other).unwrap();
        let read = std::fs::read_to_string(writer.join("notes/a.txt")).unwrap_err();
        assert_eq!(read.kind(), std::io::ErrorKind::NotFound);

        let tick = || format!("lightagent-fs-{}-1000", std::process::id());
        let writer = TestDir::create_in(parent.path(), tick);
        let mut names = [tick(), format!("{}-b", tick())].into_iter();
        let other = TestDir::create_in(parent.path(), || names.next().unwrap());
        assert_ne!(writer.path(), other.path());
        std::fs::create_dir_all(writer.path().join("notes")).unwrap();
        std::fs::write(writer.path().join("notes/a.txt"), "hello").unwrap();
        drop(other);
        let read = std::fs::read_to_string(writer.path().join("notes/a.txt")).unwrap();
        assert_eq!(read, "hello");
    }
}
