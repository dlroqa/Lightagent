//! The `lightagent` command-line interface.
//!
//! A thin surface over the one runtime: every command drives `lightagent-core`
//! and `lightagent-tools`, and the interface owns no agent logic of its own. The
//! default action is an interactive chat; the other commands set up and inspect
//! the isolated home, its profiles ("bots"), the configuration, the tools and
//! the provider.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod acp;
mod banner;
mod chat;
mod coordinator;
mod extensions;
mod generation;
mod identity_admin;
mod import;
mod jev;
mod markdown;
mod memory;
mod ops;
mod rag;
mod remote_credentials;
mod serve;
mod setup;
mod slash;
mod workers;

use std::io::IsTerminal as _;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lightagent_core::{
    AgentProfile, Config, ConfigStore, LightagentPaths, ProfileError, ProfileId, ProfileStore,
    SecretRef,
};
use lightagent_store::{SessionId, SessionStore};
use lightagent_tools::ToolRegistry;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_DATE: &str = env!("LIGHTAGENT_RELEASE_DATE");

/// The Lightagent CLI.
#[derive(Parser)]
#[command(
    name = "lightagent",
    about = "Lightagent — local intelligence with live tools",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive agent chat (the default action).
    Chat {
        /// The profile ("bot") to run; defaults to the active one.
        #[arg(long)]
        profile: Option<String>,
        /// Resume a saved session by id instead of starting a new one.
        #[arg(long)]
        session: Option<String>,
    },
    /// Set up the isolated home, a first profile and the configuration.
    Init {
        /// Reconfigure even if a home already exists.
        #[arg(long)]
        force: bool,
        /// The profile id to create (default: `default`).
        #[arg(long)]
        profile: Option<String>,
        /// The provider base URL.
        #[arg(long)]
        base_url: Option<String>,
        /// The default model id.
        #[arg(long)]
        model: Option<String>,
    },
    /// Configure Lightagent with an interactive menu.
    Setup {
        /// Open one setup section directly.
        #[arg(value_enum)]
        section: Option<setup::Section>,
    },
    /// Show or change configuration.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// List the models the provider offers.
    Models,
    /// List the enabled tools.
    Tools {
        #[command(subcommand)]
        action: Option<ToolsAction>,
    },
    /// Manage user profiles (also available as `profiles`).
    #[command(name = "profile", visible_alias = "profiles")]
    Profiles {
        #[command(subcommand)]
        action: Option<ProfilesAction>,
    },
    /// List and toggle installed extensions (capability bundles).
    Extensions {
        #[command(subcommand)]
        action: Option<ExtensionsAction>,
    },
    /// List and inspect saved sessions for the active profile.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsAction>,
    },
    /// Show each specialist role's readiness, or assign a role to a worker.
    Workers {
        #[command(subcommand)]
        action: Option<WorkersAction>,
    },
    /// Serve the Lightagent HTTP API (runs, sessions, tools, approvals + SSE).
    Serve {
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind.
        #[arg(long, default_value_t = 8735)]
        port: u16,
        /// Environment variable holding the API key (required off loopback).
        #[arg(long)]
        key_env: Option<String>,
        /// Serve the built WebUI panel from this directory (e.g. frontend/dist).
        #[arg(long)]
        web_root: Option<std::path::PathBuf>,
        /// Run as the desktop app's owned runtime: loopback only, the key read
        /// from standard input, one JSON ready line on stdout, every request
        /// checked for Host, Origin and the key; exits when standard input
        /// closes.
        #[arg(long, conflicts_with_all = ["key_env", "web_root"])]
        desktop: bool,
        /// With --desktop: serve this profile instead of the active one.
        #[arg(long, requires = "desktop")]
        profile: Option<String>,
    },
    /// Manage local identities: users, credentials, ownership and audit.
    /// Operator-only (this OS account); does not enable multi-user mode.
    Identity {
        #[command(subcommand)]
        action: identity_admin::IdentityAction,
    },
    /// Manage profile-scoped remote credentials (sealed at rest).
    /// Operator-only (this OS account); used by untrusted runs only.
    RemoteCredential {
        #[command(subcommand)]
        action: remote_credentials::RemoteCredentialAction,
    },
    /// Serve the Agent Client Protocol (ACP) over stdio, for an editor to drive.
    Acp,
    /// Show the configured agent-platform component topology.
    Architecture,

    /// Report the environment, home, profile and provider.
    Doctor,
    /// Import profiles and skills from another agent's home.
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },
    /// Index documents and search them (retrieval) for the active profile.
    Rag {
        #[command(subcommand)]
        action: RagAction,
    },
    /// Write, recall and manage the active profile's durable memory.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Print the welcome mark and exit.
    Banner {
        /// Render unconditionally (bypasses the terminal check), for CI.
        #[arg(long)]
        preview: bool,
    },
    /// Update Lightagent to the latest standalone release.
    Update {
        /// Report whether an update is available without installing it.
        #[arg(long)]
        check: bool,
        /// Reinstall even when this version is already current.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the configuration, secrets shown only as references.
    Show,
    /// Print the path to the config file.
    Path,
    /// Read one dotted key (e.g. `inference.base_url`).
    Get { key: String },
    /// Set one dotted key.
    Set { key: String, value: String },
    /// Dry run: report which profiles may send task content to Jev for model
    /// routing now, and which would be denied under the strict policy.
    /// Reads only; changes nothing.
    JevRoutingReport,
}

#[derive(Subcommand)]
enum ToolsAction {
    /// List tools available to the active profile, including connected extensions.
    List,
}

