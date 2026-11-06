//! The extension operations behind `lightagent extensions …`, shared by the
//! CLI, the HTTP API and the desktop app. Each acts on the same extension
//! roots (the global `<home>/extensions/` and the active profile's own) and
//! the same configuration, with the same checks and the same errors.
//!
//! An install grants no more than the CLI always has: it switches the
//! extension system on and takes the extension off the disabled list. The
//! MCP servers an extension declares are contacted only while `mcp.enabled`
//! is set, and an install sets it only when the caller asks
//! ([`InstallOptions::activate_mcp`]).

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use lightagent_core::{
    Config, ConfigStore, LightagentPaths, McpServerEntry, ProfileStore, RunId, SkillStore,
};
use serde::{Deserialize, Serialize};

use crate::{Extension, ExtensionStore, UninstallError, extension_dirs};

/// The file an install writes into the installed extension to record where
/// it came from. One the source itself holds is replaced.
pub const SOURCE_FILE: &str = ".lightagent-source.json";

/// Where an extension is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// `<home>/extensions/`, loaded by every profile.
    Global,
    /// The active profile's own `extensions/`, which overrides the global set.
    Profile,
}

/// Why an extension operation did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManageError {
    /// No installed extension has the name.
    NotFound(String),
    /// The request cannot work as given: a bad name, source or manifest.
    Invalid(String),
    /// What is installed does not allow it now: already installed, in use,
    /// or left by an interrupted removal.
    Conflict(String),
    /// Something outside Lightagent failed: git could not start or fetch.
    Unavailable(String),
    /// The configuration or the profile store could not be read or saved.
    Store(String),
}

impl std::fmt::Display for ManageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Invalid(message)
            | Self::Conflict(message)
            | Self::Unavailable(message)
            | Self::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ManageError {}

/// Where an installed extension came from, as its install recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A local folder.
    Local { path: PathBuf },
    /// An HTTPS Git repository: the folder in it, and the commit installed.
    Git {
        url: String,
        subdir: String,
        revision: String,
    },
}

/// One installed extension in a listing.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub name: String,
    pub version: String,
    pub description: String,
    pub active: bool,
    pub scope: Scope,
    pub skills_dir: PathBuf,
    pub mcp_servers: Vec<String>,
}

/// Every installed extension the active profile loads.
#[derive(Clone, Debug, Serialize)]
pub struct Listing {
    /// The master switch, `extensions.enabled`.
    pub enabled: bool,
    pub extensions: Vec<Summary>,
}

/// An MCP server an extension declares, as shown before it is trusted: what
/// it runs or where it connects. Of its environment and headers only the
/// names are shown, and of its auth reference only whether there is one.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpServerView {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: Vec<String>,
    },
    Http {
        name: String,
        url: String,
        headers: Vec<String>,
        auth: bool,
    },
}

/// One installed extension in full.
#[derive(Clone, Debug, Serialize)]
pub struct Detail {
    pub name: String,
    pub version: String,
    pub description: String,
    pub active: bool,
    pub scope: Scope,
    pub dir: PathBuf,
    pub instructions: String,
    pub mcp_servers: Vec<String>,
    pub mcp: Vec<McpServerView>,
    pub skills: Vec<String>,
    pub source: Option<Source>,
    /// Whether MCP is on: the extension's servers are contacted only then.
    pub mcp_enabled: bool,
}

/// How to install.
#[derive(Clone, Copy, Debug)]
pub struct InstallOptions {
    pub scope: Scope,
    /// Switch MCP on when the extension declares MCP servers.
    pub activate_mcp: bool,
}

/// What an install did.
#[derive(Clone, Debug, Serialize)]
pub struct Installed {
    pub name: String,
    pub dir: PathBuf,
    pub scope: Scope,
    pub mcp_servers: Vec<String>,
    pub active: bool,
    /// Whether MCP is on after the install.
    pub mcp_enabled: bool,
    pub source: Source,
}

/// An extension switched on or off.
#[derive(Clone, Debug, Serialize)]
pub struct Toggled {
    pub name: String,
    pub active: bool,
    /// The master switch, `extensions.enabled`.
    pub extensions_enabled: bool,
}

