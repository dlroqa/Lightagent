//! Skills from a repository or folder that is not an extension (D6-6, new
//! scope: no CLI operation installed bare skills). The skills are packaged
//! as an extension the import generates, so they install atomically into an
//! extension root with links refused and their source recorded, and the
//! extension's enable/disable is their activation. An import is inactive
//! unless the caller asks to activate it, and it never grants a tool
//! permission: skills are instructions the model reads on request.
//!
//! Supported layouts in the selected folder (`#subdir`):
//! - `extension.json`: an extension, installed as it is;
//! - `SKILL.md`: one skill;
//! - `skills/<name>/SKILL.md`: a set of skills;
//! - `<name>/SKILL.md`: a set of skills.
//!
//! Anything else is refused. Nothing fetched is run: git fetches without a
//! prompt or Git LFS smudging, and files are only read and copied.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use lightagent_core::skills::Skill;
use lightagent_core::{ConfigStore, LightagentPaths, RunId, SkillStore};
use serde::Serialize;

use crate::manage::{self, ManageError, SOURCE_FILE, Scope, Source};

/// The largest SKILL.md an import accepts.
const MAX_SKILL_BYTES: u64 = 512 * 1024;
/// The most skills one import accepts.
const MAX_SKILLS: usize = 200;

/// How to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub scope: Scope,
    /// The extension to install the skills as; derived from the source when unset.
    pub name: Option<String>,
    /// A branch, tag or commit to install (Git sources only).
    pub revision: Option<String>,
    /// Replace an installed extension of the same name; it is kept if anything fails.
    pub replace: bool,
    /// Make the skills available to runs now; otherwise they are installed inactive.
    pub activate: bool,
}

/// Which supported layout the source had.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Extension,
    SingleSkill,
    SkillsFolder,
    SkillFolders,
}

/// What an import did.
#[derive(Clone, Debug, Serialize)]
pub struct Imported {
    pub name: String,
    pub dir: PathBuf,
    pub scope: Scope,
    pub layout: Layout,
    pub skills: Vec<String>,
    pub active: bool,
    pub replaced: bool,
    pub source: Source,
}

fn invalid(message: impl Into<String>) -> ManageError {
    ManageError::Invalid(message.into())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A branch, tag or commit name git cannot read as an option or a path
/// outside its references.
pub(crate) fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 200
        && !revision.starts_with('-')
        && !revision.contains("..")
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
}

/// The folders directly in `dir` that hold a SKILL.md, by name.
fn skill_children(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| !name.starts_with('.'))
                        && plain_dir(path)
                        && plain_file(&path.join("SKILL.md"))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The skill a folder's SKILL.md names, checked.
fn read_skill(dir: &Path) -> Result<String, ManageError> {
    let file = dir.join("SKILL.md");
    let size = std::fs::symlink_metadata(&file)
        .map_err(|e| invalid(format!("{}: {e}", file.display())))?
        .len();
    if size > MAX_SKILL_BYTES {
        return Err(invalid(format!(
            "{} is larger than {} KiB",
            file.display(),
            MAX_SKILL_BYTES / 1024
        )));
    }
    let text =
        std::fs::read_to_string(&file).map_err(|e| invalid(format!("{}: {e}", file.display())))?;
    let fallback = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let skill = Skill::from_markdown(&text, fallback)
        .ok_or_else(|| invalid(format!("{} names no skill", file.display())))?;
    if !valid_name(&skill.name) {
        return Err(invalid(format!(
            "skill name {:?} must be ASCII letters, digits, '-' or '_'",
            skill.name
        )));
    }
    Ok(skill.name)
}

