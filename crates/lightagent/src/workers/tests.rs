use super::*;

use lightagent_core::identity::{Caller, PrincipalId, PrincipalSource, Role};

struct Home {
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "lightagent-workers-{}",
            lightagent_core::RunId::new().as_str()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn store(&self) -> ProfileStore {
        ProfileStore::new(&self.root)
    }

    fn profile(&self, id: &str, edit: impl FnOnce(&mut AgentProfile)) -> AgentProfile {
        let mut profile = AgentProfile::new(
            ProfileId::new(id).unwrap(),
            id,
            format!("You are {id}."),
            "profile-model",
        );
        edit(&mut profile);
        self.store().save(&profile).unwrap();
        profile
    }

    /// The assignments of `profile`, written directly.
    fn assignments(&self, profile: &ProfileId) -> WorkerAssignments {
        let store = self.store();
        let handle = store.handle(profile);
        WorkerAssignments::new(profile.clone(), handle.dir())
    }

    /// Record `owner` (a user, created when missing) as the owner of `profile`.
    fn own(&self, profile: &str, owner: &str) {
        let store = IdentityStore::new(&self.root);
        let operator = Caller::local_operator(PrincipalSource::LocalProcess);
        let owner = PrincipalId::new(owner).unwrap();
        store
            .transact(&operator, 1_800_000_000, None, |tx| {
                if tx.document().principal(&owner).is_none() {
                    tx.create_principal(owner.clone(), Role::User, None)?;
                }
                tx.assign_owner(&ProfileId::new(profile).unwrap(), &owner)?;
                Ok(())
            })
            .unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn id(raw: &str) -> ProfileId {
    ProfileId::new(raw).unwrap()
}

fn to(profile: &str) -> AssignmentTarget {
    AssignmentTarget::Profile {
        profile: id(profile),
    }
}

fn resolver(home: &Home, config: &Config, lead: &AgentProfile) -> HarnessResolver {
    HarnessResolver::new(&home.root, config, lead, RunTrust::LocalOperator)
}

fn researcher(resolver: &HarnessResolver) -> Result<WorkerSpec, String> {
    resolver.resolve(WorkerTarget::Role("researcher"))
}

fn tools(names: &[&str]) -> ToolRegistry {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    ToolRegistry::builtin().scoped(&names)
}

#[test]
fn a_role_resolves_to_the_built_in_under_the_leads_authority() {
    let home = Home::new();
    let config = Config::default();
    let lead = home.profile("lead", |lead| lead.limits.max_turns = 3);
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.label, "builtin:researcher@1");
    assert_eq!(spec.role, Some(WorkerRole::Researcher));
    assert_eq!(spec.routing.model, "profile-model");
    assert_eq!(spec.model_source, "lead");
    assert_eq!(spec.skill_labels(), ["worker-researcher@1"]);
    assert!(spec.typed_result && !spec.nested);
    assert!(spec.within_ceiling("web.fetch") && !spec.within_ceiling("fs.write"));
    assert_eq!(
        spec.limits.max_turns, 3,
        "the lead's limits bound a built-in"
    );
    assert!(spec.validity.is_none());
    // Resolving creates no profile.
    let store = home.store();
    assert!(store.load(&id("researcher")).is_err());
}

#[test]
fn a_generated_legacy_profile_is_superseded_and_a_customized_one_is_kept() {
    let home = Home::new();
    let lead = home.profile("lead", |_| {});
    let mut legacy = Config::default();
    legacy.subagents.resolution = WorkerResolution::LegacyProfiles;
    let store = home.store();
    crate::ensure_subagent_profiles(&store, &legacy, &lead).unwrap();

    let config = Config::default();
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.label, "builtin:researcher@1");

    home.profile("researcher", |profile| {
        profile.persona = "My tuned researcher.".to_owned();
    });
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.label, "legacy_profile:researcher");
    assert_eq!(spec.notes, ["legacy_profile"]);
    assert!(spec.persona.starts_with("My tuned researcher."));
    assert!(spec.persona.contains("# Role skill: worker-researcher@1"));
    assert!(
        !spec.within_ceiling("fs.write"),
        "the role ceiling still applies"
    );
}

