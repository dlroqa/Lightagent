//! `lightagent serve` — the HTTP API over the one runtime.
//!
//! Wires the Lightweight provider and the bounded tool executor into the
//! transport-agnostic [`lightagent_api`] server through a [`RunFactory`], so the
//! API crate never learns a transport. Loopback binds are open (the network is
//! the boundary); a non-loopback bind refuses to start without a key.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use lightagent_api::manager::{
    self, ProviderCapabilities, RunFactory, RunManager, RunStatus, RuntimeModel, StartRun,
};
use lightagent_api::{AppState, AuthConfig, Scope, router};
use lightagent_core::identity::{AuthorizedProvider, RunAuthorization, RunTrust};
use lightagent_core::{
    AgentEvent, AgentEventSink, AgentLoop, ApprovalDecision, ConfigStore, LightagentPaths,
    PolicyEngine, ProfileId, ProfileStore, RunId, StopReason,
};
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};
use lightagent_runtime::{RuntimeClient, RuntimeEndpoint};
use lightagent_store::SessionStore;
use lightagent_tools::{BoundedExecutor, SkillContext, ToolDefinition};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::chat::{
    LightweightFactory, configured_model, configured_registry, load_extensions, load_skills,
    open_terminal_context, resolve_profile, web_research_instructions, workspace_context,
};

/// Builds and drives a real run with the Lightweight provider per request.
struct LightweightRunFactory {
    root: PathBuf,
    /// Jev diagnostics and connection checks, held in memory for this server.
    jev: Arc<crate::coordinator::JevState>,
}

/// The run's pre-dispatch decisions, through the shared coordinator. API and
/// ACP runs both arrive here; an ACP run's `model` is always `None`.
async fn inference_for_request(
    config: &lightagent_core::Config,
    store: &ProfileStore,
    profile: &lightagent_core::AgentProfile,
    request: &StartRun,
    cancel: &CancellationToken,
) -> crate::coordinator::Preflight {
    // The saved session's profile identifies context, not a model override.
    crate::coordinator::preflight(
        crate::coordinator::PreflightRequest {
            config,
            store,
            profile,
            message: &request.message,
            explicit_model: request.model.as_deref(),
            attachment: false,
        },
        cancel,
    )
    .await
}

#[async_trait]
impl RunFactory for LightweightRunFactory {
    async fn profiles(
        &self,
        request: lightagent_api::ops::ProfileRequest,
    ) -> Result<serde_json::Value, lightagent_api::ops::OpError> {
        let paths = LightagentPaths::rooted_at(&self.root);
        crate::ops::Profiles::open(&paths)?.perform(request)
    }
    async fn tools(&self) -> Result<Vec<ToolDefinition>, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths).load().map_err(|e| e.to_string())?;
        let profiles = ProfileStore::new(&self.root);
        let profile = resolve_profile(&profiles, &config, None)?;
        let profile_dir = profiles.handle(&profile.id).dir().to_path_buf();
        let extensions = load_extensions(&self.root, &profile_dir);
        let skills = load_skills(&self.root, &profile_dir, &extensions, &config);
        let mut registry =
            configured_registry(&config, &profile_dir, &extensions, !skills.is_empty()).await;
        crate::coordinator::add_explicit_tool(
            &mut registry,
            &config,
            profile.id.as_str(),
            crate::coordinator::JevBudget::new(
                config.platform.jev.classification.max_calls_per_run,
            ),
            Arc::clone(&self.jev),
        );
        // The catalog shows what a run of the active profile would receive.
        let registry = crate::chat::tool_policy(
            &config,
            &extensions,
            &registry,
            &profile,
            RunTrust::LocalOperator,
        )
        .apply(&registry);
        Ok(registry
            .names()
            .iter()
            .filter_map(|name| registry.get(name).map(|tool| tool.definition().clone()))
            .collect())
    }

    /// What a run of `profile` with `trust` would receive, built exactly as
    /// the run builds it: the profile's remote access, the explicit Jev tool
    /// and the profile's tool policy for that trust.
    async fn tools_for(
        &self,
        profile: &ProfileId,
        trust: RunTrust,
    ) -> Result<Vec<ToolDefinition>, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths).load().map_err(|e| e.to_string())?;
        let profiles = ProfileStore::new(&self.root);
        let profile = resolve_profile(&profiles, &config, Some(profile.as_str().to_owned()))?;
        let profile_dir = profiles.handle(&profile.id).dir().to_path_buf();
        let extensions = load_extensions(&self.root, &profile_dir);
        let skills = load_skills(&self.root, &profile_dir, &extensions, &config);
        let credentials = vault_credentials(&self.root, &profile.id, trust);
        let mut registry = crate::chat::configured_registry_for(
            &config,
            &profile_dir,
            &extensions,
            !skills.is_empty(),
            crate::chat::RemoteAccess::for_run_with(trust, &profile.id, credentials.as_ref()),
        )
        .await;
        crate::coordinator::add_explicit_tool(
            &mut registry,
            &config,
            profile.id.as_str(),
            crate::coordinator::JevBudget::new(
                config.platform.jev.classification.max_calls_per_run,
            ),
            Arc::clone(&self.jev),
        );
        let registry = crate::chat::tool_policy(&config, &extensions, &registry, &profile, trust)
            .apply(&registry);
        Ok(registry
            .names()
            .iter()
            .filter_map(|name| registry.get(name).map(|tool| tool.definition().clone()))
            .collect())
    }

    async fn workers_for(
        &self,
        profile: Option<&ProfileId>,
        trust: RunTrust,
    ) -> Result<serde_json::Value, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths).load().map_err(|e| e.to_string())?;
        let profiles = ProfileStore::new(&self.root);
        let requested = profile.map(|profile| profile.as_str().to_owned());
        let lead = resolve_profile(&profiles, &config, requested)?;
        let profile_dir = profiles.handle(&lead.id).dir().to_path_buf();
        let extensions = load_extensions(&self.root, &profile_dir);
        let skills = load_skills(&self.root, &profile_dir, &extensions, &config);
        let credentials = vault_credentials(&self.root, &lead.id, trust);
        let mut registry = crate::chat::configured_registry_for(
            &config,
            &profile_dir,
            &extensions,
            !skills.is_empty(),
            crate::chat::RemoteAccess::for_run_with(trust, &lead.id, credentials.as_ref()),
        )
        .await;
        // As a run would: the read-only researcher where it is granted.
        if trust == RunTrust::RemoteUser
            && config.subagents.enabled
            && config.subagents.remote_research
            && config.subagents.resolution == lightagent_core::WorkerResolution::Builtin
        {
            registry.insert(Arc::new(lightagent_tools::builtins::AgentResearch::new()));
        }
        let registry = crate::chat::tool_policy(&config, &extensions, &registry, &lead, trust)
            .apply(&registry);
        let report = crate::workers::readiness(&self.root, &config, &lead, trust, &registry);
        serde_json::to_value(report).map_err(|error| error.to_string())
    }

    async fn jev_status_for(&self, profile: &ProfileId) -> Result<serde_json::Value, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths)
            .load()
            .map_err(|error| error.to_string())?;
        Ok(crate::coordinator::user_status(
            &config,
            profile.as_str(),
            &self.jev,
        ))
    }

    async fn jev_status(&self) -> Result<serde_json::Value, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths)
            .load()
            .map_err(|error| error.to_string())?;
        let profile = resolve_profile(&ProfileStore::new(&self.root), &config, None)?;
        Ok(crate::coordinator::status(
            &config,
            profile.id.as_str(),
            &self.jev,
        ))
    }

    async fn verify_jev(&self) -> Result<serde_json::Value, String> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths)
            .load()
            .map_err(|error| error.to_string())?;
        self.jev.verify(&config.platform.jev).await;
        self.jev_status()
            .await
            .map(|status| status["verification"].clone())
    }

    /// Resolved exactly as the run resolves it, for the profile and model
    /// the run would start with; `None` when the configuration or profile
    /// cannot be read, leaving the caller's limit in place.
    async fn history_window(
        &self,
        profile: Option<&str>,
        model: Option<&str>,
    ) -> Option<lightagent_core::HistoryWindow> {
        let paths = LightagentPaths::rooted_at(&self.root);
        let config = ConfigStore::at(&paths).load().ok()?;
        let profiles = ProfileStore::new(&self.root);
        let profile = resolve_profile(&profiles, &config, profile.map(str::to_owned)).ok()?;
        let base_url = profile
            .routing
            .base_url
            .clone()
            .unwrap_or_else(|| config.inference.base_url.clone());
        let model = model
            .map(str::to_owned)
            .unwrap_or_else(|| configured_model(&profile.routing.model, &config));
        let mut provider_config =
            ProviderConfig::new(base_url, model).with_tool_names(config.inference.tool_names);
        if let Some(key) = config
            .inference
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve())
        {
            provider_config = provider_config.with_api_key(key);
        }
        let provider = LightweightProvider::new(provider_config).ok()?;
        Some(crate::generation::resolve(&config, &provider).await.history)
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
        let (runtime_models, reasoning_content) = match RuntimeClient::new(runtime_endpoint) {
            Ok(runtime) => {
                let reasoning_content = runtime
                    .gateway()
                    .await
                    .map(|gateway| gateway.engine_capabilities.reasoning_content)
                    .unwrap_or(false);
                let models = runtime
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
                    .unwrap_or_default();
                (models, reasoning_content)
            }
            Err(_) => (Vec::new(), false),
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
            reasoning_content,
        })
    }

    async fn run(
        &self,
        request: StartRun,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.run_as(request, RunTrust::LocalOperator, sink, cancel, decisions)
            .await
    }

    /// A run for a starter of `trust`. Its tools are the enabled tools
    /// narrowed by the profile's tool policy; an untrusted run is always
    /// strict and never receives unisolated local execution.
    async fn run_as(
        &self,
        request: StartRun,
        trust: RunTrust,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.run_with(request, trust, None, sink, cancel, decisions)
            .await
    }

    /// A revocable run: the same run, with `authorization` re-checked before
    /// any external request (Jev preflight included), every model turn,
    /// every tool call, remote request and delegated worker, and every
    /// approval decision.
    async fn run_supervised(
        &self,
        request: StartRun,
        trust: RunTrust,
        authorization: RunAuthorization,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        self.run_with(request, trust, Some(authorization), sink, cancel, decisions)
            .await
    }
}

