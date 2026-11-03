//! Interactive configuration menus for the terminal CLI.

use std::io::{BufRead, IsTerminal as _, Write};

use clap::ValueEnum;
use dialoguer::{Confirm, Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use lightagent_core::SecretRef;
use lightagent_core::saved_providers::{self, Outcome};
use lightagent_core::{
    AgentProfile, ApprovalPolicy, Config, ConfigError, ConfigRevision, ConfigStore,
    DUCKDUCKGO_SEARCH_ENDPOINT, LightagentPaths, ProfileId, ProfileStore, WebMode,
};
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};

/// A setup section that can also be opened directly from the command line.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Section {
    #[value(alias = "profile")]
    Profiles,
    #[value(alias = "gateway", alias = "model")]
    Provider,
    Tools,
    Web,
    #[value(alias = "terminal", alias = "display")]
    Tui,
    Approvals,
}

pub(crate) async fn run(section: Option<Section>, json: bool) -> Result<(), String> {
    if json {
        return Err("interactive setup does not support --json".to_owned());
    }
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    paths.scaffold().map_err(|error| error.to_string())?;
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        return run_tui(section, &paths).await;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_with_io(section, &paths, stdin.lock(), stdout.lock()).await
}

/// Save what setup edited, only if nobody else wrote the config since setup
/// read it: saving its older copy would silently undo that change.
fn save_setup(
    store: &ConfigStore,
    read_at: &ConfigRevision,
    config: &Config,
) -> Result<ConfigRevision, String> {
    store.replace(read_at, config).map_err(|error| match error {
        ConfigError::Conflict { .. } => "the configuration changed while setup was open \
             (another lightagent command, the desktop app or the Web UI wrote it); \
             nothing was saved. Run setup again to start from the current settings."
            .to_owned(),
        other => other.to_string(),
    })
}

/// Full-screen-style selectors for a real terminal. Dialoguer redraws the
/// active rows in place and supplies arrow, Space, Enter and Escape handling.
async fn run_tui(section: Option<Section>, paths: &LightagentPaths) -> Result<(), String> {
    let store = ConfigStore::at(paths);
    let (mut config, mut revision) = store
        .load_with_revision()
        .map_err(|error| error.to_string())?;
    let profiles = ProfileStore::new(paths.root());
    crate::ensure_default_profile(&profiles, &config)?;
    let theme = ColorfulTheme::default();
    eprintln!("\nLightagent setup");

    loop {
        let selected = match section {
            Some(section) => Some(section),
            None => {
                let items = [
                    "Profiles",
                    "Gateway and model",
                    "Tools for CLI",
                    "Web access and search",
                    "Terminal UI",
                    "Approval prompts",
                    "Finish",
                ];
                let choice = Select::with_theme(&theme)
                    .with_prompt("Configure")
                    .items(items)
                    .default(0)
                    .interact_opt()
                    .map_err(dialog_error)?;
                match choice {
                    Some(0) => Some(Section::Profiles),
                    Some(1) => Some(Section::Provider),
                    Some(2) => Some(Section::Tools),
                    Some(3) => Some(Section::Web),
                    Some(4) => Some(Section::Tui),
                    Some(5) => Some(Section::Approvals),
                    Some(6) | None => None,
                    Some(_) => return Err("invalid setup selection".to_owned()),
                }
            }
        };
        let Some(selected) = selected else {
            eprintln!("Setup finished.");
            return Ok(());
        };

        let changed = configure_tui(selected, &mut config, paths, &theme).await?;
        if let Some(outcome) = changed {
            revision = save_setup(&store, &revision, &config)?;
            // What waits for the save: a released key file, the active
            // profile's endpoint and model.
            saved_providers::finish(paths, &outcome)?;
            eprintln!("Saved.\n");
        } else {
            eprintln!("Cancelled; no changes saved.\n");
        }
        if section.is_some() {
            return Ok(());
        }
    }
}

async fn configure_tui(
    section: Section,
    config: &mut Config,
    paths: &LightagentPaths,
    theme: &ColorfulTheme,
) -> Result<Option<Outcome>, String> {
    // A section other than the provider has nothing to finish after the save.
    let changed =
        |result: Result<bool, String>| result.map(|changed| changed.then(Outcome::default));
    match section {
        Section::Profiles => changed(configure_profiles_tui(config, paths, theme)),
        Section::Provider => configure_gateway_tui(config, paths, theme).await,
        Section::Tools => changed(configure_tools_tui(config, theme)),
        Section::Web => changed(configure_web_tui(config, theme)),
        Section::Tui => changed(configure_terminal_ui_tui(config, theme)),
        Section::Approvals => changed(configure_approvals_tui(config, paths, theme)),
    }
}

