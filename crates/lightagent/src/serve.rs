//! `lightagent serve` — the HTTP API over the one runtime.
//!
//! Wires the Lightweight provider and the bounded tool executor into the
//! transport-agnostic [`lightagent_api`] server through a [`RunFactory`], so the
//! API crate never learns a transport. Loopback binds are open (the network is
//! the boundary); a non-loopback bind refuses to start without a key.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_api::manager::{
    self, ProviderCapabilities, RunFactory, RunManager, RunStatus, RuntimeModel, StartRun,
};
use lightagent_api::{AppState, AuthConfig, Scope, router};
use lightagent_core::{
    AgentEvent, AgentEventSink, AgentLoop, ApprovalDecision, ConfigStore, LightagentPaths,
    PolicyEngine, ProfileStore, RunId, StopReason,
};
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};
use lightagent_runtime::{RuntimeClient, RuntimeEndpoint};
use lightagent_store::SessionStore;
use lightagent_tools::{BoundedExecutor, Delegation, SkillContext, ToolDefinition, ToolRegistry};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::chat::{
    LightweightFactory, configured_model, configured_registry, load_extensions, load_skills,
    open_terminal_context, resolve_profile, web_context, web_research_instructions,
    workspace_context,
};

/// Builds and drives a real run with the Lightweight provider per request.
struct LightweightRunFactory {
    root: PathBuf,
}

async fn inference_for_request(
    config: &lightagent_core::Config,
    store: &ProfileStore,
    profile: &lightagent_core::AgentProfile,
    request: &StartRun,
) -> (String, String) {
    let base_url = profile
        .routing
        .base_url
        .clone()
        .unwrap_or_else(|| config.inference.base_url.clone());
    let default_model = request
        .model
        .clone()
        .unwrap_or_else(|| configured_model(&profile.routing.model, config));
    // The saved session's profile identifies context, not a model override.
    let route = crate::jev::select_route(
        &config.platform.jev,
        &request.message,
        default_model.clone(),
        request.model.is_some(),
    )
    .await;
    let routed_profile = route
        .profile
        .as_ref()
        .and_then(|name| resolve_profile(store, config, Some(name.clone())).ok());
    crate::jev::inference_route(
        route,
        routed_profile.as_ref(),
        config,
        default_model,
        base_url,
    )
}

