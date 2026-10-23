//! Cross-profile isolation of delegated workers.
//!
//! Two owners, each with a lead profile and data of their own: durable
//! memory, saved sessions, a workspace and assignments. Workers really run
//! through the one worker-runtime path every surface uses
//! ([`super::delegation`] and `agent.delegate`), over each lead's own
//! configured, profile-scoped tools, with a scripted model. The checks are
//! about what a worker can reach, so the model only asks; the tools, the
//! resolver, the identity store and the approval engine are real.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use lightagent_core::identity::store::IdentityStore;
use lightagent_core::identity::{Caller, Denial, PrincipalId, PrincipalSource, RunAuthorization};
use lightagent_core::workers::WorkerRole;
use lightagent_core::{
    AgentProfile, AgentProvider, AssignmentTarget, Config, FinishReason, MockProvider,
    ModelRouting, ProfileId, ProfileStore, ProviderError, ProviderEvent, RiskClass, RunId, Scope,
    ToolOutcome,
};
use lightagent_store::{Session, SessionStore, StoredMessage};
use lightagent_tools::builtins::{
    AgentDeclineWorker, AgentDelegate, AgentResumeWorker, PatchApply,
};
use lightagent_tools::{Tool, ToolCtx, ToolDefinition, ToolRegistry};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use lightagent_core::identity::RunTrust;

/// Hands out clones of one scripted worker model.
struct Scripted(MockProvider);

impl lightagent_core::ProviderFactory for Scripted {
    fn provider(&self, _routing: &ModelRouting) -> Result<Arc<dyn AgentProvider>, ProviderError> {
        Ok(Arc::new(self.0.clone()))
    }
}

/// An installation with two owners' profiles.
struct World {
    root: PathBuf,
    config: Config,
}

impl World {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("lightagent-isolation-{}", RunId::new().as_str()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.tools.enabled = true;
        Self { root, config }
    }

    fn store(&self) -> ProfileStore {
        ProfileStore::new(&self.root)
    }

    /// Save a profile `id` owned by `owner`.
    fn profile(&self, id: &str, owner: &str) -> AgentProfile {
        let profile = AgentProfile::new(
            ProfileId::new(id).unwrap(),
            id,
            format!("You are {id}."),
            "profile-model",
        );
        self.store().save(&profile).unwrap();
        self.own(id, owner);
        profile
    }

    /// Make `owner` (a user, created when missing) the owner of `profile`.
    fn own(&self, profile: &str, owner: &str) {
        let store = IdentityStore::new(&self.root);
        let operator = Caller::local_operator(PrincipalSource::LocalProcess);
        let owner = PrincipalId::new(owner).unwrap();
        store
            .transact(&operator, 1_800_000_000, None, |tx| {
                if tx.document().principal(&owner).is_none() {
                    tx.create_principal(
                        owner.clone(),
                        lightagent_core::identity::Role::User,
                        None,
                    )?;
                }
                tx.assign_owner(&ProfileId::new(profile).unwrap(), &owner)?;
                Ok(())
            })
            .unwrap();
    }

    fn dir(&self, profile: &AgentProfile) -> PathBuf {
        self.store().handle(&profile.id).dir().to_path_buf()
    }

    fn workspace(&self, profile: &AgentProfile) -> PathBuf {
        self.store().handle(&profile.id).workspace_dir()
    }

    /// Remember `text` in `profile`'s durable memory, through its own tool.
    async fn remember(&self, profile: &AgentProfile, text: &str) {
        let tools = crate::memory::memory_tools(&self.dir(profile), &self.config);
        let write = tools
            .iter()
            .find(|tool| tool.definition().name == "memory.write")
            .unwrap();
        let outcome = write
            .call(
                &json!({ "text": text }),
                &ToolCtx::new(CancellationToken::new()),
            )
            .await;
        assert!(!outcome.is_error, "{}", outcome.content);
    }

    /// A saved session of `profile` holding `text`; its id.
    fn session(&self, profile: &AgentProfile, text: &str) -> String {
        let store = SessionStore::new(self.dir(profile).join("sessions"));
        let mut session = Session::new(profile.id.as_str(), "private");
        session.push_message(StoredMessage::new("user", text));
        store.save(&session).unwrap();
        session.id.as_str().to_owned()
    }