fn configure_profiles_tui(
    config: &Config,
    paths: &LightagentPaths,
    theme: &ColorfulTheme,
) -> Result<bool, String> {
    let store = ProfileStore::new(paths.root());
    let default = crate::ensure_default_profile(&store, config)?;
    let active = store.active().map_err(|error| error.to_string())?;
    eprintln!("\nProfiles");
    eprintln!(
        "Active: {}  (`default` is the protected main account)\n",
        active.as_ref().map(ProfileId::as_str).unwrap_or("default")
    );

    let Some(action) = Select::with_theme(theme)
        .with_prompt("Manage profiles")
        .items([
            "Create a profile",
            "Switch active profile",
            "Delete a profile",
            "Back",
        ])
        .default(0)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(false);
    };

    match action {
        0 => {
            let raw_id = Input::<String>::with_theme(theme)
                .with_prompt("Profile id (lowercase letters, digits, _ or -)")
                .validate_with(|value: &String| {
                    ProfileId::new(value.trim())
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .interact_text()
                .map_err(dialog_error)?;
            let id = ProfileId::new(raw_id.trim()).map_err(|error| error.to_string())?;
            let name = Input::<String>::with_theme(theme)
                .with_prompt("Display name")
                .with_initial_text(id.as_str())
                .interact_text()
                .map_err(dialog_error)?;
            let persona = Input::<String>::with_theme(theme)
                .with_prompt("Persona")
                .with_initial_text("You are a helpful local agent.")
                .interact_text()
                .map_err(dialog_error)?;
            let model = Input::<String>::with_theme(theme)
                .with_prompt("Model (`default` follows the configured model)")
                .with_initial_text("default")
                .interact_text()
                .map_err(dialog_error)?;
            create_profile(&store, id.clone(), name, persona, model, config)?;
            let activate = Confirm::with_theme(theme)
                .with_prompt(format!("Use '{}' now?", id.as_str()))
                .default(true)
                .interact_opt()
                .map_err(dialog_error)?
                .unwrap_or(false);
            if activate {
                store.set_active(&id).map_err(|error| error.to_string())?;
            }
            eprintln!("Created profile '{}'.", id.as_str());
            Ok(true)
        }
        1 => {
            let profiles = store.list().map_err(|error| error.to_string())?;
            let selected = Select::with_theme(theme)
                .with_prompt("Active profile")
                .items(profiles.iter().map(ProfileId::as_str).collect::<Vec<_>>())
                .default(
                    active
                        .as_ref()
                        .and_then(|id| profiles.iter().position(|profile| profile == id))
                        .unwrap_or(0),
                )
                .interact_opt()
                .map_err(dialog_error)?;
            let Some(selected) = selected else {
                return Ok(false);
            };
            store
                .set_active(&profiles[selected])
                .map_err(|error| error.to_string())?;
            eprintln!("Active profile is now '{}'.", profiles[selected].as_str());
            Ok(true)
        }
        2 => {
            let profiles = store
                .list()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|id| id != &default)
                .collect::<Vec<_>>();
            if profiles.is_empty() {
                eprintln!("There are no additional profiles to delete.");
                return Ok(true);
            }
            let Some(selected) = Select::with_theme(theme)
                .with_prompt("Delete profile")
                .items(profiles.iter().map(ProfileId::as_str).collect::<Vec<_>>())
                .default(0)
                .interact_opt()
                .map_err(dialog_error)?
            else {
                return Ok(false);
            };
            let id = &profiles[selected];
            let confirmed = Confirm::with_theme(theme)
                .with_prompt(format!(
                    "Delete '{}' and all of its local data?",
                    id.as_str()
                ))
                .default(false)
                .interact_opt()
                .map_err(dialog_error)?
                .unwrap_or(false);
            if !confirmed {
                return Ok(false);
            }
            delete_profile(&store, id, &default)?;
            eprintln!("Deleted profile '{}'.", id.as_str());
            Ok(true)
        }
        3 => Ok(false),
        _ => Err("invalid profile selection".to_owned()),
    }
}

