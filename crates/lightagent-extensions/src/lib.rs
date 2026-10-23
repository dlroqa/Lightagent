//! Extensions: installable capability bundles for the Lightagent runtime.
//!
//! An extension is a directory holding an `extension.json` manifest that packages
//! the primitives the runtime already understands — skills, MCP-server
//! declarations, and persona instructions — into one installable unit. Installing
//! one is dropping its directory in place; removing one is deleting it. This is
//! the Lightagent analog of a plugin, and it composes the existing seams rather
//! than adding a new capability of its own:
//!
//! ```text
//! <ext>/
//! ├── extension.json     # manifest (name, version, instructions_file?, mcp_servers?)
//! ├── ONBOARDING.md      # optional Markdown referenced by instructions_file
//! └── skills/            # optional: SKILL.md dirs the extension contributes
//!     └── <skill>/SKILL.md
//! ```
//!
//! Extensions are discovered under two roots, exactly like skills: the global
//! `<home>/extensions/` and a profile's own `<profile>/extensions/`, the profile
//! set overriding the global on a name clash. What an installed extension
//! contributes to a run is gated by [`ExtensionsConfig`]: the whole mechanism can
//! be switched off, and individual extensions disabled by name.
//!
//! An extension never bypasses an existing gate. Its skills and instructions are
//! inert composition (the same shape a hand-written skill or persona has), and an
//! MCP server it contributes is merged into the server list but still only
//! contacted when the MCP subsystem itself is enabled — so an extension widens
//! what is *available*, never what is *permitted*.
//!
//! Manifests are JSON (`serde_json`), matching the rest of the runtime's on-disk
//! format and taking on no new dependency; `mcp_servers` reuses core's
//! [`McpServerEntry`] so a contributed server is described and validated
//! identically to one written into the config by hand.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lightagent_core::paths::create_private_dir;
use lightagent_core::{ExtensionsConfig, McpServerEntry};
use serde::Deserialize;

/// One installed extension, as discovered on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension {
    /// The name used to enable/disable the extension (manifest `name`, else the
    /// directory name).
    pub name: String,
    /// The extension's own version string (informational; empty when unset).
    pub version: String,
    /// A one-line summary of what the extension adds.
    pub description: String,
    /// Persona text appended to the system prompt when the extension is active.
    pub instructions: String,
    /// MCP servers the extension contributes to a run.
    pub mcp_servers: Vec<McpServerEntry>,
    /// The extension's directory on disk.
    pub dir: PathBuf,
}

impl Extension {
    /// The directory holding the extension's contributed skills.
    pub fn skills_dir(&self) -> PathBuf {
        self.dir.join("skills")
    }
}

/// The manifest shape parsed from an `extension.json`.
///
/// Every field but the name is optional so a minimal extension — a manifest and
/// a `skills/` directory — is valid.
#[derive(Debug, Default, Deserialize)]
struct ExtensionManifest {
    name: Option<String>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    instructions: String,
    /// Optional Markdown file inside the extension, appended to instructions.
    instructions_file: Option<String>,
    #[serde(default)]
    mcp_servers: Vec<McpServerEntry>,
}

/// The extensions discovered under a set of directories.
#[derive(Clone, Debug, Default)]
pub struct ExtensionStore {
    extensions: BTreeMap<String, Extension>,
}