/// An extension removed from a root.
#[derive(Clone, Debug, Serialize)]
pub struct Removed {
    pub name: String,
    pub removed_from: PathBuf,
}

/// The extension root for `scope`.
pub fn root(paths: &LightagentPaths, scope: Scope) -> Result<PathBuf, ManageError> {
    match scope {
        Scope::Global => Ok(paths.extensions_dir()),
        Scope::Profile => profile_root(paths)?.ok_or_else(|| {
            ManageError::Invalid("no active profile — run `lightagent init` first".to_owned())
        }),
    }
}

/// The active profile's own extension root, if a profile is active.
fn profile_root(paths: &LightagentPaths) -> Result<Option<PathBuf>, ManageError> {
    let profiles = ProfileStore::new(paths.root());
    let active = profiles
        .active()
        .map_err(|error| ManageError::Store(error.to_string()))?;
    Ok(active.map(|active| profiles.handle(&active).dir().join("extensions")))
}

/// The extensions the active profile loads, and that profile's own root.
fn installed(paths: &LightagentPaths) -> Result<(ExtensionStore, Option<PathBuf>), ManageError> {
    let profiles = ProfileStore::new(paths.root());
    let active = profiles
        .active()
        .map_err(|error| ManageError::Store(error.to_string()))?;
    Ok(match active {
        Some(active) => {
            let dir = profiles.handle(&active).dir().to_path_buf();
            (
                ExtensionStore::load(&extension_dirs(paths.root(), &dir)),
                Some(dir.join("extensions")),
            )
        }
        None => (ExtensionStore::load(&[paths.extensions_dir()]), None),
    })
}

fn scope_of(ext: &Extension, own: Option<&Path>) -> Scope {
    if own.is_some_and(|own| ext.dir.starts_with(own)) {
        Scope::Profile
    } else {
        Scope::Global
    }
}

fn not_installed(name: &str) -> ManageError {
    ManageError::NotFound(format!("no installed extension named '{name}'"))
}

fn server_names(ext: &Extension) -> Vec<String> {
    ext.mcp_servers
        .iter()
        .map(|server| server.name().to_owned())
        .collect()
}

fn server_view(server: &McpServerEntry) -> McpServerView {
    match server {
        McpServerEntry::Stdio {
            name,
            command,
            args,
            env,
        } => McpServerView::Stdio {
            name: name.clone(),
            command: command.clone(),
            args: args.clone(),
            env: env.keys().cloned().collect(),
        },
        McpServerEntry::Http {
            name,
            url,
            headers,
            auth,
            ..
        } => McpServerView::Http {
            name: name.clone(),
            url: url.clone(),
            headers: headers.keys().cloned().collect(),
            auth: auth.is_some(),
        },
    }
}