/// A name for the import from its source: the folder, or the repository.
fn derived_name(source: &Path, url: Option<&str>, subdir: &Path) -> String {
    let from_subdir = subdir
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned);
    let raw = from_subdir
        .or_else(|| {
            url.and_then(|url| url.trim_end_matches('/').rsplit('/').next())
                .map(|last| last.trim_end_matches(".git").to_owned())
        })
        .or_else(|| {
            source
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned
        .trim_matches('-')
        .chars()
        .take(64)
        .collect::<String>();
    if cleaned.is_empty() {
        "skills".to_owned()
    } else {
        cleaned
    }
}

fn git(args: Vec<OsString>) -> Result<String, ManageError> {
    let output = Command::new("git")
        .args(&args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .output()
        .map_err(|e| ManageError::Unavailable(format!("could not start git: {e}")))?;
    if !output.status.success() {
        let verb = args
            .iter()
            .find(|arg| !arg.to_string_lossy().starts_with('-'))
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(ManageError::Unavailable(format!(
            "git {verb} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The folder to import from, its source record and a derived name.
fn fetch(
    work: &Path,
    source: &Path,
    revision: Option<&str>,
) -> Result<(PathBuf, Source, String), ManageError> {
    let Some((url, subdir)) = manage::git_source(source)? else {
        if revision.is_some() {
            return Err(invalid("a revision applies only to a Git source"));
        }
        let path = source
            .canonicalize()
            .map_err(|e| invalid(format!("source: {e}")))?;
        if !plain_dir(&path) {
            return Err(invalid("the source must be a folder"));
        }
        let name = derived_name(&path, None, Path::new(""));
        return Ok((path.clone(), Source::Local { path }, name));
    };
    if let Some(revision) = revision
        && !valid_revision(revision)
    {
        return Err(invalid(format!(
            "{revision:?} is not a branch, tag or commit name"
        )));
    }
    std::fs::create_dir_all(work).map_err(|e| ManageError::Store(e.to_string()))?;
    let checkout = work.join("checkout");
    let mut clone: Vec<OsString> = vec!["clone".into(), "--depth".into(), "1".into()];
    if revision.is_some() {
        clone.push("--no-checkout".into());
    }
    clone.extend([
        "--".into(),
        url.clone().into(),
        checkout.clone().into_os_string(),
    ]);
    git(clone)?;
    if let Some(revision) = revision {
        let at = |args: &[&str]| {
            let mut all: Vec<OsString> = vec!["-C".into(), checkout.clone().into_os_string()];
            all.extend(args.iter().map(OsString::from));
            all
        };
        git(at(&["fetch", "--depth", "1", "--", "origin", revision]))?;
        git(at(&["checkout", "--detach", "FETCH_HEAD"]))?;
    }
    let commit = git(vec![
        "-C".into(),
        checkout.clone().into_os_string(),
        "rev-parse".into(),
        "HEAD".into(),
    ])?;
    let base = checkout.join(&subdir);
    let inside = base
        .canonicalize()
        .ok()
        .zip(checkout.canonicalize().ok())
        .is_some_and(|(base, checkout)| base.starts_with(checkout));
    if !plain_dir(&base) || !inside {
        return Err(invalid(format!(
            "the repository has no folder {:?}",
            subdir.display().to_string()
        )));
    }
    let name = derived_name(source, Some(url.as_str()), &subdir);
    let record = Source::Git {
        url,
        subdir: subdir.to_string_lossy().replace('\\', "/"),
        revision: commit,
    };
    Ok((base, record, name))
}

/// Import skills from `source` into the root for `options.scope`.
pub fn import(
    paths: &LightagentPaths,
    source: &Path,
    options: ImportOptions,
) -> Result<Imported, ManageError> {
    let root = manage::root(paths, options.scope)?;
    let work =
        std::env::temp_dir().join(format!("lightagent-skill-import-{}", RunId::new().as_str()));
    let result = import_in(paths, &work, &root, source, &options);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn import_in(
    paths: &LightagentPaths,
    work: &Path,
    root: &Path,
    source: &Path,
    options: &ImportOptions,
) -> Result<Imported, ManageError> {
    let (base, origin, derived) = fetch(work, source, options.revision.as_deref())?;
    if let Some(name) = &options.name
        && !valid_name(name)
    {
        return Err(invalid(format!(
            "the name {name:?} must be ASCII letters, digits, '-' or '_'"
        )));
    }
    let name = options.name.clone().unwrap_or(derived);

    // The layout, and the skill folders a generated extension takes.
    let (layout, folders) = if plain_file(&base.join("extension.json")) {
        (Layout::Extension, Vec::new())
    } else if plain_file(&base.join("SKILL.md")) {
        (Layout::SingleSkill, vec![base.clone()])
    } else if plain_dir(&base.join("skills")) && !skill_children(&base.join("skills")).is_empty() {
        (Layout::SkillsFolder, skill_children(&base.join("skills")))
    } else {
        let children = skill_children(&base);
        if children.is_empty() {
            return Err(invalid(
                "no supported layout: expected extension.json, SKILL.md, skills/<name>/SKILL.md or <name>/SKILL.md in the selected folder",
            ));
        }
        (Layout::SkillFolders, children)
    };
    if folders.len() > MAX_SKILLS {
        return Err(invalid(format!("more than {MAX_SKILLS} skills")));
    }

    // The folder that is installed, with the source record in it.
    std::fs::create_dir_all(work).map_err(|e| ManageError::Store(e.to_string()))?;
    let stage = work.join("stage").join(&name);
    let mut skills = Vec::new();
    if layout == Layout::Extension {
        crate::copy_tree(&base, &stage).map_err(invalid)?;
        let manifest = std::fs::read_to_string(stage.join("extension.json"))
            .map_err(|e| invalid(format!("extension.json: {e}")))?;
        let ext = crate::parse_extension(&manifest, &name, &stage)
            .ok_or_else(|| invalid("invalid extension.json"))?;
        if options
            .name
            .as_ref()
            .is_some_and(|given| given != &ext.name)
        {
            return Err(invalid(format!(
                "this source is an extension named '{}'",
                ext.name
            )));
        }
        skills = SkillStore::load(&[ext.skills_dir()]).names();
    } else {
        for folder in &folders {
            let skill = read_skill(folder)?;
            if skills.contains(&skill) {
                return Err(invalid(format!("two skills are named {skill:?}")));
            }
            crate::copy_tree(folder, &stage.join("skills").join(&skill)).map_err(invalid)?;
            skills.push(skill);
        }
        let manifest = serde_json::json!({
            "name": name,
            "description": format!("Skills imported from {}", source.to_string_lossy()),
        });
        std::fs::write(stage.join("extension.json"), manifest.to_string())
            .map_err(|e| ManageError::Store(e.to_string()))?;
    }
    let record =
        serde_json::to_vec_pretty(&origin).map_err(|e| ManageError::Store(e.to_string()))?;
    std::fs::write(stage.join(SOURCE_FILE), record)
        .map_err(|e| ManageError::Store(e.to_string()))?;
    let manifest = std::fs::read_to_string(stage.join("extension.json"))
        .map_err(|e| invalid(format!("extension.json: {e}")))?;
    let ext_name = crate::parse_extension(&manifest, &name, &stage)
        .map(|ext| ext.name)
        .ok_or_else(|| invalid("invalid extension.json"))?;

    let destination = root.join(&ext_name);
    let exists = std::fs::symlink_metadata(&destination).is_ok();
    if exists && !options.replace {
        return Err(ManageError::Conflict(format!(
            "an extension named '{ext_name}' is already installed; choose another name or replace it"
        )));
    }
    let pid = std::process::id();
    let incoming = root.join(format!(".{ext_name}.install-incoming-{pid}"));
    let previous = root.join(format!(".{ext_name}.install-previous-{pid}"));
    let installed = if exists {
        // The new copy is installed beside the old one, then the two are
        // exchanged; the old one is put back if anything fails.
        let ext = crate::install(&stage, &incoming).map_err(|message| {
            let _ = std::fs::remove_dir_all(&incoming);
            invalid(message)
        })?;
        if let Err(error) = std::fs::rename(&destination, &previous) {
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(ManageError::Conflict(format!(
                "the installed '{ext_name}' could not be moved aside ({error}); it is unchanged"
            )));
        }
        if let Err(error) = std::fs::rename(&ext.dir, &destination) {
            let _ = std::fs::rename(&previous, &destination);
            let _ = std::fs::remove_dir_all(&incoming);
            return Err(ManageError::Store(format!(
                "could not put the new copy in place ({error}); the installed one was restored"
            )));
        }
        let _ = std::fs::remove_dir_all(&incoming);
        destination.clone()
    } else {
        crate::install(&stage, root).map_err(invalid)?.dir
    };

    let saved = ConfigStore::at(paths).update(None, |config| {
        if options.activate {
            config.extensions.enabled = true;
            config.extensions.disabled.retain(|n| n != &ext_name);
        } else if !config.extensions.disabled.iter().any(|n| n == &ext_name) {
            config.extensions.disabled.push(ext_name.clone());
            config.extensions.disabled.sort();
        }
        Ok::<_, String>(config.extensions.enabled)
    });
    let enabled = match saved {
        Ok((enabled, _)) => enabled,
        Err(error) => {
            if exists {
                let _ = std::fs::remove_dir_all(&destination);
                let _ = std::fs::rename(&previous, &destination);
            } else {
                let _ = crate::uninstall(&ext_name, root);
            }
            return Err(ManageError::Store(format!(
                "could not save the activation state ({error}); nothing was changed"
            )));
        }
    };
    if exists {
        let _ = std::fs::remove_dir_all(&previous);
    }
    Ok(Imported {
        name: ext_name,
        dir: installed,
        scope: options.scope,
        layout,
        skills,
        active: enabled && options.activate,
        replaced: exists,
        source: origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_cannot_be_options_or_leave_the_references() {
        for good in ["main", "v1.2.0", "release/2026-11", "0123456789abcdef"] {
            assert!(valid_revision(good), "{good}");
        }
        for bad in ["", "-upload-pack=x", "a..b", "a b", "a;b", "a\\b"] {
            assert!(!valid_revision(bad), "{bad}");
        }
    }

    #[test]
    fn a_name_is_derived_from_the_folder_or_the_repository() {
        assert_eq!(
            derived_name(
                Path::new("x"),
                Some("https://github.com/o/My.Skills.git"),
                Path::new("")
            ),
            "My-Skills"
        );
        assert_eq!(
            derived_name(
                Path::new("x"),
                Some("https://github.com/o/r"),
                Path::new("packs/writing")
            ),
            "writing"
        );
        assert_eq!(
            derived_name(Path::new("/tmp/my skills"), None, Path::new("")),
            "my-skills"
        );
    }
}
