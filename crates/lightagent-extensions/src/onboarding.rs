//! Profile onboarding guidance: a Markdown file installed as the profile's
//! `user-onboarding` extension, whose text joins the agent's instructions.
//! One implementation behind the terminal chat's `/onboard <file.md>` and
//! `/onboard remove`, the HTTP API and the desktop.

use std::path::Path;

use lightagent_core::Config;
use lightagent_core::paths::{create_private_dir, write_private};

/// The extension the guidance is installed as.
pub const NAME: &str = "user-onboarding";
/// The largest Markdown file read: 64 KiB.
pub const MAX_BYTES: u64 = 65_536;
const MANIFEST: &str = r#"{"name":"user-onboarding","description":"Profile onboarding guidance","instructions_file":"ONBOARDING.md"}"#;

/// Read a Markdown file: a regular file of at most [`MAX_BYTES`], in UTF-8.
pub fn read(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("could not read Markdown file {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!(
            "{} is too large (maximum {} KiB)",
            path.display(),
            MAX_BYTES / 1024
        ));
    }
    std::fs::read_to_string(path)
        .map_err(|e| format!("could not read Markdown file {}: {e}", path.display()))
}

/// Install or replace the profile's guidance. The old bundle stays
/// available until the new one is fully written and can be renamed in.
pub fn install(source: &Path, profile_dir: &Path) -> Result<(), String> {
    let body = read(source)?;
    let root = profile_dir.join("extensions");
    create_private_dir(&root).map_err(|e| e.to_string())?;
    let id = lightagent_core::RunId::new();
    let staging = profile_dir.join(format!(".onboarding-stage-{}", id.as_str()));
    let backup = profile_dir.join(format!(".onboarding-backup-{}", id.as_str()));
    let destination = root.join(NAME);
    create_private_dir(&staging).map_err(|e| e.to_string())?;
    let write_result = (|| {
        write_private(&staging.join("extension.json"), MANIFEST.as_bytes())
            .map_err(|e| e.to_string())?;
        write_private(&staging.join("ONBOARDING.md"), body.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let replacing = match std::fs::symlink_metadata(&destination) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if std::fs::read_to_string(destination.join("extension.json"))
                .ok()
                .as_deref()
                != Some(MANIFEST)
            {
                let _ = std::fs::remove_dir_all(&staging);
                return Err("an unrelated extension already uses 'user-onboarding'".to_owned());
            }
            true
        }
        Ok(_) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err("existing onboarding bundle is not a regular directory".to_owned());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("could not inspect onboarding bundle: {error}"));
        }
    };
    if replacing && let Err(error) = std::fs::rename(&destination, &backup) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("could not prepare onboarding update: {error}"));
    }
    if let Err(error) = std::fs::rename(&staging, &destination) {
        if replacing {
            let _ = std::fs::rename(&backup, &destination);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("could not install onboarding: {error}"));
    }
    if replacing {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Remove the profile's guidance.
pub fn remove(profile_dir: &Path) -> Result<(), String> {
    let root = profile_dir.join("extensions");
    let manifest = std::fs::read_to_string(root.join(NAME).join("extension.json"))
        .map_err(|e| format!("profile onboarding is not installed: {e}"))?;
    if manifest != MANIFEST {
        return Err("an unrelated extension uses 'user-onboarding'".to_owned());
    }
    crate::uninstall(NAME, &root)
}

/// The guidance installed for the profile, if any. An unrelated extension
/// under the name is an error, never mistaken for guidance.
pub fn current(profile_dir: &Path) -> Result<Option<String>, String> {
    let bundle = profile_dir.join("extensions").join(NAME);
    match std::fs::read_to_string(bundle.join("extension.json")) {
        Ok(manifest) if manifest == MANIFEST => {
            std::fs::read_to_string(bundle.join("ONBOARDING.md"))
                .map(Some)
                .map_err(|e| format!("could not read the installed onboarding: {e}"))
        }
        Ok(_) => Err("an unrelated extension uses 'user-onboarding'".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read the onboarding bundle: {error}")),
    }
}

/// What installing guidance changes in the configuration so that it
/// applies: extensions on, and the guidance not disabled.
pub fn enable(config: &mut Config) {
    config.extensions.enabled = true;
    config.extensions.disabled.retain(|name| name != NAME);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_is_read_back_and_an_unrelated_bundle_is_refused() {
        let dir = std::env::temp_dir().join(format!(
            "lightagent-onboarding-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let profile = dir.join("profile");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(current(&profile).unwrap(), None);
        let source = dir.join("guide.md");
        std::fs::write(&source, "Answer briefly.").unwrap();
        install(&source, &profile).unwrap();
        assert_eq!(
            current(&profile).unwrap().as_deref(),
            Some("Answer briefly.")
        );
        remove(&profile).unwrap();
        assert_eq!(current(&profile).unwrap(), None);

        let other = profile.join("extensions").join(NAME);
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("extension.json"),
            r#"{"name":"user-onboarding"}"#,
        )
        .unwrap();
        assert!(current(&profile).is_err());
        assert!(install(&source, &profile).is_err());

        let mut config = Config::default();
        config.extensions.enabled = false;
        config.extensions.disabled = vec![NAME.to_owned(), "other".to_owned()];
        enable(&mut config);
        assert!(config.extensions.enabled);
        assert_eq!(config.extensions.disabled, ["other"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