#[async_trait]
impl RunFactory for LightweightRunFactory {
    async fn tools(&self) -> Result<Vec<ToolDefinition>, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths).load().map_err(|e| e.to_string())?;
        let profiles = ProfileStore::new(&self.root);
        let profile = resolve_profile(&profiles, &config, None)?;
        let profile_dir = profiles.handle(&profile.id).dir().to_path_buf();
        let extensions = load_extensions(&self.root, &profile_dir);
        let skills = load_skills(&self.root, &profile_dir, &extensions, &config);
        let registry =
            configured_registry(&config, &profile_dir, &extensions, !skills.is_empty()).await;
        Ok(registry
            .names()
            .iter()
            .filter_map(|name| registry.get(name).map(|tool| tool.definition().clone()))
            .collect())
    }

    async fn provider_capabilities(&self) -> Result<ProviderCapabilities, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths)
            .load()
            .map_err(|error| error.to_string())?;
        let profiles = ProfileStore::new(&self.root);
        let profile = resolve_profile(&profiles, &config, None)?;
        let base_url = profile
            .routing
            .base_url
            .unwrap_or_else(|| config.inference.base_url.clone());
        let configured_model = configured_model(&profile.routing.model, &config);
        let api_key = config
            .inference
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve());
        let mut provider_config = ProviderConfig::new(base_url.clone(), configured_model.clone());
        if let Some(key) = api_key.clone() {
            provider_config = provider_config.with_api_key(key);
        }
        let models = LightweightProvider::new(provider_config)
            .map_err(|error| error.to_string())?
            .models()
            .await
            .map_err(|error| error.to_string())?;
        // The inference API generally exposes only the resident model. Some
        // local gateways also provide a separate, read-only runtime catalog;
        // surface it when available without making that optional control plane
        // a requirement for ordinary model discovery.
        let mut runtime_endpoint = RuntimeEndpoint::new(base_url.clone());
        if let Some(key) = api_key {
            runtime_endpoint = runtime_endpoint.with_api_key(key);
        }
        let runtime_models = match RuntimeClient::new(runtime_endpoint) {
            Ok(runtime) => runtime
                .catalog()
                .await
                .map(|catalog| {
                    catalog
                        .into_iter()
                        .map(|model| RuntimeModel {
                            id: model.id.clone(),
                            name: config
                                .inference
                                .model_aliases
                                .get(&model.id)
                                .cloned()
                                .or(model.name),
                            state: model.state,
                            supported: model.supported,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        Ok(ProviderCapabilities {
            provider: config.inference.provider,
            base_url,
            configured_model: (!configured_model.eq("default")).then_some(configured_model),
            models,
            model_aliases: config.inference.model_aliases.clone(),
            model_catalog: config.inference.model_catalog.clone(),
            runtime_models,
            streaming: true,
            tool_calls: true,
            reasoning_content: false,
        })
    }

    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = match ConfigStore::at(&paths).load() {
            Ok(config) => config,
            Err(error) => {
                fail(&sink, &format!("could not reload settings: {error}"));
                return RunStatus::Failed;
            }
        };
        let store = ProfileStore::new(&self.root);
        let mut profile = match resolve_profile(&store, &config, request.profile.clone()) {
            Ok(profile) => profile,
            Err(error) => {
                fail(&sink, &error);
                return RunStatus::Failed;
            }
        };
        if let Err(error) = crate::ensure_subagent_profiles(&store, &config, &profile) {
            fail(
                &sink,
                &format!("could not provision subagent profiles: {error}"),
            );
            return RunStatus::Failed;
        }

        let (model, base_url) = inference_for_request(&config, &store, &profile, &request).await;
        let api_key = config
            .inference
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve());

        let mut provider_config = ProviderConfig::new(base_url.clone(), model);
        if let Some(key) = &api_key {
            provider_config = provider_config.with_api_key(key.clone());
        }
        let provider = match LightweightProvider::new(provider_config) {
            Ok(provider) => provider,
            Err(error) => {
                fail(&sink, &error.to_string());
                return RunStatus::Failed;
            }
        };

        // An ACP session's `cwd` becomes the confined workspace root, so the
        // agent edits the editor's project; otherwise the profile's own workspace.
        let workspace_dir = request
            .cwd
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| store.handle(&profile.id).workspace_dir());
        let profile_dir = store.handle(&profile.id).dir().to_path_buf();
        let extensions = load_extensions(&self.root, &profile_dir);
        let skills = load_skills(&self.root, &profile_dir, &extensions, &config);
        let mut delegation = Delegation::new(
            Arc::new(store),
            Arc::new(LightweightFactory { base_url, api_key }),
            ToolRegistry::worker_default(),
            Duration::from_secs(60),
            262_144,
        );
        delegation.delegate_timeout = Duration::from_secs(config.subagents.delegate_timeout_secs);
        delegation.subagents = crate::chat::subagent_policy(&config);
        let registry =
            configured_registry(&config, &profile_dir, &extensions, !skills.is_empty()).await;
        let mut executor = BoundedExecutor::new(
            registry,
            PolicyEngine::new(profile.approval_policy.into()),
            Duration::from_secs(60),
            262_144,
        )
        .with_run(RunId::new())
        .with_delegation(delegation);
        if let Some(web) = web_context(&config) {
            executor = executor.with_web(web);
        }
        if let Some(workspace) = workspace_context(&config, workspace_dir) {
            executor = executor.with_workspace(workspace);
        }
        if let Some(open_terminal) = open_terminal_context(&config) {
            executor = executor.with_open_terminal(open_terminal);
        }
        if !skills.is_empty() {
            profile
                .persona
                .push_str(&format!("\n\n{}", skills.catalog()));
            executor = executor.with_skills(SkillContext { skills });
        }

        let extension_instructions = extensions.instructions(&config.extensions);
        if !extension_instructions.is_empty() {
            profile
                .persona
                .push_str(&format!("\n\n{extension_instructions}"));
        }

        if let Some(instructions) = web_research_instructions(&config) {
            profile.persona.push_str(&format!("\n\n{instructions}"));
        }
        if let Some(instructions) = crate::chat::autonomous_subagent_instructions(&config) {
            profile.persona.push_str(&format!("\n\n{instructions}"));
        }

        match crate::memory::relevant_catalog(&profile_dir, &config, &request.message).await {
            Ok(catalog) if !catalog.is_empty() => {
                profile.persona.push_str(&format!("\n\n{catalog}"))
            }
            Err(error) => {
                fail(&sink, &format!("could not load durable memory: {error}"));
                return RunStatus::Failed;
            }
            _ => {}
        }
        if let Err(error) = crate::memory::capture(&profile_dir, &config, &request.message, None) {
            eprintln!("could not retain durable memory: {error}");
        }
        let agent = AgentLoop::from_profile(provider, executor, &profile);
        manager::drive(
            agent,
            request.history,
            request.message,
            sink,
            cancel,
            decisions,
        )
        .await
    }
}