#[test]
fn an_assignment_resolves_first_and_its_withdrawal_stops_the_worker() {
    let home = Home::new();
    let config = Config::default();
    let lead = home.profile("lead", |_| {});
    home.profile("deep", |profile| {
        profile.persona = "Deep research.".to_owned();
    });
    let assigned = assign(&home.root, &lead.id, WorkerRole::Researcher, to("deep"));
    let assignment = assigned.unwrap();
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.label, format!("assigned:{}", assignment.id));
    assert!(spec.persona.starts_with("Deep research."));
    let validity = spec.validity.clone().expect("an assignment is re-checked");
    assert_eq!(validity(), Ok(()));

    let revoked = home.assignments(&lead.id).revoke(WorkerRole::Researcher);
    assert_eq!(revoked, Ok(true));
    assert!(validity().is_err(), "a withdrawn assignment refuses");
    // Without the assignment the role is the built-in again, for new
    // delegations only.
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.label, "builtin:researcher@1");
}

#[test]
fn a_worker_of_another_owner_is_refused_and_never_substituted() {
    let home = Home::new();
    let config = Config::default();
    let lead = home.profile("lead", |_| {});
    home.profile("theirs", |_| {});
    home.own("lead", "alice");
    home.own("theirs", "bob");
    let refused = assign(&home.root, &lead.id, WorkerRole::Researcher, to("theirs"));
    assert!(refused.is_err());

    // Written behind the CLI's back: resolution refuses it too, and does not
    // fall back to the built-in.
    let written = home
        .assignments(&lead.id)
        .assign(WorkerRole::Researcher, to("theirs"), 1);
    assert!(written.is_ok());
    let error = researcher(&resolver(&home, &config, &lead))
        .err()
        .expect("refused");
    assert!(error.contains("may not use"), "{error}");
    assert!(
        !error.contains("theirs") && !error.contains("bob"),
        "{error}"
    );

    // The same owner is accepted.
    home.profile("mine", |_| {});
    home.own("mine", "alice");
    let mine = assign(&home.root, &lead.id, WorkerRole::Researcher, to("mine"));
    assert!(mine.is_ok());
}

#[test]
fn the_legacy_profile_argument_is_for_trusted_operator_runs_only() {
    let home = Home::new();
    let lead = home.profile("lead", |_| {});
    home.profile("worker", |_| {});
    let config = Config::default();
    let operator = resolver(&home, &config, &lead);
    let spec = operator.resolve(WorkerTarget::Profile("worker")).unwrap();
    assert_eq!(spec.label, "profile:worker");
    assert!(!spec.typed_result);
    assert!(operator.resolve(WorkerTarget::Profile("lead")).is_err());

    let remote = HarnessResolver::new(&home.root, &config, &lead, RunTrust::RemoteUser);
    assert!(remote.resolve(WorkerTarget::Profile("worker")).is_err());
    let mut off = Config::default();
    off.subagents.legacy_profile_argument = false;
    let off = resolver(&home, &off, &lead);
    let refused = off.resolve(WorkerTarget::Profile("worker"));
    assert!(refused.is_err());
}

#[test]
fn a_role_route_applies_only_for_an_approved_model() {
    let home = Home::new();
    let lead = home.profile("lead", |_| {});
    let mut config = Config::default();
    config
        .subagents
        .role_models
        .insert("researcher".to_owned(), "research-model".to_owned());
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.routing.model, "profile-model");
    assert_eq!(spec.notes, ["model_route_not_approved"]);

    config.subagents.approved_models = vec!["research-model".to_owned()];
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.routing.model, "research-model");
    assert_eq!(spec.model_source, "role_route");
    assert!(spec.notes.is_empty());
}

#[test]
fn a_worker_never_receives_the_leads_own_tools() {
    let lead = tools(&["datetime.now", "web.fetch", "agent.delegate"]);
    assert_eq!(worker_base(&lead).names(), ["datetime.now", "web.fetch"]);
    for name in ["jev.classify_task", "memory.write", "memory.reflect"] {
        assert!(LEAD_ONLY_TOOLS.contains(&name));
    }
}

