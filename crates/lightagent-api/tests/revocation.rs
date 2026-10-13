//! Revocation during a run's lifecycle, through the run manager (the
//! supervisor) and a factory that drives the real agent loop with the real
//! bounded executor. Identities, credentials and models are synthetic; every
//! wait is on an explicit signal or a terminal status, never on elapsed
//! time (a generous timeout only guards against a hang).

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("support/fixtures.rs");

// --- the lifecycle -------------------------------------------------------------

// Revocation before the run starts: no run is created, nothing runs.
#[tokio::test]
async fn revocation_before_a_run_starts_refuses_it() {
    let world = setup();
    // Authenticated, then revoked before the run is created.
    let alice = world.caller(&world.alice);
    world.revoke_credential(&world.alice);
    let started = world
        .start_with(alice, "alice-bot", "early", Script::new(Model::new(vec![])))
        .await;
    assert_eq!(started.err(), Some(Denial::Revoked));
    // A profile the caller no longer holds is refused the same way.
    world
        .identity
        .update(|state| state.ownership.remove(&profile("bob-bot")))
        .unwrap();
    let started = world
        .start(
            &world.bob,
            "bob-bot",
            "orphaned",
            Script::new(Model::new(vec![])),
        )
        .await;
    assert_eq!(started.err(), Some(Denial::ProfileUnowned));
    assert_eq!(world.lab.supervised.load(Ordering::SeqCst), 0);
}

// Revocation during model inference: the stream in flight is torn down.
#[tokio::test]
async fn revocation_during_inference_stops_the_model() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "think",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    let revoked = world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(revoked, vec![run.id().to_owned()]);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        model.dropped.load(Ordering::SeqCst),
        "inference was abandoned"
    );
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert_eq!(model.requests(), 1);
}

// Revocation before a tool invocation, observed by the run itself (the
// identity changed underneath it, as another process would change the
// store): the tool never runs and the model is not asked again.
#[tokio::test]
async fn revocation_before_a_tool_blocks_it_and_any_further_turn() {
    let world = setup();
    let identity = world.identity.clone();
    let alice_key = key_id(&world.alice);
    let model = Model::reacting(
        vec![Turn::Call("probe.run", "{}"), Turn::Say("done")],
        move |number| {
            if number == 1 {
                identity
                    .update(|state| state.credentials.revoke(&alice_key, T0 + 1))
                    .unwrap()
                    .unwrap();
            }
        },
    );
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "use a tool",
            Script::new(model.clone()).with_tool(probe.clone()),
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the tool never ran");
    assert_eq!(model.requests(), 1, "no model request after the revocation");
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert!(says_withdrawn(&run.events().await, "revoked"));
    // The refusal is audited once, with codes only.
    let refusals = world
        .identity
        .audit_log()
        .into_iter()
        .filter(|record| record.outcome == AuditOutcome::Denied(Denial::Revoked))
        .count();
    assert_eq!(refusals, 1);
}

// Revocation while a tool executes (by suspension: every credential of the
// principal): the call is torn down.
#[tokio::test]
async fn suspension_while_a_tool_executes_tears_it_down() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.hold", "{}")]);
    let probe = Probe::new("probe.hold", RiskClass::Observe, true);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "hold a tool",
            Script::new(model).with_tool(probe.clone()),
        )
        .await
        .unwrap();
    signalled(&probe.started).await;
    world
        .identity
        .update(|state| {
            state
                .credentials
                .revoke_principal(&principal("alice"), T0 + 1)
        })
        .unwrap();
    let revoked = world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Principal(principal("alice")),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        probe.dropped.load(Ordering::SeqCst),
        "the tool was torn down"
    );
    assert_eq!(probe.calls(), 1);
}

