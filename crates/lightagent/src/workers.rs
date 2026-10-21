//! Specialist workers for a run: resolution, authorization, wiring, readiness.
//!
//! One place builds the [`Delegation`] every run surface hands its executor
//! (the terminal, the HTTP API and ACP), so a worker is resolved, authorized
//! and bounded the same way everywhere.
//!
//! Resolution of a role, deterministic and never substituted after a refusal:
//! 1. the requesting profile's explicit assignment of the role, if any, which
//!    must still be authorized: a private worker profile of the same owner;
//! 2. otherwise, with `subagents.resolution = "builtin"` (the default), a
//!    profile named after the role that a person customized is still used —
//!    reported as `legacy_profile` until it is assigned or superseded — and
//!    one the harness generated itself is superseded by the built-in;
//! 3. otherwise the built-in worker, under the lead's authority.
//!
//! With `subagents.resolution = "legacy_profiles"` the earlier behavior is
//! restored exactly, as a rollback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightagent_core::identity::RunTrust;
use lightagent_core::identity::store::IdentityStore;
use lightagent_core::workers::{self, Readiness, RoleReadiness, WorkerRole};
use lightagent_core::{
    AgentProfile, AssignmentTarget, Config, ModelRouting, ProfileError, ProfileId, ProfileStore,
    RunLimits, WorkerAssignment, WorkerAssignments, WorkerResolution,
};
use lightagent_extensions::ExtensionStore;
use lightagent_tools::{
    BuiltinResolver, CallGuard, Delegation, ToolRegistry, WorkerResolver, WorkerSpec, WorkerTarget,
};

/// Tools a worker is never offered, even when the lead has them: delegation
/// is added back only where depth allows, Jev's budget belongs to the lead,
/// and the lead's durable memory is written only by the lead.
const LEAD_ONLY_TOOLS: [&str; 4] = [
    "agent.delegate",
    "jev.classify_task",
    "memory.write",
    "memory.reflect",
];

/// What a run's delegation is built from.
pub(crate) struct DelegationInputs<'a> {
    pub(crate) root: &'a Path,
    pub(crate) config: &'a Config,
    /// The lead profile, with its configured (not Jev-routed) model.
    pub(crate) lead: &'a AgentProfile,
    pub(crate) trust: RunTrust,
    pub(crate) extensions: &'a ExtensionStore,
    /// The lead's effective tools, after its tool policy.
    pub(crate) registry: &'a ToolRegistry,
    /// The lead's per-call policy guard, when its policy is enforced.
    pub(crate) guard: Option<CallGuard>,
    pub(crate) enforced: bool,
    pub(crate) factory: Arc<dyn lightagent_core::ProviderFactory>,
}

/// The delegation a run's executor carries.
pub(crate) fn delegation(inputs: DelegationInputs<'_>) -> Delegation {
    let config = inputs.config;
    let mut delegation = Delegation::new(
        Arc::new(ProfileStore::new(inputs.root)),
        inputs.factory,
        worker_base(inputs.registry),
        Duration::from_secs(60),
        262_144,
    );
    delegation.delegate_timeout = Duration::from_secs(config.subagents.delegate_timeout_secs);
    delegation.subagents = crate::chat::subagent_policy(config);
    delegation.trust = inputs.trust;
    delegation.profile_policy = config.tools.profile_policy;
    delegation.network_mcp_servers = crate::chat::network_mcp_servers(config, inputs.extensions);
    delegation.call_guard = inputs.guard;
    if inputs.enforced {
        delegation.limit_ceiling = Some(inputs.lead.limits);
        delegation.approval_floor = Some(inputs.lead.approval_policy);
    }
    if config.subagents.resolution == WorkerResolution::Builtin {
        delegation.resolver = Some(Arc::new(HarnessResolver::new(
            inputs.root,
            config,
            inputs.lead,
            inputs.trust,
        )));
    }
    delegation
}