#[test]
fn readiness_reports_each_role_with_its_reasons() {
    let home = Home::new();
    let lead = home.profile("lead", |_| {});
    let config = Config::default();
    let lead_tools = tools(&[
        "datetime.now",
        "web.fetch",
        "web.search",
        "fs.read",
        "fs.list",
        "skill.read",
    ]);
    let operator = RunTrust::LocalOperator;
    let report = readiness(&home.root, &config, &lead, operator, &lead_tools);
    let entry = |role: WorkerRole| report.iter().find(|entry| entry.role == role).unwrap();
    let researcher = entry(WorkerRole::Researcher);
    assert_eq!(researcher.state, Readiness::Ready);
    assert!(researcher.reasons.is_empty());
    assert_eq!(researcher.skill.as_deref(), Some("worker-researcher@1"));
    assert_eq!(researcher.model.as_deref(), Some("profile-model"));
    assert!(!researcher.tools.contains(&"fs.read".to_owned()));
    let coder = entry(WorkerRole::Coder);
    assert_eq!(coder.state, Readiness::Degraded);
    assert_eq!(coder.reasons, ["workspace_write"]);
    assert_eq!(entry(WorkerRole::Reviewer).state, Readiness::Ready);
    assert_eq!(entry(WorkerRole::Tester).state, Readiness::Unavailable);

    let remote = RunTrust::RemoteUser;
    let report = readiness(&home.root, &config, &lead, remote, &lead_tools);
    let denied = Readiness::PermissionDenied;
    assert!(report.iter().all(|entry| entry.state == denied));
    let mut off = Config::default();
    off.subagents.enabled = false;
    let report = readiness(&home.root, &off, &lead, operator, &lead_tools);
    assert!(
        report
            .iter()
            .all(|entry| entry.state == Readiness::Disabled)
    );
}

#[test]
fn a_private_worker_must_use_the_leads_endpoint_and_an_approved_model() {
    let home = Home::new();
    let lead = home.profile("lead", |_| {});
    let mut config = Config::default();
    home.profile("elsewhere", |profile| {
        profile.routing.base_url = Some("https://other.example/v1".to_owned());
    });
    home.profile("odd-model", |profile| {
        profile.routing.model = "unlisted-model".to_owned();
    });
    for (worker, why) in [("elsewhere", "endpoint"), ("odd-model", "not approved")] {
        let assigned = assign(&home.root, &lead.id, WorkerRole::Researcher, to(worker));
        assert!(assigned.is_ok());
        let error = researcher(&resolver(&home, &config, &lead))
            .err()
            .expect("refused, not replaced by the built-in");
        assert!(error.contains(why), "{error}");
    }
    // The installation approving the model makes it usable.
    config.subagents.approved_models = vec!["unlisted-model".to_owned()];
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.routing.model, "unlisted-model");
}

#[test]
fn a_private_worker_never_runs_looser_or_longer_than_its_lead() {
    let home = Home::new();
    let config = Config::default();
    let lead = home.profile("lead", |lead| {
        lead.approval_policy = lightagent_core::ApprovalPolicy::Strict;
        lead.limits.max_tool_calls = 4;
    });
    home.profile("loose", |profile| {
        profile.approval_policy = lightagent_core::ApprovalPolicy::Permissive;
        profile.limits.max_tool_calls = 100;
    });
    let assigned = assign(&home.root, &lead.id, WorkerRole::Researcher, to("loose"));
    assert!(assigned.is_ok());
    let spec = researcher(&resolver(&home, &config, &lead)).unwrap();
    assert_eq!(spec.approval, lightagent_core::ApprovalPolicy::Strict);
    assert_eq!(spec.limits.max_tool_calls, 4);
}

#[test]
fn a_customized_legacy_profile_that_fails_a_check_is_refused_not_replaced() {
    let home = Home::new();
    let config = Config::default();
    let lead = home.profile("lead", |_| {});
    home.profile("researcher", |profile| {
        profile.persona = "Tuned.".to_owned();
        profile.routing.base_url = Some("https://other.example/v1".to_owned());
    });
    let error = researcher(&resolver(&home, &config, &lead))
        .err()
        .expect("refused");
    assert!(error.contains("endpoint"), "{error}");
}
