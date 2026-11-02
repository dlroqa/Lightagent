//! The tools workspace: the folder the file and terminal tools are confined
//! to. `lightagent setup tools`, the HTTP API and the desktop's Settings
//! read and set it through these functions, inside one config update, so
//! every surface means the same folder.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ProfileStore;
use crate::config::Config;
use crate::paths::LightagentPaths;

/// When a workspace change takes effect.
pub const EFFECT: &str = "Applies to the next run. A run in progress keeps its folder; an editor's ACP session keeps using the editor's folder.";

/// The workspace as the next run of the active profile uses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceView {
    /// The configured folder, when one is set.
    pub configured: Option<String>,
    /// The folder the next run of the active profile uses: the configured
    /// one, or that profile's own `workspace/`.
    pub effective: Option<String>,
    /// `config` or `profile`.
    pub source: &'static str,
    /// The active profile.
    pub profile: Option<String>,
    /// Whether the effective folder exists now; a run creates a missing one.
    pub exists: bool,
    pub tools_enabled: bool,
    pub terminal_enabled: bool,
}

/// The workspace for the next run of the active profile.
pub fn view(config: &Config, paths: &LightagentPaths) -> WorkspaceView {
    let profiles = ProfileStore::new(paths.root());
    let profile = profiles.active().ok().flatten();
    let effective: Option<PathBuf> = match &config.tools.workspace {
        Some(folder) => Some(PathBuf::from(folder)),
        None => profile
            .as_ref()
            .map(|id| profiles.handle(id).workspace_dir()),
    };
    WorkspaceView {
        configured: config.tools.workspace.clone(),
        exists: effective.as_ref().is_some_and(|folder| folder.is_dir()),
        effective: effective.map(|folder| folder.display().to_string()),
        source: if config.tools.workspace.is_some() {
            "config"
        } else {
            "profile"
        },
        profile: profile.map(|id| id.as_str().to_owned()),
        tools_enabled: config.tools.enabled,
        terminal_enabled: config.tools.enabled && config.tools.allow_terminal,
    }
}

/// A folder typed by a person, trimmed: absolute, so the terminal and the
/// desktop runtime mean the same folder, and either an existing folder or a
/// new one whose parent folder exists (the next run creates it).
pub fn validate(folder: &str) -> Result<String, String> {
    let folder = folder.trim();
    if folder.chars().any(char::is_control) {
        return Err("the workspace folder contains a control character".to_owned());
    }
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err(
            "enter an absolute folder path: a relative one would mean a different folder for each process"
                .to_owned(),
        );
    }
    match std::fs::metadata(path) {
        Ok(found) if found.is_dir() => Ok(folder.to_owned()),
        Ok(_) => Err("that path is a file, not a folder".to_owned()),
        Err(_) => match path.parent().map(std::fs::metadata) {
            Some(Ok(parent)) if parent.is_dir() => Ok(folder.to_owned()),
            _ => Err(
                "the folder does not exist, and neither does the folder it would be created in"
                    .to_owned(),
            ),
        },
    }
}

/// Set the workspace folder; `None` or blank clears it, so runs use the
/// active profile's own `workspace/`. An invalid folder changes nothing.
pub fn set(config: &mut Config, folder: Option<&str>) -> Result<(), String> {
    config.tools.workspace = match folder.map(str::trim).filter(|folder| !folder.is_empty()) {
        Some(folder) => Some(validate(folder)?),
        None => None,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_absolute_and_a_folder_or_inside_one() {
        let root = std::env::temp_dir().join(format!(
            "lightagent-workspaces-{}",
            crate::ids::RunId::new().as_str()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let existing = root.display().to_string();
        let new = root.join("new").display().to_string();
        assert_eq!(validate(&existing), Ok(existing.clone()));
        assert_eq!(validate(&format!("  {new}  ")), Ok(new.clone()));
        assert!(
            validate("relative/folder")
                .unwrap_err()
                .contains("absolute")
        );
        assert!(
            validate(&file.display().to_string())
                .unwrap_err()
                .contains("file")
        );
        assert!(validate(&root.join("a").join("b").display().to_string()).is_err());
        assert!(validate(&format!("{existing}\u{7}")).is_err());

        let mut config = Config::default();
        set(&mut config, Some(&new)).unwrap();
        assert_eq!(config.tools.workspace.as_deref(), Some(new.as_str()));
        assert!(set(&mut config, Some("relative")).is_err());
        assert_eq!(config.tools.workspace.as_deref(), Some(new.as_str()));
        set(&mut config, Some("  ")).unwrap();
        assert_eq!(config.tools.workspace, None);
        set(&mut config, Some(&new)).unwrap();
        set(&mut config, None).unwrap();
        assert_eq!(config.tools.workspace, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_view_names_the_configured_folder() {
        let paths = LightagentPaths::rooted_at(std::env::temp_dir().join(format!(
            "lightagent-workspaces-view-{}",
            crate::ids::RunId::new().as_str()
        )));
        let mut config = Config::default();
        let view_now = view(&config, &paths);
        assert_eq!((view_now.source, view_now.configured), ("profile", None));
        config.tools.workspace = Some("/projects/lab".to_owned());
        let view_now = view(&config, &paths);
        assert_eq!(view_now.source, "config");
        assert_eq!(view_now.effective.as_deref(), Some("/projects/lab"));
    }
}