/// The tools a worker may be drawn from: the lead's effective tools, without
/// the lead-only ones.
pub(crate) fn worker_base(lead: &ToolRegistry) -> ToolRegistry {
    LEAD_ONLY_TOOLS
        .iter()
        .fold(lead.clone(), |registry, name| registry.without(name))
}

/// Resolves a run's delegation targets against the requesting profile's
/// assignments, ownership and the configuration.
pub(crate) struct HarnessResolver {
    root: PathBuf,
    config: Config,
    requester: ProfileId,
    lead_limits: RunLimits,
    trust: RunTrust,
    builtin: BuiltinResolver,
}

impl HarnessResolver {
    pub(crate) fn new(root: &Path, config: &Config, lead: &AgentProfile, trust: RunTrust) -> Self {
        let routing = ModelRouting {
            model: crate::chat::configured_model(&lead.routing.model, config),
            ..lead.routing.clone()
        };
        let mut builtin = BuiltinResolver::new(routing, lead.approval_policy);
        builtin.roles = WorkerRole::ALL
            .into_iter()
            .filter(|role| allowed(config, *role))
            .collect();
        builtin.routes = WorkerRole::ALL
            .into_iter()
            .filter_map(|role| approved_route(config, role).map(|model| (role, model)))
            .collect();
        Self {
            root: root.to_path_buf(),
            config: config.clone(),
            requester: lead.id.clone(),
            lead_limits: lead.limits,
            trust,
            builtin,
        }
    }

    fn assignments(&self) -> WorkerAssignments {
        let dir = ProfileStore::new(&self.root)
            .handle(&self.requester)
            .dir()
            .to_path_buf();
        WorkerAssignments::new(self.requester.clone(), &dir)
    }

    /// The built-in spec of `role`, bounded by the lead's limits, with any
    /// routing fallback recorded.
    fn builtin_spec(&self, role: WorkerRole) -> WorkerSpec {
        let mut spec = self.builtin.spec(role);
        spec.limits = spec.limits.intersect(&self.lead_limits);
        if self.config.subagents.role_models.contains_key(role.as_str())
            && !self.builtin.routes.contains_key(&role)
        {
            spec.notes.push("model_route_not_approved");
        }
        spec
    }

    /// A private worker profile of the requester's owner, under the role's
    /// ceiling, with the role skill added to its own persona.
    fn profile_spec(
        &self,
        role: Option<WorkerRole>,
        profile: &ProfileId,
        label: String,
    ) -> Result<WorkerSpec, String> {
        let refused = || match role {
            Some(role) => format!(
                "worker role '{}' resolves to a worker this profile may not use",
                role.as_str()
            ),
            None => "this profile may not delegate to that worker".to_owned(),
        };
        if profile == &self.requester {
            return Err(refused());
        }
        let store = ProfileStore::new(&self.root);
        let worker = match store.load(profile) {
            Ok(worker) => worker,
            Err(ProfileError::NotFound { .. }) => return Err(refused()),
            Err(_) => return Err("the worker profile could not be read".to_owned()),
        };
        if same_owner(&self.root, &self.requester, profile).is_err() {
            return Err(refused());
        }
        let mut spec = WorkerSpec::from_profile(&worker, label);
        if let Some(role) = role {
            let definition = workers::builtin(role);
            spec.role = Some(role);
            spec.persona = format!("{}\n\n{}", worker.persona, definition.persona());
            spec.ceiling = Some(definition.toolsets());
            spec.skills = vec![definition.skill()];
            spec.nested = false;
            spec.typed_result = true;
        }
        Ok(spec)
    }

    /// A profile named after `role` that a person customized, if any. One
    /// the harness generated from a lead is not customized.
    fn customized_legacy_profile(&self, role: WorkerRole) -> Option<ProfileId> {
        let id = ProfileId::new(role.as_str()).ok()?;
        let profile = ProfileStore::new(&self.root).load(&id).ok()?;
        if crate::is_generated_subagent_profile(&profile, role.as_str()) {
            None
        } else {
            Some(id)
        }
    }