    /// A run of `lead`: its own configured tools (adjusted by `edit`), its
    /// delegation through the shared worker-runtime factory, its workspace
    /// and patch store, under `authorization`.
    async fn run(
        &self,
        lead: &AgentProfile,
        worker: &MockProvider,
        authorization: Option<RunAuthorization>,
        edit: impl FnOnce(&mut ToolRegistry),
    ) -> ToolCtx {
        let profile_dir = self.dir(lead);
        let extensions = crate::chat::load_extensions(&self.root, &profile_dir);
        let mut registry = crate::chat::configured_registry_for(
            &self.config,
            &profile_dir,
            &extensions,
            false,
            crate::chat::RemoteAccess::Operator,
        )
        .await;
        edit(&mut registry);
        let delegation = super::delegation(super::DelegationInputs {
            root: &self.root,
            config: &self.config,
            lead,
            trust: RunTrust::LocalOperator,
            extensions: &extensions,
            registry: &registry,
            guard: None,
            enforced: false,
            factory: Arc::new(Scripted(worker.clone())),
        });
        let workspace = crate::chat::workspace_context(&self.config, self.workspace(lead)).unwrap();
        let mut ctx = ToolCtx::new(CancellationToken::new())
            .with_delegation(delegation)
            .with_workspace(workspace)
            .with_patches(lightagent_tools::builtins::PatchContext::default());
        if let Some(authorization) = authorization {
            ctx = ctx.with_authorization(authorization);
        }
        ctx
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn call(id: &str, name: &str, args: Value) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some(name.into()),
            arguments: Some(args.to_string()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage: None,
        },
    ]
}

fn answer(text: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Content(text.into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: None,
        },
    ]
}

async fn delegate(ctx: &ToolCtx, args: Value) -> (ToolOutcome, Value) {
    let outcome = AgentDelegate::new().call(&args, ctx).await;
    let report = serde_json::from_str(&outcome.content).unwrap_or(Value::Null);
    (outcome, report)
}