// Revocation during an approval wait ends the wait; the approval can no
// longer be delivered, and the awaited call never runs.
#[tokio::test]
async fn revocation_during_an_approval_wait_ends_it() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.mutate", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.mutate", RiskClass::Mutating, false);
    let mut script = Script::new(model).with_tool(probe.clone());
    script.strict = true;
    let run = world
        .start(&world.alice, "alice-bot", "ask first", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Profile(profile("alice-bot")),
            Denial::ProfileNotGranted,
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(!run.decide(ApprovalDecision::grant(pending.approval_id)));
    assert_eq!(probe.calls(), 0);
    assert!(says_withdrawn(&run.events().await, "profile_not_granted"));
}

// 12. A stale approval: decided (and delivered) after the identity changed
// but before the run observed it. The run checks before acting on it.
#[tokio::test]
async fn a_stale_approval_never_resumes_the_run() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.mutate", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.mutate", RiskClass::Mutating, false);
    let mut script = Script::new(model.clone()).with_tool(probe.clone());
    script.strict = true;
    let run = world
        .start(&world.alice, "alice-bot", "stale", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world.revoke_credential(&world.alice);
    assert!(
        run.decide(ApprovalDecision::grant(pending.approval_id)),
        "the change was not yet observed, so the decision is queued"
    );
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the approved call never ran");
    assert_eq!(model.requests(), 1);
    assert_eq!(run.revocation(), Some(Denial::Revoked));
}

// Profile reassignment while runs are active: the previous owner's run
// stops at the supervisor's re-check; the new owner's and unrelated runs
// continue.
#[tokio::test]
async fn reassignment_stops_only_the_previous_owners_run() {
    let world = setup();
    let alice_model = Model::new(vec![Turn::Hold]);
    let bob_model = Model::new(vec![Turn::Hold]);
    let alice_run = world
        .start(
            &world.alice,
            "alice-bot",
            "alice",
            Script::new(alice_model.clone()),
        )
        .await
        .unwrap();
    let bob_run = world
        .start(&world.bob, "bob-bot", "bob", Script::new(bob_model.clone()))
        .await
        .unwrap();
    signalled(&alice_model.started).await;
    signalled(&bob_model.started).await;
    world
        .identity
        .update(|state| {
            state
                .ownership
                .reassign(profile("alice-bot"), principal("bob"))
        })
        .unwrap();
    let revoked = world.manager.revalidate().await;
    assert_eq!(
        revoked,
        vec![(alice_run.id().to_owned(), Denial::ProfileNotGranted)]
    );
    assert_eq!(finished(&alice_run).await, RunStatus::Cancelled);
    assert_eq!(bob_run.status().await, RunStatus::Running);
    assert!(!bob_model.dropped.load(Ordering::SeqCst));
    // Nothing further is revoked by a second pass.
    assert!(world.manager.revalidate().await.is_empty());
    bob_run.cancel();
    assert_eq!(finished(&bob_run).await, RunStatus::Cancelled);
    assert_eq!(bob_run.revocation(), None, "cancelled, not revoked");
}

