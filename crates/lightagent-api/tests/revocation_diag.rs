//! Diagnostics for a hang seen once in the revocation latency measurement
//! (check run 39728608914, job 119658211315: "the run never finished").
//! Diagnostic branch only; never merged.
//!
//! `traced_measurement` is `measure_detection_latency_and_sweep_overhead`
//! with phase reporting: the same trials, thresholds and 30 s guard. On a
//! timeout it reports what the run had reached before failing as before.
//! `cancelled_runs_are_seen_to_finish` repeats one cancellation and the
//! wait `finished` does, a bounded number of times, and classifies any wait
//! that does not end. Neither masks a failure.

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("support/fixtures.rs");

use std::sync::OnceLock;
use std::time::Instant;

fn since_start() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

macro_rules! phase {
    ($($arg:tt)*) => {
        eprintln!("[{:>9.3}s] {}", since_start().as_secs_f64(), format_args!($($arg)*))
    };
}

/// Why a wait for a run to finish did not end.
fn classify(status: RunStatus) -> &'static str {
    if status.is_terminal() {
        "LOST_WAKEUP: the run is terminal, but the waiter in wait_from was never woken"
    } else {
        "CANCELLATION_STALL: the run has not reached a terminal status"
    }
}

/// `finished`, with the same guard, reporting the run's state on a timeout.
async fn finished_traced(run: &RunState, label: &str, dropped: Option<&AtomicBool>) -> RunStatus {
    let observed = AtomicUsize::new(0);
    let waited = tokio::time::timeout(HANG_GUARD, async {
        let mut seen = 0;
        loop {
            let (events, status) = run.wait_from(seen).await;
            seen += events.len();
            observed.store(seen, Ordering::SeqCst);
            if status.is_terminal() {
                return status;
            }
        }
    })
    .await;
    match waited {
        Ok(status) => status,
        Err(_) => {
            let status = run.status().await;
            let events = run.events().await;
            phase!(
                "HANG {label}: no terminal status seen within {HANG_GUARD:?}; status now {status:?}; \
                 events buffered {} (waiter saw {}); last event {:?}; cancel token cancelled {}; \
                 model stream dropped {:?}; revocation {:?}",
                events.len(),
                observed.load(Ordering::SeqCst),
                events.last(),
                run.cancel_token().is_cancelled(),
                dropped.map(|flag| flag.load(Ordering::SeqCst)),
                run.revocation(),
            );
            let class = classify(status);
            phase!("HANG {label}: {class}");
            panic!("the run never finished: {class}");
        }
    }
}

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

/// `measure_detection_latency_and_sweep_overhead`, traced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "diagnostic: run by the diagnostic workflow with --ignored"]
async fn traced_measurement() {
    use std::fmt::Write as _;
    since_start();
    phase!("traced measurement: start");
    let mut report = String::new();
    writeln!(
        report,
        "### Revocation detection latency (traced)\n\n| interval | trials | min | median | p95 | max |\n|---|---|---|---|---|---|"
    )
    .unwrap();
    for interval in [Duration::from_millis(250), Duration::from_secs(1)] {
        let mut latencies = Vec::new();
        for trial in 0..10 {
            let label = format!("latency {interval:?} trial {trial}");
            let mut world = setup();
            world.manager = RunManager::new(world.lab.clone()).with_revalidation_interval(interval);
            phase!("{label}: starting a held run");
            let (run, model) = holding(
                &world,
                &world.alice,
                "alice-bot",
                &format!("latency-{trial}"),
            )
            .await;
            phase!("{label}: model asked; revoking the credential");
            let revoked_at = Instant::now();
            world.revoke_credential(&world.alice);
            let status = finished_traced(&run, &label, Some(model.dropped.as_ref())).await;
            let latency = revoked_at.elapsed();
            phase!("{label}: finished {status:?} after {latency:?}");
            assert_eq!(status, RunStatus::Cancelled);
            latencies.push(latency);
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
        "\n### Sweep overhead (durable store, traced)\n\n| runs | sweep time | store reads | per-run |\n|---|---|---|---|"
    )
    .unwrap();
    for count in [1usize, 50, 200] {
        phase!("sweep {count}: starting {count} held durable runs");
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
        phase!("sweep {count}: runs started; revalidating");
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
        phase!("sweep {count}: revalidated in {took:?}; cancelling each run and waiting");
        let mut slowest = Duration::ZERO;
        for (index, (run, model)) in runs.iter().enumerate() {
            let cancelled_at = Instant::now();
            run.cancel();
            finished_traced(
                run,
                &format!("sweep {count} run {index}"),
                Some(model.dropped.as_ref()),
            )
            .await;
            slowest = slowest.max(cancelled_at.elapsed());
        }
        phase!("sweep {count}: all runs finished; slowest {slowest:?}");
        std::fs::remove_dir_all(&root).ok();
    }
    phase!("traced measurement: done");
    println!("{report}");
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut summary = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        summary.write_all(report.as_bytes()).unwrap();
    }
}