#[derive(Subcommand)]
enum WorkersAction {
    /// Each role's readiness: its worker, model, skill, tools and reasons.
    Status {
        /// The lead profile (default: the active one).
        #[arg(long)]
        profile: Option<String>,
    },
    /// Assign a role to the built-in worker or to a private worker profile.
    Assign {
        /// researcher, coder, reviewer or tester.
        role: String,
        /// A worker profile with the same owner as the lead profile.
        #[arg(long, conflicts_with = "builtin", required_unless_present = "builtin")]
        worker: Option<String>,
        /// Pin the built-in worker, superseding a profile named after the role.
        #[arg(long)]
        builtin: bool,
        /// The lead profile (default: the active one).
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove a role's assignment, so the role resolves as if it had none.
    Revoke {
        role: String,
        /// The lead profile (default: the active one).
        #[arg(long)]
        profile: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfilesAction {
    /// List the profiles and mark the active one.
    List,
    /// Show one profile.
    Show { id: String },
    /// Create a profile.
    #[command(visible_alias = "add")]
    Create {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        persona: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Make a profile the active one.
    Use { id: String },
    /// Give a profile an operator-owned capability preset (e.g. `research@1`).
    Preset { id: String, preset: String },
    /// Delete a profile and all of its local data.
    Delete { id: String },
}

#[derive(Subcommand)]
enum ExtensionsAction {
    /// List installed extensions, marking which are active.
    List,
    /// Show one extension's manifest and what it contributes.
    Show { name: String },
    /// Activate an installed extension (clears it from the disabled list).
    Enable { name: String },
    /// Keep an extension installed but inactive.
    Disable { name: String },
    /// Install an extension from a local directory or an HTTPS Git repository.
    Install {
        source: std::path::PathBuf,
        /// Install for the active profile only.
        #[arg(long)]
        profile: bool,
    },
    /// Uninstall an extension from the global or active-profile store.
    Uninstall {
        name: String,
        /// Remove the active profile's copy instead of the global one.
        #[arg(long)]
        profile: bool,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Remember a fact.
    Add {
        text: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// List every memory.
    List,
    /// Show structured working knowledge, optionally focused on a topic.
    Reflect { topic: Option<String> },
    /// Recall the memories most relevant to a query.
    Search {
        query: String,
        #[arg(long)]
        top_k: Option<usize>,
    },
    /// Show likely durable facts from a saved session for review.
    Candidates { session: String },
    /// Save one reviewed session message as a durable memory.
    Promote {
        session: String,
        /// One-based message number from `sessions show` or `memory candidates`.
        message: usize,
        /// Optionally edit the fact before saving it.
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Correct an existing fact by id.
    Update { id: String, text: String },
    /// Forget one memory by id.
    Forget { id: String },
    /// Forget everything.
    Clear,
}

#[derive(Subcommand)]
enum RagAction {
    /// Index a file, or every file in a directory.
    Add {
        path: std::path::PathBuf,
        /// Name to index under (default: the file name).
        #[arg(long)]
        source: Option<String>,
    },
    /// Search the index and print the best passages.
    Search {
        query: String,
        #[arg(long)]
        top_k: Option<usize>,
    },
    /// List the indexed sources.
    List,
    /// Empty the index.
    Clear,
}

#[derive(Subcommand)]
enum ImportSource {
    /// Import Hermes profiles (persona, model routing) and skills.
    Hermes {
        /// The Hermes home to read (default: $HERMES_HOME, else ~/.hermes).
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// Show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite profiles and skills that already exist.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum SessionsAction {
    /// List saved sessions, newest first.
    List,
    /// Show one session's transcript and run history.
    Show { id: String },
    /// Delete one session.
    Delete { id: String },
}

/// Entry point: parse, dispatch, and turn an error into a message and exit code.
pub fn run_cli() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(dispatch(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<(), String> {
    match cli.command {
        None => {
            if std::io::stdin().is_terminal() {
                chat::run(None, None, cli.json, VERSION, RELEASE_DATE).await
            } else {
                println!(
                    "Run `lightagent --help` for the available commands, or `lightagent chat` to start."
                );
                Ok(())
            }
        }
        Some(Command::Chat { profile, session }) => {
            chat::run(profile, session, cli.json, VERSION, RELEASE_DATE).await
        }
        Some(Command::Init {
            force,
            profile,
            base_url,
            model,
        }) => init(force, profile, base_url, model, cli.json),
        Some(Command::Setup { section }) => setup::run(section, cli.json).await,
        Some(Command::Config { action }) => {
            config_cmd(action.unwrap_or(ConfigAction::Show), cli.json)
        }
        Some(Command::Models) => models(cli.json).await,
        Some(Command::Tools { action }) => {
            let _ = action.unwrap_or(ToolsAction::List);
            tools_list(cli.json).await
        }
        Some(Command::Profiles { action }) => {
            profiles_cmd(action.unwrap_or(ProfilesAction::List), cli.json)
        }
        Some(Command::Workers { action }) => {
            let action = action.unwrap_or(WorkersAction::Status { profile: None });
            workers_cmd(action, cli.json).await
        }
        Some(Command::Extensions { action }) => match action.unwrap_or(ExtensionsAction::List) {
            ExtensionsAction::List => extensions::list(cli.json),
            ExtensionsAction::Show { name } => extensions::show(&name, cli.json),
            ExtensionsAction::Enable { name } => extensions::enable(&name, cli.json),
            ExtensionsAction::Disable { name } => extensions::disable(&name, cli.json),
            ExtensionsAction::Install { source, profile } => {
                extensions::install(&source, profile, cli.json)
            }
            ExtensionsAction::Uninstall { name, profile } => {
                extensions::uninstall(&name, profile, cli.json)
            }
        },
        Some(Command::Sessions { action }) => {
            sessions_cmd(action.unwrap_or(SessionsAction::List), cli.json)
        }
        Some(Command::Serve {
            host,
            port,
            key_env,
            web_root,
            desktop,
            profile,
        }) => {
            if desktop {
                serve::run_desktop(host, port, profile).await
            } else {
                serve::run(host, port, key_env, web_root).await
            }
        }
        Some(Command::Identity { action }) => identity_admin::run(action, cli.json),
        Some(Command::RemoteCredential { action }) => remote_credentials::run(action, cli.json),
        Some(Command::Acp) => acp::run().await,
        Some(Command::Architecture) => architecture(cli.json),
        Some(Command::Doctor) => doctor(cli.json).await,
        Some(Command::Import { source }) => match source {
            ImportSource::Hermes {
                from,
                dry_run,
                force,
            } => import::hermes(from, dry_run, force, cli.json),
        },
        Some(Command::Rag { action }) => match action {
            RagAction::Add { path, source } => rag::add(path, source, cli.json).await,
            RagAction::Search { query, top_k } => rag::search(query, top_k, cli.json).await,
            RagAction::List => rag::list(cli.json),
            RagAction::Clear => rag::clear(cli.json),
        },
        Some(Command::Memory { action }) => match action {
            MemoryAction::Add { text, kind, tags } => memory::add(text, kind, tags, cli.json),
            MemoryAction::List => memory::list(cli.json),
            MemoryAction::Reflect { topic } => memory::reflect(topic, cli.json),
            MemoryAction::Search { query, top_k } => memory::search(query, top_k, cli.json).await,
            MemoryAction::Candidates { session } => memory::candidates(session, cli.json),
            MemoryAction::Promote {
                session,
                message,
                text,
                kind,
                tags,
            } => memory::promote(session, message, text, kind, tags, cli.json),
            MemoryAction::Update { id, text } => memory::update(id, text, cli.json),
            MemoryAction::Forget { id } => memory::forget(id, cli.json),
            MemoryAction::Clear => memory::clear(cli.json),
        },
        Some(Command::Banner { preview }) => {
            if preview {
                print!(
                    "{}",
                    banner::render(VERSION, std::env::var_os("NO_COLOR").is_none())
                );
            } else if banner::should_show(cli.json) {
                banner::print(VERSION);
            }
            Ok(())
        }
        Some(Command::Update { check, force }) => {
            release_update::run(
                release_update::Cli::Lightagent,
                VERSION,
                release_update::Request {
                    check,
                    force,
                    json: cli.json,
                },
            )
            .await
        }
    }
}

/// Resolve the home paths, mapping the error to a message.
fn paths() -> Result<LightagentPaths, String> {
    LightagentPaths::resolve().map_err(|error| error.to_string())
}

fn load_config(paths: &LightagentPaths) -> Result<Config, String> {
    ConfigStore::at(paths)
        .load()
        .map_err(|error| error.to_string())
}

fn active_profile_id(store: &ProfileStore) -> Result<Option<ProfileId>, String> {
    store.active().map_err(|error| error.to_string())
}

/// The main account is always the protected `default` profile. Persist it on
/// first use of profile management so switching away and back works even on an
/// installation that previously relied on the built-in, unsaved fallback.
pub(crate) fn ensure_default_profile(
    store: &ProfileStore,
    config: &Config,
) -> Result<ProfileId, String> {
    let id = ProfileId::new("default").map_err(|error| error.to_string())?;
    match store.load(&id) {
        Ok(_) => {}
        Err(ProfileError::NotFound { .. }) => {
            let mut profile = default_profile(config)?;
            let handle = store.handle(&id);
            if handle.dir().exists() {
                // An earlier chat may have created sessions or a persona before
                // profile management wrote the manifest. Keep those files.
                match std::fs::read_to_string(handle.soul_file()) {
                    Ok(persona) => profile.persona = persona,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
                store.save(&profile).map_err(|error| error.to_string())?;
            } else {
                store.create(&profile).map_err(|error| error.to_string())?;
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    if store.active().map_err(|error| error.to_string())?.is_none() {
        store.set_active(&id).map_err(|error| error.to_string())?;
    }
    Ok(id)
}

pub(crate) fn default_profile(config: &Config) -> Result<AgentProfile, String> {
    let id = ProfileId::new("default").map_err(|error| error.to_string())?;
    let model = config
        .inference
        .model
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut profile = AgentProfile::new(
        id,
        "Default",
        "You are Lightagent, a helpful local agent with live tools.",
        model,
    );
    profile.approval_policy = config.security.approval_policy;
    Ok(profile)
}

/// Under `subagents.resolution = "legacy_profiles"` only (the rollback),
/// create the role profiles the earlier behavior cloned from the lead.
/// Existing profiles are never changed: people can tune a specialist persona,
/// model, or policy and it will be preserved for later runs. With the
/// built-in catalog, roles need no profiles, so none are created.
pub(crate) fn ensure_subagent_profiles(
    store: &ProfileStore,
    config: &Config,
    lead: &AgentProfile,
) -> Result<(), String> {
    if !config.subagents.enabled
        || config.subagents.resolution != lightagent_core::WorkerResolution::LegacyProfiles
    {
        return Ok(());
    }

    for role in &config.subagents.allowed_roles {
        let id = ProfileId::new(role).map_err(|error| error.to_string())?;
        match store.load(&id) {
            Ok(_) => continue,
            Err(ProfileError::NotFound { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }

        let mut worker = lead.clone();
        worker.id = id;
        worker.name = format!("{role} subagent");
        worker.description = format!("Focused {role} worker for bounded delegated tasks.");
        worker.persona = format!("{}\n\n{}", lead.persona, subagent_role_instruction(role));
        match store.create(&worker) {
            Ok(()) | Err(ProfileError::AlreadyExists { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Whether `profile` is exactly what [`ensure_subagent_profiles`] generated
/// for `role`, rather than a profile a person customized. A generated one is
/// superseded by the built-in worker; a customized one keeps being used.
pub(crate) fn is_generated_subagent_profile(profile: &AgentProfile, role: &str) -> bool {
    profile.name == format!("{role} subagent")
        && profile.description == format!("Focused {role} worker for bounded delegated tasks.")
        && profile.persona.ends_with(subagent_role_instruction(role))
}

fn subagent_role_instruction(role: &str) -> &'static str {
    match role {
        "researcher" => {
            "You are a research specialist. Investigate the assigned question with the available tools, distinguish evidence from inference, and return concise findings with sources."
        }
        "coder" => {
            "You are an implementation specialist. Make only the scoped changes requested, validate them proportionately, and report the files and verification performed."
        }
        "reviewer" => {
            "You are a review specialist. Inspect the assigned work for correctness, regressions, safety, and missing tests; report actionable findings in priority order."
        }
        "tester" => {
            "You are a testing specialist. Run or design the most relevant checks for the assigned change and report results, failures, and any coverage gaps."
        }
        _ => {
            "You are a focused specialist subagent. Complete only the assigned bounded task, use the available tools carefully, and return concise evidence-backed results to the lead agent."
        }
    }
}

/// Print the deployment topology without contacting optional endpoints.
fn architecture(json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = load_config(&paths)?;
    let component = |endpoint: &lightagent_core::PlatformEndpointConfig| {
        if endpoint.enabled {
            format!(
                "configured @ {}",
                endpoint.base_url.as_deref().unwrap_or("(invalid)")
            )
        } else {
            "not configured".to_owned()
        }
    };

    if json {
        let value = serde_json::json!({
            "harness": "Lightagent CLI/API and browser UI",
            "web_ui": "served by `lightagent serve --web-root`",
            "jev": component(&config.platform.jev.endpoint),
            "qdrant": component(&config.platform.qdrant.endpoint),
            "infinity": component(&config.platform.infinity.endpoint),
            "searxng": if config.web.enabled { config.web.search.endpoint.clone() } else { None },
            "open_terminal": component(&config.platform.open_terminal.endpoint),
            "lightweight": config.inference.base_url,
        });
        println!("{value:#}");
        return Ok(());
    }

    println!("Agent harness / Open WebUI");
    println!(
        "├─ Jev: routing and confidence decisions [{}]",
        component(&config.platform.jev.endpoint)
    );
    println!(
        "├─ Qdrant + Infinity: retrieve and rerank knowledge [{}; {}]",
        component(&config.platform.qdrant.endpoint),
        component(&config.platform.infinity.endpoint)
    );
    println!(
        "├─ SearXNG: current web information [{}]",
        config
            .web
            .search
            .endpoint
            .as_deref()
            .filter(|_| config.web.enabled)
            .unwrap_or("not configured")
    );
    println!(
        "├─ Open Terminal: isolated code execution [{}]",
        component(&config.platform.open_terminal.endpoint)
    );
    println!(
        "└─ Lightweight: local inference and API gateway [{}]",
        config.inference.base_url
    );
    Ok(())
}

// --- doctor -----------------------------------------------------------------
async fn doctor(json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = load_config(&paths)?;
    let store = ProfileStore::new(paths.root());
    let active = active_profile_id(&store)?;
    let profiles = store.list().map_err(|error| error.to_string())?;
    let tool_count = ToolRegistry::builtin().names().len();
    let home_exists = paths.root().exists();

    if json {
        let value = serde_json::json!({
            "version": VERSION,
            "home": paths.root().display().to_string(),
            "home_exists": home_exists,
            "config_file": paths.config_file().display().to_string(),
            "provider": config.inference.provider,
            "base_url": config.inference.base_url,
            "active_profile": active.as_ref().map(|id| id.as_str().to_string()),
            "profiles": profiles.iter().map(|id| id.as_str().to_string()).collect::<Vec<_>>(),
            "tools": tool_count,
            "banner": std::io::stderr().is_terminal(),
        });
        println!("{value:#}");
        return Ok(());
    }

    let mut out = String::new();
    out.push_str(&format!("Lightagent {VERSION}\n"));
    out.push_str(&format!(
        "Home:      {}{}\n",
        paths.root().display(),
        if home_exists {
            ""
        } else {
            "  (not created — run `lightagent init`)"
        }
    ));
    out.push_str(&format!("Config:    {}\n", paths.config_file().display()));
    out.push_str(&format!(
        "Provider:  {} @ {}\n",
        config.inference.provider, config.inference.base_url
    ));
    out.push_str(&format!(
        "Profile:   {}\n",
        active
            .as_ref()
            .map(|id| id.as_str())
            .unwrap_or("(none active)")
    ));
    out.push_str(&format!(
        "Profiles:  {}\n",
        if profiles.is_empty() {
            "(none)".to_string()
        } else {
            profiles
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    out.push_str(&format!("Tools:     {tool_count} built-in\n"));
    out.push_str("Provider:  configured (run a chat or serve request to verify it)\n");
    print!("{out}");
    Ok(())
}

// --- tools ------------------------------------------------------------------

async fn tools_list(json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = load_config(&paths)?;
    let profiles = ProfileStore::new(paths.root());
    let profile = chat::resolve_profile(&profiles, &config, None)?;
    let profile_dir = profiles.handle(&profile.id).dir().to_path_buf();
    let extensions = chat::load_extensions(paths.root(), &profile_dir);
    let skills = chat::load_skills(paths.root(), &profile_dir, &extensions, &config);
    let mut registry =
        chat::configured_registry(&config, &profile_dir, &extensions, !skills.is_empty()).await;
    coordinator::add_explicit_tool(
        &mut registry,
        &config,
        profile.id.as_str(),
        coordinator::JevBudget::new(config.platform.jev.classification.max_calls_per_run),
        coordinator::JevState::new(),
    );
    // What a run of the active profile receives under its tool policy. In
    // report-only mode every tool is listed and the summary says what
    // enforcement would withhold. The summary goes to stderr, so the JSON
    // output keeps its shape.
    let tool_policy = chat::tool_policy(
        &config,
        &extensions,
        &registry,
        &profile,
        lightagent_core::identity::RunTrust::LocalOperator,
    );
    if let Some(line) = tool_policy.summary(profile.id.as_str()) {
        eprintln!("{line}");
    }
    let registry = tool_policy.apply(&registry);
    let policy = lightagent_core::PolicyEngine::new(profile.approval_policy.into());
    let mut rows = Vec::new();
    for name in registry.names() {
        if let Some(tool) = registry.get(&name) {
            let definition = tool.definition();
            let request = lightagent_core::ApprovalRequest::new(
                &definition.name,
                definition.risk,
                definition.scopes.clone(),
                "{}",
            );
            let permission = match policy.evaluate(&request, std::time::SystemTime::now()) {
                lightagent_core::ApprovalNeed::AutoApprove => "auto",
                lightagent_core::ApprovalNeed::Require(_) => "ask",
                lightagent_core::ApprovalNeed::Deny(_) => "block",
            };
            rows.push((
                definition.name.clone(),
                definition.risk.as_str().to_string(),
                permission,
                definition.description.clone(),
            ));
        }
    }

    if json {
        let value: Vec<_> = rows
            .iter()
            .map(|(name, risk, permission, description)| {
                serde_json::json!({ "name": name, "risk": risk, "permission": permission, "description": description })
            })
            .collect();
        println!("{}", serde_json::Value::Array(value));
        return Ok(());
    }

    let mut out = String::from("Tools:\n");
    for (name, risk, permission, description) in rows {
        out.push_str(&format!(
            "  {name:<16} [{permission}, {risk}]  {description}\n"
        ));
    }
    print!("{out}");
    Ok(())
}

// --- workers ----------------------------------------------------------------

/// The tools a run of `lead` would have under its tool policy, as an
/// operator: what its workers are drawn from.
async fn lead_tools(root: &std::path::Path, config: &Config, lead: &AgentProfile) -> ToolRegistry {
    let profiles = ProfileStore::new(root);
    let profile_dir = profiles.handle(&lead.id).dir().to_path_buf();
    let extensions = chat::load_extensions(root, &profile_dir);
    let skills = chat::load_skills(root, &profile_dir, &extensions, config);
    let registry =
        chat::configured_registry(config, &profile_dir, &extensions, !skills.is_empty()).await;
    let trust = lightagent_core::identity::RunTrust::LocalOperator;
    chat::tool_policy(config, &extensions, &registry, lead, trust).apply(&registry)
}

fn worker_role(raw: &str) -> Result<lightagent_core::WorkerRole, String> {
    lightagent_core::WorkerRole::parse(raw)
        .ok_or_else(|| format!("unknown role {raw:?}: use researcher, coder, reviewer or tester"))
}

async fn workers_cmd(action: WorkersAction, json: bool) -> Result<(), String> {
    let paths = paths()?;
    let config = load_config(&paths)?;
    let profiles = ProfileStore::new(paths.root());
    match action {
        WorkersAction::Status { profile } => {
            let lead = chat::resolve_profile(&profiles, &config, profile)?;
            let tools = lead_tools(paths.root(), &config, &lead).await;
            let trust = lightagent_core::identity::RunTrust::LocalOperator;
            let report = workers::readiness(paths.root(), &config, &lead, trust, &tools);
            if json {
                let value = serde_json::to_value(&report).map_err(|error| error.to_string())?;
                println!("{value}");
                return Ok(());
            }
            let mut out = format!("Workers for {}:\n", lead.id.as_str());
            for entry in report {
                let reasons = if entry.reasons.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", entry.reasons.join(", "))
                };
                out.push_str(&format!(
                    "  {:<10} {:<17} {}{reasons}\n",
                    entry.role.as_str(),
                    entry.state.as_str(),
                    entry.worker
                ));
            }
            print!("{out}");
            Ok(())
        }
        WorkersAction::Assign {
            role,
            worker,
            builtin: _,
            profile,
        } => {
            let lead = chat::resolve_profile(&profiles, &config, profile)?;
            let role = worker_role(&role)?;
            let target = match worker {
                Some(id) => lightagent_core::AssignmentTarget::Profile {
                    profile: ProfileId::new(&id).map_err(|error| error.to_string())?,
                },
                None => lightagent_core::AssignmentTarget::Builtin,
            };
            let assignment = workers::assign(paths.root(), &lead.id, role, target)?;
            if json {
                let value = serde_json::to_value(&assignment).map_err(|error| error.to_string())?;
                println!("{value}");
            } else {
                println!(
                    "Assigned {} for {} ({}).",
                    role.as_str(),
                    lead.id.as_str(),
                    assignment.id
                );
            }
            Ok(())
        }
        WorkersAction::Revoke { role, profile } => {
            let lead = chat::resolve_profile(&profiles, &config, profile)?;
            let role = worker_role(&role)?;
            let dir = profiles.handle(&lead.id).dir().to_path_buf();
            let removed = lightagent_core::WorkerAssignments::new(lead.id.clone(), &dir)
                .revoke(role)
                .map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "role": role, "revoked": removed })
                );
            } else if removed {
                println!(
                    "Revoked the {} assignment of {}.",
                    role.as_str(),
                    lead.id.as_str()
                );
            } else {
                println!("{} had no {} assignment.", lead.id.as_str(), role.as_str());
            }
            Ok(())
        }
    }
}

// --- profiles ---------------------------------------------------------------

fn profiles_cmd(action: ProfilesAction, json: bool) -> Result<(), String> {
    let paths = paths()?;
    let profiles = ops::Profiles::open(&paths).map_err(|error| error.to_string())?;
    match action {
        ProfilesAction::List => {
            let list = profiles.list().map_err(|error| error.to_string())?;
            if json {
                let value = serde_json::json!({
                    "active": list.active,
                    "profiles": list.profiles,
                });
                println!("{value:#}");
                return Ok(());
            }
            if list.profiles.is_empty() {
                println!("No profiles yet. Create one with `lightagent profile create <id>`.");
                return Ok(());
            }
            for id in &list.profiles {
                let marker = if list.active.as_deref() == Some(id.as_str()) {
                    "* "
                } else {
                    "  "
                };
                println!("{marker}{id}");
            }
            Ok(())
        }
        ProfilesAction::Show { id } => {
            let profile = profiles.show(&id).map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_value(&profile).map_err(|e| e.to_string())?
                );
            } else {
                println!("id:      {}", profile.id.as_str());
                println!("name:    {}", profile.name);
                println!("model:   {}", profile.routing.model);
                println!("persona: {}", first_line(&profile.persona));
            }
            Ok(())
        }
        ProfilesAction::Create {
            id,
            name,
            persona,
            model,
        } => {
            let id = profiles
                .create(lightagent_api::ops::CreateProfile {
                    id,
                    name,
                    persona,
                    model,
                })
                .map_err(|error| error.to_string())?;
            if json {
                println!("{}", serde_json::json!({ "created": id.as_str() }));
            } else {
                println!("Created profile '{}'.", id.as_str());
            }
            Ok(())
        }
        ProfilesAction::Preset { id, preset } => {
            let id = profiles
                .preset(&id, &preset)
                .map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "profile": id.as_str(), "preset": preset })
                );
            } else {
                println!("Profile '{}' now has the {preset} preset.", id.as_str());
            }
            Ok(())
        }
        ProfilesAction::Use { id } => {
            let id = profiles
                .use_profile(&id)
                .map_err(|error| error.to_string())?;
            if json {
                println!("{}", serde_json::json!({ "active": id.as_str() }));
            } else {
                println!("Active profile is now '{}'.", id.as_str());
            }
            Ok(())
        }
        ProfilesAction::Delete { id } => {
            let id = profiles.delete(&id).map_err(|error| error.to_string())?;
            if json {
                println!("{}", serde_json::json!({ "deleted": id.as_str() }));
            } else {
                println!("Deleted profile '{}'.", id.as_str());
            }
            Ok(())
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

// --- sessions ---------------------------------------------------------------

fn sessions_cmd(action: SessionsAction, json: bool) -> Result<(), String> {
    let paths = paths()?;
    let store = ProfileStore::new(paths.root());
    let active = active_profile_id(&store)?
        .map(Ok)
        .unwrap_or_else(|| ProfileId::new("default"))
        .map_err(|error| error.to_string())?;
    let handle = store.handle(&active);
    let sessions = SessionStore::at_profile(&handle);

    match action {
        SessionsAction::List => {
            let list = sessions.list().map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_value(&list).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            if list.is_empty() {
                println!("No saved sessions for profile '{}'.", active.as_str());
                return Ok(());
            }
            for summary in list {
                println!(
                    "{}  {:<24}  {} msgs, {} runs",
                    summary.id.as_str(),
                    truncate(&summary.title, 24),
                    summary.message_count,
                    summary.run_count
                );
            }
            Ok(())
        }
        SessionsAction::Show { id } => {
            let id = SessionId::parse(&id).map_err(|error| error.to_string())?;
            let session = sessions.load(&id).map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_value(&session).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            println!(
                "session {}  (profile {})",
                session.id.as_str(),
                session.profile
            );
            println!("title: {}", session.title);
            for (index, message) in session.messages.iter().enumerate() {
                println!(
                    "  {} [{}] {}",
                    index + 1,
                    message.role,
                    first_line(&message.content)
                );
            }
            for run in &session.runs {
                println!(
                    "  run {} — {} ({} tool calls)",
                    run.run_id,
                    run.stop_reason.as_deref().unwrap_or("?"),
                    run.tools.len()
                );
                for tool in &run.tools {
                    println!(
                        "    {} {} ({}){}",
                        tool.id,
                        tool.tool,
                        tool.outcome,
                        tool.source
                            .as_ref()
                            .map(|source| format!("  {source}"))
                            .unwrap_or_default()
                    );
                    if !tool.result_excerpt.is_empty() {
                        println!(
                            "      {}",
                            truncate(&tool.result_excerpt.replace('\n', " "), 300)
                        );
                    }
                }
            }
            Ok(())
        }
        SessionsAction::Delete { id } => {
            let id = SessionId::parse(&id).map_err(|error| error.to_string())?;
            if sessions.delete(&id).map_err(|error| error.to_string())? {
                println!("Deleted session {}.", id.as_str());
            } else {
                println!("No session {} to delete.", id.as_str());
            }
            Ok(())
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

// --- config -----------------------------------------------------------------

fn config_cmd(action: ConfigAction, json: bool) -> Result<(), String> {
    let paths = paths()?;
    let store = ConfigStore::at(&paths);
    match action {
        ConfigAction::Path => {
            println!("{}", store.path().display());
            Ok(())
        }
        ConfigAction::Show => {
            let config = store.load().map_err(|error| error.to_string())?;
            let value = config.redacted_json();
            if json {
                println!("{value}");
            } else {
                println!("{value:#}");
            }
            Ok(())
        }
        ConfigAction::Get { key } => {
            let config = store.load().map_err(|error| error.to_string())?;
            match get_key(&config, &key) {
                Some(value) => {
                    println!("{value}");
                    Ok(())
                }
                None => Err(format!("unknown or unreadable config key '{key}'")),
            }
        }
        ConfigAction::Set { key, value } => {
            let mut config = store.load().map_err(|error| error.to_string())?;
            set_key(&mut config, &key, &value)?;
            store.save(&config).map_err(|error| error.to_string())?;
            println!("Set {key} = {value}");
            Ok(())
        }
        ConfigAction::JevRoutingReport => {
            let config = store.load().map_err(|error| error.to_string())?;
            let profiles = ProfileStore::new(paths.root())
                .list()
                .map_err(|error| error.to_string())?;
            let report = jev_routing_report(&config, &profiles);
            if json {
                println!("{report}");
            } else {
                print!("{}", jev_routing_report_text(&report));
            }
            Ok(())
        }
    }
}

/// The routing-permission dry run: for every profile on disk (and the
/// built-in `default`), whether it may route now and under `strict`. Profile
/// ids and fixed vocabulary only; no message, session or key is read.
fn jev_routing_report(config: &Config, profiles: &[ProfileId]) -> serde_json::Value {
    let jev = &config.platform.jev;
    let strict = lightagent_core::JevConfig {
        routing_permission: lightagent_core::JevRoutingPermission::Strict,
        ..jev.clone()
    };
    let mut ids: Vec<String> = profiles.iter().map(|id| id.as_str().to_owned()).collect();
    if !ids.iter().any(|id| id == "default") {
        ids.push("default".to_owned());
    }
    ids.sort();
    ids.dedup();
    let rows: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "profile": id,
                "now": jev.routing_grant(id).as_str(),
                "under_strict": strict.routing_grant(id).as_str(),
            })
        })
        .collect();
    let denied: Vec<&String> = ids
        .iter()
        .filter(|id| !strict.routing_grant(id).permits())
        .collect();
    let unknown: Vec<&String> = jev
        .routing_allowed_profiles
        .iter()
        .filter(|allowed| !ids.contains(allowed))
        .collect();
    let routing_active = jev.endpoint.enabled
        && (jev
            .allowed_models
            .iter()
            .any(|model| !model.trim().is_empty())
            || !jev.allowed_profiles.is_empty());
    serde_json::json!({
        "report": "jev-routing-permission-dry-run",
        "changes_made": false,
        "routing_enabled": routing_active,
        "routing_input": jev.routing_input.as_str(),
        "permission_policy": jev.routing_permission.as_str(),
        "compatibility": jev.routing_permission == lightagent_core::JevRoutingPermission::LegacyAllProfiles,
        "profiles": rows,
        "denied_under_strict": denied,
        "allowed_profiles_not_found": unknown,
    })
}

fn jev_routing_report_text(report: &serde_json::Value) -> String {
    let text = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_owned();
    let mut out = String::from("Jev model-routing permission (dry run; nothing was changed)\n\n");
    out.push_str(&format!(
        "Policy: {}{}\nRouting active: {}\nRouting input: {}\n\n",
        text(&report["permission_policy"]),
        if report["compatibility"] == true {
            " (compatibility: every profile may send task content for routing)"
        } else {
            ""
        },
        if report["routing_enabled"] == true {
            "yes"
        } else {
            "no"
        },
        text(&report["routing_input"]),
    ));
    out.push_str("Profile                        Now            Under strict\n");
    for row in report["profiles"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{:<30} {:<14} {}\n",
            text(&row["profile"]),
            text(&row["now"]),
            text(&row["under_strict"]),
        ));
    }
    let denied: Vec<String> = report["denied_under_strict"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    let unknown: Vec<String> = report["allowed_profiles_not_found"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    if !unknown.is_empty() {
        out.push_str(&format!(
            "\nListed but not found on disk: {}\n",
            unknown.join(", ")
        ));
    }
    out.push_str(&format!(
        "\nDenied under strict: {}\n",
        if denied.is_empty() {
            "none".to_owned()
        } else {
            denied.join(", ")
        }
    ));
    if report["compatibility"] == true {
        out.push_str(
            "\nTo enforce, grant the profiles that should keep routing, then switch the policy:\n  \
             lightagent config set platform.jev.routing_allowed_profiles <profile,...>\n  \
             lightagent config set platform.jev.routing_permission strict\n",
        );
    }
    out
}

/// Read a small, fixed set of dotted keys.
fn get_key(config: &Config, key: &str) -> Option<String> {
    match key {
        "inference.provider" => Some(config.inference.provider.clone()),
        "inference.base_url" => Some(config.inference.base_url.clone()),
        "inference.model" => Some(config.inference.model.clone().unwrap_or_default()),
        "inference.api_key" => Some(
            config
                .inference
                .api_key
                .as_ref()
                .map(SecretRef::redacted)
                .unwrap_or_default(),
        ),
        "inference.device" => Some(config.inference.device.clone()),
        key if key.starts_with("inference.model_catalog.") => config
            .inference
            .model_catalog
            .get(&key["inference.model_catalog.".len()..])
            .cloned(),
        key if key.starts_with("inference.model_alias.") => config
            .inference
            .model_aliases
            .get(&key["inference.model_alias.".len()..])
            .cloned(),
        "agent.max_turns" => Some(config.agent.max_turns.to_string()),
        "agent.max_tool_calls" => Some(config.agent.max_tool_calls.to_string()),
        // Empty means no time limit.
        "agent.wall_clock_secs" => Some(
            config
                .agent
                .wall_clock_secs
                .map(|secs| secs.to_string())
                .unwrap_or_default(),
        ),
        "extensions.enabled" => Some(config.extensions.enabled.to_string()),
        "memory.auto_capture" => Some(config.memory.auto_capture.to_string()),
        "memory.inject_recent" => Some(config.memory.inject_recent.to_string()),
        "memory.top_k" => Some(config.memory.top_k.to_string()),
        "web.enabled" => Some(config.web.enabled.to_string()),
        "web.search.endpoint" => Some(config.web.search.endpoint.clone().unwrap_or_default()),
        "web.search.query_param" => Some(config.web.search.query_param.clone()),
        "web.search.max_results" => Some(config.web.search.max_results.to_string()),
        "web.max_fetch_bytes" => Some(config.web.max_fetch_bytes.to_string()),
        "web.timeout_secs" => Some(config.web.timeout_secs.to_string()),
        "rag.realtime_enabled" => Some(config.rag.realtime_enabled.to_string()),
        "tools.profile_policy" => Some(config.tools.profile_policy.as_str().to_owned()),
        "platform.jev.enabled" => Some(config.platform.jev.endpoint.enabled.to_string()),
        "platform.jev.base_url" => Some(
            config
                .platform
                .jev
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.jev.model" => Some(config.platform.jev.model.clone()),
        "platform.jev.confidence_threshold" => {
            Some(config.platform.jev.confidence_threshold.to_string())
        }
        "platform.jev.allowed_models" => Some(config.platform.jev.allowed_models.join(",")),
        "platform.jev.allowed_profiles" => Some(config.platform.jev.allowed_profiles.join(",")),
        "platform.jev.timeout_secs" => Some(config.platform.jev.timeout_secs.to_string()),
        "platform.jev.routing_input" => Some(config.platform.jev.routing_input.as_str().to_owned()),
        "platform.jev.routing_permission" => {
            Some(config.platform.jev.routing_permission.as_str().to_owned())
        }
        "platform.jev.routing_allowed_profiles" => {
            Some(config.platform.jev.routing_allowed_profiles.join(","))
        }
        "platform.jev.classification.workflow_guidance" => Some(
            config
                .platform
                .jev
                .classification
                .workflow_guidance
                .to_string(),
        ),
        "platform.jev.classification.mode" => {
            Some(config.platform.jev.classification.mode.as_str().to_owned())
        }
        "subagents.selection" => Some(config.subagents.selection.as_str().to_owned()),
        "inference.tool_names" => Some(config.inference.tool_names.as_str().to_owned()),
        "subagents.selection_profiles" => Some(config.subagents.selection_profiles.join(",")),
        "platform.jev.classification.allowed_profiles" => Some(
            config
                .platform
                .jev
                .classification
                .allowed_profiles
                .join(","),
        ),
        "platform.jev.classification.max_input_chars" => Some(
            config
                .platform
                .jev
                .classification
                .max_input_chars
                .to_string(),
        ),
        "platform.jev.classification.default_threshold" => Some(
            config
                .platform
                .jev
                .classification
                .default_threshold
                .to_string(),
        ),
        "platform.jev.classification.version_thresholds" => Some(
            config
                .platform
                .jev
                .classification
                .version_thresholds
                .iter()
                .map(|(version, threshold)| format!("{version}={threshold}"))
                .collect::<Vec<_>>()
                .join(","),
        ),
        "platform.qdrant.enabled" => Some(config.platform.qdrant.endpoint.enabled.to_string()),
        "platform.qdrant.base_url" => Some(
            config
                .platform
                .qdrant
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.qdrant.collection" => Some(config.platform.qdrant.collection.clone()),
        "platform.qdrant.timeout_secs" => Some(config.platform.qdrant.timeout_secs.to_string()),
        "platform.infinity.enabled" => Some(config.platform.infinity.endpoint.enabled.to_string()),
        "platform.infinity.base_url" => Some(
            config
                .platform
                .infinity
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.infinity.embedding_model" => {
            Some(config.platform.infinity.embedding_model.clone())
        }
        "platform.infinity.rerank_model" => Some(config.platform.infinity.rerank_model.clone()),
        "platform.infinity.timeout_secs" => Some(config.platform.infinity.timeout_secs.to_string()),
        "platform.open_terminal.enabled" => {
            Some(config.platform.open_terminal.endpoint.enabled.to_string())
        }
        "platform.open_terminal.base_url" => Some(
            config
                .platform
                .open_terminal
                .endpoint
                .base_url
                .clone()
                .unwrap_or_default(),
        ),
        "platform.open_terminal.request_timeout_secs" => Some(
            config
                .platform
                .open_terminal
                .request_timeout_secs
                .to_string(),
        ),
        "platform.open_terminal.execution_timeout_secs" => Some(
            config
                .platform
                .open_terminal
                .execution_timeout_secs
                .to_string(),
        ),
        "platform.open_terminal.poll_interval_ms" => {
            Some(config.platform.open_terminal.poll_interval_ms.to_string())
        }
        "platform.open_terminal.max_output_bytes" => {
            Some(config.platform.open_terminal.max_output_bytes.to_string())
        }
        "tui.show_reasoning" => Some(config.tui.show_reasoning.to_string()),
        "runtime.preferred_device" => Some(config.runtime.preferred_device.clone()),
        "runtime.allow_cpu_fallback" => Some(config.runtime.allow_cpu_fallback.to_string()),
        "runtime.n_ctx" => config.runtime.n_ctx.map(|v| v.to_string()),
        "runtime.threads" => config.runtime.threads.map(|v| v.to_string()),
        "runtime.kv_type" => config.runtime.kv_type.clone(),
        "runtime.load_mode" => config.runtime.load_mode.clone(),
        "runtime.ubatch" => config.runtime.ubatch.map(|v| v.to_string()),
        "generation.max_output_tokens" => {
            config.generation.max_output_tokens.map(|v| v.to_string())
        }
        "generation.reserve_answer_tokens" => config
            .generation
            .reserve_answer_tokens
            .map(|v| v.to_string()),
        "generation.context_window" => config.generation.context_window.map(|v| v.to_string()),
        "generation.reasoning_effort" => config.generation.reasoning_effort.clone(),
        "generation.unknown_capacity_turn_tokens" => config
            .generation
            .unknown_capacity_turn_tokens
            .map(|v| v.to_string()),
        "generation.max_run_output_tokens" => config
            .generation
            .max_run_output_tokens
            .map(|v| v.to_string()),
        "generation.continuations" => config.generation.continuations.map(|v| v.to_string()),
        "generation.adaptive_allocation" => {
            Some(config.generation.adaptive_allocation.as_str().to_owned())
        }
        _ => None,
    }
}

/// Write a small, fixed set of dotted keys.
fn set_key(config: &mut Config, key: &str, value: &str) -> Result<(), String> {
    match key {
        "inference.base_url" => config.inference.base_url = value.to_string(),
        "inference.model" => config.inference.model = Some(value.to_string()),
        // The value is an environment-variable name, never the secret itself.
        // An empty value clears the reference.
        "inference.api_key" => {
            config.inference.api_key = parse_opt_string(value).map(SecretRef::env);
        }
        "inference.device" => config.inference.device = value.to_string(),
        key if key.starts_with("inference.model_catalog.") => {
            let model_id = key["inference.model_catalog.".len()..].trim();
            if model_id.is_empty() {
                return Err("model catalog entry needs a model id".to_owned());
            }
            let label = value.trim();
            if label.is_empty() {
                config.inference.model_catalog.remove(model_id);
            } else {
                config
                    .inference
                    .model_catalog
                    .insert(model_id.to_owned(), label.to_owned());
            }
        }
        key if key.starts_with("inference.model_alias.") => {
            let model_id = key["inference.model_alias.".len()..].trim();
            if model_id.is_empty() {
                return Err("model alias needs a model id".to_owned());
            }
            let label = value.trim();
            if label.is_empty() {
                config.inference.model_aliases.remove(model_id);
            } else {
                config
                    .inference
                    .model_aliases
                    .insert(model_id.to_owned(), label.to_owned());
            }
        }
        "agent.max_turns" => {
            config.agent.max_turns = parse_u32(value, "agent.max_turns")?;
        }
        "agent.max_tool_calls" => {
            config.agent.max_tool_calls = parse_u32(value, "agent.max_tool_calls")?;
        }
        // An empty value (or `none`) removes the time limit.
        "agent.wall_clock_secs" => {
            config.agent.wall_clock_secs = match value.trim() {
                "" | "none" => None,
                _ => Some(parse_u64(value, "agent.wall_clock_secs")?),
            };
        }
        "extensions.enabled" => {
            config.extensions.enabled = parse_bool(value)?;
        }
        "memory.auto_capture" => config.memory.auto_capture = parse_bool(value)?,
        "memory.inject_recent" => {
            config.memory.inject_recent = parse_usize(value, "memory.inject_recent")?;
        }
        "memory.top_k" => {
            let top_k = parse_usize(value, "memory.top_k")?;
            if top_k == 0 {
                return Err("memory.top_k must be at least 1".to_owned());
            }
            config.memory.top_k = top_k;
        }
        "web.enabled" => config.web.enabled = parse_bool(value)?,
        "web.search.endpoint" => config.web.search.endpoint = parse_opt_string(value),
        "web.search.query_param" => config.web.search.query_param = value.trim().to_owned(),
        "web.search.max_results" => {
            config.web.search.max_results = parse_usize(value, "web.search.max_results")?;
        }
        "web.max_fetch_bytes" => {
            config.web.max_fetch_bytes = parse_usize(value, "web.max_fetch_bytes")?;
        }
        "web.timeout_secs" => {
            config.web.timeout_secs = parse_u64(value, "web.timeout_secs")?;
        }
        "rag.realtime_enabled" => config.rag.realtime_enabled = parse_bool(value)?,
        "tools.profile_policy" => config.tools.profile_policy = value.parse()?,
        "platform.jev.enabled" => config.platform.jev.endpoint.enabled = parse_bool(value)?,
        "platform.jev.base_url" => config.platform.jev.endpoint.base_url = parse_opt_string(value),
        "platform.jev.model" => config.platform.jev.model = value.trim().to_owned(),
        "platform.jev.confidence_threshold" => {
            config.platform.jev.confidence_threshold = value
                .trim()
                .parse()
                .map_err(|_| "platform.jev.confidence_threshold must be a number".to_owned())?;
        }
        "platform.jev.allowed_models" => {
            config.platform.jev.allowed_models = value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.timeout_secs" => {
            config.platform.jev.timeout_secs = parse_u64(value, "platform.jev.timeout_secs")?;
        }
        "platform.jev.routing_input" => config.platform.jev.routing_input = value.parse()?,
        "platform.jev.routing_permission" => {
            config.platform.jev.routing_permission = value.parse()?;
        }
        "platform.jev.routing_allowed_profiles" => {
            config.platform.jev.routing_allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.workflow_guidance" => {
            config.platform.jev.classification.workflow_guidance = parse_bool(value)?;
        }
        "platform.jev.classification.mode" => {
            config.platform.jev.classification.mode = value.parse()?;
        }
        "subagents.selection" => {
            config.subagents.selection = value.parse()?;
        }
        "inference.tool_names" => {
            config.inference.tool_names = value.parse()?;
        }
        "subagents.selection_profiles" => {
            config.subagents.selection_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.allowed_profiles" => {
            config.platform.jev.classification.allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.jev.classification.max_input_chars" => {
            config.platform.jev.classification.max_input_chars = parse_usize(value, key)?;
        }
        "platform.jev.classification.default_threshold" => {
            config.platform.jev.classification.default_threshold = value
                .trim()
                .parse()
                .map_err(|_| format!("{key} must be a number"))?;
        }
        "platform.jev.classification.version_thresholds" => {
            config.platform.jev.classification.version_thresholds =
                parse_version_thresholds(value)?;
        }
        "platform.jev.allowed_profiles" => {
            config.platform.jev.allowed_profiles = value
                .split(',')
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
                .map(str::to_owned)
                .collect();
        }
        "platform.qdrant.enabled" => config.platform.qdrant.endpoint.enabled = parse_bool(value)?,
        "platform.qdrant.base_url" => {
            config.platform.qdrant.endpoint.base_url = parse_opt_string(value)
        }
        "platform.qdrant.collection" => config.platform.qdrant.collection = value.trim().to_owned(),
        "platform.qdrant.timeout_secs" => {
            config.platform.qdrant.timeout_secs = parse_u64(value, key)?
        }
        "platform.infinity.enabled" => {
            config.platform.infinity.endpoint.enabled = parse_bool(value)?
        }
        "platform.infinity.base_url" => {
            config.platform.infinity.endpoint.base_url = parse_opt_string(value)
        }
        "platform.infinity.embedding_model" => {
            config.platform.infinity.embedding_model = value.trim().to_owned()
        }
        "platform.infinity.rerank_model" => {
            config.platform.infinity.rerank_model = value.trim().to_owned()
        }
        "platform.infinity.timeout_secs" => {
            config.platform.infinity.timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.enabled" => {
            config.platform.open_terminal.endpoint.enabled = parse_bool(value)?
        }
        "platform.open_terminal.base_url" => {
            config.platform.open_terminal.endpoint.base_url = parse_opt_string(value)
        }
        "platform.open_terminal.request_timeout_secs" => {
            config.platform.open_terminal.request_timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.execution_timeout_secs" => {
            config.platform.open_terminal.execution_timeout_secs = parse_u64(value, key)?
        }
        "platform.open_terminal.poll_interval_ms" => {
            config.platform.open_terminal.poll_interval_ms = parse_u64(value, key)?
        }
        "platform.open_terminal.max_output_bytes" => {
            config.platform.open_terminal.max_output_bytes = value
                .parse()
                .map_err(|_| format!("{key} must be a nonnegative integer"))?
        }
        "tui.show_reasoning" => config.tui.show_reasoning = parse_bool(value)?,
        "runtime.preferred_device" => config.runtime.preferred_device = value.to_string(),
        "runtime.allow_cpu_fallback" => {
            config.runtime.allow_cpu_fallback = parse_bool(value)?;
        }
        "runtime.n_ctx" => config.runtime.n_ctx = parse_opt_u32(value)?,
        "runtime.threads" => config.runtime.threads = parse_opt_u32(value)?,
        "runtime.ubatch" => config.runtime.ubatch = parse_opt_u32(value)?,
        "runtime.kv_type" => config.runtime.kv_type = parse_opt_string(value),
        "runtime.load_mode" => config.runtime.load_mode = parse_opt_string(value),
        "generation.max_output_tokens" => {
            config.generation.max_output_tokens = parse_opt_u32(value)?
        }
        "generation.reserve_answer_tokens" => {
            config.generation.reserve_answer_tokens = parse_opt_u32(value)?
        }
        "generation.context_window" => config.generation.context_window = parse_opt_u32(value)?,
        "generation.reasoning_effort" => {
            config.generation.reasoning_effort = parse_opt_string(value)
        }
        "generation.max_run_output_tokens" => {
            config.generation.max_run_output_tokens = parse_opt_u32(value)?
        }
        "generation.unknown_capacity_turn_tokens" => {
            config.generation.unknown_capacity_turn_tokens = parse_opt_u32(value)?
        }
        "generation.continuations" => config.generation.continuations = parse_opt_u32(value)?,
        "generation.adaptive_allocation" => {
            config.generation.adaptive_allocation = value.parse()?;
        }
        _ => return Err(format!("unknown or read-only config key '{key}'")),
    }
    Ok(())
}

/// Parse an optional string key: an empty value clears it.
/// `jev-1.13.0=0.8,jev-1.14.0=0.85`; an empty value clears every entry.
fn parse_version_thresholds(
    value: &str,
) -> Result<std::collections::BTreeMap<String, f32>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (version, threshold) = entry
                .split_once('=')
                .ok_or_else(|| format!("{entry:?} must be written version=threshold"))?;
            let threshold = threshold
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("{entry:?} needs a numeric threshold"))?;
            Ok((version.trim().to_owned(), threshold))
        })
        .collect()
}

fn parse_opt_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse an optional `u32` key: an empty value clears it.
fn parse_opt_u32(value: &str) -> Result<Option<u32>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<u32>()
        .map(Some)
        .map_err(|_| format!("expected a non-negative integer, got '{value}'"))
}

fn parse_u32(value: &str, key: &str) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

fn parse_usize(value: &str, key: &str) -> Result<usize, String> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

fn parse_u64(value: &str, key: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("{key} expects a non-negative integer, got '{value}'"))
}

/// Parse a boolean config value, accepting the usual spellings.
fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("expected a boolean, got '{value}'")),
    }
}

// --- init -------------------------------------------------------------------

fn init(
    force: bool,
    profile: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    json: bool,
) -> Result<(), String> {
    let paths = paths()?;
    let already = paths.config_file().exists();
    if already && !force {
        return Err(format!(
            "already initialised at {} — pass --force to reconfigure",
            paths.root().display()
        ));
    }
    paths.scaffold().map_err(|error| error.to_string())?;

    let mut config = ConfigStore::at(&paths)
        .load()
        .map_err(|error| error.to_string())?;
    if let Some(base_url) = base_url {
        config.inference.base_url = base_url;
    }
    if let Some(model) = &model {
        config.inference.model = Some(model.clone());
    }
    ConfigStore::at(&paths)
        .save(&config)
        .map_err(|error| error.to_string())?;

    let profile_id = profile.unwrap_or_else(|| "default".to_string());
    let id = ProfileId::new(&profile_id).map_err(|error| error.to_string())?;
    let store = ProfileStore::new(paths.root());
    ensure_default_profile(&store, &config)?;
    if id.as_str() != "default" {
        match store.load(&id) {
            Ok(_) => {}
            Err(ProfileError::NotFound { .. }) => {
                let profile = AgentProfile::new(
                    id.clone(),
                    id.as_str(),
                    "You are a helpful local agent.",
                    model.unwrap_or_else(|| "default".to_string()),
                );
                store.create(&profile).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    store.set_active(&id).map_err(|error| error.to_string())?;

    if json {
        let value = serde_json::json!({
            "home": paths.root().display().to_string(),
            "config_file": paths.config_file().display().to_string(),
            "active_profile": id.as_str(),
            "base_url": config.inference.base_url,
        });
        println!("{value:#}");
    } else {
        println!("Initialised Lightagent at {}", paths.root().display());
        println!("  active profile: {}", id.as_str());
        println!("  provider:       {}", config.inference.base_url);
        println!("Run `lightagent chat` to start.");
    }
    Ok(())
}

// --- models -----------------------------------------------------------------

async fn models(json: bool) -> Result<(), String> {
    use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};

    let paths = paths()?;
    let config = load_config(&paths)?;
    let mut provider_config = ProviderConfig::new(config.inference.base_url.clone(), "default");
    if let Some(secret) = &config.inference.api_key
        && let Some(value) = secret.resolve()
    {
        provider_config = provider_config.with_api_key(value);
    }
    let provider = LightweightProvider::new(provider_config).map_err(|error| error.to_string())?;

    match provider.models().await {
        Ok(models) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_value(&models).map_err(|e| e.to_string())?
                );
            } else if models.is_empty() {
                println!("The provider offers no models (is one loaded?).");
            } else {
                for model in models {
                    println!("{model}");
                }
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "could not reach the provider at {}: {error}",
            config.inference.base_url
        )),
    }
}

/// The clap command tree, pinned in `docs/desktop/cli-tree.txt` so the
/// desktop capability inventory notices every added or removed command,
/// option, argument, value or alias.
#[cfg(test)]
mod desktop_inventory {
    use clap::CommandFactory as _;

    fn walk(command: &clap::Command, path: &str, out: &mut Vec<String>) {
        for alias in command.get_visible_aliases() {
            out.push(format!("alias {path} = {alias}"));
        }
        for arg in command.get_arguments() {
            let id = arg.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            let name = match arg.get_long() {
                Some(long) => format!("--{long}"),
                None => format!("<{id}>"),
            };
            out.push(format!("arg {path} {name}"));
            if arg.get_action().takes_values() {
                for value in arg.get_possible_values() {
                    if !value.is_hide_set() {
                        out.push(format!("value {path} {name} {}", value.get_name()));
                    }
                }
            }
        }
        for sub in command.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            let child = format!("{path} {}", sub.get_name());
            out.push(format!("cmd {child}"));
            walk(sub, &child, out);
        }
    }

    #[test]
    fn the_cli_tree_matches_the_desktop_inventory() {
        let mut out = vec!["cmd lightagent".to_owned()];
        walk(&super::Cli::command(), "lightagent", &mut out);
        out.sort();
        let actual = out.join("\n") + "\n";
        let expected = include_str!("../../../docs/desktop/cli-tree.txt").replace("\r\n", "\n");
        assert!(
            actual == expected,
            "the CLI changed: update docs/desktop/cli-tree.txt and the matching rows in \
             docs/desktop/desktop-capabilities.json\n----- actual cli tree -----\n{actual}----- end -----"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_recovers_default_directory_without_manifest() {
        let home = std::env::temp_dir().join(format!(
            "lightagent-default-profile-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let store = ProfileStore::new(&home);
        let id = ProfileId::new("default").unwrap();
        let handle = store.scaffold(&id).unwrap();
        std::fs::write(handle.soul_file(), "My existing persona").unwrap();
        std::fs::write(handle.sessions_dir().join("keep.txt"), "existing session").unwrap();

        assert_eq!(
            ensure_default_profile(&store, &Config::default()).unwrap(),
            id
        );
        assert_eq!(store.load(&id).unwrap().persona, "My existing persona");
        assert_eq!(store.active().unwrap(), Some(id));
        assert_eq!(
            std::fs::read_to_string(handle.sessions_dir().join("keep.txt")).unwrap(),
            "existing session"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn legacy_resolution_provisions_missing_role_profiles_without_overwriting_them() {
        let home = std::env::temp_dir().join(format!(
            "lightagent-subagent-profiles-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let store = ProfileStore::new(&home);
        let mut config = Config::default();
        config.subagents.resolution = lightagent_core::WorkerResolution::LegacyProfiles;
        let lead = default_profile(&config).unwrap();

        ensure_subagent_profiles(&store, &config, &lead).unwrap();
        let researcher = ProfileId::new("researcher").unwrap();
        assert!(
            store
                .load(&researcher)
                .unwrap()
                .persona
                .contains("research specialist")
        );
        assert!(is_generated_subagent_profile(
            &store.load(&researcher).unwrap(),
            "researcher"
        ));

        let mut customized = store.load(&researcher).unwrap();
        customized.persona = "Keep my custom researcher persona.".to_owned();
        store.save(&customized).unwrap();
        ensure_subagent_profiles(&store, &config, &lead).unwrap();
        assert_eq!(store.load(&researcher).unwrap().persona, customized.persona);
        assert!(!is_generated_subagent_profile(&customized, "researcher"));

        std::fs::remove_dir_all(home).unwrap();
    }

    /// Gap 1 (closed): built-in workers are not cloned into ordinary
    /// profiles named after their roles.
    #[test]
    fn seeded_built_in_workers_create_no_profiles() {
        let home = std::env::temp_dir().join(format!(
            "lightagent-subagent-builtins-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let store = ProfileStore::new(&home);
        let config = Config::default();
        let lead = default_profile(&config).unwrap();
        ensure_subagent_profiles(&store, &config, &lead).unwrap();
        let created = config
            .subagents
            .allowed_roles
            .iter()
            .filter(|role| store.load(&ProfileId::new(role.as_str()).unwrap()).is_ok())
            .count();
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(created, 0, "built-in roles were cloned into profiles");
    }

    #[test]
    fn singular_profile_command_and_plural_alias_parse() {
        for command in ["profile", "profiles"] {
            let cli = Cli::try_parse_from(["lightagent", command, "use", "aria"])
                .expect("profile command");
            assert!(matches!(
                cli.command,
                Some(Command::Profiles {
                    action: Some(ProfilesAction::Use { id })
                }) if id == "aria"
            ));
        }

        let cli = Cli::try_parse_from(["lightagent", "profile", "delete", "aria"])
            .expect("profile delete");
        assert!(matches!(
            cli.command,
            Some(Command::Profiles {
                action: Some(ProfilesAction::Delete { id })
            }) if id == "aria"
        ));
    }

    #[test]
    fn known_config_keys_round_trip() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "inference.base_url").as_deref(),
            Some("http://127.0.0.1:11434")
        );
        set_key(&mut config, "inference.model", "demo").unwrap();
        assert_eq!(get_key(&config, "inference.model").as_deref(), Some("demo"));

        set_key(&mut config, "inference.api_key", "LIGHTAGENT_PROVIDER_KEY").unwrap();
        assert_eq!(
            get_key(&config, "inference.api_key").as_deref(),
            Some("${env:LIGHTAGENT_PROVIDER_KEY}")
        );
        set_key(&mut config, "inference.api_key", "").unwrap();
        assert_eq!(get_key(&config, "inference.api_key").as_deref(), Some(""));

        set_key(&mut config, "rag.realtime_enabled", "false").unwrap();
        assert_eq!(
            get_key(&config, "rag.realtime_enabled").as_deref(),
            Some("false")
        );

        set_key(&mut config, "tui.show_reasoning", "false").unwrap();
        assert_eq!(
            get_key(&config, "tui.show_reasoning").as_deref(),
            Some("false")
        );

        set_key(&mut config, "memory.auto_capture", "false").unwrap();
        set_key(&mut config, "memory.inject_recent", "0").unwrap();
        assert_eq!(
            get_key(&config, "memory.auto_capture").as_deref(),
            Some("false")
        );
        assert_eq!(
            get_key(&config, "memory.inject_recent").as_deref(),
            Some("0")
        );
        assert!(set_key(&mut config, "memory.top_k", "0").is_err());
    }

    #[test]
    fn agent_limit_keys_round_trip() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "agent.wall_clock_secs").as_deref(),
            Some("300")
        );
        set_key(&mut config, "agent.wall_clock_secs", "900").unwrap();
        assert_eq!(config.agent.wall_clock_secs, Some(900));
        set_key(&mut config, "agent.wall_clock_secs", "none").unwrap();
        assert_eq!(config.agent.wall_clock_secs, None);
        assert_eq!(
            get_key(&config, "agent.wall_clock_secs").as_deref(),
            Some("")
        );

        set_key(&mut config, "agent.max_turns", "12").unwrap();
        assert_eq!(get_key(&config, "agent.max_turns").as_deref(), Some("12"));
        assert!(set_key(&mut config, "agent.max_tool_calls", "many").is_err());
    }

    #[test]
    fn model_aliases_round_trip() {
        let mut config = Config::default();
        let key = "inference.model_alias.qwen3-1.7b-q4_k_m";
        set_key(&mut config, key, "Quinn").unwrap();
        assert_eq!(get_key(&config, key).as_deref(), Some("Quinn"));
        set_key(&mut config, key, "").unwrap();
        assert!(get_key(&config, key).is_none());
        assert!(set_key(&mut config, "inference.model_alias.", "x").is_err());
    }

    #[test]
    fn model_catalog_entries_round_trip() {
        let mut config = Config::default();
        let key = "inference.model_catalog.qwen3-1.7b-q4_k_m";
        set_key(&mut config, key, "Quinn").unwrap();
        assert_eq!(get_key(&config, key).as_deref(), Some("Quinn"));
        set_key(&mut config, key, "").unwrap();
        assert!(get_key(&config, key).is_none());
        assert!(set_key(&mut config, "inference.model_catalog.", "x").is_err());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let mut config = Config::default();
        assert!(get_key(&config, "nope").is_none());
        assert!(set_key(&mut config, "nope", "x").is_err());
    }

    #[test]
    fn runtime_config_keys_round_trip() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "runtime.preferred_device").as_deref(),
            Some("auto")
        );
        // An optional numeric key reads as absent until set, then clears again.
        assert!(get_key(&config, "runtime.n_ctx").is_none());
        set_key(&mut config, "runtime.preferred_device", "cuda").unwrap();
        set_key(&mut config, "runtime.n_ctx", "8192").unwrap();
        assert_eq!(get_key(&config, "runtime.n_ctx").as_deref(), Some("8192"));
        set_key(&mut config, "runtime.n_ctx", "").unwrap();
        assert!(get_key(&config, "runtime.n_ctx").is_none());
        for key in [
            "generation.max_output_tokens",
            "generation.reserve_answer_tokens",
            "generation.context_window",
            "generation.max_run_output_tokens",
            "generation.unknown_capacity_turn_tokens",
            "generation.continuations",
        ] {
            assert!(get_key(&config, key).is_none());
            set_key(&mut config, key, "2048").unwrap();
            assert_eq!(get_key(&config, key).as_deref(), Some("2048"));
            set_key(&mut config, key, "").unwrap();
            assert!(get_key(&config, key).is_none());
        }
        assert_eq!(
            get_key(&config, "generation.adaptive_allocation").as_deref(),
            Some("off")
        );
        for mode in ["plan", "jev", "off"] {
            set_key(&mut config, "generation.adaptive_allocation", mode).unwrap();
            assert_eq!(
                get_key(&config, "generation.adaptive_allocation").as_deref(),
                Some(mode)
            );
        }
        assert!(set_key(&mut config, "generation.adaptive_allocation", "always").is_err());
        set_key(&mut config, "generation.reasoning_effort", "low").unwrap();
        assert_eq!(
            get_key(&config, "generation.reasoning_effort").as_deref(),
            Some("low")
        );
        // A non-numeric context is refused.
        assert!(set_key(&mut config, "runtime.threads", "lots").is_err());
    }

    #[test]
    fn first_line_takes_only_the_first() {
        assert_eq!(first_line("a\nb\nc"), "a");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn specialist_selection_is_off_until_set_explicitly() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "subagents.selection").as_deref(),
            Some("off")
        );
        set_key(&mut config, "subagents.selection", "active").unwrap();
        set_key(&mut config, "subagents.selection_profiles", "lead, other").unwrap();
        assert_eq!(
            get_key(&config, "subagents.selection_profiles").as_deref(),
            Some("lead,other")
        );
        assert!(config.subagents.selection_active_for("lead"));
        assert!(!config.subagents.selection_active_for("third"));
        // Selection never turns classification on by itself.
        assert_eq!(
            config.platform.jev.classification.mode,
            lightagent_core::JevClassificationMode::Off
        );
        assert!(set_key(&mut config, "subagents.selection", "always").is_err());
        config.subagents.selection_max_workers = 4;
        assert!(config.validate().is_err(), "never all four");
        config.subagents.selection_max_workers = 3;
        config.subagents.selection_threshold_percent = 30;
        assert!(config.validate().is_err());
    }

    #[test]
    fn classification_keys_are_separate_from_routing_keys() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "platform.jev.classification.mode").as_deref(),
            Some("off")
        );
        set_key(&mut config, "platform.jev.enabled", "true").unwrap();
        set_key(
            &mut config,
            "platform.jev.base_url",
            "https://api.typesafe.ai",
        )
        .unwrap();
        // Enabling the endpoint does not enable classification.
        assert_eq!(
            config.platform.jev.classification.mode,
            lightagent_core::JevClassificationMode::Off
        );
        set_key(&mut config, "platform.jev.classification.mode", "Shadow").unwrap();
        set_key(
            &mut config,
            "platform.jev.classification.allowed_profiles",
            "default, research",
        )
        .unwrap();
        set_key(
            &mut config,
            "platform.jev.classification.version_thresholds",
            "jev-1.13.0=0.8",
        )
        .unwrap();
        set_key(
            &mut config,
            "platform.jev.classification.max_input_chars",
            "300",
        )
        .unwrap();
        assert_eq!(
            get_key(&config, "platform.jev.classification.mode").as_deref(),
            Some("shadow")
        );
        assert_eq!(
            get_key(&config, "platform.jev.classification.allowed_profiles").as_deref(),
            Some("default,research")
        );
        assert_eq!(
            get_key(&config, "platform.jev.classification.version_thresholds").as_deref(),
            Some("jev-1.13.0=0.8")
        );
        assert!(config.validate().is_ok());
        // Routing settings are untouched by classification settings.
        assert!(config.platform.jev.allowed_models.is_empty());
        assert!(config.platform.jev.allowed_profiles.is_empty());
        assert!(set_key(&mut config, "platform.jev.classification.mode", "always").is_err());
        assert!(
            set_key(
                &mut config,
                "platform.jev.classification.version_thresholds",
                "jev-1.13.0"
            )
            .is_err()
        );
        set_key(&mut config, "platform.jev.routing_input", "legacy_raw").unwrap();
        assert_eq!(
            get_key(&config, "platform.jev.routing_input").as_deref(),
            Some("legacy_raw")
        );
        assert!(set_key(&mut config, "platform.jev.routing_input", "raw").is_err());
        set_key(
            &mut config,
            "platform.jev.classification.workflow_guidance",
            "true",
        )
        .unwrap();
        assert!(config.platform.jev.classification.workflow_guidance);
        set_key(
            &mut config,
            "platform.jev.classification.version_thresholds",
            "",
        )
        .unwrap();
        assert!(
            config
                .platform
                .jev
                .classification
                .version_thresholds
                .is_empty()
        );
    }

    #[test]
    fn routing_permission_keys_round_trip_and_reject_invalid_values() {
        let mut config = Config::default();
        assert_eq!(
            get_key(&config, "platform.jev.routing_permission").as_deref(),
            Some("strict")
        );
        assert_eq!(
            get_key(&config, "platform.jev.routing_allowed_profiles").as_deref(),
            Some("")
        );
        set_key(
            &mut config,
            "platform.jev.routing_allowed_profiles",
            " lead , research ,",
        )
        .unwrap();
        assert_eq!(
            config.platform.jev.routing_allowed_profiles,
            ["lead", "research"]
        );
        set_key(
            &mut config,
            "platform.jev.routing_permission",
            "legacy_all_profiles",
        )
        .unwrap();
        assert_eq!(
            get_key(&config, "platform.jev.routing_permission").as_deref(),
            Some("legacy_all_profiles")
        );
        for invalid in ["all", "", "true", "allow"] {
            assert!(
                set_key(&mut config, "platform.jev.routing_permission", invalid).is_err(),
                "{invalid:?}"
            );
        }
        assert_eq!(
            config.platform.jev.routing_permission,
            lightagent_core::JevRoutingPermission::LegacyAllProfiles,
            "a refused value leaves the policy as it was"
        );
        set_key(
            &mut config,
            "platform.jev.routing_allowed_profiles",
            "Bad Id!",
        )
        .unwrap();
        assert!(
            config.validate().is_err(),
            "an invalid id is refused on save"
        );
        // Clearing the list revokes every profile under strict.
        set_key(&mut config, "platform.jev.routing_allowed_profiles", "").unwrap();
        set_key(&mut config, "platform.jev.routing_permission", "strict").unwrap();
        assert!(config.validate().is_ok());
        assert!(!config.platform.jev.routing_grant("lead").permits());
    }

    /// The dry-run report over a synthetic installation: an existing routing
    /// configuration under the compatibility policy, three profiles on disk,
    /// one of them granted ahead of the switch.
    fn synthetic_routing_installation() -> (Config, Vec<ProfileId>) {
        let config: Config = serde_json::from_str(
            r#"{"platform":{"jev":{"enabled":true,"base_url":"https://jev.invalid","allowed_models":["fast"],"routing_input":"legacy_raw","routing_allowed_profiles":["lead","retired"]}}}"#,
        )
        .unwrap();
        let profiles = ["lead", "research", "sensitive"]
            .map(|id| ProfileId::new(id).unwrap())
            .to_vec();
        (config, profiles)
    }

    #[test]
    fn routing_report_lists_who_strict_would_deny_and_changes_nothing() {
        let (config, profiles) = synthetic_routing_installation();
        let before = config.clone();
        let report = jev_routing_report(&config, &profiles);
        assert_eq!(config, before, "a dry run changes nothing");
        assert_eq!(report["changes_made"], false);
        assert_eq!(report["compatibility"], true);
        assert_eq!(report["permission_policy"], "legacy_all_profiles");
        assert_eq!(
            report["denied_under_strict"],
            serde_json::json!(["default", "research", "sensitive"])
        );
        assert_eq!(
            report["allowed_profiles_not_found"],
            serde_json::json!(["retired"])
        );
        assert_eq!(
            jev_routing_report_text(&report),
            "Jev model-routing permission (dry run; nothing was changed)\n\
             \n\
             Policy: legacy_all_profiles (compatibility: every profile may send task content for routing)\n\
             Routing active: yes\n\
             Routing input: legacy_raw\n\
             \n\
             Profile                        Now            Under strict\n\
             default                        compatibility  denied\n\
             lead                           compatibility  allowed\n\
             research                       compatibility  denied\n\
             sensitive                      compatibility  denied\n\
             \n\
             Listed but not found on disk: retired\n\
             \n\
             Denied under strict: default, research, sensitive\n\
             \n\
             To enforce, grant the profiles that should keep routing, then switch the policy:\n  \
             lightagent config set platform.jev.routing_allowed_profiles <profile,...>\n  \
             lightagent config set platform.jev.routing_permission strict\n"
        );
        // Only profile ids and fixed vocabulary: no endpoint or key.
        assert!(!report.to_string().contains("jev.invalid"));

        // After the operator switches, the report shows enforcement.
        let mut strict = config.clone();
        strict.platform.jev.routing_permission = lightagent_core::JevRoutingPermission::Strict;
        let report = jev_routing_report(&strict, &profiles);
        assert_eq!(report["compatibility"], false);
        assert_eq!(report["profiles"][1]["now"], "allowed");
        assert_eq!(report["profiles"][2]["now"], "denied");
        assert!(!jev_routing_report_text(&report).contains("To enforce"));
    }
}