// 14. Concurrent runs: revoking one credential stops exactly the runs it
// started; another credential of the same user, other users and operators
// are unaffected.
#[tokio::test]
async fn concurrent_runs_are_revoked_precisely() {
    let world = setup();
    let mut runs = Vec::new();
    for (token, profile_id, message) in [
        (&world.alice, "alice-bot", "a1"),
        (&world.alice, "alice-bot", "a2"),
        (&world.alice_second, "alice-bot", "a3"),
        (&world.bob, "bob-bot", "b1"),
        (&world.ops, "alice-bot", "o1"),
    ] {
        let model = Model::new(vec![Turn::Hold]);
        let run = world
            .start(token, profile_id, message, Script::new(model.clone()))
            .await
            .unwrap();
        runs.push((message, run, model));
    }
    for (_, _, model) in &runs {
        signalled(&model.started).await;
    }
    let operator = world.operator();
    let mut revoked = world
        .manager
        .revoke(
            &operator,
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    revoked.sort();
    let mut expected: Vec<String> = runs[..2]
        .iter()
        .map(|(_, run, _)| run.id().to_owned())
        .collect();
    expected.sort();
    assert_eq!(revoked, expected);
    // Ownership and grants bound users' runs only: the operator's run on the
    // same profile is untouched.
    let by_profile = world
        .manager
        .affected_by(&RevocationScope::Profile(profile("alice-bot")))
        .await;
    assert!(
        by_profile
            .iter()
            .all(|run| run.owner().trust() == RunTrust::RemoteUser)
    );
    for (message, run, _) in &runs[..2] {
        assert_eq!(finished(run).await, RunStatus::Cancelled, "{message}");
    }
    for (message, run, model) in &runs[2..] {
        assert_eq!(run.status().await, RunStatus::Running, "{message}");
        assert!(!model.dropped.load(Ordering::SeqCst), "{message}");
        assert_eq!(run.revocation(), None);
    }
    // Revoking again finds nothing new.
    assert!(
        world
            .manager
            .revoke(
                &operator,
                &RevocationScope::Credential(key_id(&world.alice)),
                Denial::Revoked,
            )
            .await
            .unwrap()
            .is_empty()
    );
    for (_, run, _) in &runs[2..] {
        run.cancel();
        finished(run).await;
    }
}

// Only an operator holding `administer` may revoke through the supervisor.
#[tokio::test]
async fn only_an_operator_may_revoke_runs() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(&world.bob, "bob-bot", "mine", Script::new(model.clone()))
        .await
        .unwrap();
    signalled(&model.started).await;
    let scope = RevocationScope::Principal(principal("bob"));
    let user = world.caller(&world.alice);
    assert_eq!(
        world.manager.revoke(&user, &scope, Denial::Revoked).await,
        Err(Denial::MissingCapability)
    );
    let mut narrowed = world.operator();
    narrowed.restriction.capabilities = Some([Capability::RunsWrite].into());
    assert_eq!(
        world
            .manager
            .revoke(&narrowed, &scope, Denial::Revoked)
            .await,
        Err(Denial::MissingCapability)
    );
    assert_eq!(run.status().await, RunStatus::Running);
    // The operator's ordinary cancellation still works and is not a
    // revocation.
    run.cancel();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(run.revocation(), None);
}