impl LightweightRunFactory {
    /// The run itself. `authorization` is `None` for a run that is not
    /// revocable, which then behaves exactly as it always has.
    async fn run_with(
        &self,
        request: StartRun,
        trust: RunTrust,
        authorization: Option<RunAuthorization>,
        sink: AgentEventSink,
        cancel: CancellationToken,
        decisions: UnboundedReceiver<ApprovalDecision>,
    ) -> RunStatus {
        if let Some(authorization) = &authorization
            && let Err(denial) = authorization.verify()
        {
            let _ = sink.send(AgentEvent::Error {
                message: lightagent_core::identity::revoked_message(denial),
            });
            let _ = sink.send(AgentEvent::RunCompleted {
                reason: StopReason::Cancelled,
            });
            return RunStatus::Cancelled;
        }
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

        let preflight = inference_for_request(&config, &store, &profile, &request, &cancel).await;
        let (model, base_url) = (preflight.model.clone(), preflight.base_url.clone());
        if let Some((model, kept_default)) = preflight.routing_status.clone() {
            let _ = sink.send(AgentEvent::RouteSelected {
                source: "Jev".to_owned(),
                model,
                kept_default,
                permission: preflight
                    .routing_permission
                    .map(|permission| permission.as_str().to_owned()),
                fallback: preflight.routing_fallback.map(str::to_owned),
            });
        }
        let api_key = config
            .inference
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve());

        let mut provider_config = ProviderConfig::new(base_url.clone(), model)
            .with_tool_names(config.inference.tool_names);
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
        // The window the model is served with, as the provider reports it;
        // the lead and every worker it delegates to are budgeted against it.
        let generation = crate::generation::resolve(&config, &provider).await.lead;

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
        // An untrusted run's remote credentials: the profile's own vault
        // grants once the installation has a vault.
        let credentials = vault_credentials(&self.root, &profile.id, trust);
        let mut registry = crate::chat::configured_registry_for(
            &config,
            &profile_dir,
            &extensions,
            !skills.is_empty(),
            crate::chat::RemoteAccess::for_run_with(trust, &profile.id, credentials.as_ref()),
        )
        .await;
        // One Jev budget per run covers the preflight and every explicit call.
        let budget = crate::coordinator::JevBudget::new(
            config.platform.jev.classification.max_calls_per_run,
        );
        budget.record(preflight.jev_calls);
        crate::coordinator::add_explicit_tool(
            &mut registry,
            &config,
            profile.id.as_str(),
            budget,
            Arc::clone(&self.jev),
        );
        // An untrusted run may be offered the read-only researcher, when the
        // operator turned it on; its strict policy still requires the profile
        // to list it. Generic delegation stays withheld from it.
        if trust == RunTrust::RemoteUser
            && config.subagents.enabled
            && config.subagents.remote_research
            && config.subagents.resolution == lightagent_core::WorkerResolution::Builtin
        {
            registry.insert(Arc::new(lightagent_tools::builtins::AgentResearch::new()));
        }
        // The profile's tool policy narrows the enabled tools before anything
        // sees them: the model, the workflow policy, the executor and any
        // delegated worker. It never adds a tool or relaxes an approval.
        let tool_policy =
            crate::chat::tool_policy(&config, &extensions, &registry, &profile, trust);
        if let Some(line) = tool_policy.summary(profile.id.as_str()) {
            eprintln!("· {line}");
        }
        let registry = tool_policy.apply(&registry);
        let registry_snapshot = registry.clone();
        let guard = tool_policy.enforced.then(|| {
            crate::chat::tool_policy_guard(
                self.root.clone(),
                profile.id.clone(),
                trust,
                registry_snapshot,
            )
        });
        // Delegated workers draw from these same effective tools, under the
        // same guard, through the one worker-runtime factory.
        let delegation = crate::workers::delegation(crate::workers::DelegationInputs {
            root: &self.root,
            config: &config,
            lead: &profile,
            trust,
            extensions: &extensions,
            registry: &registry,
            guard: guard.clone(),
            enforced: tool_policy.enforced,
            factory: Arc::new(LightweightFactory {
                base_url,
                api_key,
                tool_names: config.inference.tool_names,
            }),
        });
        delegation.generation.set(Some(generation.clone()));
        // Classification only advises. The workflow policy inspects the
        // registry, the approval policy and the limits without changing them;
        // the executor below is built exactly as without Jev.
        let mut planned = None;
        if let Some(report) = preflight.classification.as_ref() {
            let selection = std::time::Instant::now();
            let inspection = PolicyEngine::new(profile.approval_policy.into());
            let decision = crate::coordinator::workflow::evaluate(
                &crate::coordinator::workflow::WorkflowInputs {
                    report,
                    registry: &registry,
                    policy: &inspection,
                    limits: &profile.limits,
                    config: &config,
                    cancelled: cancel.is_cancelled(),
                },
            );
            let selection = selection.elapsed();
            if decision.applied
                && let Some(guidance) = decision.guidance
            {
                profile.persona.push_str(&format!("\n\n{guidance}"));
            }
            // Specialist selection: decided from the same classification and
            // the requesting profile's own role readiness for these tools.
            // Only an applied plan reaches the lead, as fixed text, and it
            // only orders the lead's delegations.
            let specialists = crate::coordinator::selection::requested(&config).then(|| {
                crate::workers::readiness(&self.root, &config, &profile, trust, &registry)
            });
            let specialists = specialists.and_then(|readiness| {
                crate::coordinator::selection::decide(
                    &crate::coordinator::selection::SelectionInputs {
                        config: &config,
                        profile: profile.id.as_str(),
                        report,
                        readiness: &readiness,
                        registry: &registry,
                        policy: &inspection,
                        limits: &profile.limits,
                        trust,
                        cancelled: cancel.is_cancelled(),
                    },
                )
            });
            if let Some(chosen) = &specialists
                && let (Some(instruction), Some(order)) = (&chosen.instruction, &chosen.order)
            {
                profile.persona.push_str(&format!("\n\n{instruction}"));
                delegation.plan.set(Some(order.clone()));
            }
            planned.clone_from(&specialists);
            if let Some(mut diagnostics) =
                crate::coordinator::diagnostics(&preflight, decision.applied)
            {
                crate::coordinator::annotate(
                    &mut diagnostics,
                    &decision,
                    selection,
                    preflight.routing_input,
                );
                if let Some(chosen) = &specialists {
                    crate::coordinator::annotate_selection(&mut diagnostics, chosen);
                }
                self.jev.record(profile.id.as_str(), diagnostics.clone());
                let _ = sink.send(AgentEvent::TaskClassified {
                    diagnostics: Box::new(diagnostics),
                });
            }
        }
        // Opted in, the run's output pool is divided by its plan before the
        // lead or any worker reserves from it.
        if let Some(line) = crate::coordinator::allocation::install(
            &config,
            preflight.classification.as_ref(),
            planned.as_ref(),
            &generation.budget,
        ) {
            eprintln!("{line}");
        }
        let mut executor = BoundedExecutor::new(
            registry,
            PolicyEngine::new(profile.approval_policy.into()),
            lightagent_tools::DEFAULT_PER_CALL_TIMEOUT,
            262_144,
        )
        .with_run(RunId::new())
        .with_delegation(delegation);
        if let Some(authorization) = &authorization {
            executor = executor.with_authorization(authorization.clone());
        }
        if let Some(guard) = guard {
            executor = executor.with_call_guard(guard);
        }
        if let Some(web) = crate::chat::web_context_for(
            &config,
            crate::chat::RemoteAccess::for_run_with(trust, &profile.id, credentials.as_ref()),
        ) {
            executor = executor.with_web(web);
        }
        if let Some(workspace) = workspace_context(&config, workspace_dir) {
            executor = executor.with_workspace(workspace);
        }
        if let Some(open_terminal) = open_terminal_context(&config) {
            executor = executor.with_open_terminal(open_terminal);
        }
        if let Some(actions) = crate::chat::actions_context(&config) {
            executor = executor.with_actions(actions);
        }
        executor = executor.with_patches(lightagent_tools::builtins::PatchContext::default());
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
        let authorization =
            authorization.unwrap_or_else(|| RunAuthorization::unconditional(cancel.clone()));
        let provider = AuthorizedProvider::new(provider, authorization.clone());
        let agent = AgentLoop::from_profile(provider, executor, &profile)
            .with_generation(generation.budget);
        manager::drive_supervised(
            agent,
            request.history,
            request.message,
            sink,
            cancel,
            decisions,
            authorization,
        )
        .await
    }
}