impl ExtensionStore {
    /// Load every `<dir>/<ext>/extension.json` under each directory in turn; a
    /// later directory's extension replaces an earlier one of the same name (so a
    /// profile's extensions override the global set). A directory that does not
    /// exist, an entry without a readable manifest, and a manifest that does not
    /// parse are all skipped rather than failing the load.
    pub fn load(dirs: &[PathBuf]) -> Self {
        let mut extensions = BTreeMap::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || is_staging(&entry.file_name().to_string_lossy()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(path.join("extension.json")) else {
                    continue;
                };
                let fallback = entry.file_name().to_string_lossy().into_owned();
                if let Some(ext) = parse_extension(&text, &fallback, &path) {
                    extensions.insert(ext.name.clone(), ext);
                }
            }
        }
        Self { extensions }
    }

    /// Look up an installed extension by name.
    pub fn get(&self, name: &str) -> Option<&Extension> {
        self.extensions.get(name)
    }

    /// Every installed extension, name-sorted.
    pub fn all(&self) -> impl Iterator<Item = &Extension> {
        self.extensions.values()
    }

    /// The installed extension names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.extensions.keys().cloned().collect()
    }

    /// Whether any extension was found.
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    /// The number of installed extensions.
    pub fn len(&self) -> usize {
        self.extensions.len()
    }

    /// Whether an installed extension is active under the given config: the
    /// mechanism is enabled and the extension is not on the disabled list.
    pub fn is_active(&self, name: &str, config: &ExtensionsConfig) -> bool {
        config.enabled && !config.disabled.iter().any(|disabled| disabled == name)
    }

    /// The active extensions under the given config, name-sorted.
    pub fn active<'a>(
        &'a self,
        config: &'a ExtensionsConfig,
    ) -> impl Iterator<Item = &'a Extension> {
        self.extensions
            .values()
            .filter(move |ext| self.is_active(&ext.name, config))
    }

    /// The skill directories contributed by the active extensions, in name order.
    ///
    /// These slot between the global and per-profile skill directories so that an
    /// extension's skills override the global defaults but a profile can still
    /// override an extension's.
    pub fn skill_dirs(&self, config: &ExtensionsConfig) -> Vec<PathBuf> {
        self.active(config).map(Extension::skills_dir).collect()
    }

    /// The MCP servers contributed by the active extensions, in name order.
    ///
    /// These are merged into the configured server list but are still only
    /// contacted when the MCP subsystem is enabled.
    pub fn mcp_servers(&self, config: &ExtensionsConfig) -> Vec<McpServerEntry> {
        self.active(config)
            .flat_map(|ext| ext.mcp_servers.iter().cloned())
            .collect()
    }

    /// The persona block contributed by the active extensions: each extension's
    /// instructions under a heading naming it. Empty when none contribute any.
    pub fn instructions(&self, config: &ExtensionsConfig) -> String {
        let mut out = String::new();
        for ext in self.active(config) {
            let body = ext.instructions.trim();
            if body.is_empty() {
                continue;
            }
            if out.is_empty() {
                out.push_str("# Active extensions\n\n");
            }
            out.push_str(&format!("## Extension: {}\n{}\n\n", ext.name, body));
        }
        out.trim_end().to_owned()
    }
}

/// Parse an `extension.json` into an [`Extension`], falling back to the directory
/// name when the manifest omits `name`.
fn parse_extension(text: &str, fallback_name: &str, dir: &Path) -> Option<Extension> {
    let manifest: ExtensionManifest = serde_json::from_str(text).ok()?;
    let mut instructions = manifest.instructions;
    if let Some(file) = manifest.instructions_file {
        let relative = Path::new(&file);
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        let root = dir.canonicalize().ok()?;
        let full = dir.join(relative).canonicalize().ok()?;
        if !full.starts_with(root) || std::fs::metadata(&full).ok()?.len() > 65_536 {
            return None;
        }
        let file_text = std::fs::read_to_string(full).ok()?;
        if !instructions.trim().is_empty() {
            instructions.push_str("\n\n");
        }
        instructions.push_str(&file_text);
    }
    let name = manifest
        .name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_name.to_owned());
    if name.trim().is_empty() {
        return None;
    }
    Some(Extension {
        name,
        version: manifest.version,
        description: manifest.description,
        instructions,
        mcp_servers: manifest.mcp_servers,
        dir: dir.to_path_buf(),
    })
}

/// The extension directories to load for a profile: the global set, then the
/// profile's own (which overrides on a name clash). Mirrors
/// `lightagent_core::skill_dirs`.
pub fn extension_dirs(home: &Path, profile_dir: &Path) -> Vec<PathBuf> {
    vec![home.join("extensions"), profile_dir.join("extensions")]
}

