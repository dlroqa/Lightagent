//! The bounded revalidation monitor: a revocation reaches a run that takes
//! no action (a long model stream, a held tool, a worker, an approval
//! wait). The monitor is paced by a clock the test drives, so each sweep
//! happens exactly when the test says; nothing waits on elapsed time except
//! the ignored latency measurement at the end, which runs in its own CI job.

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("support/fixtures.rs");

use lightagent_api::manager::{
    DEFAULT_REVALIDATION_INTERVAL, MAX_REVALIDATION_INTERVAL, MIN_REVALIDATION_INTERVAL,
    RevalidationInterval,
};

/// Start a run holding the model stream and wait until the model is asked.
async fn holding(
    world: &World,
    token: &str,
    profile_id: &str,
    message: &str,
) -> (Arc<RunState>, Model) {
    let model = Model::new(vec![Turn::Hold]);
    let run = world
        .start(token, profile_id, message, Script::new(model.clone()))
        .await
        .unwrap();
    signalled(&model.started).await;
    (run, model)
}

// Revocation during an idle model stream: no action happens, yet the next
// sweep revokes the run and tears the stream down.
#[tokio::test]
async fn an_idle_model_stream_is_revoked_at_the_next_sweep() {
    let world = setup();
    let (run, model) = holding(&world, &world.alice, "alice-bot", "idle").await;
    // Changed underneath the run (as another process would): nothing yet.
    world.revoke_credential(&world.alice);
    assert_eq!(run.status().await, RunStatus::Running);
    let state = tick(&world.manager, &world.ticks).await;
    assert_eq!(state.revoked, 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        model.dropped.load(Ordering::SeqCst),
        "the stream was torn down"
    );
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert_eq!(model.requests(), 1, "no further model request");
}