async fn configure_gateway_tui(
    config: &mut Config,
    paths: &LightagentPaths,
    theme: &ColorfulTheme,
) -> Result<Option<Outcome>, String> {
    eprintln!("\nProvider & model for Lightagent CLI");
    eprintln!("↑↓ navigate  ENTER select  ESC cancel\n");

    // The shared list, as the desktop's Settings shows it: the local
    // endpoint first, then each saved provider.
    let listed = saved_providers::list(config)
        .into_iter()
        .filter(|provider| provider.kind != "custom")
        .collect::<Vec<_>>();
    let mut providers = listed
        .iter()
        .map(|provider| format!("{}  ({})", provider.name, provider.base_url))
        .collect::<Vec<_>>();
    let custom_index = providers.len();
    providers.push("Custom endpoint  (enter URL manually)".to_owned());
    let remove_index = if listed.len() > 1 {
        let index = providers.len();
        providers.push("Remove a saved custom provider".to_owned());
        Some(index)
    } else {
        None
    };
    let default = listed
        .iter()
        .position(|provider| provider.active)
        .unwrap_or(custom_index);
    let Some(selected) = Select::with_theme(theme)
        .with_prompt("Provider")
        .items(providers)
        .default(default)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(None);
    };

    if Some(selected) == remove_index {
        let saved = &listed[1..];
        let names = saved
            .iter()
            .map(|provider| format!("{}  ({})", provider.name, provider.base_url))
            .collect::<Vec<_>>();
        let Some(remove) = Select::with_theme(theme)
            .with_prompt("Remove saved provider")
            .items(names)
            .default(0)
            .interact_opt()
            .map_err(dialog_error)?
        else {
            return Ok(None);
        };
        // The shared removal: the stored key is deleted after the save, and
        // a removed active provider leaves the local endpoint active.
        let outcome = saved_providers::remove(config, paths, &saved[remove].id)
            .map_err(|error| error.to_string())?;
        eprintln!("Removed {}.", saved[remove].name);
        return Ok(Some(outcome));
    }

    let (provider_id, base_url) = if selected < custom_index {
        (
            listed[selected].id.clone(),
            listed[selected].base_url.clone(),
        )
    } else {
        let name = Input::<String>::with_theme(theme)
            .with_prompt("Provider name")
            .validate_with(|value: &String| -> Result<(), &str> {
                if value.trim().is_empty() {
                    Err("enter a name for this provider")
                } else {
                    Ok(())
                }
            })
            .interact_text()
            .map_err(dialog_error)?;
        let base_url = Input::<String>::with_theme(theme)
            .with_prompt("OpenAI-compatible base URL")
            .validate_with(|value: &String| -> Result<(), &str> {
                if value.starts_with("http://") || value.starts_with("https://") {
                    Ok(())
                } else {
                    Err("enter an http:// or https:// URL")
                }
            })
            .interact_text()
            .map_err(dialog_error)?;
        // The shared creation refuses a name or endpoint another saved
        // provider has: they would share a key file, or one endpoint's key.
        match saved_providers::create(config, &name, &base_url) {
            Ok(provider) => (provider.id, provider.base_url),
            Err(error) => {
                eprintln!("{error}");
                return Ok(None);
            }
        }
    };

    // The provider's own key only: the one bound to its endpoint.
    let mut api_key = lightagent_core::provider_keys::bound_key(config, &base_url).cloned();
    let mut stored_key = None;
    let mut models = provider_models(&base_url, api_key.as_ref()).await;
    if api_key.is_none()
        && models
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("401 Unauthorized"))
    {
        let save_key = Confirm::with_theme(theme)
            .with_prompt("This provider requires an API key. Paste and save it privately?")
            .default(true)
            .interact_opt()
            .map_err(dialog_error)?
            .unwrap_or(false);
        if save_key {
            let secret = store_provider_key_tui(paths, &provider_id, theme)?;
            api_key = Some(secret.clone());
            stored_key = Some(secret);
            models = provider_models(&base_url, api_key.as_ref()).await;
        }
    }
    let models = match models {
        Ok(models) => models,
        Err(error) => {
            eprintln!("Could not list models: {error}");
            Vec::new()
        }
    };
    let mut items = vec!["Automatic — follow the loaded model".to_owned()];
    items.extend(models.iter().cloned());
    let default = config
        .inference
        .model
        .as_ref()
        .and_then(|current| models.iter().position(|model| model == current))
        .map_or(0, |index| index + 1);
    let Some(selected) = Select::with_theme(theme)
        .with_prompt("Model")
        .items(items)
        .default(default)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(None);
    };
    let model = selected.checked_sub(1).map(|index| models[index].clone());
    // The shared selection: the endpoint takes only its own key, and the
    // active profile follows after the save.
    let (_, outcome) = saved_providers::select(config, &provider_id, model.as_deref())
        .map_err(|error| error.to_string())?;
    if let Some(secret) = stored_key {
        lightagent_core::provider_keys::use_key(config, Some(secret));
    }
    Ok(Some(outcome))
}

async fn provider_models(
    base_url: &str,
    api_key: Option<&SecretRef>,
) -> Result<Vec<String>, String> {
    let mut config = ProviderConfig::new(base_url, "default");
    // A configured key that cannot be used: nothing is sent without it.
    if let Some(key) = api_key
        .map_or(Ok(None), SecretRef::for_request)
        .map_err(|error| format!("{error}; nothing was sent"))?
    {
        config = config.with_api_key(key);
    }
    LightweightProvider::new(config)
        .map_err(|error| error.to_string())?
        .models()
        .await
        .map_err(|error| error.to_string())
}

fn store_provider_key_tui(
    paths: &LightagentPaths,
    provider_id: &str,
    theme: &ColorfulTheme,
) -> Result<SecretRef, String> {
    let key = Password::with_theme(theme)
        .with_prompt("Provider API key")
        .interact()
        .map_err(dialog_error)?;
    // The one shared store, as the HTTP API and the desktop write it.
    lightagent_core::provider_keys::store_key(paths, provider_id, &key)
}