/// Install a local extension directory atomically into an extension root.
/// Symlinks are refused so a package cannot copy files outside its source.
pub fn install(source: &Path, root: &Path) -> Result<Extension, String> {
    let source = source.canonicalize().map_err(|e| format!("source: {e}"))?;
    if !source.is_dir() {
        return Err("extension source must be a directory".to_owned());
    }
    let manifest = std::fs::read_to_string(source.join("extension.json"))
        .map_err(|e| format!("extension.json: {e}"))?;
    let fallback = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = parse_extension(&manifest, fallback, &source)
        .ok_or_else(|| "invalid extension.json".to_owned())?;
    if !valid_name(&ext.name) {
        return Err(
            "extension name must contain only ASCII letters, numbers, '-' or '_'".to_owned(),
        );
    }
    for server in &ext.mcp_servers {
        if server.name().trim().is_empty() {
            return Err("an MCP server has an empty name".to_owned());
        }
        match server {
            McpServerEntry::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(format!(
                    "MCP server '{}' has an empty command",
                    server.name()
                ));
            }
            McpServerEntry::Http { url, .. }
                if !(url.starts_with("http://") || url.starts_with("https://")) =>
            {
                return Err(format!(
                    "MCP server '{}' needs an http(s) URL",
                    server.name()
                ));
            }
            _ => {}
        }
    }
    create_private_dir(root).map_err(|e| format!("extension root: {e}"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("extension root: {e}"))?;
    if root.starts_with(&source) {
        return Err("extension root cannot be inside its source directory".to_owned());
    }
    let destination = root.join(&ext.name);
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(format!(
            "extension '{}' is already installed in {}",
            ext.name,
            root.display()
        ));
    }
    let staging = root.join(format!(".{}.install-{}", ext.name, std::process::id()));
    if staging.exists() {
        return Err(format!(
            "staging directory {} already exists",
            staging.display()
        ));
    }
    let result = (|| {
        copy_tree(&source, &staging)?;
        std::fs::rename(&staging, &destination)
            .map_err(|e| format!("could not finish installation: {e}"))
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(Extension {
        dir: destination,
        ..ext
    })
}

/// The suffix of the hidden directory an uninstall renames an extension to
/// before deleting it (`.<name>.uninstalling`).
const UNINSTALLING: &str = ".uninstalling";

/// Install and uninstall work in hidden siblings of an extension
/// (`.<name>.install-<pid>`, `.<name>.uninstalling`). They are never loaded:
/// one may still hold a manifest while its files are being copied or removed.
fn is_staging(file_name: &str) -> bool {
    file_name.starts_with('.')
        && (file_name.contains(".install-") || file_name.ends_with(UNINSTALLING))
}

/// What [`try_uninstall`] removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uninstalled {
    /// The installed extension, completely.
    Removed,
    /// What an earlier, interrupted uninstall had left behind.
    Finished,
}

/// Why an extension could not be uninstalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UninstallError {
    /// The name is not a valid extension name, so it names no path.
    InvalidName,
    /// Nothing is installed under the name, and nothing is left to finish.
    NotInstalled { name: String },
    /// What is there is not an installed extension this may remove: a link,
    /// a file, an invalid manifest or one naming another extension.
    Refused { name: String, reason: String },
    /// A running process still uses the extension's directory or a file in
    /// it (on Windows, an MCP server that is still exiting from it), or the
    /// system denied access. Nothing was removed.
    InUse { name: String, reason: String },
    /// An earlier, interrupted removal left the directory without its
    /// manifest and with entries that nothing shows belong to the extension.
    /// Nothing was removed.
    PartiallyRemoved { name: String, remaining: usize },
    /// The extension no longer loads, but some of its files could not be
    /// deleted yet. Uninstalling the name again finishes the removal.
    Unfinished { name: String, reason: String },
    /// Any other failure, as the system reported it.
    Io { name: String, reason: String },
}

impl std::fmt::Display for UninstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName => f.write_str("invalid extension name"),
            Self::NotInstalled { name } => write!(f, "extension '{name}' is not installed"),
            Self::Refused { name, reason } => {
                write!(f, "extension '{name}' cannot be uninstalled: {reason}")
            }
            Self::InUse { name, reason } => write!(
                f,
                "extension '{name}' is in use by a running process or not accessible \
                 ({reason}); it was left installed"
            ),
            Self::PartiallyRemoved { name, remaining } => write!(
                f,
                "extension '{name}' was partially removed earlier: its extension.json is missing \
                 and {remaining} entries remain, left in place because nothing shows they belong \
                 to it"
            ),
            Self::Unfinished { name, reason } => write!(
                f,
                "extension '{name}' was uninstalled, but some of its files could not be deleted \
                 yet ({reason}); uninstall it again to finish"
            ),
            Self::Io { name, reason } => write!(f, "could not uninstall '{name}': {reason}"),
        }
    }
}