/// The vault credentials of an untrusted run of `profile`; `None` for a
/// trusted run or an installation without a vault.
fn vault_credentials(
    root: &std::path::Path,
    profile: &ProfileId,
    trust: RunTrust,
) -> Option<crate::chat::ProfileCredentials> {
    (trust == RunTrust::RemoteUser)
        .then(|| crate::chat::ProfileCredentials::load(root, profile))
        .flatten()
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
        jev: crate::coordinator::JevState::new(),
    });
    Ok(RunManager::new(factory))
}

/// The protocol `serve --desktop` speaks to the desktop host
/// (docs/desktop/contract/operations.json).
const DESKTOP_PROTOCOL: u32 = 1;

/// `serve --desktop`: the desktop app's owned runtime
/// (docs/desktop/DESKTOP_ARCHITECTURE.md §4, §6).
///
/// The first line of standard input is the per-launch key; the rest of
/// standard input is a lifeline the desktop host holds open for as long as it
/// lives, so the runtime exits however the host ends. Once bound, one JSON
/// handshake line goes to stdout, and every request must then pass the
/// desktop guard: the exact Host, no Origin, and the key.
pub async fn run_desktop(host: String, port: u16, profile: Option<String>) -> Result<(), String> {
    if host != "127.0.0.1" {
        return Err("--desktop binds 127.0.0.1 only".to_string());
    }
    let key = read_desktop_key()?;
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    paths.scaffold().map_err(|error| error.to_string())?;
    let config = ConfigStore::at(&paths)
        .load()
        .map_err(|error| error.to_string())?;
    let store = ProfileStore::new(paths.root());
    crate::ensure_default_profile(&store, &config)?;
    let active = match profile {
        Some(id) => {
            let id = ProfileId::new(id).map_err(|error| error.to_string())?;
            store.load(&id).map_err(|error| error.to_string())?;
            id
        }
        None => store
            .active()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "no active profile".to_string())?,
    };
    let sessions = SessionStore::at_profile(&store.handle(&active));
    let context_limit = config.runtime.n_ctx.unwrap_or(4_096) as usize;
    let factory = Arc::new(LightweightRunFactory {
        root: paths.root().to_path_buf(),
        jev: crate::coordinator::JevState::new(),
    });
    let state = AppState {
        manager: RunManager::new(factory),
        auth: AuthConfig::keyed(key.clone(), [Scope::Admin]),
        sessions,
        session_profile: active.as_str().to_owned(),
        context_limit,
        config_store: Some(ConfigStore::at(&paths)),
        busy_sessions: Arc::new(tokio::sync::Mutex::new(Default::default())),
        web_root: None,
    };

    let listener = TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|error| format!("could not bind {host}:{port}: {error}"))?;
    let bound = listener.local_addr().map_err(|error| error.to_string())?;
    let guard = lightagent_api::desktop::DesktopGuard::new(bound, &key);
    let app = router(state).layer(axum::middleware::from_fn_with_state(
        guard,
        lightagent_api::desktop::guard,
    ));

    let handshake = serde_json::json!({
        "lightagent": "serve",
        "protocol": DESKTOP_PROTOCOL,
        "version": crate::VERSION,
        "addr": bound.to_string(),
        "profile": active.as_str(),
        "capabilities": [
            "runs", "sessions", "approvals", "settings", "tools", "skills", "workers", "provider", "jev"
        ],
    });
    {
        use std::io::Write as _;
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{handshake}")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("could not report ready: {error}"))?;
    }
    eprintln!(
        "Lightagent desktop runtime on {bound} (profile '{}')",
        active.as_str()
    );

    let lifeline = CancellationToken::new();
    {
        let lifeline = lifeline.clone();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
            lifeline.cancel();
            // Let in-flight responses finish briefly, then go regardless:
            // the host that would read them is gone.
            std::thread::sleep(std::time::Duration::from_secs(5));
            std::process::exit(0);
        });
    }
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move { lifeline.cancelled().await })
        .await
        .map_err(|error| error.to_string())
}