fn configure_tools_tui(config: &mut Config, theme: &ColorfulTheme) -> Result<bool, String> {
    eprintln!("\nTools for Lightagent CLI");
    eprintln!("↑↓ navigate  SPACE toggle  ENTER confirm  ESC cancel\n");
    let items = [
        "🔎 Web Search & Page Fetch  (web.search, web.fetch)",
        "⚡ Realtime RAG             (rag.realtime)",
        "📁 File Operations          (fs.read, fs.list, fs.write)",
        "⌨  Terminal & Processes     (terminal.run)",
        "🧩 Installed Extensions     (skills and capability bundles)",
        "🔌 MCP Servers              (tools from configured servers)",
    ];
    let realtime_rag =
        config.rag.realtime_enabled && config.web.enabled && config.web.search.endpoint.is_some();
    let defaults = [
        config.web.enabled,
        realtime_rag,
        config.tools.enabled,
        config.tools.enabled && config.tools.allow_terminal,
        config.extensions.enabled,
        config.mcp.enabled,
    ];
    let Some(selected) = MultiSelect::with_theme(theme)
        .with_prompt("Enable tools")
        .items(items)
        .defaults(&defaults)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(false);
    };
    let enabled = |index| selected.contains(&index);
    let realtime_rag = enabled(1);
    let terminal = enabled(3);
    config.web.enabled = enabled(0) || realtime_rag;
    config.rag.realtime_enabled = realtime_rag;
    config.tools.enabled = enabled(2) || terminal;
    config.tools.allow_terminal = terminal;
    config.extensions.enabled = enabled(4);
    config.mcp.enabled = enabled(5);

    if config.web.enabled && config.web.search.endpoint.is_none() {
        use_duckduckgo(config);
        eprintln!("Web search will use DuckDuckGo (no account or API key required).");
    }
    if config.tools.enabled && config.tools.workspace.is_none() {
        // The shared rule, checked as it is typed: an absolute folder that
        // exists, or a new one inside an existing folder.
        let workspace = Input::<String>::with_theme(theme)
            .with_prompt("Workspace folder (blank uses profile workspace)")
            .allow_empty(true)
            .validate_with(|value: &String| -> Result<(), String> {
                if value.trim().is_empty() {
                    Ok(())
                } else {
                    lightagent_core::workspaces::validate(value).map(|_| ())
                }
            })
            .interact_text()
            .map_err(dialog_error)?;
        lightagent_core::workspaces::set(config, Some(&workspace))?;
    }
    if realtime_rag && !enabled(0) {
        eprintln!("Web Search & Page Fetch were also enabled because realtime RAG uses them.");
    }
    if terminal && !enabled(2) {
        eprintln!(
            "File Operations were also enabled because terminal access uses the same confined workspace."
        );
    }
    eprintln!("Always available: date/time, skills, delegation, and memory.");
    Ok(true)
}

fn configure_web_tui(config: &mut Config, theme: &ColorfulTheme) -> Result<bool, String> {
    let default = if !config.web.enabled {
        0
    } else if uses_duckduckgo(config) {
        2
    } else if config.web.search.endpoint.is_some() {
        3
    } else {
        1
    };
    let Some(selected) = Select::with_theme(theme)
        .with_prompt("Web tools")
        .items([
            "Off",
            "Fetch web pages only",
            "Agentic search — DuckDuckGo (no account)",
            "Agentic search — SearXNG or custom JSON endpoint",
        ])
        .default(default)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(false);
    };
    let mode = WebMode::ALL[selected];
    let endpoint = if mode == WebMode::Custom {
        let current = config
            .web
            .search
            .endpoint
            .as_deref()
            .filter(|_| !uses_duckduckgo(config))
            .unwrap_or("http://127.0.0.1:8080/search?format=json");
        Some(
            Input::<String>::with_theme(theme)
                .with_prompt("SearXNG search URL")
                .with_initial_text(current)
                .interact_text()
                .map_err(dialog_error)?,
        )
    } else {
        None
    };
    mode.apply(config, endpoint.as_deref());
    Ok(true)
}

fn configure_approvals_tui(
    config: &mut Config,
    paths: &LightagentPaths,
    theme: &ColorfulTheme,
) -> Result<bool, String> {
    let default = match config.security.approval_policy {
        ApprovalPolicy::Balanced => 0,
        ApprovalPolicy::Strict => 1,
        ApprovalPolicy::Permissive => 2,
    };
    let Some(selected) = Select::with_theme(theme)
        .with_prompt("Approval prompts")
        .items([
            "Balanced — prompt before changes or commands",
            "Strict — prompt for everything beyond basic reads",
            "Permissive — auto-approve lower-risk tools",
        ])
        .default(default)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(false);
    };
    let policy = match selected {
        0 => ApprovalPolicy::Balanced,
        1 => ApprovalPolicy::Strict,
        2 => ApprovalPolicy::Permissive,
        _ => return Err("invalid approval selection".to_owned()),
    };
    config.security.approval_policy = policy;
    update_active_profile(paths, |profile| profile.approval_policy = policy)?;
    Ok(true)
}

