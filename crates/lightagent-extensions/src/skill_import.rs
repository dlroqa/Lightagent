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

/// The git transport an import may use: HTTPS for every caller. Only this
/// module's tests add a local `file://` repository, built on the runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    Https,
    #[cfg(test)]
    LocalFixture,
}

impl Transport {
    fn protocol(self) -> &'static str {
        match self {
            Self::Https => "https",
            #[cfg(test)]
            Self::LocalFixture => "file",
        }
    }
}

fn git_source_for(
    source: &Path,
    transport: Transport,
) -> Result<Option<(String, PathBuf)>, ManageError> {
    match transport {
        Transport::Https => manage::git_source(source),
        #[cfg(test)]
        Transport::LocalFixture => Ok(tests::fixture_source(source)),
    }
}

fn git(args: Vec<OsString>, transport: Transport) -> Result<String, ManageError> {
    let output = Command::new("git")
        .args(&args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", transport.protocol())
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
    transport: Transport,
) -> Result<(PathBuf, Source, String), ManageError> {
    let Some((url, subdir)) = git_source_for(source, transport)? else {
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
    git(clone, transport)?;
    if let Some(revision) = revision {
        let at = |args: &[&str]| {
            let mut all: Vec<OsString> = vec!["-C".into(), checkout.clone().into_os_string()];
            all.extend(args.iter().map(OsString::from));
            all
        };
        git(
            at(&["fetch", "--depth", "1", "--", "origin", revision]),
            transport,
        )?;
        git(at(&["checkout", "--detach", "FETCH_HEAD"]), transport)?;
    }
    let commit = git(
        vec![
            "-C".into(),
            checkout.clone().into_os_string(),
            "rev-parse".into(),
            "HEAD".into(),
        ],
        transport,
    )?;
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
    let result = import_in(paths, &work, &root, source, &options, Transport::Https);
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// [`import`] over a chosen transport; only tests choose another.
#[cfg(test)]
fn import_with(
    paths: &LightagentPaths,
    source: &Path,
    options: ImportOptions,
    transport: Transport,
) -> Result<Imported, ManageError> {
    let root = manage::root(paths, options.scope)?;
    let work =
        std::env::temp_dir().join(format!("lightagent-skill-import-{}", RunId::new().as_str()));
    let result = import_in(paths, &work, &root, source, &options, transport);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn import_in(
    paths: &LightagentPaths,
    work: &Path,
    root: &Path,
    source: &Path,
    options: &ImportOptions,
    transport: Transport,
) -> Result<Imported, ManageError> {
    let (base, origin, derived) = fetch(work, source, options.revision.as_deref(), transport)?;
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

    /// A `file://` source (with an optional `#folder`) for the local fixture
    /// transport; anything else is a local folder.
    pub(super) fn fixture_source(source: &Path) -> Option<(String, PathBuf)> {
        let text = source.to_string_lossy();
        if !text.starts_with("file://") {
            return None;
        }
        let (url, subdir) = text.split_once('#').unwrap_or((&text, ""));
        Some((url.to_owned(), PathBuf::from(subdir)))
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lightagent-skill-fixture-{label}-{}",
            RunId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture\n---\n{body}\n"),
        )
        .unwrap();
    }

    /// A Git repository on the runner: `v1` tagged, then `v2` on the
    /// branch. Any commit may be fetched by its id.
    fn fixture(dir: &Path) -> (String, String) {
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        write_skill(&dir.join("skills").join("alpha"), "alpha", "version one");
        write_skill(
            &dir.join("packs").join("writing"),
            "writing",
            "a single skill",
        );
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-q", "-m", "v1"]);
        run_git(dir, &["tag", "v1"]);
        let first = run_git(dir, &["rev-parse", "HEAD"]);
        write_skill(&dir.join("skills").join("alpha"), "alpha", "version two");
        run_git(dir, &["commit", "-q", "-am", "v2"]);
        let second = run_git(dir, &["rev-parse", "HEAD"]);
        (first, second)
    }

    fn file_url(path: &Path) -> String {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.starts_with('/') {
            format!("file://{text}")
        } else {
            format!("file:///{text}")
        }
    }

    fn home(label: &str) -> LightagentPaths {
        let paths = LightagentPaths::rooted_at(scratch(label));
        ConfigStore::at(&paths)
            .update(None, |config| {
                config.extensions.enabled = true;
                Ok::<_, String>(())
            })
            .unwrap();
        paths
    }

    fn options(revision: Option<&str>, replace: bool) -> ImportOptions {
        ImportOptions {
            scope: Scope::Global,
            name: Some("fixture".to_owned()),
            revision: revision.map(str::to_owned),
            replace,
            activate: false,
        }
    }

    fn commit_of(imported: &Imported) -> String {
        match &imported.source {
            Source::Git { revision, .. } => revision.clone(),
            other => panic!("not a Git source: {other:?}"),
        }
    }

    /// Every file under `dir`, by relative path, with its bytes.
    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    found.push((relative, std::fs::read(&path).unwrap()));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn a_git_fixture_installs_the_requested_revision_and_records_its_commit() {
        let repo = scratch("repo").join("repo");
        let (first, second) = fixture(&repo);
        let paths = home("revisions");
        let installed = manage::root(&paths, Scope::Global).unwrap().join("fixture");
        let url = PathBuf::from(file_url(&repo));
        let read = || {
            std::fs::read_to_string(installed.join("skills").join("alpha").join("SKILL.md"))
                .unwrap()
        };

        // A tag installs the tagged commit, inactive, and records it.
        let imported = import_with(
            &paths,
            &url,
            options(Some("v1"), false),
            Transport::LocalFixture,
        )
        .unwrap();
        assert_eq!(imported.layout, Layout::SkillsFolder);
        assert_eq!(imported.skills, vec!["alpha".to_owned()]);
        assert!(!imported.active);
        assert_eq!(commit_of(&imported), first);
        assert!(read().contains("version one"));
        let record = std::fs::read_to_string(installed.join(SOURCE_FILE)).unwrap();
        assert!(record.contains(&first), "{record}");

        // The default branch replaces it with the newer commit.
        let imported =
            import_with(&paths, &url, options(None, true), Transport::LocalFixture).unwrap();
        assert!(imported.replaced);
        assert_eq!(commit_of(&imported), second);
        assert!(read().contains("version two"));

        // A commit id installs exactly that commit.
        let imported = import_with(
            &paths,
            &url,
            options(Some(&first), true),
            Transport::LocalFixture,
        )
        .unwrap();
        assert_eq!(commit_of(&imported), first);
        assert!(read().contains("version one"));
    }

    #[test]
    fn a_folder_in_the_repository_is_selected_with_its_layout() {
        let repo = scratch("folder").join("repo");
        let (_, second) = fixture(&repo);
        let paths = home("folder-home");
        let url = PathBuf::from(format!("{}#packs/writing", file_url(&repo)));
        let options = ImportOptions {
            name: None,
            ..options(None, false)
        };
        let imported = import_with(&paths, &url, options, Transport::LocalFixture).unwrap();
        assert_eq!(imported.name, "writing");
        assert_eq!(imported.layout, Layout::SingleSkill);
        assert_eq!(imported.skills, vec!["writing".to_owned()]);
        match &imported.source {
            Source::Git {
                subdir, revision, ..
            } => {
                assert_eq!(subdir, "packs/writing");
                assert_eq!(revision, &second);
            }
            other => panic!("not a Git source: {other:?}"),
        }
    }

    #[test]
    fn a_failed_fetch_keeps_the_installed_copy() {
        let work = scratch("failures");
        let repo = work.join("repo");
        fixture(&repo);
        let paths = home("failures-home");
        let root = manage::root(&paths, Scope::Global).unwrap();
        let url = PathBuf::from(file_url(&repo));
        import_with(
            &paths,
            &url,
            options(Some("v1"), false),
            Transport::LocalFixture,
        )
        .unwrap();
        let before = snapshot(&root.join("fixture"));

        let error = import_with(
            &paths,
            &url,
            options(Some("no-such-branch"), true),
            Transport::LocalFixture,
        )
        .unwrap_err();
        assert!(matches!(error, ManageError::Unavailable(_)), "{error:?}");

        let missing = PathBuf::from(format!("{}#no/such/folder", file_url(&repo)));
        let error = import_with(
            &paths,
            &missing,
            options(None, true),
            Transport::LocalFixture,
        )
        .unwrap_err();
        assert!(matches!(error, ManageError::Invalid(_)), "{error:?}");

        let gone = PathBuf::from(file_url(&work.join("gone")));
        let error =
            import_with(&paths, &gone, options(None, true), Transport::LocalFixture).unwrap_err();
        assert!(matches!(error, ManageError::Unavailable(_)), "{error:?}");

        assert_eq!(snapshot(&root.join("fixture")), before);
        let leftovers: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

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