    fn resolve_role(&self, role: WorkerRole) -> Result<WorkerSpec, String> {
        let assignment = self
            .assignments()
            .get(role)
            .map_err(|error| error.code().to_owned())?;
        match assignment {
            Some(assignment) => self.assigned_spec(role, assignment),
            None => match self.customized_legacy_profile(role) {
                Some(profile) => {
                    let label = format!("legacy_profile:{}", profile.as_str());
                    let mut spec = self.profile_spec(Some(role), &profile, label)?;
                    spec.limits = spec.limits.intersect(&self.lead_limits);
                    spec.notes.push("legacy_profile");
                    Ok(spec)
                }
                None => Ok(self.builtin_spec(role)),
            },
        }
    }

    fn assigned_spec(
        &self,
        role: WorkerRole,
        assignment: WorkerAssignment,
    ) -> Result<WorkerSpec, String> {
        let label = format!("assigned:{}", assignment.id);
        let mut spec = match &assignment.target {
            AssignmentTarget::Builtin => {
                let mut spec = self.builtin_spec(role);
                spec.label = format!("{label} ({})", spec.label);
                spec
            }
            AssignmentTarget::Profile { profile } => {
                let mut spec = self.profile_spec(Some(role), profile, label)?;
                spec.limits = spec.limits.intersect(&self.lead_limits);
                spec
            }
        };
        // The assignment, its target and their ownership are checked again
        // before every worker model turn and tool call.
        let root = self.root.clone();
        let requester = self.requester.clone();
        spec.validity = Some(Arc::new(move || {
            still_assigned(&root, &requester, role, &assignment)
        }));
        Ok(spec)
    }
}

impl WorkerResolver for HarnessResolver {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        match target {
            WorkerTarget::Role(raw) => match WorkerRole::parse(raw) {
                Some(role) if allowed(&self.config, role) => self.resolve_role(role),
                _ => Err(format!("worker role '{raw}' is not allowed")),
            },
            WorkerTarget::Profile(raw) => {
                if self.trust != RunTrust::LocalOperator
                    || !self.config.subagents.legacy_profile_argument
                {
                    return Err("this run delegates by role only".to_owned());
                }
                let Ok(profile) = ProfileId::new(raw) else {
                    return Err(format!("invalid worker profile id: {raw:?}"));
                };
                self.profile_spec(None, &profile, format!("profile:{raw}"))
            }
        }
    }
}

/// Whether the configuration enables `role`.
fn allowed(config: &Config, role: WorkerRole) -> bool {
    config
        .subagents
        .allowed_roles
        .iter()
        .any(|allowed| allowed == role.as_str())
}

/// The operator route of `role`, when its model is approved.
fn approved_route(config: &Config, role: WorkerRole) -> Option<String> {
    let model = config.subagents.role_models.get(role.as_str())?;
    config
        .subagents
        .approved_models
        .iter()
        .any(|approved| approved == model)
        .then(|| model.clone())
}

/// Whether two profiles have the same effective owner (or both none, as
/// operator-managed profiles do). Unreadable identity state refuses.
fn same_owner(root: &Path, requester: &ProfileId, worker: &ProfileId) -> Result<(), &'static str> {
    let store = IdentityStore::new(root);
    let Ok(document) = store.load() else {
        return Err("worker_ownership_unreadable");
    };
    let ownership = store.effective_ownership(&document);
    let owner = |profile: &ProfileId| ownership.get(profile).map(|record| record.owner.clone());
    if owner(requester) == owner(worker) {
        Ok(())
    } else {
        Err("worker_not_owned_by_requester")
    }
}

/// Whether `assignment` is still the requester's assignment of `role`, and
/// its target still exists under the same owner.
fn still_assigned(
    root: &Path,
    requester: &ProfileId,
    role: WorkerRole,
    assignment: &WorkerAssignment,
) -> Result<(), String> {
    let revoked = || "the worker assignment was withdrawn".to_owned();
    let dir = ProfileStore::new(root)
        .handle(requester)
        .dir()
        .to_path_buf();
    let current = WorkerAssignments::new(requester.clone(), &dir)
        .get(role)
        .map_err(|_| revoked())?;
    if current.as_ref() != Some(assignment) {
        return Err(revoked());
    }
    if let AssignmentTarget::Profile { profile } = &assignment.target {
        if ProfileStore::new(root).load(profile).is_err() {
            return Err(revoked());
        }
        same_owner(root, requester, profile).map_err(|_| revoked())?;
    }
    Ok(())
}