fn fail(sink: &AgentEventSink, message: &str) {
    let _ = sink.send(AgentEvent::Error {
        message: message.to_owned(),
    });
    let _ = sink.send(AgentEvent::RunCompleted {
        reason: StopReason::Error,
    });
}

/// Build a run manager that resolves extensions and settings for each run —
/// shared by `serve` and `acp`.
pub(crate) async fn build_run_manager() -> Result<RunManager, String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    let factory = Arc::new(LightweightRunFactory {
        root: paths.root().to_path_buf(),
    });
    Ok(RunManager::new(factory))
}

/// Bind and serve the API until interrupted.
pub async fn run(
    host: String,
    port: u16,
    key_env: Option<String>,
    web_root: Option<PathBuf>,
) -> Result<(), String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    let config = ConfigStore::at(&paths)
        .load()
        .map_err(|error| error.to_string())?;
    let store = ProfileStore::new(paths.root());
    let active = store
        .active()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no active profile — run `lightagent init` first".to_string())?;
    let sessions = SessionStore::at_profile(&store.handle(&active));

    let is_loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    let auth = match key_env.and_then(|var| std::env::var(&var).ok()) {
        Some(key) => AuthConfig::keyed(key, [Scope::Admin]),
        None => {
            if !is_loopback {
                return Err(
                    "a non-loopback bind requires --key-env naming a variable holding an API key"
                        .to_string(),
                );
            }
            AuthConfig::open()
        }
    };

    let context_limit = config.runtime.n_ctx.unwrap_or(4_096) as usize;
    let factory = Arc::new(LightweightRunFactory {
        root: paths.root().to_path_buf(),
    });
    let state = AppState {
        manager: RunManager::new(factory),
        auth,
        sessions,
        session_profile: active.as_str().to_owned(),
        context_limit,
        config_store: Some(ConfigStore::at(&paths)),
        busy_sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
        web_root: web_root.clone(),
    };

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|error| format!("could not bind {addr}: {error}"))?;
    let bound = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or(addr);
    println!(
        "Lightagent API listening on http://{bound}/api/lightagent/v1  (profile '{}')",
        active.as_str()
    );
    if is_loopback {
        println!("Loopback bind: no API key required.");
    }
    if let Some(root) = &web_root {
        println!(
            "Serving the panel from {} at http://{bound}/",
            root.display()
        );
    }
    axum::serve(listener, router(state).into_make_service())
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn saved_session_identity_profile_keeps_jev_active_but_explicit_model_bypasses_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            assert!(String::from_utf8_lossy(&bytes[..count]).starts_with("POST /v1/systemone "));
            let body = r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"model:fast","confidence":0.99,"probabilities":{"default":0.01,"model:fast":0.99}}},"usage":{"input_tokens":10,"output_tokens":2}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let mut config = lightagent_core::Config::default();
        config.platform.jev.endpoint.enabled = true;
        config.platform.jev.endpoint.base_url = Some(format!("http://{address}"));
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        let profile = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("identity").unwrap(),
            "Identity",
            "Original persona",
            "original-model",
        );
        let original = profile.clone();
        let store = ProfileStore::new(std::env::temp_dir());
        let mut request = StartRun {
            message: "hello".to_owned(),
            history: vec![],
            profile: Some("identity".to_owned()),
            model: None,
            cwd: None,
        };
        let (model, _) = inference_for_request(&config, &store, &profile, &request).await;
        assert_eq!(model, "fast");
        assert_eq!(profile, original);
        server.await.unwrap();
        request.model = Some("explicit".to_owned());
        let (model, _) = inference_for_request(&config, &store, &profile, &request).await;
        assert_eq!(model, "explicit");
    }
}
