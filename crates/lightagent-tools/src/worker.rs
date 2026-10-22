//! Resolved workers: what `agent.delegate` instantiates.
//!
//! A delegation names a target: a role, or (legacy, operator runs only) a
//! profile. A [`WorkerResolver`] turns it into a [`WorkerSpec`]: the persona,
//! routing, tool bounds, approval policy, limits and skills of one bounded
//! child run. The harness supplies the resolver, because only it knows the
//! requesting profile's assignments, ownership and configuration; this crate
//! supplies [`BuiltinResolver`] for the built-in catalog and the factory that
//! builds the child run from a spec, shared by every caller.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lightagent_core::workers::{self, WorkerRole};
use lightagent_core::{AgentProfile, ApprovalPolicy, ModelRouting, RunLimits, Skill};

/// Re-checked before every worker model turn and tool call: whether the
/// resolved worker may still act (its assignment, its ownership, …). An
/// `Err` carries a fixed, non-sensitive reason.
pub type WorkerValidity = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// What a delegation names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerTarget<'a> {
    /// A role, resolved by the server.
    Role(&'a str),
    /// A worker profile by id: the legacy argument.
    Profile(&'a str),
}

/// A worker resolved for one delegation.
#[derive(Clone)]
pub struct WorkerSpec {
    /// What ran, for the result and the audit trail: `builtin:<role>@<v>`,
    /// `assigned:<id>` or `profile:<id>`.
    pub label: String,
    pub role: Option<WorkerRole>,
    /// The system prompt.
    pub persona: String,
    pub routing: ModelRouting,
    /// Where the model came from: `lead`, `role_route` or `worker_profile`.
    pub model_source: &'static str,
    /// A definitional ceiling every offered tool must be inside, whatever
    /// the run's policy mode (a built-in role's ceiling). `None`: none.
    pub ceiling: Option<Vec<String>>,
    /// A worker profile's own `toolsets`, applied under the run's
    /// `tools.profile_policy` exactly as the lead's are. `None`: none.
    pub toolsets: Option<Vec<String>>,
    pub approval: ApprovalPolicy,
    pub limits: RunLimits,
    /// Skills added to the worker's skill context (the role skill).
    pub skills: Vec<Skill>,
    /// May the worker itself be offered `agent.delegate` (depth permitting)?
    pub nested: bool,
    /// Return the typed result rather than the bare answer.
    pub typed_result: bool,
    pub validity: Option<WorkerValidity>,
    /// Fixed codes recorded with the result (a routing fallback, a legacy
    /// profile in use, …).
    pub notes: Vec<&'static str>,
}

impl WorkerSpec {
    /// A worker run under a profile: the legacy shape, with the profile's
    /// own toolsets now applied.
    pub fn from_profile(profile: &AgentProfile, label: String) -> Self {
        Self {
            label,
            role: None,
            persona: profile.persona.clone(),
            routing: profile.routing.clone(),
            model_source: "worker_profile",
            ceiling: None,
            toolsets: Some(profile.toolsets.clone()),
            approval: profile.approval_policy,
            limits: profile.limits,
            skills: Vec::new(),
            nested: true,
            typed_result: false,
            validity: None,
            notes: Vec::new(),
        }
    }

    /// Whether `tool` is inside the definitional ceiling.
    pub fn within_ceiling(&self, tool: &str) -> bool {
        self.ceiling.as_ref().is_none_or(|ceiling| {
            ceiling.iter().any(|entry| match entry.strip_suffix(".*") {
                Some(prefix) => tool
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => entry == tool,
            })
        })
    }

    /// The skill labels the worker was given, e.g. `worker-researcher@1`.
    pub fn skill_labels(&self) -> Vec<String> {
        self.skills.iter().map(Skill::label).collect()
    }
}

/// Resolves a delegation target. Implemented by the harness.
pub trait WorkerResolver: Send + Sync {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String>;
}

/// The built-in catalog under the lead's authority: a role runs on the
/// lead's routing (or an operator-approved route), with the lead's approval
/// policy, inside the role's tool ceiling and resource ceiling. It carries no
/// profile, so it selects no profile's data.
#[derive(Clone, Debug)]
pub struct BuiltinResolver {
    /// The lead's routing, inherited by every role without a route.
    pub routing: ModelRouting,
    pub approval: ApprovalPolicy,
    /// Roles the operator enabled.
    pub roles: BTreeSet<WorkerRole>,
    /// Operator routes already checked against the approved models.
    pub routes: BTreeMap<WorkerRole, String>,
}

impl BuiltinResolver {
    /// Every role enabled, on the lead's routing, with no routes.
    pub fn new(routing: ModelRouting, approval: ApprovalPolicy) -> Self {
        Self {
            routing,
            approval,
            roles: WorkerRole::ALL.into_iter().collect(),
            routes: BTreeMap::new(),
        }
    }

    /// The spec of `role`.
    pub fn spec(&self, role: WorkerRole) -> WorkerSpec {
        let worker = workers::builtin(role);
        let mut routing = self.routing.clone();
        let model_source = match self.routes.get(&role) {
            Some(model) => {
                routing.model = model.clone();
                "role_route"
            }
            None => "lead",
        };
        WorkerSpec {
            label: worker.label(),
            role: Some(role),
            persona: worker.persona(),
            routing,
            model_source,
            ceiling: Some(worker.toolsets()),
            toolsets: None,
            approval: self.approval,
            limits: worker.limits(),
            skills: vec![worker.skill()],
            nested: false,
            typed_result: true,
            validity: None,
            notes: Vec::new(),
        }
    }
}

impl WorkerResolver for BuiltinResolver {
    fn resolve(&self, target: WorkerTarget<'_>) -> Result<WorkerSpec, String> {
        match target {
            WorkerTarget::Role(raw) => match WorkerRole::parse(raw) {
                Some(role) if self.roles.contains(&role) => Ok(self.spec(role)),
                _ => Err(format!("worker role '{raw}' is not allowed")),
            },
            WorkerTarget::Profile(_) => Err("this run delegates by role only".to_owned()),
        }
    }
}
