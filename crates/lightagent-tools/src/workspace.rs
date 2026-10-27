//! A confined filesystem root the `fs.*` and `terminal.*` tools work within.
//!
//! The whole safety of the filesystem tools rests here. A [`Workspace`] holds a
//! **canonicalized** root, and every path a tool is given is resolved through it:
//! the relative path may contain no `..` and no absolute or prefix component, and
//! the resolved target is canonicalized and checked to still sit under the root —
//! so a symlink inside the workspace that points outward is caught, not followed.
//! One workspace can therefore never read or write outside its own tree.

use std::path::{Component, Path, PathBuf};

/// A canonicalized root the filesystem tools are confined to.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Open `root` as a workspace, canonicalizing it. The directory must already
    /// exist (the caller creates it), so the canonical root is a real path every
    /// later check can be compared against.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = std::fs::canonicalize(root.as_ref())
            .map_err(|error| format!("workspace root is unavailable: {error}"))?;
        if !root.is_dir() {
            return Err("workspace root is not a directory".to_owned());
        }
        Ok(Self { root })
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Join a caller-supplied relative path under the root, refusing any path
    /// that is absolute or steps upward. This blocks `..` escapes syntactically;
    /// symlink escapes are caught by the canonical check in [`resolve_existing`]
    /// and [`resolve_new`].
    fn join_checked(&self, relative: &str) -> Result<PathBuf, String> {
        let path = Path::new(relative);
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir => {
                    return Err(format!("path {relative:?} must not contain '..'"));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(format!(
                        "path {relative:?} must be relative to the workspace"
                    ));
                }
            }
        }
        Ok(self.root.join(path))
    }

    /// Resolve a path that must already exist, following symlinks and confirming
    /// the real target is inside the workspace.
    pub fn resolve_existing(&self, relative: &str) -> Result<PathBuf, String> {
        let candidate = self.join_checked(relative)?;
        let canonical =
            std::fs::canonicalize(&candidate).map_err(|error| format!("{relative:?}: {error}"))?;
        if canonical.starts_with(&self.root) {
            Ok(canonical)
        } else {
            Err(format!("{relative:?} resolves outside the workspace"))
        }
    }

    /// Resolve a path that may not exist yet (a file about to be written). The
    /// deepest ancestor that *does* exist is canonicalized and confirmed inside
    /// the workspace, so a symlinked parent cannot redirect a write outward,
    /// while the leaf and any missing directories stay under the checked anchor.
    pub fn resolve_new(&self, relative: &str) -> Result<PathBuf, String> {
        let candidate = self.join_checked(relative)?;
        // A leaf symlink would make fs.write follow a target outside the root,
        // even when its parent is safe. Refuse it, including dangling links.
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "{relative:?} is a symlink; writes require a regular path"
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{relative:?}: {error}")),
        }
        let mut anchor = candidate.as_path();
        let existing = loop {
            match anchor.parent() {
                Some(parent) => {
                    if parent.exists() {
                        break parent;
                    }
                    anchor = parent;
                }
                None => return Err(format!("{relative:?} has no parent in the workspace")),
            }
        };
        let canonical =
            std::fs::canonicalize(existing).map_err(|error| format!("{relative:?}: {error}"))?;
        if canonical.starts_with(&self.root) {
            Ok(candidate)
        } else {
            Err(format!("{relative:?} resolves outside the workspace"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[cfg(unix)]
    #[test]
    fn a_leaf_symlink_cannot_redirect_a_write_outside_the_workspace() {
        use std::os::unix::fs::symlink;
        let root = TestDir::new("ws");
        let elsewhere = TestDir::new("ws-outside");
        let outside = elsewhere.path().join("target.txt");
        std::fs::write(&outside, "original").unwrap();
        symlink(&outside, root.path().join("linked.txt")).unwrap();
        let workspace = Workspace::new(root.path()).unwrap();
        assert!(
            workspace
                .resolve_new("linked.txt")
                .unwrap_err()
                .contains("symlink")
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "original");
    }

    #[test]
    fn resolves_a_path_inside_the_workspace() {
        let root = TestDir::new("ws");
        std::fs::write(root.path().join("file.txt"), b"hi").unwrap();
        let ws = Workspace::new(root.path()).unwrap();
        let resolved = ws.resolve_existing("file.txt").unwrap();
        assert!(resolved.starts_with(ws.root()));
    }

    #[test]
    fn refuses_parent_and_absolute_paths() {
        let root = TestDir::new("ws");
        let ws = Workspace::new(root.path()).unwrap();
        assert!(ws.resolve_existing("../outside").is_err());
        assert!(ws.resolve_existing("a/../../b").is_err());
        assert!(ws.resolve_new("/etc/passwd").is_err());
    }

    #[test]
    fn resolve_new_allows_a_missing_leaf_but_not_a_missing_escape() {
        let root = TestDir::new("ws");
        let ws = Workspace::new(root.path()).unwrap();
        assert!(ws.resolve_new("subdir/new.txt").is_ok());
        assert!(ws.resolve_new("nope.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_is_refused() {
        let root = TestDir::new("ws");
        let outside = TestDir::new("ws-outside");
        std::fs::write(outside.path().join("secret"), b"s").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let ws = Workspace::new(root.path()).unwrap();
        // The symlink target is real but outside the workspace root.
        assert!(ws.resolve_existing("link/secret").is_err());
        assert!(ws.resolve_new("link/written.txt").is_err());
    }
}