/// Where `ext` came from, if its install recorded it.
pub fn source_of(ext: &Extension) -> Option<Source> {
    let text = std::fs::read_to_string(ext.dir.join(SOURCE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// `extensions list`: every installed extension the active profile loads.
pub fn list(paths: &LightagentPaths, config: &Config) -> Result<Listing, ManageError> {
    let (store, own) = installed(paths)?;
    let extensions = store
        .all()
        .map(|ext| Summary {
            name: ext.name.clone(),
            version: ext.version.clone(),
            description: ext.description.clone(),
            active: store.is_active(&ext.name, &config.extensions),
            scope: scope_of(ext, own.as_deref()),
            skills_dir: ext.skills_dir(),
            mcp_servers: server_names(ext),
        })
        .collect();
    Ok(Listing {
        enabled: config.extensions.enabled,
        extensions,
    })
}

/// `extensions show <name>`: one extension's manifest and contributions.
pub fn show(paths: &LightagentPaths, config: &Config, name: &str) -> Result<Detail, ManageError> {
    let (store, own) = installed(paths)?;
    let ext = store.get(name).ok_or_else(|| not_installed(name))?;
    Ok(Detail {
        name: ext.name.clone(),
        version: ext.version.clone(),
        description: ext.description.clone(),
        active: store.is_active(name, &config.extensions),
        scope: scope_of(ext, own.as_deref()),
        dir: ext.dir.clone(),
        instructions: ext.instructions.clone(),
        mcp_servers: server_names(ext),
        mcp: ext.mcp_servers.iter().map(server_view).collect(),
        skills: SkillStore::load(&[ext.skills_dir()]).names(),
        source: source_of(ext),
        mcp_enabled: config.mcp.enabled,
    })
}

/// `extensions enable|disable <name>`: keep an installed extension active or
/// inactive. A name that is not installed is refused, so a typo is caught.
pub fn set_enabled(
    paths: &LightagentPaths,
    name: &str,
    enabled: bool,
) -> Result<Toggled, ManageError> {
    let (store, _) = installed(paths)?;
    if store.get(name).is_none() {
        return Err(not_installed(name));
    }
    let (extensions_enabled, _) = ConfigStore::at(paths)
        .update(None, |config| {
            if enabled {
                config.extensions.disabled.retain(|n| n != name);
            } else if !config.extensions.disabled.iter().any(|n| n == name) {
                config.extensions.disabled.push(name.to_owned());
                config.extensions.disabled.sort();
            }
            Ok::<_, String>(config.extensions.enabled)
        })
        .map_err(|error| ManageError::Store(error.to_string()))?;
    Ok(Toggled {
        name: name.to_owned(),
        active: extensions_enabled && enabled,
        extensions_enabled,
    })
}

/// `extensions install <source>`: a local folder, or an HTTPS Git repository
/// (`#subdir` selects a folder in it), into the root for `options.scope`.
/// An extension already installed there is refused and left as it is. If
/// the source record or the settings cannot be saved, the new copy is
/// removed again.
pub fn install(
    paths: &LightagentPaths,
    source: &Path,
    options: InstallOptions,
) -> Result<Installed, ManageError> {
    let root = root(paths, options.scope)?;
    let (ext, origin) = fetch_and_install(source, &root)?;
    let recorded = serde_json::to_vec_pretty(&origin)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            std::fs::write(ext.dir.join(SOURCE_FILE), bytes).map_err(|error| error.to_string())
        });
    if let Err(error) = recorded {
        let _ = crate::uninstall(&ext.name, &root);
        return Err(ManageError::Store(format!(
            "could not record the extension's source: {error}"
        )));
    }
    let saved = ConfigStore::at(paths).update(None, |config| {
        config.extensions.enabled = true;
        config.extensions.disabled.retain(|name| name != &ext.name);
        if options.activate_mcp && !ext.mcp_servers.is_empty() {
            config.mcp.enabled = true;
        }
        Ok::<_, String>(config.mcp.enabled)
    });
    match saved {
        Ok((mcp_enabled, _)) => Ok(Installed {
            name: ext.name.clone(),
            dir: ext.dir.clone(),
            scope: options.scope,
            mcp_servers: server_names(&ext),
            active: true,
            mcp_enabled,
            source: origin,
        }),
        Err(error) => {
            let _ = crate::uninstall(&ext.name, &root);
            Err(ManageError::Store(format!(
                "could not save extension settings: {error}"
            )))
        }
    }
}

/// `extensions uninstall <name>`: remove exactly that installed copy from
/// the root for `scope`.
pub fn uninstall(
    paths: &LightagentPaths,
    name: &str,
    scope: Scope,
) -> Result<Removed, ManageError> {
    let root = root(paths, scope)?;
    crate::try_uninstall(name, &root).map_err(|error| match &error {
        UninstallError::InvalidName => ManageError::Invalid(error.to_string()),
        UninstallError::NotInstalled { .. } => ManageError::NotFound(error.to_string()),
        UninstallError::Io { .. } => ManageError::Store(error.to_string()),
        UninstallError::Refused { .. }
        | UninstallError::InUse { .. }
        | UninstallError::PartiallyRemoved { .. }
        | UninstallError::Unfinished { .. } => ManageError::Conflict(error.to_string()),
    })?;
    Ok(Removed {
        name: name.to_owned(),
        removed_from: root,
    })
}

/// A refusal from [`crate::install`]: a copy already there is a conflict,
/// anything else a source that cannot be installed.
fn install_refusal(message: String) -> ManageError {
    if message.contains("is already installed") || message.contains("staging directory") {
        ManageError::Conflict(message)
    } else {
        ManageError::Invalid(message)
    }
}

/// Install from a local folder, or from a shallow clone of an HTTPS Git
/// repository that is removed afterwards. Nothing fetched is run: the clone
/// is read and copied like a local folder.
fn fetch_and_install(source: &Path, root: &Path) -> Result<(Extension, Source), ManageError> {
    let Some((url, subdir)) = git_source(source)? else {
        let ext = crate::install(source, root).map_err(install_refusal)?;
        let path = source
            .canonicalize()
            .unwrap_or_else(|_| source.to_path_buf());
        return Ok((ext, Source::Local { path }));
    };
    let checkout = std::env::temp_dir().join(format!(
        "lightagent-extension-fetch-{}",
        RunId::new().as_str()
    ));
    let result = (|| {
        let output = Command::new("git")
            .args(["clone", "--depth", "1", "--"])
            .arg(&url)
            .arg(&checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "https")
            .output()
            .map_err(|e| ManageError::Unavailable(format!("could not start git: {e}")))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            return Err(ManageError::Unavailable(format!(
                "git clone failed: {}",
                detail.trim()
            )));
        }
        let head = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| ManageError::Unavailable(format!("could not start git: {e}")))?;
        let revision = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        if !head.status.success() || revision.is_empty() {
            return Err(ManageError::Unavailable(
                "could not read the cloned commit".to_owned(),
            ));
        }
        let ext = crate::install(&checkout.join(&subdir), root).map_err(install_refusal)?;
        Ok((
            ext,
            Source::Git {
                url: url.clone(),
                subdir: subdir.to_string_lossy().replace('\\', "/"),
                revision,
            },
        ))
    })();
    let _ = std::fs::remove_dir_all(&checkout);
    result
}