/// Start a run holding its model stream, cancel it, and wait as `finished`
/// does, `DIAG_ITERATIONS` times (default 20000) within `DIAG_BUDGET_SECS`
/// (default 300). A wait that does not end within 5 s is classified and
/// reported; the test stops after three and fails if there was any.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "diagnostic: run by the diagnostic workflow with --ignored"]
async fn cancelled_runs_are_seen_to_finish() {
    since_start();
    let setting = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let iterations = setting("DIAG_ITERATIONS", 20_000);
    let budget = Duration::from_secs(setting("DIAG_BUDGET_SECS", 300));
    let guard = Duration::from_secs(5);
    let world = setup();
    let began = Instant::now();
    let (mut done, mut lost, mut stalled) = (0u64, 0u64, 0u64);
    let mut slowest = Duration::ZERO;
    phase!("stress: up to {iterations} iterations within {budget:?}; guard {guard:?}");
    for iteration in 0..iterations {
        if began.elapsed() >= budget {
            phase!("stress: time budget reached");
            break;
        }
        let model = Model::new(vec![Turn::Hold]);
        let run = world
            .start(
                &world.alice_second,
                "alice-bot",
                &format!("stress-{iteration}"),
                Script::new(model.clone()),
            )
            .await
            .unwrap();
        signalled(&model.started).await;
        let cancelled_at = Instant::now();
        run.cancel();
        let waited = tokio::time::timeout(guard, async {
            let mut seen = 0;
            loop {
                let (events, status) = run.wait_from(seen).await;
                seen += events.len();
                if status.is_terminal() {
                    return status;
                }
            }
        })
        .await;
        done += 1;
        match waited {
            Ok(status) => {
                assert_eq!(status, RunStatus::Cancelled);
                slowest = slowest.max(cancelled_at.elapsed());
            }
            Err(_) => {
                let status = run.status().await;
                let events = run.events().await;
                if status.is_terminal() {
                    lost += 1;
                } else {
                    stalled += 1;
                }
                phase!(
                    "HANG stress iteration {iteration}: status now {status:?}; events {}; last event {:?}; \
                     model stream dropped {}; {}",
                    events.len(),
                    events.last(),
                    model.dropped.load(Ordering::SeqCst),
                    classify(status),
                );
                if lost + stalled >= 3 {
                    phase!("stress: three hangs; stopping early");
                    break;
                }
            }
        }
    }
    let summary = format!(
        "### Cancelled-run wait stress\n\n{done} iterations in {:?}: lost wakeups {lost}, cancellation stalls {stalled}, slowest finish {slowest:?}.\n",
        began.elapsed()
    );
    phase!("{summary}");
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(summary.as_bytes()).unwrap();
    }
    assert_eq!(
        (lost, stalled),
        (0, 0),
        "waits that never ended: lost wakeups {lost}, cancellation stalls {stalled}"
    );
}