/// Everything the worker's model was shown, as one text.
fn seen_by(worker: &MockProvider) -> String {
    worker
        .requests()
        .iter()
        .flat_map(|request| request.messages.iter())
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_workers_memory_and_sessions_are_its_own_leads_only() {
    let world = World::new();
    let alice = world.profile("iso-mem-alice", "alice");
    world.profile("iso-mem-alice-helper", "alice");
    let bob = world.profile("iso-mem-bob", "bob");
    world
        .remember(&alice, "The alice secret code is ALICE-FACT.")
        .await;
    world
        .remember(&bob, "The bob secret code is BOB-FACT.")
        .await;
    let bobs_session = world.session(&bob, "BOB-SESSION-SECRET");

    // A worker of Alice's run asks for every secret code and for Bob's
    // session by its exact id: it reads through Alice's tools only.
    let worker = MockProvider::new(vec![
        call("w1", "memory.search", json!({ "query": "secret code" })),
        call(
            "w2",
            "session.lookup",
            json!({ "session_id": bobs_session }),
        ),
        answer("done"),
    ]);
    let ctx = world.run(&alice, &worker, None, |_| {}).await;
    let (outcome, _) = delegate(
        &ctx,
        json!({ "profile": "iso-mem-alice-helper", "task": "Find every secret code." }),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.content);
    let seen = seen_by(&worker);
    assert!(seen.contains("ALICE-FACT"), "{seen}");
    for leaked in ["BOB-FACT", "BOB-SESSION-SECRET"] {
        assert!(!seen.contains(leaked), "{leaked} reached Alice's worker");
    }
}

#[tokio::test]
async fn another_owners_profile_is_never_a_worker_and_is_never_named() {
    let world = World::new();
    let alice = world.profile("iso-guess-alice", "alice");
    let _bob = world.profile("iso-guess-bob", "bob");
    world.profile("iso-guess-bob-helper", "bob");
    let worker = MockProvider::new(vec![answer("should never run")]);
    let ctx = world.run(&alice, &worker, None, |_| {}).await;

    // A guessed legacy profile argument, and Alice's own lead.
    for target in ["iso-guess-bob-helper", "iso-guess-bob", "iso-guess-alice"] {
        let (outcome, _) = delegate(&ctx, json!({ "profile": target, "task": "Help." })).await;
        assert!(outcome.is_error, "{target}");
        assert!(
            !outcome.content.contains("iso-guess-bob"),
            "{}",
            outcome.content
        );
    }
    // An assignment written to point at Bob's profile (the CLI refuses to
    // create one) is refused at resolution, never replaced by the built-in.
    let refused = super::assign(
        &world.root,
        &alice.id,
        WorkerRole::Researcher,
        AssignmentTarget::Profile {
            profile: ProfileId::new("iso-guess-bob-helper").unwrap(),
        },
    );
    assert!(refused.is_err());
    let dir = world.dir(&alice);
    lightagent_core::WorkerAssignments::new(alice.id.clone(), &dir)
        .assign(
            WorkerRole::Researcher,
            AssignmentTarget::Profile {
                profile: ProfileId::new("iso-guess-bob-helper").unwrap(),
            },
            1,
        )
        .unwrap();
    let (outcome, _) = delegate(&ctx, json!({ "role": "researcher", "task": "Help." })).await;
    assert!(outcome.is_error);
    assert!(
        !outcome.content.contains("iso-guess-bob"),
        "{}",
        outcome.content
    );
    assert!(worker.requests().is_empty(), "no worker ever started");
}

#[tokio::test]
async fn a_workers_workspace_is_its_leads_and_stays_confined() {
    let world = World::new();
    let alice = world.profile("iso-ws-alice", "alice");
    let bob = world.profile("iso-ws-bob", "bob");
    std::fs::create_dir_all(world.workspace(&alice)).unwrap();
    std::fs::create_dir_all(world.workspace(&bob)).unwrap();
    std::fs::write(world.workspace(&alice).join("notes.txt"), "ALICE-NOTE").unwrap();
    let bobs_file = world.workspace(&bob).join("secret.txt");
    std::fs::write(&bobs_file, "BOB-NOTE").unwrap();
    let climb = format!("../../{}/workspace/secret.txt", bob.id.as_str());

    let worker = MockProvider::new(vec![
        call(
            "w1",
            "fs.read",
            json!({ "path": bobs_file.display().to_string() }),
        ),
        call("w2", "fs.read", json!({ "path": climb })),
        call("w3", "fs.read", json!({ "path": "notes.txt" })),
        answer("done"),
    ]);
    let ctx = world.run(&alice, &worker, None, |_| {}).await;
    let (outcome, report) = delegate(
        &ctx,
        json!({ "role": "reviewer", "task": "Read the notes." }),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.content);
    let calls = report["tool_calls"].as_array().unwrap();
    let ok: Vec<bool> = calls.iter().map(|call| call["ok"] == true).collect();
    assert_eq!(ok, [false, false, true], "{calls:?}");
    let seen = seen_by(&worker);
    assert!(seen.contains("ALICE-NOTE"));
    assert!(
        !seen.contains("BOB-NOTE"),
        "Bob's file reached Alice's worker"
    );
}

#[tokio::test]
async fn patches_and_parked_approvals_belong_to_the_run_that_made_them() {
    let world = World::new();
    let alice = world.profile("iso-park-alice", "alice");
    let bob = world.profile("iso-park-bob", "bob");
    std::fs::create_dir_all(world.workspace(&alice)).unwrap();
    std::fs::write(world.workspace(&alice).join("a.txt"), "old").unwrap();

    // Alice's coder proposes a patch, then reaches a write that needs a
    // person's approval and is parked under a one-time handle.
    let alices_worker = MockProvider::new(vec![
        call(
            "w1",
            "patch.propose",
            json!({ "summary": "update", "changes": [{ "path": "a.txt", "content": "new" }] }),
        ),
        call(
            "w2",
            "fs.write",
            json!({ "path": "a.txt", "content": "direct" }),
        ),
        answer("continued without the write"),
    ]);
    let alices_run = world.run(&alice, &alices_worker, None, |_| {}).await;
    let (_, report) = delegate(
        &alices_run,
        json!({ "role": "coder", "task": "Update a.txt.", "write_scope": ["a.txt"] }),
    )
    .await;
    assert_eq!(report["status"], "awaiting_approval", "{report}");
    let handle = report["approval"]["handle"].as_str().unwrap().to_owned();
    let patch_id = seen_by(&alices_worker)
        .split('"')
        .find(|part| part.starts_with("pt-"))
        .unwrap()
        .to_owned();

    // Bob's run holds neither: the handle and the patch id resolve to
    // nothing there, and using them takes nothing away from Alice's run.
    let bobs_worker = MockProvider::new(Vec::new());
    let bobs_run = world.run(&bob, &bobs_worker, None, |_| {}).await;
    let resumed = AgentResumeWorker::new()
        .call(&json!({ "handle": handle }), &bobs_run)
        .await;
    assert!(resumed.is_error);
    assert!(
        resumed.content.contains("no worker is waiting"),
        "{}",
        resumed.content
    );
    let applied = PatchApply::new()
        .call(&json!({ "patch_id": patch_id }), &bobs_run)
        .await;
    assert!(applied.is_error);
    assert_eq!(
        std::fs::read_to_string(world.workspace(&alice).join("a.txt")).unwrap(),
        "old"
    );
    let alices = alices_run.delegation.as_ref().unwrap();
    assert_eq!(alices.parked.len(), 1, "Alice's worker is still waiting");

    // Alice's own decision still reaches it, once.
    let declined = AgentDeclineWorker::new()
        .call(&json!({ "handle": handle }), &alices_run)
        .await;
    assert!(!declined.is_error, "{}", declined.content);
    let again = AgentResumeWorker::new()
        .call(&json!({ "handle": handle }), &alices_run)
        .await;
    assert!(again.is_error, "a handle is used once");
}

/// A stand-in for `datetime.now` that runs `effect` when called, so a test
/// can change ownership or authorization while a worker is between steps.
struct Midway {
    definition: ToolDefinition,
    effect: Box<dyn Fn() + Send + Sync>,
}

impl Midway {
    fn new(effect: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            definition: ToolDefinition::new(
                "datetime.now",
                "The current time.",
                json!({ "type": "object" }),
                RiskClass::Observe,
                vec![Scope::new("time:read")],
            ),
            effect: Box::new(effect),
        })
    }
}