fn configure_terminal_ui_tui(config: &mut Config, theme: &ColorfulTheme) -> Result<bool, String> {
    eprintln!("\nTerminal UI");
    eprintln!("↑↓ navigate  ENTER select  ESC cancel\n");
    let default = usize::from(!config.tui.show_reasoning);
    let Some(selected) = Select::with_theme(theme)
        .with_prompt("Agent reasoning")
        .items([
            "Show reasoning — stream the reasoning panel",
            "Hide reasoning — show the animated Lightagent star",
        ])
        .default(default)
        .interact_opt()
        .map_err(dialog_error)?
    else {
        return Ok(false);
    };
    config.tui.show_reasoning = selected == 0;
    Ok(true)
}

fn dialog_error(error: dialoguer::Error) -> String {
    error.to_string()
}

async fn run_with_io<R: BufRead, W: Write>(
    section: Option<Section>,
    paths: &LightagentPaths,
    reader: R,
    writer: W,
) -> Result<(), String> {
    let store = ConfigStore::at(paths);
    let (mut config, revision) = store
        .load_with_revision()
        .map_err(|error| error.to_string())?;
    let profiles = ProfileStore::new(paths.root());
    crate::ensure_default_profile(&profiles, &config)?;
    let mut prompt = Prompt { reader, writer };

    writeln!(prompt.writer, "\nLightagent setup").map_err(io_error)?;
    writeln!(
        prompt.writer,
        "Choose settings by number; Enter keeps the shown default.\n"
    )
    .map_err(io_error)?;

    if let Some(section) = section {
        configure(section, &mut config, paths, &mut prompt).await?;
    } else {
        loop {
            summary(&config, &mut prompt.writer)?;
            let choice = prompt.choose(
                "What would you like to configure?",
                &[
                    "Profiles",
                    "Gateway and model",
                    "Local file and terminal tools",
                    "Web access and search",
                    "Terminal UI",
                    "Approval prompts",
                    "Save and exit",
                ],
                7,
            )?;
            if choice == 7 {
                break;
            }
            let section = match choice {
                1 => Section::Profiles,
                2 => Section::Provider,
                3 => Section::Tools,
                4 => Section::Web,
                5 => Section::Tui,
                6 => Section::Approvals,
                _ => return Err("invalid setup selection".to_owned()),
            };
            configure(section, &mut config, paths, &mut prompt).await?;
        }
    }

    save_setup(&store, &revision, &config)?;
    writeln!(
        prompt.writer,
        "\nSaved. Restart Lightagent to apply these settings."
    )
    .map_err(io_error)?;
    Ok(())
}

async fn configure<R: BufRead, W: Write>(
    section: Section,
    config: &mut Config,
    paths: &LightagentPaths,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    match section {
        Section::Profiles => configure_profiles(config, paths, prompt),
        Section::Provider => configure_gateway(config, paths, prompt).await,
        Section::Tools => configure_tools(config, prompt),
        Section::Web => configure_web(config, prompt),
        Section::Tui => configure_terminal_ui(config, prompt),
        Section::Approvals => configure_approvals(config, paths, prompt),
    }
}

fn configure_profiles<R: BufRead, W: Write>(
    config: &Config,
    paths: &LightagentPaths,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let store = ProfileStore::new(paths.root());
    let default = crate::ensure_default_profile(&store, config)?;
    let active = store.active().map_err(|error| error.to_string())?;
    writeln!(
        prompt.writer,
        "Active profile: {} (`default` is the protected main account)",
        active.as_ref().map(ProfileId::as_str).unwrap_or("default")
    )
    .map_err(io_error)?;
    let action = prompt.choose(
        "Manage profiles",
        &[
            "Create a profile",
            "Switch active profile",
            "Delete a profile",
            "Back",
        ],
        4,
    )?;

    match action {
        1 => {
            let raw_id = prompt.input("Profile id (lowercase letters, digits, _ or -)", "")?;
            let id = ProfileId::new(raw_id.trim()).map_err(|error| error.to_string())?;
            let name = prompt.input("Display name", id.as_str())?;
            let persona = prompt.input("Persona", "You are a helpful local agent.")?;
            let model =
                prompt.input("Model (`default` follows the configured model)", "default")?;
            create_profile(&store, id.clone(), name, persona, model, config)?;
            let activate =
                prompt.choose(&format!("Use '{}' now?", id.as_str()), &["Yes", "No"], 1)?;
            if activate == 1 {
                store.set_active(&id).map_err(|error| error.to_string())?;
            }
            writeln!(prompt.writer, "Created profile '{}'.", id.as_str()).map_err(io_error)
        }
        2 => {
            let profiles = store.list().map_err(|error| error.to_string())?;
            let labels = profiles.iter().map(ProfileId::as_str).collect::<Vec<_>>();
            let selected = prompt.choose(
                "Active profile",
                &labels,
                active
                    .as_ref()
                    .and_then(|id| profiles.iter().position(|profile| profile == id))
                    .map_or(1, |index| index + 1),
            )?;
            let id = &profiles[selected - 1];
            store.set_active(id).map_err(|error| error.to_string())?;
            writeln!(prompt.writer, "Active profile is now '{}'.", id.as_str()).map_err(io_error)
        }
        3 => {
            let profiles = store
                .list()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|id| id != &default)
                .collect::<Vec<_>>();
            if profiles.is_empty() {
                return writeln!(prompt.writer, "There are no additional profiles to delete.")
                    .map_err(io_error);
            }
            let labels = profiles.iter().map(ProfileId::as_str).collect::<Vec<_>>();
            let selected = prompt.choose("Delete profile", &labels, 1)?;
            let id = &profiles[selected - 1];
            let confirmed = prompt.choose(
                &format!("Delete '{}' and all of its local data?", id.as_str()),
                &["Delete", "Cancel"],
                2,
            )?;
            if confirmed == 1 {
                delete_profile(&store, id, &default)?;
                writeln!(prompt.writer, "Deleted profile '{}'.", id.as_str()).map_err(io_error)?;
            }
            Ok(())
        }
        4 => Ok(()),
        _ => Err("invalid profile selection".to_owned()),
    }
}