// Revocation during continuous token streaming: the stream stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continuous_streaming_is_revoked_and_stops() {
    let world = setup();
    let model = Model::new(vec![Turn::Tokens]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "talk",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    // Tokens are flowing.
    tokio::time::timeout(HANG_GUARD, async {
        while model.tokens.load(Ordering::SeqCst) < 10 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    world.revoke_credential(&world.alice);
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(model.dropped.load(Ordering::SeqCst));
    let after = model.tokens.load(Ordering::SeqCst);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(model.tokens.load(Ordering::SeqCst), after, "no token after");
}

// Cancellation during provider connection setup: a connection that never
// completes is abandoned.
#[tokio::test]
async fn a_connection_being_set_up_is_abandoned() {
    let world = setup();
    let model = Model::new(vec![Turn::Connect]);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "connect",
            Script::new(model.clone()),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    world.revoke_credential(&world.alice);
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        model.dropped.load(Ordering::SeqCst),
        "the connect was dropped"
    );
}

// Revocation during an approval wait, by profile reassignment: the wait
// ends, the call never runs, and a late decision is refused.
#[tokio::test]
async fn an_approval_wait_is_revoked_by_reassignment() {
    let world = setup();
    let model = Model::new(vec![Turn::Call("probe.mutate", "{}"), Turn::Say("done")]);
    let probe = Probe::new("probe.mutate", RiskClass::Mutating, false);
    let mut script = Script::new(model).with_tool(probe.clone());
    script.strict = true;
    let run = world
        .start(&world.alice, "alice-bot", "ask", script)
        .await
        .unwrap();
    let (bob_run, _) = holding(&world, &world.bob, "bob-bot", "bob").await;
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    world
        .identity
        .update(|state| {
            state
                .ownership
                .reassign(profile("alice-bot"), principal("bob"))
        })
        .unwrap();
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(run.revocation(), Some(Denial::ProfileNotGranted));
    assert!(!run.decide(ApprovalDecision::grant(pending.approval_id)));
    assert_eq!(probe.calls(), 0);
    assert_eq!(bob_run.status().await, RunStatus::Running, "bob unaffected");
    bob_run.cancel();
    finished(&bob_run).await;
}

// Revocation during a long tool call (a held remote request): the call is
// torn down. (The HTTP MCP and web request futures are torn down the same
// way: `lightagent-mcp` and `lightagent-tools` revocation tests.)
#[tokio::test]
async fn a_long_tool_call_is_revoked() {
    let world = setup();
    let probe = Probe::new("probe.hold", RiskClass::External, true);
    let run = world
        .start(
            &world.alice,
            "alice-bot",
            "long call",
            Script::new(Model::new(vec![Turn::Call("probe.hold", "{}")])).with_tool(probe.clone()),
        )
        .await
        .unwrap();
    signalled(&probe.started).await;
    world.revoke_credential(&world.alice);
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(probe.dropped.load(Ordering::SeqCst));
    assert_eq!(probe.calls(), 1, "no further call");
}

// Revocation during delegation: the worker's held stream is torn down.
#[tokio::test]
async fn a_delegated_worker_is_revoked() {
    let root = std::env::temp_dir().join(format!("lightagent-monitor-{}", RunId::new().as_str()));
    let profiles = ProfileStore::new(root.clone());
    let mut worker_profile = AgentProfile::new(profile("worker"), "Worker", "You help.", "w");
    worker_profile.approval_policy = lightagent_core::ApprovalPolicy::Permissive;
    profiles.save(&worker_profile).unwrap();
    let world = setup();
    let worker = Model::new(vec![Turn::Hold]);
    let mut script = Script::new(Model::new(vec![Turn::Call(
        "agent.delegate",
        r#"{"profile":"worker","task":"help"}"#,
    )]));
    script
        .tools
        .insert(Arc::new(lightagent_tools::builtins::AgentDelegate::new()));
    script.delegation = Some(Delegation::new(
        Arc::new(ProfileStore::new(root.clone())),
        Arc::new(worker.clone()),
        ToolRegistry::new(),
        Duration::from_secs(60),
        65_536,
    ));
    let run = world
        .start(&world.alice, "alice-bot", "delegate", script)
        .await
        .unwrap();
    awaiting_approval(&run).await;
    let pending = run.pending().await.unwrap();
    assert!(run.decide(ApprovalDecision::grant(pending.approval_id)));
    signalled(&worker.started).await;
    world.revoke_credential(&world.alice);
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert!(
        worker.dropped.load(Ordering::SeqCst),
        "the worker was torn down"
    );
    std::fs::remove_dir_all(&root).ok();
}

// Credential expiration during an active run, and an idle event stream of
// a subscriber whose credential expires: both end at the next sweep.
#[tokio::test]
async fn expiry_is_observed_by_sweeps_for_runs_and_streams() {
    let world = setup();
    let (run, _) = holding(&world, &world.alice, "alice-bot", "expiring").await;
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
    world.clock.store(T0 + 100, Ordering::SeqCst);
    tick(&world.manager, &world.ticks).await;
    let mut rest = Vec::new();
    tokio::time::timeout(HANG_GUARD, stream.read_to_end(&mut rest))
        .await
        .expect("the idle stream must end")
        .unwrap();
    assert_eq!(
        run.status().await,
        RunStatus::Running,
        "the run's own credential holds"
    );
    world.clock.store(T0 + 1_000, Ordering::SeqCst);
    tick(&world.manager, &world.ticks).await;
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(run.revocation(), Some(Denial::Expired));
}

// Many concurrent runs: one sweep checks each once, revokes exactly the
// revoked principal's runs, and leaves every other run alone.
#[tokio::test]
async fn many_concurrent_runs_are_swept_precisely() {
    let world = setup();
    let mut runs = Vec::new();
    for index in 0..12 {
        for (token, profile_id) in [
            (&world.alice, "alice-bot"),
            (&world.bob, "bob-bot"),
            (&world.ops, "bob-bot"),
        ] {
            let message = format!("{profile_id}-{index}-{}", key_id(token));
            let (run, model) = holding(&world, token, profile_id, &message).await;
            runs.push((token.clone(), run, model));
        }
    }
    world
        .identity
        .update(|state| {
            state
                .credentials
                .revoke_principal(&principal("bob"), T0 + 1)
        })
        .unwrap();
    let state = tick(&world.manager, &world.ticks).await;
    assert_eq!(state.checks, 36, "one check per active run");
    assert_eq!(state.revoked, 12);
    for (token, run, model) in &runs {
        if token == &world.bob {
            assert_eq!(finished(run).await, RunStatus::Cancelled);
            assert!(model.dropped.load(Ordering::SeqCst));
        } else {
            assert_eq!(run.status().await, RunStatus::Running);
            assert!(!model.dropped.load(Ordering::SeqCst));
        }
    }
    // A second sweep finds nothing new and checks only the 24 still active.
    let state = tick(&world.manager, &world.ticks).await;
    assert_eq!((state.checks, state.revoked), (36 + 24, 12));
    for (_, run, _) in &runs {
        run.cancel();
        finished(run).await;
    }
}

/// A persistent authority over a scratch home with `research` owned by
/// `alice`, and her token.
fn durable(root: &std::path::Path) -> (Arc<IdentityAuthority>, String) {
    std::fs::create_dir_all(root).unwrap();
    ProfileStore::new(root)
        .create(&AgentProfile::new(
            profile("research"),
            "Research",
            "Persona",
            "fast",
        ))
        .unwrap();
    let operator = Caller::local_operator(PrincipalSource::LocalProcess);
    let token = match IdentityStore::new(root)
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
    (
        Arc::new(IdentityAuthority::persistent(Arc::new(IdentityStore::new(
            root,
        )))),
        token,
    )
}

async fn start_durable(
    manager: &RunManager,
    lab: &Lab,
    identity: &Arc<IdentityAuthority>,
    token: &str,
    message: &str,
) -> (Arc<RunState>, Model) {
    let auth = AuthConfig::multi_user(identity.clone(), None);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let caller = auth.caller(&headers).unwrap();
    let owner = RunOwner::new(
        principal("alice"),
        Some(profile("research")),
        RunTrust::RemoteUser,
    );
    let model = Model::new(vec![Turn::Hold]);
    lab.script(message, Script::new(model.clone()));
    let run = manager
        .start_supervised(
            StartRun {
                message: message.to_owned(),
                history: Vec::new(),
                profile: Some("research".to_owned()),
                model: None,
                cwd: None,
            },
            owner.clone(),
            identity.run_check(caller, owner),
            identity.generation(),
        )
        .await
        .unwrap();
    signalled(&model.started).await;
    (run, model)
}

// Cross-process changes and store efficiency: a durable authority reads the
// store once per sweep however many runs it checks, a change written by
// another handle (another process) is seen at the next sweep, and a
// corrupt store fails closed for every protected run.
#[tokio::test]
async fn durable_sweeps_read_once_and_fail_closed() {
    let root = std::env::temp_dir().join(format!("lightagent-monitor-{}", RunId::new().as_str()));
    let (identity, token) = durable(&root);
    let lab = Arc::new(Lab::default());
    let (paced, ticks) = manual_clock();
    let manager = RunManager::new(lab.clone()).with_revalidation(paced);
    let mut runs = Vec::new();
    for index in 0..8 {
        runs.push(start_durable(&manager, &lab, &identity, &token, &format!("d{index}")).await);
    }
    let before = identity.store_reads();
    let state = tick(&manager, &ticks).await;
    assert_eq!(state.checks, 8);
    assert_eq!(
        identity.store_reads() - before,
        1,
        "one store read per sweep"
    );
    // Another process grants nothing new but changes the store: the next
    // sweep still reads it (no generation shortcut).
    let operator = Caller::local_operator(PrincipalSource::LocalProcess);
    IdentityStore::new(&root)
        .transact(&operator, T0 + 1, None, |tx| {
            tx.create_principal(principal("carol"), Role::User, None)
        })
        .unwrap();
    let before = identity.store_reads();
    tick(&manager, &ticks).await;
    assert_eq!(identity.store_reads() - before, 1);
    assert!(runs.iter().all(|(run, _)| run.revocation().is_none()));
    // Corruption: every protected run fails closed at the next sweep.
    std::fs::write(root.join("identity").join("state.json"), b"{corrupt").unwrap();
    let state = tick(&manager, &ticks).await;
    assert_eq!(state.revoked, 8);
    for (run, model) in &runs {
        assert_eq!(finished(run).await, RunStatus::Cancelled);
        assert!(model.dropped.load(Ordering::SeqCst));
    }
    std::fs::remove_dir_all(&root).ok();
}

// No uncontrolled polling or task leaks: the monitor exists only while a
// revocable run is active, stops after the last one ends, restarts for the
// next, and is never started by trusted single-user runs.
#[tokio::test]
async fn the_monitor_runs_only_while_revocable_runs_are_active() {
    let world = setup();
    let state = world.manager.monitor_state();
    assert_eq!(*state.borrow(), MonitorState::default());
    let (run, _) = holding(&world, &world.alice, "alice-bot", "first").await;
    assert!(state.borrow().running);
    run.cancel();
    finished(&run).await;
    let after = tick(&world.manager, &world.ticks).await;
    assert!(!after.running, "the monitor stopped with no active run");
    assert_eq!(after.started, 1);
    let (run, _) = holding(&world, &world.alice, "alice-bot", "second").await;
    assert!(state.borrow().running);
    assert_eq!(state.borrow().started, 2, "a new monitor for the new run");
    run.cancel();
    finished(&run).await;
    assert!(!tick(&world.manager, &world.ticks).await.running);

    // Trusted runs never start it.
    let lab = Arc::new(Lab::default());
    let manager = RunManager::new(lab.clone());
    let model = Model::new(vec![Turn::Say("hi")]);
    lab.script("trusted", Script::new(model));
    let trusted = manager
        .start(StartRun {
            message: "trusted".to_owned(),
            history: Vec::new(),
            profile: None,
            model: None,
            cwd: None,
        })
        .await;
    assert_eq!(finished(&trusted).await, RunStatus::Completed);
    assert_eq!(*manager.monitor_state().borrow(), MonitorState::default());
}

#[test]
fn the_interval_is_bounded() {
    assert_eq!(
        RevalidationInterval::new(Duration::ZERO).period(),
        MIN_REVALIDATION_INTERVAL
    );
    assert_eq!(
        RevalidationInterval::new(Duration::from_secs(86_400)).period(),
        MAX_REVALIDATION_INTERVAL
    );
    assert_eq!(
        RevalidationInterval::new(DEFAULT_REVALIDATION_INTERVAL).period(),
        Duration::from_secs(5)
    );
}

/// Detection latency and sweep overhead on the real interval clock. Run by
/// the dedicated "Revocation detection latency" CI job; the results go to
/// the job summary. Bounds asserted are generous (interval + 2 s): this is a
/// measurement, not a real-time guarantee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement: run by the dedicated CI job with --ignored"]
async fn measure_detection_latency_and_sweep_overhead() {
    use std::fmt::Write as _;
    let mut report = String::new();
    writeln!(
        report,
        "### Revocation detection latency\n\nRunner: {} {}; tokio multi-thread runtime, 2 workers; synthetic in-memory authority; one held model stream per run; revocation written without notifying the run (pull only).\n\n| interval | trials | min | median | p95 | max |\n|---|---|---|---|---|---|",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
    .unwrap();
    for interval in [Duration::from_millis(250), Duration::from_secs(1)] {
        let mut latencies = Vec::new();
        for trial in 0..10 {
            let mut world = setup();
            world.manager = RunManager::new(world.lab.clone()).with_revalidation_interval(interval);
            let (run, _) = holding(
                &world,
                &world.alice,
                "alice-bot",
                &format!("latency-{trial}"),
            )
            .await;
            let revoked_at = std::time::Instant::now();
            world.revoke_credential(&world.alice);
            assert_eq!(finished(&run).await, RunStatus::Cancelled);
            latencies.push(revoked_at.elapsed());
        }
        latencies.sort();
        let max = latencies[latencies.len() - 1];
        assert!(max <= interval + Duration::from_secs(2), "{max:?}");
        writeln!(
            report,
            "| {interval:?} | {} | {:?} | {:?} | {:?} | {max:?} |",
            latencies.len(),
            latencies[0],
            latencies[latencies.len() / 2],
            latencies[latencies.len() * 95 / 100],
        )
        .unwrap();
    }

    writeln!(
        report,
        "\n### Sweep overhead (durable store)\n\nOne sweep over N held runs against a persistent authority on the runner's disk.\n\n| runs | sweep time | store reads | per-run |\n|---|---|---|---|"
    )
    .unwrap();
    for count in [1usize, 50, 200] {
        let root =
            std::env::temp_dir().join(format!("lightagent-overhead-{}", RunId::new().as_str()));
        let (identity, token) = durable(&root);
        let lab = Arc::new(Lab::default());
        let (paced, _ticks) = manual_clock();
        let manager = RunManager::new(lab.clone()).with_revalidation(paced);
        let mut runs = Vec::new();
        for index in 0..count {
            runs.push(start_durable(&manager, &lab, &identity, &token, &format!("o{index}")).await);
        }
        let reads = identity.store_reads();
        let started = std::time::Instant::now();
        let revoked = manager.revalidate().await;
        let took = started.elapsed();
        assert!(revoked.is_empty());
        let reads = identity.store_reads() - reads;
        assert_eq!(reads, 1);
        writeln!(
            report,
            "| {count} | {took:?} | {reads} | {:?} |",
            took / u32::try_from(count).unwrap()
        )
        .unwrap();
        for (run, _) in &runs {
            run.cancel();
            finished(run).await;
        }
        std::fs::remove_dir_all(&root).ok();
    }
    println!("{report}");
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut summary = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        summary.write_all(report.as_bytes()).unwrap();
    }
}