/// Assign `role` of `requester` to `target` after checking the target is
/// usable by it. The CLI's `workers assign`.
pub(crate) fn assign(
    root: &Path,
    requester: &ProfileId,
    role: WorkerRole,
    target: AssignmentTarget,
) -> Result<WorkerAssignment, String> {
    if let AssignmentTarget::Profile { profile } = &target {
        if profile == requester {
            return Err("a profile cannot be its own worker".to_owned());
        }
        if ProfileStore::new(root).load(profile).is_err() {
            return Err(format!("no profile '{}'", profile.as_str()));
        }
        same_owner(root, requester, profile)
            .map_err(|_| "that profile has a different owner".to_owned())?;
    }
    let dir = ProfileStore::new(root)
        .handle(requester)
        .dir()
        .to_path_buf();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    WorkerAssignments::new(requester.clone(), &dir)
        .assign(role, target, now)
        .map_err(|error| error.to_string())
}

/// Each role's readiness for `lead`, against the tools its runs would have.
/// A snapshot: every run rechecks what it is actually given.
pub(crate) fn readiness(
    root: &Path,
    config: &Config,
    lead: &AgentProfile,
    trust: RunTrust,
    lead_tools: &ToolRegistry,
) -> Vec<RoleReadiness> {
    let resolver = HarnessResolver::new(root, config, lead, trust);
    let base = worker_base(lead_tools);
    WorkerRole::ALL
        .into_iter()
        .map(|role| role_readiness(&resolver, config, role, trust, &base))
        .collect()
}

fn role_readiness(
    resolver: &HarnessResolver,
    config: &Config,
    role: WorkerRole,
    trust: RunTrust,
    base: &ToolRegistry,
) -> RoleReadiness {
    let definition = workers::builtin(role);
    let unready = |state, worker: String, reasons| RoleReadiness {
        role,
        state,
        worker,
        model: None,
        model_source: "lead",
        skill: None,
        tools: Vec::new(),
        reasons,
    };
    if !config.subagents.enabled {
        return unready(
            Readiness::Disabled,
            definition.label(),
            vec!["subagents_disabled"],
        );
    }
    if !allowed(config, role) {
        return unready(
            Readiness::Disabled,
            definition.label(),
            vec!["role_not_allowed"],
        );
    }
    if trust == RunTrust::RemoteUser {
        return unready(
            Readiness::PermissionDenied,
            definition.label(),
            vec!["requires_isolation"],
        );
    }
    if config.subagents.resolution == WorkerResolution::LegacyProfiles {
        return unready(
            Readiness::Degraded,
            format!("legacy_profile:{}", role.as_str()),
            vec!["legacy_resolution"],
        );
    }
    let spec = match resolver.resolve(WorkerTarget::Role(role.as_str())) {
        Ok(spec) => spec,
        Err(_) => {
            return unready(
                Readiness::PermissionDenied,
                definition.label(),
                vec!["assignment_refused"],
            );
        }
    };
    let tools: Vec<String> = base
        .names()
        .into_iter()
        .filter(|name| spec.within_ceiling(name))
        .collect();
    let (mut state, mut reasons) = definition.assess(&tools);
    reasons.extend(spec.notes.iter().copied());
    if state == Readiness::Ready && !spec.notes.is_empty() {
        state = Readiness::Degraded;
    }
    RoleReadiness {
        role,
        state,
        worker: spec.label.clone(),
        model: Some(spec.routing.model.clone()),
        model_source: spec.model_source,
        skill: spec.skill_labels().into_iter().next(),
        tools,
        reasons,
    }
}

#[cfg(test)]
mod tests;