fn create_profile(
    store: &ProfileStore,
    id: ProfileId,
    name: String,
    persona: String,
    model: String,
    config: &Config,
) -> Result<(), String> {
    let mut profile = AgentProfile::new(id, name, persona, model);
    profile.approval_policy = config.security.approval_policy;
    store.create(&profile).map_err(|error| error.to_string())
}

fn delete_profile(store: &ProfileStore, id: &ProfileId, default: &ProfileId) -> Result<(), String> {
    if store.active().map_err(|error| error.to_string())?.as_ref() == Some(id) {
        store
            .set_active(default)
            .map_err(|error| error.to_string())?;
    }
    store.delete(id).map_err(|error| error.to_string())
}

async fn configure_gateway<R: BufRead, W: Write>(
    config: &mut Config,
    paths: &LightagentPaths,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let base_url = prompt.input("Gateway URL", &config.inference.base_url)?;
    // The endpoint's own key only: a key saved for another endpoint is never
    // carried to this one.
    lightagent_core::provider_keys::select_endpoint(config, &base_url);

    let mut provider_config = ProviderConfig::new(base_url, "default");
    // A configured key that cannot be used: nothing is sent without it.
    let key = config
        .inference
        .api_key
        .as_ref()
        .map_or(Ok(None), SecretRef::for_request);
    if let Ok(Some(key)) = &key {
        provider_config = provider_config.with_api_key(key.clone());
    }
    let models = match key.map(|_| LightweightProvider::new(provider_config)) {
        Err(problem) => {
            writeln!(
                prompt.writer,
                "Could not list models: {problem}; nothing was sent. Automatic selection remains available."
            )
            .map_err(io_error)?;
            Vec::new()
        }
        Ok(Ok(provider)) => match provider.models().await {
            Ok(models) => models,
            Err(error) => {
                writeln!(
                    prompt.writer,
                    "Could not list models yet ({error}). Automatic selection remains available."
                )
                .map_err(io_error)?;
                Vec::new()
            }
        },
        Ok(Err(error)) => {
            writeln!(prompt.writer, "Could not open the gateway: {error}").map_err(io_error)?;
            Vec::new()
        }
    };

    let mut labels = vec!["Automatic — follow the model loaded in Lightweight".to_owned()];
    labels.extend(models.iter().map(|model| format!("Use {model}")));
    let default = config
        .inference
        .model
        .as_ref()
        .and_then(|current| models.iter().position(|model| model == current))
        .map_or(1, |index| index + 2);
    let choices = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let selected = prompt.choose("Model", &choices, default)?;
    let model = if selected == 1 {
        None
    } else {
        Some(models[selected - 2].clone())
    };
    config.inference.model = model.clone();
    update_active_profile(paths, |profile| {
        profile.routing.model = model.clone().unwrap_or_else(|| "default".to_owned());
    })
}