#[async_trait]
impl Tool for Midway {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        (self.effect)();
        ToolOutcome::ok("2026-10-21T00:00:00Z")
    }
}

#[tokio::test]
async fn an_ownership_change_mid_run_stops_the_assigned_worker_and_withholds_it() {
    let world = World::new();
    let alice = world.profile("iso-own-alice", "alice");
    world.profile("iso-own-alice-helper", "alice");
    world.profile("iso-own-bob", "bob");
    super::assign(
        &world.root,
        &alice.id,
        WorkerRole::Researcher,
        AssignmentTarget::Profile {
            profile: ProfileId::new("iso-own-alice-helper").unwrap(),
        },
    )
    .unwrap();
    let worker = MockProvider::new(vec![
        call("w1", "datetime.now", json!({})),
        call("w2", "datetime.now", json!({})),
        answer("FINAL-ANSWER from a worker that was given away"),
    ]);
    let root = world.root.clone();
    let ctx = world
        .run(&alice, &worker, None, |registry| {
            // The helper changes hands while the worker is between steps.
            registry.insert(Midway::new(move || {
                let store = IdentityStore::new(&root);
                let operator = Caller::local_operator(PrincipalSource::LocalProcess);
                store
                    .transact(&operator, 1_800_000_001, None, |tx| {
                        tx.reassign_owner(
                            &ProfileId::new("iso-own-alice-helper").unwrap(),
                            &PrincipalId::new("bob").unwrap(),
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }));
        })
        .await;
    let (outcome, _) = delegate(
        &ctx,
        json!({ "role": "researcher", "task": "What time is it?" }),
    )
    .await;
    assert!(outcome.is_error, "{}", outcome.content);
    assert!(
        !outcome.content.contains("FINAL-ANSWER"),
        "{}",
        outcome.content
    );
    assert!(
        worker.requests().len() < 3,
        "the worker never got its last turn"
    );
}

#[tokio::test]
async fn a_revoked_run_stops_its_worker_mid_run_and_withholds_it() {
    let world = World::new();
    let alice = world.profile("iso-rev-alice", "alice");
    let revoked = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&revoked);
    let cancel = CancellationToken::new();
    let authorization = RunAuthorization::revocable(
        Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Err(Denial::Revoked)
            } else {
                Ok(())
            }
        }),
        cancel.clone(),
        Some(1),
    );
    let worker = MockProvider::new(vec![
        call("w1", "datetime.now", json!({})),
        call("w2", "datetime.now", json!({})),
        answer("FINAL-ANSWER after revocation"),
    ]);
    let trigger = Arc::clone(&revoked);
    let ctx = world
        .run(&alice, &worker, Some(authorization), |registry| {
            // The run's credential is revoked while its worker works.
            registry.insert(Midway::new(move || trigger.store(true, Ordering::SeqCst)));
        })
        .await;
    let (outcome, _) = delegate(
        &ctx,
        json!({ "role": "researcher", "task": "What time is it?" }),
    )
    .await;
    assert!(outcome.is_error, "{}", outcome.content);
    assert!(
        !outcome.content.contains("FINAL-ANSWER"),
        "{}",
        outcome.content
    );
    // And no new worker starts for the revoked run.
    let (again, _) = delegate(&ctx, json!({ "role": "researcher", "task": "Again." })).await;
    assert!(again.is_error);
}