/// An HTTPS Git source (`https://…` or `git+https://…`, `#subdir` optional),
/// or `None` for a local folder. A URL with credentials, and a sub-folder
/// that would leave the repository, are refused.
pub(crate) fn git_source(source: &Path) -> Result<Option<(String, PathBuf)>, ManageError> {
    let text = source.to_string_lossy();
    let raw = text.strip_prefix("git+").unwrap_or(&text);
    if !raw.starts_with("https://") {
        return Ok(None);
    }
    let (url, subdir) = raw.split_once('#').unwrap_or((raw, ""));
    if url.len() <= "https://".len() {
        return Err(ManageError::Invalid(
            "Git URL needs a host and repository path".to_owned(),
        ));
    }
    let authority = url
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("");
    if authority.contains('@') {
        return Err(ManageError::Invalid(
            "Git URL must not contain credentials; use a Git credential helper".to_owned(),
        ));
    }
    let subdir = PathBuf::from(subdir);
    if !subdir.as_os_str().is_empty()
        && !subdir
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(ManageError::Invalid(
            "Git extension subdirectory must stay within the repository".to_owned(),
        ));
    }
    Ok(Some((url.to_owned(), subdir)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sources_accept_a_relative_extension_subdirectory() {
        assert_eq!(git_source(Path::new("./my-tool")).unwrap(), None);
        assert_eq!(
            git_source(Path::new(
                "git+https://example.org/tools.git#extensions/my-tool"
            ))
            .unwrap(),
            Some((
                "https://example.org/tools.git".to_owned(),
                PathBuf::from("extensions/my-tool")
            ))
        );
        assert!(git_source(Path::new("https://example.org/tools.git#../other")).is_err());
        assert!(git_source(Path::new("https://token@example.org/tools.git")).is_err());
    }

    #[test]
    fn an_install_refusal_names_its_kind() {
        assert!(matches!(
            install_refusal("extension 'a' is already installed in /x".to_owned()),
            ManageError::Conflict(_)
        ));
        assert!(matches!(
            install_refusal("invalid extension.json".to_owned()),
            ManageError::Invalid(_)
        ));
    }
}
