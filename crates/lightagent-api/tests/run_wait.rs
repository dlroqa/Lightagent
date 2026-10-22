//! A cancelled run is always seen to finish. Every observer of a run (the
//! session recorder, event streams) waits with `RunState::wait_from`, which
//! once could miss a run's last change and wait forever. The unit tests in
//! `manager.rs` pin that interleaving exactly; this is a bounded stress of
//! the real run path.

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("support/fixtures.rs");

/// Runs started, held on their model stream, cancelled and waited for.
const ITERATIONS: usize = 3_000;

// Each run is waited for by two observers at once; every wait must end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_runs_are_always_seen_to_finish() {
    let world = setup();
    for iteration in 0..ITERATIONS {
        let model = Model::new(vec![Turn::Hold]);
        let run = world
            .start(
                &world.alice_second,
                "alice-bot",
                &format!("wait-{iteration}"),
                Script::new(model.clone()),
            )
            .await
            .unwrap();
        signalled(&model.started).await;
        let observer = tokio::spawn({
            let run = Arc::clone(&run);
            async move { finished(&run).await }
        });
        run.cancel();
        assert_eq!(
            finished(&run).await,
            RunStatus::Cancelled,
            "iteration {iteration}"
        );
        assert_eq!(
            observer.await.unwrap(),
            RunStatus::Cancelled,
            "iteration {iteration}"
        );
    }
}