fn configure_tools<R: BufRead, W: Write>(
    config: &mut Config,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let default = match (config.tools.enabled, config.tools.allow_terminal) {
        (false, _) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    let selected = prompt.choose(
        "Local tools",
        &[
            "Off",
            "Files — confined to a workspace",
            "Files and terminal — confined to a workspace",
        ],
        default,
    )?;
    config.tools.enabled = selected != 1;
    config.tools.allow_terminal = selected == 3;
    if config.tools.enabled {
        let current = config.tools.workspace.as_deref().unwrap_or("");
        let workspace = prompt.input(
            "Workspace folder (blank uses the active profile workspace)",
            current,
        )?;
        // A new folder takes the shared rule; an unchanged one stays as it is.
        if workspace != current {
            lightagent_core::workspaces::set(config, Some(&workspace))?;
        }
    }
    Ok(())
}

fn configure_web<R: BufRead, W: Write>(
    config: &mut Config,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let default = if !config.web.enabled {
        1
    } else if uses_duckduckgo(config) {
        3
    } else if config.web.search.endpoint.is_some() {
        4
    } else {
        2
    };
    let selected = prompt.choose(
        "Web tools",
        &[
            "Off",
            "Fetch web pages only",
            "Agentic search — DuckDuckGo (no account)",
            "Agentic search — SearXNG or custom JSON endpoint",
        ],
        default,
    )?;
    let mode = WebMode::ALL[selected - 1];
    let endpoint = if mode == WebMode::Custom {
        let current = config
            .web
            .search
            .endpoint
            .as_deref()
            .filter(|_| !uses_duckduckgo(config))
            .unwrap_or("http://127.0.0.1:8080/search?format=json");
        Some(prompt.input("SearXNG search URL", current)?)
    } else {
        None
    };
    mode.apply(config, endpoint.as_deref());
    Ok(())
}

fn configure_approvals<R: BufRead, W: Write>(
    config: &mut Config,
    paths: &LightagentPaths,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let default = match config.security.approval_policy {
        ApprovalPolicy::Balanced => 1,
        ApprovalPolicy::Strict => 2,
        ApprovalPolicy::Permissive => 3,
    };
    let selected = prompt.choose(
        "Approval prompts",
        &[
            "Balanced — prompt before changes or commands",
            "Strict — prompt for everything beyond basic reads",
            "Permissive — auto-approve lower-risk tools",
        ],
        default,
    )?;
    let policy = match selected {
        1 => ApprovalPolicy::Balanced,
        2 => ApprovalPolicy::Strict,
        3 => ApprovalPolicy::Permissive,
        _ => return Err("invalid approval selection".to_owned()),
    };
    config.security.approval_policy = policy;
    update_active_profile(paths, |profile| profile.approval_policy = policy)
}

fn configure_terminal_ui<R: BufRead, W: Write>(
    config: &mut Config,
    prompt: &mut Prompt<R, W>,
) -> Result<(), String> {
    let default = if config.tui.show_reasoning { 1 } else { 2 };
    let selected = prompt.choose(
        "Agent reasoning",
        &[
            "Show reasoning — stream the reasoning panel",
            "Hide reasoning — show the animated Lightagent star",
        ],
        default,
    )?;
    config.tui.show_reasoning = selected == 1;
    Ok(())
}

fn update_active_profile(
    paths: &LightagentPaths,
    update: impl FnOnce(&mut lightagent_core::AgentProfile),
) -> Result<(), String> {
    let profiles = ProfileStore::new(paths.root());
    let Some(id) = profiles.active().map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let mut profile = profiles.load(&id).map_err(|error| error.to_string())?;
    update(&mut profile);
    profiles.save(&profile).map_err(|error| error.to_string())
}

fn summary(config: &Config, writer: &mut impl Write) -> Result<(), String> {
    let model = config.inference.model.as_deref().unwrap_or("automatic");
    let tools = match (config.tools.enabled, config.tools.allow_terminal) {
        (false, _) => "off",
        (true, false) => "files",
        (true, true) => "files + terminal",
    };
    let web = if !config.web.enabled {
        "off".to_owned()
    } else if uses_duckduckgo(config) {
        "agentic search (DuckDuckGo) + fetch".to_owned()
    } else if let Some(endpoint) = &config.web.search.endpoint {
        format!("agentic search ({endpoint}) + fetch")
    } else {
        "fetch only".to_owned()
    };
    let realtime_rag = if config.rag.realtime_enabled
        && config.web.enabled
        && config.web.search.endpoint.is_some()
    {
        "on"
    } else {
        "off"
    };
    writeln!(writer, "Current settings:").map_err(io_error)?;
    writeln!(writer, "  Gateway: {}", config.inference.base_url).map_err(io_error)?;
    writeln!(writer, "  Model:   {model}").map_err(io_error)?;
    writeln!(writer, "  Tools:   {tools}").map_err(io_error)?;
    writeln!(writer, "  Web:     {web}").map_err(io_error)?;
    writeln!(writer, "  RAG:     realtime {realtime_rag}").map_err(io_error)?;
    writeln!(
        writer,
        "  Reasoning: {}\n",
        if config.tui.show_reasoning {
            "show"
        } else {
            "hide (animated star)"
        }
    )
    .map_err(io_error)
}

fn uses_duckduckgo(config: &Config) -> bool {
    config.web.search.endpoint.as_deref() == Some(DUCKDUCKGO_SEARCH_ENDPOINT)
}

fn use_duckduckgo(config: &mut Config) {
    config.web.search.endpoint = Some(DUCKDUCKGO_SEARCH_ENDPOINT.to_owned());
    config.web.search.query_param = "q".to_owned();
    config.web.search.api_key = None;
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

struct Prompt<R, W> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> Prompt<R, W> {
    fn input(&mut self, label: &str, current: &str) -> Result<String, String> {
        if current.is_empty() {
            write!(self.writer, "{label}: ").map_err(io_error)?;
        } else {
            write!(self.writer, "{label} [{current}]: ").map_err(io_error)?;
        }
        self.writer.flush().map_err(io_error)?;
        let value = self.line()?;
        Ok(if value.is_empty() {
            current.to_owned()
        } else {
            value
        })
    }

    fn choose(&mut self, label: &str, options: &[&str], default: usize) -> Result<usize, String> {
        loop {
            writeln!(self.writer, "{label}:").map_err(io_error)?;
            for (index, option) in options.iter().enumerate() {
                let marker = if index + 1 == default {
                    " (current)"
                } else {
                    ""
                };
                writeln!(self.writer, "  {}. {option}{marker}", index + 1).map_err(io_error)?;
            }
            write!(self.writer, "Select [{default}]: ").map_err(io_error)?;
            self.writer.flush().map_err(io_error)?;
            let value = self.line()?;
            if value.is_empty() {
                return Ok(default);
            }
            if let Ok(choice) = value.parse::<usize>()
                && (1..=options.len()).contains(&choice)
            {
                return Ok(choice);
            }
            writeln!(
                self.writer,
                "Please enter a number from 1 to {}.\n",
                options.len()
            )
            .map_err(io_error)?;
        }
    }

    fn line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(io_error)?;
        Ok(line.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::RunId;
    use std::io::Cursor;

    fn scratch_paths() -> LightagentPaths {
        LightagentPaths::rooted_at(
            std::env::temp_dir().join(format!("lightagent-setup-{}", RunId::new().as_str())),
        )
    }

    #[test]
    fn profiles_can_be_created_activated_and_deleted_from_setup() {
        let paths = scratch_paths();
        paths.scaffold().unwrap();
        let config = Config::default();
        let mut output = Vec::new();
        let mut create = Prompt {
            reader: Cursor::new("1\naria\n\n\n\n1\n"),
            writer: &mut output,
        };
        configure_profiles(&config, &paths, &mut create).unwrap();

        let store = ProfileStore::new(paths.root());
        let aria = ProfileId::new("aria").unwrap();
        let default = ProfileId::new("default").unwrap();
        assert!(store.load(&default).is_ok());
        assert!(store.load(&aria).is_ok());
        assert_eq!(store.active().unwrap(), Some(aria.clone()));

        let mut delete = Prompt {
            reader: Cursor::new("3\n\n1\n"),
            writer: Vec::new(),
        };
        configure_profiles(&config, &paths, &mut delete).unwrap();
        assert!(store.load(&aria).is_err());
        assert!(store.load(&default).is_ok());
        assert_eq!(store.active().unwrap(), Some(default));
        std::fs::remove_dir_all(paths.root()).ok();
    }

    #[test]
    fn tools_are_selected_without_config_keys() {
        let project = std::env::temp_dir()
            .join("lightagent-setup-project")
            .display()
            .to_string();
        let mut config = Config::default();
        let mut output = Vec::new();
        let mut prompt = Prompt {
            reader: Cursor::new(format!("3\n{project}\n")),
            writer: &mut output,
        };
        configure_tools(&mut config, &mut prompt).unwrap();
        assert!(config.tools.enabled);
        assert!(config.tools.allow_terminal);
        assert_eq!(config.tools.workspace.as_deref(), Some(project.as_str()));
    }

    #[test]
    fn a_relative_workspace_folder_is_refused() {
        let mut config = Config::default();
        let mut output = Vec::new();
        let mut prompt = Prompt {
            reader: Cursor::new("3\nrelative/project\n"),
            writer: &mut output,
        };
        let error = configure_tools(&mut config, &mut prompt).unwrap_err();
        assert!(error.contains("absolute"), "{error}");
        assert_eq!(config.tools.workspace, None);
    }

    #[test]
    fn web_search_prompts_for_a_human_readable_url() {
        let mut config = Config::default();
        let mut output = Vec::new();
        let mut prompt = Prompt {
            reader: Cursor::new("4\nhttps://search.example/search?format=json\n"),
            writer: &mut output,
        };
        configure_web(&mut config, &mut prompt).unwrap();
        assert!(config.web.enabled);
        assert!(config.rag.realtime_enabled);
        assert_eq!(
            config.web.search.endpoint.as_deref(),
            Some("https://search.example/search?format=json")
        );
    }

    #[test]
    fn web_search_can_use_duckduckgo_without_an_account() {
        let mut config = Config::default();
        let mut output = Vec::new();
        let mut prompt = Prompt {
            reader: Cursor::new("3\n"),
            writer: &mut output,
        };
        configure_web(&mut config, &mut prompt).unwrap();
        assert!(config.web.enabled);
        assert!(config.rag.realtime_enabled);
        assert!(uses_duckduckgo(&config));
        assert!(config.web.search.api_key.is_none());
    }

    #[test]
    fn terminal_ui_can_hide_reasoning() {
        let mut config = Config::default();
        let mut output = Vec::new();
        let mut prompt = Prompt {
            reader: Cursor::new("2\n"),
            writer: &mut output,
        };
        configure_terminal_ui(&mut config, &mut prompt).unwrap();
        assert!(!config.tui.show_reasoning);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Show reasoning"));
        assert!(output.contains("Hide reasoning"));
        assert!(output.contains("animated Lightagent star"));

        let mut prompt = Prompt {
            reader: Cursor::new("1\n"),
            writer: Vec::new(),
        };
        configure_terminal_ui(&mut config, &mut prompt).unwrap();
        assert!(config.tui.show_reasoning);
    }
}