impl std::error::Error for UninstallError {}

/// Remove exactly one installed extension from a selected root. See
/// [`try_uninstall`]; this reports its outcome as text.
pub fn uninstall(name: &str, root: &Path) -> Result<(), String> {
    try_uninstall(name, root)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Remove exactly one installed extension from a selected root, or finish an
/// earlier removal of it that was interrupted.
///
/// It only ever removes what it has verified, and only under `root`:
/// - An installed extension (a real directory, not a link, whose manifest
///   names it) is first renamed to `.<name>.uninstalling` beside it, so it
///   stops loading at once, and only then deleted. While a process still uses
///   the directory, Windows refuses the rename, so that case fails before
///   anything is removed and the extension stays installed and intact.
/// - If deleting the renamed directory is interrupted, the extension is
///   already gone; uninstalling the name again finishes the removal.
/// - A directory that an earlier, interrupted removal left without its
///   manifest is removed only if it is empty. Anything still in it is
///   reported, never deleted.
pub fn try_uninstall(name: &str, root: &Path) -> Result<Uninstalled, UninstallError> {
    if !valid_name(name) {
        return Err(UninstallError::InvalidName);
    }
    let destination = root.join(name);
    let trash = root.join(format!(".{name}{UNINSTALLING}"));
    match std::fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Nothing installed: finish a removal interrupted after its rename.
            if !real_dir(&trash, name)? {
                return Err(UninstallError::NotInstalled {
                    name: name.to_owned(),
                });
            }
            std::fs::remove_dir_all(&trash).map_err(|error| failure(name, &error))?;
            return Ok(Uninstalled::Finished);
        }
        Err(error) => return Err(failure(name, &error)),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(refused(name, "it is not a regular directory"));
        }
        Ok(_) => {}
    }
    match std::fs::read_to_string(destination.join("extension.json")) {
        Ok(manifest) => {
            let ext = parse_extension(&manifest, name, &destination)
                .ok_or_else(|| refused(name, "its extension.json is invalid"))?;
            if ext.name != name {
                return Err(refused(
                    name,
                    &format!("its extension.json declares the name '{}'", ext.name),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Left by an earlier, interrupted removal. Only an empty directory
            // is removed: nothing shows that anything else belongs to it.
            let remaining = std::fs::read_dir(&destination)
                .map_err(|error| failure(name, &error))?
                .count();
            if remaining > 0 {
                return Err(UninstallError::PartiallyRemoved {
                    name: name.to_owned(),
                    remaining,
                });
            }
            std::fs::remove_dir(&destination).map_err(|error| failure(name, &error))?;
            return Ok(Uninstalled::Finished);
        }
        Err(error) => return Err(failure(name, &error)),
    }
    // What an earlier, interrupted removal left must go first: the rename
    // needs its name.
    if real_dir(&trash, name)? {
        std::fs::remove_dir_all(&trash).map_err(|error| failure(name, &error))?;
    }
    // Renaming removes nothing, and fails while the directory is in use.
    std::fs::rename(&destination, &trash).map_err(|error| failure(name, &error))?;
    std::fs::remove_dir_all(&trash).map_err(|error| UninstallError::Unfinished {
        name: name.to_owned(),
        reason: error.to_string(),
    })?;
    Ok(Uninstalled::Removed)
}

/// Whether `path` is a real directory. A link or a file there is refused,
/// never followed or removed.
fn real_dir(path: &Path, name: &str) -> Result<bool, UninstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(refused(
            name,
            &format!("{} is not a regular directory", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(failure(name, &error)),
    }
}

fn refused(name: &str, reason: &str) -> UninstallError {
    UninstallError::Refused {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

/// A failure as the system reported it. On Windows a sharing or lock
/// violation, or a denied access, is how a directory or file still in use by
/// a running process fails to move or delete.
fn failure(name: &str, error: &std::io::Error) -> UninstallError {
    let in_use = cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33));
    if in_use {
        UninstallError::InUse {
            name: name.to_owned(),
            reason: error.to_string(),
        }
    } else {
        UninstallError::Io {
            name: name.to_owned(),
            reason: error.to_string(),
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    create_private_dir(destination)
        .map_err(|e| format!("could not create {}: {e}", destination.display()))?;
    for entry in std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_string_lossy() == ".git" {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(format!(
                "extension contains a symlink: {}",
                entry.path().display()
            ));
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("could not copy {}: {e}", entry.path().display()))?;
        } else {
            return Err(format!(
                "extension contains an unsupported file: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. Tests in this module run in parallel
    /// threads of one process, so the name cannot rest on the clock alone: a
    /// coarse timer hands two threads the same nanoseconds and they then load
    /// each other's extensions.
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lightagent-ext-{}-{}",
            std::process::id(),
            lightagent_core::RunId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_extension(root: &Path, dir: &str, manifest: &str) {
        let path = root.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("extension.json"), manifest).unwrap();
    }

    #[test]
    fn parses_a_full_manifest() {
        let manifest = r#"{
            "name": "web-research",
            "version": "0.2.0",
            "description": "Deep web research",
            "instructions": "Prefer primary sources.",
            "mcp_servers": [
                { "transport": "stdio", "name": "fetch", "command": "fetch-server" }
            ]
        }"#;
        let ext = parse_extension(manifest, "fallback", Path::new("/x")).unwrap();
        assert_eq!(ext.name, "web-research");
        assert_eq!(ext.version, "0.2.0");
        assert_eq!(ext.description, "Deep web research");
        assert_eq!(ext.instructions, "Prefer primary sources.");
        assert_eq!(ext.mcp_servers.len(), 1);
        assert_eq!(ext.mcp_servers[0].name(), "fetch");
        assert_eq!(ext.skills_dir(), Path::new("/x/skills"));
    }

    #[test]
    fn name_falls_back_to_the_directory() {
        let ext = parse_extension("{}", "dirname", Path::new("/x")).unwrap();
        assert_eq!(ext.name, "dirname");
        assert!(ext.mcp_servers.is_empty());
    }

    #[test]
    fn malformed_manifest_is_skipped() {
        assert!(parse_extension("{ not json", "d", Path::new("/x")).is_none());
    }

    #[test]
    fn profile_overrides_global_on_name_clash() {
        let global = scratch();
        let profile = scratch();
        write_extension(
            &global,
            "a",
            r#"{ "name": "a", "description": "global a" }"#,
        );
        write_extension(&global, "b", r#"{ "name": "b", "description": "b" }"#);
        write_extension(
            &profile,
            "a",
            r#"{ "name": "a", "description": "profile a" }"#,
        );

        let store = ExtensionStore::load(&[global.clone(), profile.clone()]);
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("a").unwrap().description, "profile a");
        assert_eq!(store.get("a").unwrap().dir, profile.join("a"));

        std::fs::remove_dir_all(&global).ok();
        std::fs::remove_dir_all(&profile).ok();
    }

    #[test]
    fn config_gates_what_is_active() {
        let root = scratch();
        write_extension(
            &root,
            "keep",
            r#"{ "name": "keep", "instructions": "keep me", "mcp_servers": [
                { "transport": "http", "name": "k", "url": "http://127.0.0.1:1" } ] }"#,
        );
        write_extension(
            &root,
            "drop",
            r#"{ "name": "drop", "instructions": "drop me", "mcp_servers": [
                { "transport": "http", "name": "d", "url": "http://127.0.0.1:2" } ] }"#,
        );
        let store = ExtensionStore::load(std::slice::from_ref(&root));

        // Both active by default.
        let all_on = ExtensionsConfig::default();
        assert_eq!(store.active(&all_on).count(), 2);
        assert_eq!(store.mcp_servers(&all_on).len(), 2);
        assert!(store.instructions(&all_on).contains("keep me"));
        assert!(store.instructions(&all_on).contains("drop me"));

        // Disabling one removes exactly its contributions.
        let one_off = ExtensionsConfig {
            enabled: true,
            disabled: vec!["drop".to_owned()],
        };
        assert!(store.is_active("keep", &one_off));
        assert!(!store.is_active("drop", &one_off));
        assert_eq!(store.mcp_servers(&one_off).len(), 1);
        assert_eq!(store.mcp_servers(&one_off)[0].name(), "k");
        assert!(store.instructions(&one_off).contains("keep me"));
        assert!(!store.instructions(&one_off).contains("drop me"));
        assert_eq!(store.skill_dirs(&one_off), vec![root.join("keep/skills")]);

        // The master switch stops everything.
        let all_off = ExtensionsConfig {
            enabled: false,
            disabled: Vec::new(),
        };
        assert_eq!(store.active(&all_off).count(), 0);
        assert!(store.mcp_servers(&all_off).is_empty());
        assert!(store.instructions(&all_off).is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extension_dirs_are_global_then_profile() {
        let dirs = extension_dirs(Path::new("/root-x"), Path::new("/root-x/profiles/p"));
        assert_eq!(dirs[0], Path::new("/root-x/extensions"));
        assert_eq!(dirs[1], Path::new("/root-x/profiles/p/extensions"));
    }

    #[test]
    fn a_markdown_instructions_file_is_loaded_and_removable() {
        let scratch = scratch();
        let source = scratch.join("source");
        let root = scratch.join("installed");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("extension.json"),
            r#"{
            "name":"onboarding", "instructions_file":"ONBOARDING.md"
        }"#,
        )
        .unwrap();
        std::fs::write(
            source.join("ONBOARDING.md"),
            "Use the available tools wisely.",
        )
        .unwrap();
        install(&source, &root).unwrap();
        let store = ExtensionStore::load(std::slice::from_ref(&root));
        assert!(
            store
                .instructions(&ExtensionsConfig::default())
                .contains("Use the available tools wisely.")
        );
        uninstall("onboarding", &root).unwrap();
        assert!(ExtensionStore::load(std::slice::from_ref(&root)).is_empty());
        std::fs::remove_dir_all(scratch).ok();
    }

    #[test]
    fn instructions_file_cannot_escape_the_extension() {
        let scratch = scratch();
        std::fs::write(
            scratch.join("extension.json"),
            r#"{"name":"bad","instructions_file":"../outside.md"}"#,
        )
        .unwrap();
        assert!(
            parse_extension(
                &std::fs::read_to_string(scratch.join("extension.json")).unwrap(),
                "bad",
                &scratch
            )
            .is_none()
        );
        std::fs::remove_dir_all(scratch).ok();
    }

    #[test]
    fn install_discover_and_uninstall_a_bundle() {
        let scratch = scratch();
        let source = scratch.join("source");
        let root = scratch.join("installed");
        std::fs::create_dir_all(source.join("skills/echo")).unwrap();
        std::fs::write(
            source.join("extension.json"),
            r#"{
            "name":"echo", "mcp_servers":[
                {"transport":"stdio","name":"echo","command":"python3","args":["server.py"]}
            ]
        }"#,
        )
        .unwrap();
        std::fs::write(source.join("server.py"), "print('hello')").unwrap();
        std::fs::write(source.join("skills/echo/SKILL.md"), "# Echo").unwrap();

        let installed = install(&source, &root).unwrap();
        assert_eq!(installed.name, "echo");
        assert!(installed.dir.join("server.py").is_file());
        assert_eq!(ExtensionStore::load(std::slice::from_ref(&root)).len(), 1);
        assert!(
            install(&source, &root)
                .unwrap_err()
                .contains("already installed")
        );
        uninstall("echo", &root).unwrap();
        assert!(ExtensionStore::load(std::slice::from_ref(&root)).is_empty());
        assert!(source.join("server.py").is_file());
        std::fs::remove_dir_all(scratch).ok();
    }

    #[test]
    fn rejects_path_names_and_symlinked_package_content() {
        let scratch = scratch();
        let source = scratch.join("source");
        let root = scratch.join("installed");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("extension.json"), r#"{"name":"../escape"}"#).unwrap();
        assert!(install(&source, &root).unwrap_err().contains("name must"));
        assert!(uninstall("../escape", &root).is_err());
        std::fs::write(source.join("extension.json"), r#"{"name":"safe"}"#).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", source.join("outside")).unwrap();
            assert!(install(&source, &root).unwrap_err().contains("symlink"));
            assert!(!root.join("safe").exists());
        }
        std::fs::remove_dir_all(scratch).ok();
    }
}