/// The launch key: the first line of standard input, never an argument or an
/// environment variable, where other processes could read it.
fn read_desktop_key() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("could not read the desktop key ({})", error.kind()))?;
    let key = line.trim();
    if key.len() < 32 || !key.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(
            "--desktop needs the launch key as the first line of standard input".to_string(),
        );
    }
    Ok(key.to_owned())
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
        jev: crate::coordinator::JevState::new(),
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
pub(crate) mod routing_tests {
    use super::*;
    use std::time::Duration;
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
        config.platform.jev.routing_allowed_profiles = vec!["identity".to_owned()];
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
        let crate::coordinator::Preflight {
            model,
            routing_status: status,
            ..
        } = inference_for_request(
            &config,
            &store,
            &profile,
            &request,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(model, "fast");
        assert_eq!(status, Some(("fast".to_owned(), false)));
        assert_eq!(profile, original);
        server.await.unwrap();
        request.model = Some("explicit".to_owned());
        let crate::coordinator::Preflight {
            model,
            routing_status: status,
            ..
        } = inference_for_request(
            &config,
            &store,
            &profile,
            &request,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(model, "explicit");
        assert_eq!(status, None);
    }

    #[tokio::test]
    async fn a_cancelled_run_keeps_its_configured_model_without_contacting_jev() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut config = lightagent_core::Config::default();
        config.platform.jev.endpoint.enabled = true;
        config.platform.jev.endpoint.base_url = Some(format!("http://{address}"));
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        config.platform.jev.routing_allowed_profiles = vec!["identity".to_owned()];
        let profile = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("identity").unwrap(),
            "Identity",
            "Original persona",
            "original-model",
        );
        let store = ProfileStore::new(std::env::temp_dir());
        let request = StartRun {
            message: "hello".to_owned(),
            history: vec![],
            profile: None,
            model: None,
            cwd: None,
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let crate::coordinator::Preflight {
            model,
            routing_status: status,
            ..
        } = inference_for_request(&config, &store, &profile, &request, &cancel).await;
        assert_eq!(model, "original-model");
        // The existing `routing.selected` contract is unchanged: Jev was
        // enabled and consulted for this run, and the default was kept.
        assert_eq!(status, Some(("original-model".to_owned(), true)));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "a cancelled run must not reach Jev"
        );
    }

    /// An OpenAI-compatible gateway advertising Lightweight's `Auto` beside a
    /// concrete model, recording each chat-completion body it receives.
    pub(crate) async fn gateway_stub() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let chats = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&chats);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let log = std::sync::Arc::clone(&log);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buf = [0; 8192];
                    while let Ok(count) = stream.read(&mut buf).await {
                        if count == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buf[..count]);
                        let Some(offset) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&bytes[..offset]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if bytes.len() >= offset + 4 + length {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes).into_owned();
                    let response = if request.starts_with("GET /v1/models") {
                        let body = r#"{"object":"list","data":[{"id":"Auto"},{"id":"fast"}]}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    } else if request.starts_with("GET ") {
                        // Any other probe (`/v1/capabilities`, say) finds
                        // an older gateway that does not serve it.
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_owned()
                    } else {
                        let body = request
                            .split_once("\r\n\r\n")
                            .map(|(_, body)| body.to_owned())
                            .unwrap_or_default();
                        log.lock().unwrap().push(body);
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
                         data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n\
                         data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                         data: [DONE]\n\n"
                            .to_owned()
                    };
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (format!("http://{address}"), chats)
    }

    #[tokio::test]
    async fn api_and_acp_runs_send_auto_to_the_provider_unchanged() {
        use crate::coordinator::test_support::{answer, jev_stub};
        let (gateway, chats) = gateway_stub().await;
        let (jev, jev_seen) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let message = "find recent papers on batteries";
        let scratch = std::env::temp_dir().join(format!(
            "lightagent-auto-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = scratch.join("acp-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        // An API run has no cwd; an ACP session supplies its editor's cwd.
        for (label, cwd) in [
            ("api", None),
            ("acp", Some(workspace.to_string_lossy().into_owned())),
        ] {
            let root = scratch.join(label);
            let mut config = lightagent_core::Config::default();
            config.inference.base_url = gateway.clone();
            config.memory.inject_recent = 0;
            config.platform.jev.endpoint.enabled = true;
            config.platform.jev.endpoint.base_url = Some(jev.clone());
            config.platform.jev.allowed_models = vec!["fast".to_owned()];
            config.platform.jev.classification.mode =
                lightagent_core::JevClassificationMode::Shadow;
            config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
            ConfigStore::at(&LightagentPaths::rooted_at(&root))
                .save(&config)
                .unwrap();
            let store = ProfileStore::new(&root);
            let lead = lightagent_core::AgentProfile::new(
                lightagent_core::ProfileId::new("lead").unwrap(),
                "Lead",
                "Persona",
                "Auto",
            );
            store.create(&lead).unwrap();
            store.set_active(&lead.id).unwrap();
            let factory = LightweightRunFactory {
                root: root.clone(),
                jev: crate::coordinator::JevState::new(),
            };
            let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
            let (_decide, decisions) = tokio::sync::mpsc::unbounded_channel();
            let status = factory
                .run(
                    StartRun {
                        message: message.to_owned(),
                        history: vec![],
                        profile: None,
                        model: None,
                        cwd,
                    },
                    sink,
                    CancellationToken::new(),
                    decisions,
                )
                .await;
            let mut seen = Vec::new();
            while let Ok(event) = events.try_recv() {
                seen.push(event);
            }
            assert_eq!(status, RunStatus::Completed, "{label}: {seen:?}");
            assert!(
                seen.contains(&AgentEvent::RouteSelected {
                    source: "Jev".to_owned(),
                    model: "Auto".to_owned(),
                    kept_default: true,
                    // Auto is decided before the permission is consulted.
                    permission: None,
                    fallback: Some("auto_route".to_owned()),
                }),
                "{label}: {seen:?}"
            );
            let diagnostics = seen
                .iter()
                .find_map(|event| match event {
                    AgentEvent::TaskClassified { diagnostics } => Some(diagnostics.clone()),
                    _ => None,
                })
                .expect("shadow mode records one classification");
            assert_eq!(diagnostics.mode, "shadow");
            assert_eq!(diagnostics.category.as_deref(), Some("research"));
            assert!(!diagnostics.influenced, "shadow never influences the run");
            assert_eq!(diagnostics.jev_calls, 1, "routing skipped for Auto");
            assert!(!format!("{diagnostics:?}").contains("batteries"));
            assert_eq!(
                seen.iter()
                    .filter(|event| matches!(event, AgentEvent::TaskClassified { .. }))
                    .count(),
                1
            );
        }
        let chats = chats.lock().unwrap().clone();
        assert_eq!(chats.len(), 2, "one model turn per run");
        for body in &chats {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(
                body["model"], "Auto",
                "the provider receives Auto unchanged"
            );
        }
        let jev_bodies = jev_seen.lock().unwrap().clone();
        assert_eq!(
            jev_bodies.len(),
            2,
            "one classification per run, no routing"
        );
        for body in &jev_bodies {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(body["questions"].get("route").is_none());
            assert!(body["questions"].get("task").is_some());
        }
        std::fs::remove_dir_all(scratch).ok();
    }

    /// Jev classification fails, times out, answers garbage, is refused for
    /// the profile, or confidently says no retrieval is needed: every run
    /// still completes and offers the model exactly the retrieval tools a run
    /// without Jev offers. Classification is advisory; it never gates a tool.
    #[tokio::test]
    async fn a_failed_or_refused_classification_never_withholds_retrieval() {
        use crate::coordinator::test_support::{answer, jev_stub};
        let (gateway, chats) = gateway_stub().await;
        let (unavailable, _) = jev_stub(503, "{}".to_owned(), Duration::ZERO).await;
        let slow_answer = answer("research", 0.95);
        let (slow, _) = jev_stub(200, slow_answer, Duration::from_secs(5)).await;
        let (malformed, _) = jev_stub(200, "{\"answers\":".to_owned(), Duration::ZERO).await;
        let no_retrieval = answer("conversation", 0.99);
        let (confident, _) = jev_stub(200, no_retrieval, Duration::ZERO).await;
        let (research, _) = jev_stub(200, answer("research", 0.99), Duration::ZERO).await;
        let scratch = std::env::temp_dir().join(format!(
            "lightagent-jev-advisory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // (case, Jev endpoint, profile permitted to classify, outcome)
        let cases = [
            ("off", None, false, None),
            ("unavailable", Some(unavailable), true, Some("failed")),
            ("timeout", Some(slow), true, Some("failed")),
            ("malformed", Some(malformed), true, Some("failed")),
            ("denied", Some(confident.clone()), false, Some("skipped")),
            (
                "confident_no_retrieval",
                Some(confident),
                true,
                Some("classified"),
            ),
            ("research_guided", Some(research), true, Some("classified")),
        ];
        let count = cases.len();
        for (label, jev, permitted, outcome) in cases {
            let root = scratch.join(label);
            let mut config = lightagent_core::Config::default();
            config.inference.base_url = gateway.clone();
            config.memory.inject_recent = 0;
            config.web.enabled = true;
            config.web.search.endpoint =
                Some(lightagent_core::DUCKDUCKGO_SEARCH_ENDPOINT.to_owned());
            config.rag.realtime_enabled = true;
            if let Some(jev) = jev {
                config.platform.jev.endpoint.enabled = true;
                config.platform.jev.endpoint.base_url = Some(jev);
                config.platform.jev.timeout_secs = 1;
                config.platform.jev.classification.mode =
                    lightagent_core::JevClassificationMode::Active;
                config.platform.jev.classification.workflow_guidance = true;
                if permitted {
                    config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
                }
            }
            ConfigStore::at(&LightagentPaths::rooted_at(&root))
                .save(&config)
                .unwrap();
            let store = ProfileStore::new(&root);
            let lead = lightagent_core::AgentProfile::new(
                lightagent_core::ProfileId::new("lead").unwrap(),
                "Lead",
                "Persona",
                "Auto",
            );
            store.create(&lead).unwrap();
            store.set_active(&lead.id).unwrap();
            let factory = LightweightRunFactory {
                root: root.clone(),
                jev: crate::coordinator::JevState::new(),
            };
            let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
            let (_decide, decisions) = tokio::sync::mpsc::unbounded_channel();
            let status = factory
                .run(
                    StartRun {
                        message: "search the web for today's battery news".to_owned(),
                        history: vec![],
                        profile: None,
                        model: None,
                        cwd: None,
                    },
                    sink,
                    CancellationToken::new(),
                    decisions,
                )
                .await;
            let mut seen = Vec::new();
            while let Ok(event) = events.try_recv() {
                seen.push(event);
            }
            assert_eq!(status, RunStatus::Completed, "{label}: {seen:?}");
            let classified = seen.iter().find_map(|event| match event {
                AgentEvent::TaskClassified { diagnostics } => Some(diagnostics.outcome.clone()),
                _ => None,
            });
            assert_eq!(classified.as_deref(), outcome, "{label}");
        }

        let chats = chats.lock().unwrap().clone();
        assert_eq!(chats.len(), count, "one model turn per run");
        let offered = |body: &str| -> Vec<String> {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            let mut names: Vec<String> = body["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                .collect();
            names.sort();
            names
        };
        let baseline = offered(&chats[0]);
        for tool in ["rag.realtime", "web.search", "web.fetch"] {
            assert!(
                baseline.iter().any(|name| name == tool),
                "{tool}: {baseline:?}"
            );
        }
        for (at, body) in chats.iter().enumerate() {
            assert_eq!(offered(body), baseline, "run {at} offered other tools");
        }
        std::fs::remove_dir_all(scratch).ok();
    }

    /// The routing permission through the real run factory, for an API run
    /// (no cwd) and an ACP run (an editor cwd): the same check on the profile
    /// the run resolved to, read from the configuration at each run, so a
    /// grant or a revocation applies to the next run without a restart.
    #[tokio::test]
    async fn api_and_acp_runs_route_only_for_a_currently_permitted_profile() {
        use crate::coordinator::test_support::{answer, jev_stub};
        let (gateway, _) = gateway_stub().await;
        let (jev, jev_seen) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let scratch = std::env::temp_dir().join(format!(
            "lightagent-routing-permission-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = scratch.join("acp-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let root = scratch.join("home");
        let paths = LightagentPaths::rooted_at(&root);
        let mut config = lightagent_core::Config::default();
        config.inference.base_url = gateway;
        config.memory.inject_recent = 0;
        config.platform.jev.endpoint.enabled = true;
        config.platform.jev.endpoint.base_url = Some(jev);
        config.platform.jev.allowed_models = vec!["careful".to_owned()];
        let store = ProfileStore::new(&root);
        let lead = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("lead").unwrap(),
            "Lead",
            "Persona",
            "fast",
        );
        store.create(&lead).unwrap();
        store.set_active(&lead.id).unwrap();
        let factory = LightweightRunFactory {
            root: root.clone(),
            jev: crate::coordinator::JevState::new(),
        };
        // A private marker that must never reach Jev while denied.
        let message = "summarize the private journal entry for lead";
        for (step, granted) in [
            ("strict-unlisted", false),
            ("granted", true),
            ("revoked", false),
        ] {
            config.platform.jev.routing_allowed_profiles = if granted {
                vec!["lead".to_owned()]
            } else {
                Vec::new()
            };
            ConfigStore::at(&paths).save(&config).unwrap();
            for (label, cwd) in [
                ("api", None),
                ("acp", Some(workspace.to_string_lossy().into_owned())),
            ] {
                jev_seen.lock().unwrap().clear();
                let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
                let (_decide, decisions) = tokio::sync::mpsc::unbounded_channel();
                let status = factory
                    .run(
                        StartRun {
                            message: message.to_owned(),
                            history: vec![],
                            profile: None,
                            model: None,
                            cwd,
                        },
                        sink,
                        CancellationToken::new(),
                        decisions,
                    )
                    .await;
                let mut seen = Vec::new();
                while let Ok(event) = events.try_recv() {
                    seen.push(event);
                }
                assert_eq!(status, RunStatus::Completed, "{step} {label}: {seen:?}");
                let route = seen
                    .iter()
                    .find_map(|event| match event {
                        AgentEvent::RouteSelected {
                            model,
                            permission,
                            fallback,
                            ..
                        } => Some((model.clone(), permission.clone(), fallback.clone())),
                        _ => None,
                    })
                    .expect("routing.selected is reported while Jev is enabled");
                let requests = jev_seen.lock().unwrap().clone();
                if granted {
                    assert_eq!(requests.len(), 1, "{step} {label}");
                    assert_eq!(route.1.as_deref(), Some("allowed"), "{step} {label}");
                } else {
                    assert!(requests.is_empty(), "{step} {label}: {requests:?}");
                    assert_eq!(
                        route,
                        (
                            "fast".to_owned(),
                            Some("denied".to_owned()),
                            Some("profile_not_permitted".to_owned())
                        ),
                        "{step} {label}"
                    );
                    // The denial carries fixed vocabulary only.
                    let reported = format!("{seen:?}");
                    assert!(!reported.contains("private journal"), "{step} {label}");
                }
            }
        }
        std::fs::remove_dir_all(scratch).ok();
    }

    /// One real run through the factory against a stub gateway, returning its
    /// status, events and the bodies the provider received.
    async fn run_through_factory(
        config: lightagent_core::Config,
        message: &str,
    ) -> (RunStatus, Vec<AgentEvent>, Vec<serde_json::Value>) {
        run_through_factory_with(config, message, Vec::new(), RunTrust::LocalOperator).await
    }

    /// As [`run_through_factory`], for a `lead` profile with `toolsets`,
    /// started by a caller of `trust`.
    async fn run_through_factory_with(
        mut config: lightagent_core::Config,
        message: &str,
        toolsets: Vec<String>,
        trust: RunTrust,
    ) -> (RunStatus, Vec<AgentEvent>, Vec<serde_json::Value>) {
        let (gateway, chats) = gateway_stub().await;
        // A random component: on macOS two tests started in parallel can
        // read the same clock value, and then share (and delete) one home.
        let root = std::env::temp_dir().join(format!(
            "lightagent-workflow-{}-{}",
            std::process::id(),
            RunId::new().as_str()
        ));
        config.inference.base_url = gateway;
        config.memory.inject_recent = 0;
        ConfigStore::at(&LightagentPaths::rooted_at(&root))
            .save(&config)
            .unwrap();
        let store = ProfileStore::new(&root);
        let mut lead = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("lead").unwrap(),
            "Lead",
            "Persona",
            "fast",
        );
        lead.toolsets = toolsets;
        store.create(&lead).unwrap();
        store.set_active(&lead.id).unwrap();
        let factory = LightweightRunFactory {
            root: root.clone(),
            jev: crate::coordinator::JevState::new(),
        };
        let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (_decide, decisions) = tokio::sync::mpsc::unbounded_channel();
        let status = factory
            .run_as(
                StartRun {
                    message: message.to_owned(),
                    history: vec![],
                    profile: None,
                    model: None,
                    cwd: None,
                },
                trust,
                sink,
                CancellationToken::new(),
                decisions,
            )
            .await;
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        let bodies = chats
            .lock()
            .unwrap()
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        std::fs::remove_dir_all(root).ok();
        (status, seen, bodies)
    }

    /// A gateway answering `GET /v1/capabilities` and `GET /v1/models` with
    /// the given statuses and bodies, and anything else with 404.
    async fn probe_stub(capabilities: (u16, String), models: (u16, String)) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (capabilities, models) = (capabilities.clone(), models.clone());
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buf = [0; 4096];
                    while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(count) => bytes.extend_from_slice(&buf[..count]),
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let (status, body) = if request.starts_with("GET /v1/capabilities") {
                        capabilities
                    } else if request.starts_with("GET /v1/models") {
                        models
                    } else {
                        (404, String::new())
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        format!("http://{address}")
    }

    /// The history window serve resolves for the active `lead` profile
    /// (model `fast`) against a stub gateway, under `config`.
    async fn resolved_history(
        capabilities: (u16, String),
        models: (u16, String),
        mut config: lightagent_core::Config,
    ) -> lightagent_core::HistoryWindow {
        let root = std::env::temp_dir().join(format!(
            "lightagent-history-{}-{}",
            std::process::id(),
            RunId::new().as_str()
        ));
        config.inference.base_url = probe_stub(capabilities, models).await;
        ConfigStore::at(&LightagentPaths::rooted_at(&root))
            .save(&config)
            .unwrap();
        let store = ProfileStore::new(&root);
        let lead = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("lead").unwrap(),
            "Lead",
            "Persona",
            "fast",
        );
        store.create(&lead).unwrap();
        store.set_active(&lead.id).unwrap();
        let factory = LightweightRunFactory {
            root: root.clone(),
            jev: crate::coordinator::JevState::new(),
        };
        let window = factory.history_window(None, None).await.unwrap();
        std::fs::remove_dir_all(root).ok();
        window
    }

    #[tokio::test]
    async fn serve_resolves_the_history_window_like_the_run_does() {
        use lightagent_core::HistorySource;
        let capabilities = |id: &str, tokens: u32| {
            (
                200,
                serde_json::json!({
                    "protocol": {"name": "lightweight-public-inference", "version": 1},
                    "state": {"model_loaded": true, "model": {"id": id, "context_length": tokens}},
                })
                .to_string(),
            )
        };
        let plain_models = (200, r#"{"data":[{"id":"fast"}]}"#.to_owned());
        let placed = |n_ctx: Option<u32>, window: Option<u32>| {
            let mut config = lightagent_core::Config::default();
            config.runtime.n_ctx = n_ctx;
            config.generation.context_window = window;
            config
        };

        // Served capacity above the placement setting is used, not cut down.
        let above = resolved_history(
            capabilities("fast", 32_768),
            plain_models.clone(),
            placed(Some(4_096), None),
        )
        .await;
        assert_eq!(
            (above.tokens, above.source),
            (32_768, HistorySource::Provider)
        );
        // A configured ceiling below it narrows it.
        let ceiling = resolved_history(
            capabilities("fast", 32_768),
            plain_models.clone(),
            placed(None, Some(8_192)),
        )
        .await;
        assert_eq!(
            (ceiling.tokens, ceiling.source),
            (8_192, HistorySource::Configured)
        );
        // A configured figure above the served capacity never stretches it.
        let stretched = resolved_history(
            capabilities("fast", 8_192),
            plain_models.clone(),
            placed(None, Some(65_536)),
        )
        .await;
        assert_eq!(
            (stretched.tokens, stretched.source),
            (8_192, HistorySource::Provider)
        );
        // An older gateway: no capabilities, the window on its model row.
        let row = (
            200,
            r#"{"data":[{"id":"fast","n_ctx":16384,"context_length":16384,"hermes":{}}]}"#
                .to_owned(),
        );
        let older = resolved_history((404, String::new()), row, placed(None, None)).await;
        assert_eq!(
            (older.tokens, older.source),
            (16_384, HistorySource::Provider)
        );
        // Malformed discovery falls back to the placement setting, labelled so.
        let malformed = resolved_history(
            (200, "not json".into()),
            plain_models.clone(),
            placed(Some(12_288), None),
        )
        .await;
        assert_eq!(
            (malformed.tokens, malformed.source),
            (12_288, HistorySource::Placement)
        );
        // Another resident model's capacity is never borrowed.
        let other = resolved_history(
            capabilities("other@64k", 65_536),
            plain_models.clone(),
            placed(None, None),
        )
        .await;
        assert_eq!(
            (other.tokens, other.source),
            (4_096, HistorySource::Fallback)
        );
    }

    fn system_prompt(body: &serde_json::Value) -> String {
        body["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn tool_names(body: &serde_json::Value) -> Vec<String> {
        let mut names: Vec<String> = body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn classification(events: &[AgentEvent]) -> Option<lightagent_core::ClassificationDiagnostics> {
        events.iter().find_map(|event| match event {
            AgentEvent::TaskClassified { diagnostics } => Some((**diagnostics).clone()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn workflow_guidance_reaches_the_provider_only_when_active_eligible_and_enabled() {
        use crate::coordinator::test_support::{answer, jev_stub};
        let (jev, _) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let message = "find recent papers on solid-state batteries";
        let with_web = |mode, guidance, web: bool| {
            let mut config = lightagent_core::Config::default();
            config.web.enabled = web;
            config.web.search.endpoint = Some("http://127.0.0.1:9/search".to_owned());
            config.platform.jev.endpoint.enabled = true;
            config.platform.jev.endpoint.base_url = Some(jev.clone());
            config.platform.jev.classification.mode = mode;
            config.platform.jev.classification.workflow_guidance = guidance;
            config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
            config
        };
        use lightagent_core::JevClassificationMode::{Active, Off, Shadow};

        // Baseline: Jev classification off.
        let (status, events, bodies) =
            run_through_factory(with_web(Off, true, true), message).await;
        assert_eq!(status, RunStatus::Completed);
        assert!(classification(&events).is_none(), "off records nothing");
        let baseline_prompt = system_prompt(&bodies[0]);
        let baseline_tools = tool_names(&bodies[0]);
        assert!(!baseline_prompt.contains("Task assessment"));
        assert!(baseline_tools.contains(&"web.search".to_owned()));

        // Shadow: evaluated, never applied, run unchanged.
        let (_, events, bodies) = run_through_factory(with_web(Shadow, true, true), message).await;
        let shadow = classification(&events).unwrap();
        assert_eq!(shadow.workflow_eligibility.as_deref(), Some("eligible"));
        assert!(!shadow.workflow_applied && !shadow.influenced);
        assert_eq!(system_prompt(&bodies[0]), baseline_prompt);
        assert_eq!(tool_names(&bodies[0]), baseline_tools);

        // Active without workflow_guidance: still advisory only.
        let (_, events, bodies) = run_through_factory(with_web(Active, false, true), message).await;
        assert!(!classification(&events).unwrap().workflow_applied);
        assert_eq!(system_prompt(&bodies[0]), baseline_prompt);

        // Active with workflow_guidance: one fixed instruction, same tools.
        let (_, events, bodies) = run_through_factory(with_web(Active, true, true), message).await;
        let applied = classification(&events).unwrap();
        assert!(applied.workflow_applied && applied.influenced);
        assert_eq!(applied.workflow.as_deref(), Some("research"));
        assert_eq!(
            applied.workflow_capability.as_deref(),
            Some("retrieval_tools")
        );
        assert!(applied.workflow_selection_us.is_some());
        let prompt = system_prompt(&bodies[0]);
        let guidance = crate::coordinator::workflow::RESEARCH_GUIDANCE;
        assert!(prompt.contains(guidance));
        assert_eq!(
            prompt.replacen(&format!("\n\n{guidance}"), "", 1),
            baseline_prompt,
            "the only difference from the run without Jev is the fixed guidance"
        );
        assert!(
            !prompt.contains("batteries"),
            "nothing from the message is interpolated"
        );
        assert_eq!(
            tool_names(&bodies[0]),
            baseline_tools,
            "no tool is added or removed"
        );

        // Without retrieval tools, research guidance has nothing to point at.
        let (_, events, bodies) = run_through_factory(with_web(Active, true, false), message).await;
        let unconfigured = classification(&events).unwrap();
        assert_eq!(
            unconfigured.workflow_eligibility.as_deref(),
            Some("not_configured")
        );
        assert!(!unconfigured.workflow_applied);
        assert!(!system_prompt(&bodies[0]).contains("Task assessment"));
        assert!(!tool_names(&bodies[0]).contains(&"web.search".to_owned()));
    }

    /// A Jev stub that answers nothing until two requests are waiting, then
    /// answers both. A preflight that sent its two requests one after the
    /// other would time out on the first.
    async fn barrier_jev(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let barrier = std::sync::Arc::clone(&barrier);
                let body = body.clone();
                tokio::spawn(async move {
                    let _ = crate::coordinator::classify_read_request(&mut stream).await;
                    barrier.wait().await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn routing_and_classification_run_concurrently_in_the_preflight() {
        use crate::coordinator::test_support::answer;
        let jev = barrier_jev(answer("research", 0.95)).await;
        let mut config = lightagent_core::Config::default();
        config.platform.jev.endpoint.enabled = true;
        config.platform.jev.endpoint.base_url = Some(jev);
        config.platform.jev.timeout_secs = 2;
        config.platform.jev.allowed_models = vec!["fast".to_owned()];
        config.platform.jev.routing_allowed_profiles = vec!["lead".to_owned()];
        config.platform.jev.classification.mode = lightagent_core::JevClassificationMode::Shadow;
        config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
        let (status, events, _) =
            run_through_factory(config, "find recent papers on solid-state batteries").await;
        assert_eq!(status, RunStatus::Completed);
        let diagnostics = classification(&events).unwrap();
        assert_eq!(
            diagnostics.outcome, "classified",
            "both requests were in flight together; sequential sends would time out"
        );
        assert_eq!(diagnostics.jev_calls, 2);
        assert!(
            diagnostics.preflight_latency_ms < 2_000,
            "sequential sends would wait out the first request's timeout"
        );
    }

    /// Off, shadow and active-with-guidance under identical stubs: the same
    /// gateway, a Jev stub with a fixed delay, five runs each. A simulation of
    /// the preflight's overhead, not a measurement of real agent outcomes.
    #[tokio::test]
    async fn preflight_overhead_by_mode_is_measured_under_fixed_stubs() {
        use crate::coordinator::evaluation::metrics::{Ratio, latency_json};
        use crate::coordinator::test_support::{answer, jev_stub};
        use lightagent_core::JevClassificationMode::{Active, Off, Shadow};
        const RUNS: usize = 5;
        const JEV_DELAY: Duration = Duration::from_millis(100);
        let message = "find recent papers on solid-state batteries";
        let mut modes = serde_json::Map::new();
        let mut tool_lists = Vec::new();
        for (label, mode, guidance) in [
            ("off", Off, false),
            ("shadow", Shadow, false),
            ("active_with_guidance", Active, true),
        ] {
            let (jev, seen) = jev_stub(200, answer("research", 0.95), JEV_DELAY).await;
            let mut config = lightagent_core::Config::default();
            config.web.enabled = true;
            config.web.search.endpoint = Some("http://127.0.0.1:9/search".to_owned());
            config.platform.jev.endpoint.enabled = true;
            config.platform.jev.endpoint.base_url = Some(jev);
            config.platform.jev.allowed_models = vec!["fast".to_owned()];
            config.platform.jev.routing_allowed_profiles = vec!["lead".to_owned()];
            config.platform.jev.classification.mode = mode;
            config.platform.jev.classification.workflow_guidance = guidance;
            config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
            let (mut wall, mut preflight, mut tokens_in, mut tokens_out) =
                (Vec::new(), Vec::new(), 0u64, 0u64);
            let (mut eligible, mut applied, mut fallbacks, mut provider_requests) =
                (0u32, 0u32, 0u32, 0usize);
            for _ in 0..RUNS {
                let started = std::time::Instant::now();
                let (status, events, bodies) = run_through_factory(config.clone(), message).await;
                wall.push(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                assert_eq!(status, RunStatus::Completed);
                provider_requests += bodies.len();
                tool_lists.push(tool_names(&bodies[0]));
                match classification(&events) {
                    Some(diagnostics) => {
                        assert_ne!(mode, Off);
                        preflight.push(diagnostics.preflight_latency_ms);
                        tokens_in += diagnostics.input_tokens.unwrap_or(0);
                        tokens_out += diagnostics.output_tokens.unwrap_or(0);
                        eligible += u32::from(
                            diagnostics.workflow_eligibility.as_deref() == Some("eligible"),
                        );
                        applied += u32::from(diagnostics.workflow_applied);
                        fallbacks += u32::from(diagnostics.fallback_reason.is_some());
                    }
                    None => assert_eq!(mode, Off, "off records nothing"),
                }
            }
            let requests = seen.lock().unwrap().len();
            let expected = if mode == Off { RUNS } else { 2 * RUNS };
            assert_eq!(
                requests, expected,
                "{label}: routing always, classification only when on"
            );
            let runs = u32::try_from(RUNS).unwrap();
            match mode {
                Off => assert_eq!(applied, 0),
                Shadow => assert_eq!((eligible, applied), (runs, 0)),
                Active => assert_eq!((eligible, applied), (runs, runs)),
            }
            modes.insert(
                label.to_owned(),
                serde_json::json!({
                    "runs": RUNS,
                    "jev_requests": requests,
                    "jev_requests_per_run": requests as f64 / RUNS as f64,
                    "provider_requests": provider_requests,
                    "tool_calls": 0,
                    "end_to_end_latency": latency_json(&wall),
                    "preflight_latency": latency_json(&preflight),
                    "fallback_rate": Ratio::new(fallbacks, runs).json(),
                    "workflow_eligible": Ratio::new(eligible, runs).json(),
                    "workflow_applied": Ratio::new(applied, runs).json(),
                    "jev_tokens": {"input": tokens_in, "output": tokens_out},
                }),
            );
        }
        assert!(
            tool_lists.windows(2).all(|pair| pair[0] == pair[1]),
            "no mode changes the tools offered to the provider"
        );
        let report = serde_json::json!({
            "report": "jev-preflight-performance",
            "measurement": "simulation",
            "disclaimer": "Local stubs with a fixed 100 ms Jev delay and an instant gateway. These numbers show the shape of the preflight's overhead and request count; they are not real latency, cost or answer quality.",
            "jev_stub_delay_ms": JEV_DELAY.as_millis() as u64,
            "routing": "enabled with one allowlisted model; the stub's answer is not a route, so routing falls back and keeps the configured model",
            "modes": modes,
            "cost": "Not estimated: stub usage is synthetic and TypeSafe pricing is unverified.",
        });
        let mut summary = String::from(
            "# Jev preflight performance (simulation)\n\n> Local stubs; not real latency, cost or answer quality.\n\n| Mode | Jev requests/run | Preflight p50 ms | End-to-end p50 ms | Eligible | Applied |\n|---|---|---|---|---|---|\n",
        );
        for (label, row) in report["modes"].as_object().unwrap() {
            summary.push_str(&format!(
                "| {label} | {} | {} | {} | {} | {} |\n",
                row["jev_requests_per_run"],
                row["preflight_latency"]["p50_ms"],
                row["end_to_end_latency"]["p50_ms"],
                row["workflow_eligible"]["numerator"],
                row["workflow_applied"]["numerator"],
            ));
        }
        crate::coordinator::evaluation::write_report("performance-simulation", &report, &summary);
    }

    /// Every capability a profile could ask for: web search and fetch, the
    /// workspace tools and the terminal.
    fn tool_config(mode: lightagent_core::ToolPolicyMode) -> lightagent_core::Config {
        let mut config = lightagent_core::Config::default();
        config.web.enabled = true;
        config.web.search.endpoint = Some("http://127.0.0.1:9/search".to_owned());
        config.tools.enabled = true;
        config.tools.allow_terminal = true;
        config.tools.profile_policy = mode;
        config
    }

    fn strings(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    #[tokio::test]
    async fn off_ignores_toolsets_and_report_only_changes_nothing() {
        use lightagent_core::ToolPolicyMode::{Off, ReportOnly};
        let (_, _, baseline) = run_through_factory(tool_config(Off), "hi").await;
        let baseline = tool_names(&baseline[0]);
        assert!(baseline.contains(&"terminal.run".to_owned()));
        for mode in [Off, ReportOnly] {
            let (status, _, bodies) = run_through_factory_with(
                tool_config(mode),
                "hi",
                strings(&["datetime.now"]),
                RunTrust::LocalOperator,
            )
            .await;
            assert_eq!(status, RunStatus::Completed);
            assert_eq!(tool_names(&bodies[0]), baseline, "{mode:?}");
        }
    }

    #[tokio::test]
    async fn an_enforced_profile_policy_shapes_the_provider_request() {
        use lightagent_core::ToolPolicyMode::{Enforce, Strict};
        for mode in [Enforce, Strict] {
            let (status, _, bodies) = run_through_factory_with(
                tool_config(mode),
                "hi",
                strings(&["datetime.now", "fs.read"]),
                RunTrust::LocalOperator,
            )
            .await;
            assert_eq!(status, RunStatus::Completed);
            assert_eq!(
                tool_names(&bodies[0]),
                ["datetime.now", "fs.read"],
                "{mode:?}"
            );
        }
        // Strict with no list offers nothing; enforce with no list keeps all.
        let (_, _, strict) = run_through_factory_with(
            tool_config(Strict),
            "hi",
            Vec::new(),
            RunTrust::LocalOperator,
        )
        .await;
        assert!(tool_names(&strict[0]).is_empty());
        let (_, _, legacy) = run_through_factory_with(
            tool_config(Enforce),
            "hi",
            Vec::new(),
            RunTrust::LocalOperator,
        )
        .await;
        assert!(tool_names(&legacy[0]).contains(&"terminal.run".to_owned()));
    }

    #[tokio::test]
    async fn an_untrusted_run_gets_only_the_read_only_researcher_and_only_when_granted() {
        let listed = strings(&["web.*", "agent.research", "agent.delegate", "datetime.now"]);
        let run = |remote_research: bool| {
            let mut config = tool_config(lightagent_core::ToolPolicyMode::Off);
            config.subagents.remote_research = remote_research;
            run_through_factory_with(config, "hi", listed.clone(), RunTrust::RemoteUser)
        };
        // Off (the default): nothing that delegates.
        let (_, _, bodies) = run(false).await;
        let names = tool_names(&bodies[0]);
        assert!(!names.contains(&"agent.research".to_owned()), "{names:?}");
        assert!(!names.contains(&"agent.delegate".to_owned()), "{names:?}");
        // On: the read-only researcher, never generic delegation.
        let (status, _, bodies) = run(true).await;
        assert_eq!(status, RunStatus::Completed);
        let names = tool_names(&bodies[0]);
        assert!(names.contains(&"agent.research".to_owned()), "{names:?}");
        assert!(!names.contains(&"agent.delegate".to_owned()), "{names:?}");
        // An operator run is not given it: it delegates by role instead.
        let mut config = tool_config(lightagent_core::ToolPolicyMode::Off);
        config.subagents.remote_research = true;
        let (_, _, bodies) = run_through_factory(config, "hi").await;
        assert!(!tool_names(&bodies[0]).contains(&"agent.research".to_owned()));
    }

    #[tokio::test]
    async fn readiness_is_reported_per_role_for_operators_and_untrusted_users() {
        let root = std::env::temp_dir().join(format!(
            "lightagent-readiness-{}",
            lightagent_core::RunId::new().as_str()
        ));
        let mut config = tool_config(lightagent_core::ToolPolicyMode::Off);
        config.subagents.remote_research = true;
        ConfigStore::at(&LightagentPaths::rooted_at(&root))
            .save(&config)
            .unwrap();
        let store = ProfileStore::new(&root);
        let mut lead = lightagent_core::AgentProfile::new(
            lightagent_core::ProfileId::new("lead").unwrap(),
            "Lead",
            "Persona",
            "fast",
        );
        lead.toolsets = strings(&["web.*", "agent.research"]);
        store.create(&lead).unwrap();
        store.set_active(&lead.id).unwrap();
        let factory = LightweightRunFactory {
            root: root.clone(),
            jev: crate::coordinator::JevState::new(),
        };
        let state = |report: &serde_json::Value, role: &str| {
            report
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["role"] == role)
                .map(|entry| entry["state"].as_str().unwrap().to_owned())
                .unwrap()
        };
        let operator = factory
            .workers_for(None, RunTrust::LocalOperator)
            .await
            .unwrap();
        assert_eq!(state(&operator, "researcher"), "ready");
        assert_eq!(state(&operator, "tester"), "unavailable");
        let remote = factory
            .workers_for(Some(&lead.id), RunTrust::RemoteUser)
            .await
            .unwrap();
        assert_eq!(state(&remote, "researcher"), "ready");
        for role in ["coder", "reviewer", "tester"] {
            assert_eq!(state(&remote, role), "permission_denied");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn the_legacy_rollback_opens_no_delegation_to_an_untrusted_run() {
        // Rollback on, research on, and the profile lists both delegation
        // tools: the untrusted run still gets no generic delegation, and its
        // research tool has no legacy resolution to fall back on.
        let mut config = tool_config(lightagent_core::ToolPolicyMode::Off);
        config.subagents.resolution = lightagent_core::WorkerResolution::LegacyProfiles;
        config.subagents.remote_research = true;
        let listed = strings(&["web.*", "agent.research", "agent.delegate"]);
        let (_, _, bodies) =
            run_through_factory_with(config, "hi", listed, RunTrust::RemoteUser).await;
        let names = tool_names(&bodies[0]);
        assert!(!names.contains(&"agent.delegate".to_owned()), "{names:?}");
        assert!(!names.contains(&"agent.research".to_owned()), "{names:?}");
    }

    #[tokio::test]
    async fn an_untrusted_run_never_receives_unisolated_local_execution() {
        // Configured off, and the profile lists everything: an untrusted run
        // is still strict and gets no local files, terminal or delegation.
        let (status, _, bodies) = run_through_factory_with(
            tool_config(lightagent_core::ToolPolicyMode::Off),
            "hi",
            strings(&[
                "fs.*",
                "terminal.run",
                "agent.delegate",
                "web.*",
                "datetime.now",
            ]),
            RunTrust::RemoteUser,
        )
        .await;
        assert_eq!(status, RunStatus::Completed);
        let names = tool_names(&bodies[0]);
        assert_eq!(names, ["datetime.now", "web.fetch", "web.search"]);
        // Identity alone grants nothing: an empty list gives no tools.
        let (_, _, empty) = run_through_factory_with(
            tool_config(lightagent_core::ToolPolicyMode::Off),
            "hi",
            Vec::new(),
            RunTrust::RemoteUser,
        )
        .await;
        assert!(tool_names(&empty[0]).is_empty());
    }

    #[tokio::test]
    async fn jev_workflow_eligibility_follows_the_effective_tools() {
        use crate::coordinator::test_support::{answer, jev_stub};
        let (jev, _) = jev_stub(200, answer("research", 0.95), Duration::ZERO).await;
        let with_jev = |mode| {
            let mut config = tool_config(mode);
            config.platform.jev.endpoint.enabled = true;
            config.platform.jev.endpoint.base_url = Some(jev.clone());
            config.platform.jev.classification.mode =
                lightagent_core::JevClassificationMode::Active;
            config.platform.jev.classification.workflow_guidance = true;
            config.platform.jev.classification.allowed_profiles = vec!["lead".to_owned()];
            config
        };
        let message = "find recent papers on solid-state batteries";
        // Web tools withheld by the profile: research has nothing to use and
        // guidance is not applied, however confident the classification.
        let (_, events, bodies) = run_through_factory_with(
            with_jev(lightagent_core::ToolPolicyMode::Enforce),
            message,
            strings(&["datetime.now"]),
            RunTrust::LocalOperator,
        )
        .await;
        let decision = classification(&events).unwrap();
        assert_eq!(decision.category.as_deref(), Some("research"));
        assert_eq!(
            decision.workflow_eligibility.as_deref(),
            Some("not_configured")
        );
        assert!(!decision.workflow_applied);
        assert_eq!(
            tool_names(&bodies[0]),
            ["datetime.now"],
            "classification added no tool"
        );
        assert!(
            !system_prompt(&bodies[0]).contains(crate::coordinator::workflow::RESEARCH_GUIDANCE)
        );
        // With web tools permitted, the same classification is eligible.
        let (_, events, _) = run_through_factory_with(
            with_jev(lightagent_core::ToolPolicyMode::Enforce),
            message,
            strings(&["datetime.now", "web.*"]),
            RunTrust::LocalOperator,
        )
        .await;
        assert_eq!(
            classification(&events)
                .unwrap()
                .workflow_eligibility
                .as_deref(),
            Some("eligible")
        );
    }
}