// 8. Credential expiry during an open event stream: the expiring
// subscriber's stream ends at once while the run (started with another,
// longer credential) continues; the run stops when its own credential
// expires. An approval for a revoked run is refused as stale.
#[tokio::test]
async fn expiry_ends_streams_and_runs_and_stales_approvals() {
    let world = setup();
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "watched",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    let addr = serve(world.manager.clone(), world.auth.clone()).await;
    let mut stream = TcpStream::connect(&addr).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /api/lightagent/v1/runs/{}/events HTTP/1.1\r\nHost: local\r\nConnection: close\r\nAuthorization: Bearer {}\r\n\r\n",
                run.id(),
                world.alice_short
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains("run.started") {
        let read = tokio::time::timeout(HANG_GUARD, stream.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert!(read > 0, "the stream closed before any event");
        seen.extend_from_slice(&buffer[..read]);
    }
    // The subscriber's credential expires; the supervisor asks open
    // streams to re-check at once.
    world.clock.store(T0 + 100, Ordering::SeqCst);
    assert!(world.manager.revalidate().await.is_empty(), "the run holds");
    let mut rest = Vec::new();
    tokio::time::timeout(HANG_GUARD, stream.read_to_end(&mut rest))
        .await
        .expect("an expired subscriber's stream must end")
        .unwrap();
    assert_eq!(run.status().await, RunStatus::Running);

    // The run's own credential expires.
    world.clock.store(T0 + 1_000, Ordering::SeqCst);
    let revoked = world.manager.revalidate().await;
    assert_eq!(revoked, vec![(run.id().to_owned(), Denial::Expired)]);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    let (status, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{}", run.id()),
        &world.ops,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["revocation"],
        json!("expired")
    );
    let (status, body) = http(
        &addr,
        "POST",
        &format!("/api/lightagent/v1/approvals/{}", run.id()),
        &world.ops,
        Some(r#"{"approve":true}"#),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("withdrawn") && body.contains("expired"));
}

// 15. A persistent authority whose store becomes unreadable fails closed:
// the run stops at its next check.
#[tokio::test]
async fn an_unreadable_identity_store_stops_revocable_runs() {
    let root =
        std::env::temp_dir().join(format!("lightagent-revocation-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&root).unwrap();
    ProfileStore::new(&root)
        .create(&AgentProfile::new(
            profile("research"),
            "Research",
            "Persona",
            "fast",
        ))
        .unwrap();
    let operator = Caller::local_operator(PrincipalSource::LocalProcess);
    let store = IdentityStore::new(&root);
    let token = match store
        .transact(&operator, T0, None, |tx| {
            tx.create_principal(principal("alice"), Role::User, None)?;
            tx.assign_owner(&profile("research"), &principal("alice"))?;
            tx.issue_credential(&principal("alice"), Restriction::default(), None)
                .map(|(_, token)| token.expose().to_owned())
        })
        .unwrap()
    {
        lightagent_core::identity::store::Outcome::Applied { value, .. } => value,
        lightagent_core::identity::store::Outcome::Replayed { .. } => panic!("not replayed"),
    };
    let identity = Arc::new(IdentityAuthority::persistent(Arc::new(IdentityStore::new(
        &root,
    ))));
    let auth = AuthConfig::multi_user(identity.clone(), None);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let caller = auth.caller(&headers).unwrap();
    let lab = Arc::new(Lab::default());
    let (paced, _ticks) = manual_clock();
    let manager = RunManager::new(lab.clone()).with_revalidation(paced);
    let model = Model::new(vec![Turn::Hold]);
    lab.script("durable", Script::new(model.clone()));
    let owner = RunOwner::new(
        principal("alice"),
        Some(profile("research")),
        RunTrust::RemoteUser,
    );
    let generation = identity.generation();
    assert_eq!(generation, Some(1));
    let run = manager
        .start_supervised(
            StartRun {
                message: "durable".to_owned(),
                history: Vec::new(),
                profile: Some("research".to_owned()),
                model: None,
                cwd: None,
            },
            owner.clone(),
            identity.run_check(caller, owner),
            generation,
        )
        .await
        .unwrap();
    assert_eq!(run.authorization().generation(), Some(1));
    signalled(&model.started).await;
    assert!(manager.revalidate().await.is_empty());
    std::fs::write(root.join("identity").join("state.json"), b"{corrupt").unwrap();
    let revoked = manager.revalidate().await;
    assert_eq!(revoked.len(), 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    std::fs::remove_dir_all(&root).ok();
}

// 18. The single-user API is unchanged: runs are not revocable, never
// checked, and report no revocation.
#[tokio::test]
async fn single_user_runs_are_unchanged() {
    let lab = Arc::new(Lab::default());
    let manager = RunManager::new(lab.clone());
    let model = Model::new(vec![Turn::Call("probe.run", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    lab.script("hello", Script::new(model.clone()).with_tool(probe.clone()));
    let addr = serve(manager.clone(), AuthConfig::open()).await;
    let (status, body) = http(
        &addr,
        "POST",
        "/api/lightagent/v1/runs",
        "",
        Some(r#"{"message":"hello"}"#),
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run = manager.get(&id).await.unwrap();
    assert_eq!(finished(&run).await, RunStatus::Completed);
    assert_eq!(probe.calls(), 1);
    assert_eq!(model.requests(), 2);
    assert!(!run.authorization().is_revocable());
    assert_eq!(run.authorization().checks(), 0);
    assert_eq!(lab.plain.load(Ordering::SeqCst), 1);
    assert_eq!(lab.supervised.load(Ordering::SeqCst), 0);
    assert!(manager.revalidate().await.is_empty());
    let (_, body) = http(
        &addr,
        "GET",
        &format!("/api/lightagent/v1/runs/{id}"),
        "",
        None,
    )
    .await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert!(body.get("revocation").is_none(), "{body}");
}

// 11. Delegated workers: none starts for a revoked run; one in flight is
// torn down with its run; and the worker's own tools and model turns check
// the run's authorization.
#[tokio::test]
async fn delegated_workers_follow_the_runs_authorization() {
    let root =
        std::env::temp_dir().join(format!("lightagent-delegation-{}", RunId::new().as_str()));
    let profiles = ProfileStore::new(root.clone());
    let mut worker_profile = AgentProfile::new(profile("worker"), "Worker", "You help.", "w");
    worker_profile.approval_policy = lightagent_core::ApprovalPolicy::Permissive;
    profiles.save(&worker_profile).unwrap();
    let delegation = |worker: &Model, tools: ToolRegistry| {
        Delegation::new(
            Arc::new(ProfileStore::new(root.clone())),
            Arc::new(worker.clone()),
            tools,
            Duration::from_secs(60),
            65_536,
        )
    };
    let delegate = r#"{"profile":"worker","task":"help"}"#;
    let mut builtin = ToolRegistry::new();
    builtin.insert(Arc::new(lightagent_tools::builtins::AgentDelegate::new()));

    // In flight: the run approves the delegation, the worker's model holds,
    // and revoking the run tears the worker down.
    let world = setup();
    let worker = Model::new(vec![Turn::Hold]);
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate)]);
    let mut script = Script::new(parent);
    script.tools = builtin.clone();
    script.delegation = Some(delegation(&worker, ToolRegistry::new()));
    let run = world
        .start(&world.alice, "alice-bot", "delegate", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    assert!(run.decide(ApprovalDecision::grant(pending.approval_id)));
    signalled(&worker.started).await;
    world
        .manager
        .revoke(
            &world.operator(),
            &RevocationScope::Credential(key_id(&world.alice)),
            Denial::Revoked,
        )
        .await
        .unwrap();
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        worker.dropped.load(Ordering::SeqCst),
        "the worker was torn down"
    );

    // Revoked before delegation is approved: no worker starts.
    let worker = Model::new(vec![Turn::Say("never")]);
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate)]);
    let mut script = Script::new(parent);
    script.tools = builtin.clone();
    script.delegation = Some(delegation(&worker, ToolRegistry::new()));
    let run = world
        .start(&world.bob, "bob-bot", "late delegate", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world.revoke_credential(&world.bob);
    run.decide(ApprovalDecision::grant(pending.approval_id));
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(worker.requests(), 0, "no worker was started");

    // Revoked mid-worker, observed by the worker's own checks.
    let world = setup();
    let identity = world.identity.clone();
    let alice_key = key_id(&world.alice);
    let worker = Model::reacting(vec![Turn::Call("probe.run", "{}")], move |_| {
        identity
            .update(|state| state.credentials.revoke(&alice_key, T0 + 1))
            .unwrap()
            .unwrap();
    });
    let probe = Probe::new("probe.run", RiskClass::Observe, false);
    let mut worker_tools = ToolRegistry::new();
    worker_tools.insert(probe.clone());
    let parent = Model::new(vec![Turn::Call("agent.delegate", delegate), Turn::Say("x")]);
    let mut script = Script::new(parent.clone());
    script.tools = builtin;
    script.delegation = Some(delegation(&worker, worker_tools));
    let run = world
        .start(&world.alice, "alice-bot", "mid worker", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    assert!(run.decide(ApprovalDecision::grant(pending.approval_id)));
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(probe.calls(), 0, "the worker's tool never ran");
    assert_eq!(worker.requests(), 1, "the worker took no second turn");
    assert_eq!(parent.requests(), 1, "the run took no second turn");
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    std::fs::remove_dir_all(&root).ok();
}
