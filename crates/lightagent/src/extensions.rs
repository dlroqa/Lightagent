//! `lightagent extensions` — install, remove, list and toggle capability bundles.
//!
//! An extension is discovered under the global `<home>/extensions/` directory and
//! the active profile's own; installing one is dropping its directory in place.
//! Installation copies a local bundle into the selected store; uninstall removes
//! exactly that installed copy. The operations are
//! [`lightagent_extensions::manage`], shared with the HTTP API and the desktop
//! app; this module prints their results.

use std::path::Path;

use lightagent_core::{Config, ConfigStore, LightagentPaths};
use lightagent_extensions::manage::{self, InstallOptions, Scope};

fn paths() -> Result<LightagentPaths, String> {
    LightagentPaths::resolve().map_err(|error| error.to_string())
}

fn config(paths: &LightagentPaths) -> Result<Config, String> {
    ConfigStore::at(paths)
        .load()
        .map_err(|error| error.to_string())
}

fn scope(profile: bool) -> Scope {
    if profile {
        Scope::Profile
    } else {
        Scope::Global
    }
}

/// Install a local bundle and activate the extension system it needs. The
/// CLI switches MCP on for an extension that brings MCP servers, and says so.
pub fn install(source: &Path, profile: bool, json: bool) -> Result<(), String> {
    let options = InstallOptions {
        scope: scope(profile),
        activate_mcp: true,
    };
    let installed =
        manage::install(&paths()?, source, options).map_err(|error| error.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "name": installed.name, "dir": installed.dir,
                "mcp_servers": installed.mcp_servers.len(), "active": true
            })
        );
    } else {
        println!(
            "Installed '{}' in {}.",
            installed.name,
            installed.dir.display()
        );
        if !installed.mcp_servers.is_empty() {
            println!(
                "MCP enabled; run `lightagent tools list` to verify the server and its tools."
            );
        }
    }
    Ok(())
}

/// Remove an installed bundle from the selected store.
pub fn uninstall(name: &str, profile: bool, json: bool) -> Result<(), String> {
    let removed =
        manage::uninstall(&paths()?, name, scope(profile)).map_err(|error| error.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "name": removed.name, "removed_from": removed.removed_from })
        );
    } else {
        println!(
            "Uninstalled '{}' from {}.",
            removed.name,
            removed.removed_from.display()
        );
    }
    Ok(())
}

/// `extensions list` — every installed extension, marking the active ones.
pub fn list(json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = config(&paths)?;
    let listing = manage::list(&paths, &config).map_err(|error| error.to_string())?;
    if json {
        let value = serde_json::json!({
            "enabled": listing.enabled,
            "extensions": listing.extensions.iter().map(|ext| serde_json::json!({
                "name": ext.name,
                "version": ext.version,
                "description": ext.description,
                "active": ext.active,
                "scope": ext.scope,
                "skills_dir": ext.skills_dir,
                "mcp_servers": ext.mcp_servers,
            })).collect::<Vec<_>>(),
        });
        println!("{value:#}");
        return Ok(());
    }
    if listing.extensions.is_empty() {
        println!(
            "No extensions installed. Use `lightagent extensions install <directory>` or add one under {}.",
            paths.extensions_dir().display()
        );
        return Ok(());
    }
    if !listing.enabled {
        println!("(extensions are disabled globally — `lightagent config set` extensions.enabled)");
    }
    for ext in &listing.extensions {
        let mark = if ext.active { "●" } else { "○" };
        if ext.description.is_empty() {
            println!("{mark} {}", ext.name);
        } else {
            println!("{mark} {} — {}", ext.name, ext.description);
        }
    }
    Ok(())
}

/// `extensions show <name>` — one extension's manifest and contributions.
pub fn show(name: &str, json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = config(&paths)?;
    let ext = manage::show(&paths, &config, name).map_err(|error| error.to_string())?;
    if json {
        let value = serde_json::json!({
            "name": ext.name,
            "version": ext.version,
            "description": ext.description,
            "active": ext.active,
            "scope": ext.scope,
            "dir": ext.dir,
            "instructions": ext.instructions,
            "mcp_servers": ext.mcp_servers,
            "mcp": ext.mcp,
            "skills": ext.skills,
            "source": ext.source,
        });
        println!("{value:#}");
        return Ok(());
    }
    println!("{}", ext.name);
    if !ext.version.is_empty() {
        println!("  version: {}", ext.version);
    }
    if !ext.description.is_empty() {
        println!("  {}", ext.description);
    }
    println!("  active:  {}", if ext.active { "yes" } else { "no" });
    println!("  dir:     {}", ext.dir.display());
    if !ext.mcp_servers.is_empty() {
        println!("  mcp:     {}", ext.mcp_servers.join(", "));
    }
    if !ext.skills.is_empty() {
        println!("  skills:  {}", ext.skills.join(", "));
    }
    if !ext.instructions.trim().is_empty() {
        println!("  instructions:\n{}", indent(ext.instructions.trim()));
    }
    Ok(())
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `extensions enable <name>` — clear an extension from the disabled list.
pub fn enable(name: &str, json: bool) -> Result<(), String> {
    toggle(name, false, json)
}

/// `extensions disable <name>` — keep an extension installed but inactive.
pub fn disable(name: &str, json: bool) -> Result<(), String> {
    toggle(name, true, json)
}

fn toggle(name: &str, disable: bool, json: bool) -> Result<(), String> {
    let toggled =
        manage::set_enabled(&paths()?, name, !disable).map_err(|error| error.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "name": name, "active": toggled.active })
        );
    } else if disable {
        println!("Disabled '{name}'.");
    } else if toggled.extensions_enabled {
        println!("Enabled '{name}'.");
    } else {
        println!(
            "Enabled '{name}', but extensions are off globally (extensions.enabled is false)."
        );
    }
    Ok(())
}
