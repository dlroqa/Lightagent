//! Switching profile: start the new runtime, then commit, all or nothing
//! (docs/desktop/DESKTOP_ARCHITECTURE.md §7).
//!
//! 1. One lifecycle change at a time: a switch holds the host's lifecycle lock,
//!    so a restart (by the person or by crash recovery) cannot interleave, and
//!    while it runs the host refuses to start runs or answer approvals.
//! 2. While runs are working the switch is refused, unless the person chose to
//!    stop them: pending approvals are then rejected explicitly (recorded, not
//!    lost), the runs are cancelled, and the switch goes ahead only once none
//!    is left.
//! 3. A second runtime starts on the chosen profile with its own key while the
//!    first keeps serving.
//! 4. Just before committing, the old runtime is checked once more for runs.
//! 5. Commit: make the profile active through the new runtime (the CLI sees the
//!    same choice), then route the app to it, end the old runtime's streams
//!    with `profile_switch`, and stop the old runtime.
//!
//! If any step before the commit's active-profile write succeeds fails, the
//! unused new runtime is stopped and the old runtime, the active profile and
//! the app are exactly as they were. The same steps stop runs before the app
//! quits.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{Host, Runtime, lock, sidecar};

/// How long cancelled runs may take to reach a terminal state.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub status: &'static str,
    pub runs: Vec<Value>,
    pub profile: String,
}

/// Why a switch did not happen. In every case nothing was changed.
#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub message: String,
}

impl Failure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// What must happen before the runtime may be replaced.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Proceed,
    Blocked,
    StopRunsThenProceed,
}

pub fn plan(active_runs: usize, stop_runs: bool) -> Plan {
    match (active_runs, stop_runs) {
        (0, _) => Plan::Proceed,
        (_, false) => Plan::Blocked,
        (_, true) => Plan::StopRunsThenProceed,
    }
}

/// Operations the host refuses while a switch is in progress: each would
/// start or resume work on the runtime that is about to be replaced.
pub fn blocked_while_switching(operation: &str) -> bool {
    matches!(operation, "runs.create" | "approvals.respond")
}

/// One lifecycle change at a time, and the run gate while it lasts.
struct Switching<'a> {
    host: &'a Host,
    _lifecycle: tokio::sync::OwnedMutexGuard<()>,
}

impl<'a> Switching<'a> {
    fn begin(host: &'a Host) -> Result<Self, Failure> {
        let lifecycle = Arc::clone(&host.lifecycle).try_lock_owned().map_err(|_| {
            Failure::new(
                "busy",
                "a profile switch or runtime restart is already in progress",
            )
        })?;
        host.switching.store(true, Ordering::Release);
        Ok(Self {
            host,
            _lifecycle: lifecycle,
        })
    }
}

impl Drop for Switching<'_> {
    fn drop(&mut self) {
        self.host.switching.store(false, Ordering::Release);
    }
}

async fn call(
    host: &Host,
    base: &str,
    authorization: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = host
        .http
        .request(method, format!("{base}{path}"))
        .header(reqwest::header::AUTHORIZATION, authorization);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("the runtime did not answer: {}", error.without_url()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("the runtime's answer was cut off: {}", error.without_url()))?;
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(value);
    }
    let message = value["error"]["message"]
        .as_str()
        .or_else(|| value["error"].as_str())
        .unwrap_or("the request failed");
    Err(format!("{message} ({})", status.as_u16()))
}

fn runs_of(value: &Value) -> Vec<Value> {
    value["runs"].as_array().cloned().unwrap_or_default()
}

/// The runs the current runtime is still working on.
pub async fn active_runs(host: &Host) -> Result<Vec<Value>, String> {
    let (base, authorization) = host.connection()?;
    let runs = call(
        host,
        &base,
        &authorization,
        reqwest::Method::GET,
        "/api/lightagent/v1/runs",
        None,
    )
    .await?;
    Ok(runs_of(&runs))
}

/// Reject pending approvals, cancel every run, and wait until none is left.
pub async fn stop_runs(host: &Host) -> Result<(), String> {
    let (base, authorization) = host.connection()?;
    let approvals = call(
        host,
        &base,
        &authorization,
        reqwest::Method::GET,
        "/api/lightagent/v1/approvals",
        None,
    )
    .await?;
    for row in approvals["approvals"].as_array().into_iter().flatten() {
        if row["pending"].is_null() {
            continue;
        }
        if let Some(run) = row["run"].as_str() {
            call(
                host,
                &base,
                &authorization,
                reqwest::Method::POST,
                &format!("/api/lightagent/v1/approvals/{run}"),
                Some(json!({ "approve": false })),
            )
            .await?;
        }
    }
    let runs = call(
        host,
        &base,
        &authorization,
        reqwest::Method::GET,
        "/api/lightagent/v1/runs",
        None,
    )
    .await?;
    for run in runs_of(&runs) {
        if let Some(id) = run["id"].as_str() {
            // A run may finish on its own meanwhile; the drain below decides.
            let _ = call(
                host,
                &base,
                &authorization,
                reqwest::Method::POST,
                &format!("/api/lightagent/v1/runs/{id}/cancel"),
                Some(json!({})),
            )
            .await;
        }
    }
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    loop {
        let runs = call(
            host,
            &base,
            &authorization,
            reqwest::Method::GET,
            "/api/lightagent/v1/runs",
            None,
        )
        .await?;
        if runs_of(&runs).is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the runs did not stop within {} s; nothing was changed",
                DRAIN_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// End the old runtime's relayed streams; their next poll reports the reason.
fn end_streams(host: &Host) {
    let subscriptions: Vec<_> = lock(&host.subscriptions).values().cloned().collect();
    for subscription in subscriptions {
        if let Some(task) = lock(&subscription.task).take() {
            task.abort();
        }
        {
            let mut state = lock(&subscription.state);
            state.closed = true;
            state.error = Some("profile_switch".to_owned());
        }
        subscription.notify.notify_waiters();
    }
}

/// Stop a runtime off the caller's path (its grace period can take seconds).
fn discard(runtime: sidecar::Sidecar) {
    std::thread::spawn(move || drop(runtime));
}

/// The switch for the UI: an outcome, or an error that says nothing changed.
pub async fn switch(host: &Arc<Host>, id: String, stop: bool) -> Result<Outcome, String> {
    let result = try_switch(host, id, stop).await;
    match &result {
        Ok(outcome) => host.e2e.note_once(
            "SWITCH_RESULT",
            &format!(
                "id={} status={} serving={}",
                outcome.profile,
                outcome.status,
                serving(host)
            ),
        ),
        Err(failure) => host.e2e.note_once(
            "SWITCH_RESULT",
            &format!(
                "status=error kind={} serving={}",
                failure.kind,
                serving(host)
            ),
        ),
    }
    result.map_err(|failure| failure.message)
}

fn serving(host: &Host) -> String {
    match &*lock(&host.runtime) {
        Runtime::Ready(sidecar) => sidecar.handshake.profile.clone().unwrap_or_default(),
        _ => "none".to_owned(),
    }
}

async fn try_switch(host: &Arc<Host>, id: String, stop: bool) -> Result<Outcome, Failure> {
    // The id must be one the contract accepts as a path segment.
    host.contract
        .resolve("PUT", &format!("/api/lightagent/v1/profiles/{id}/active"))
        .map_err(|message| Failure::new("invalid", message))?;
    let _switching = Switching::begin(host)?;
    let runs = active_runs(host)
        .await
        .map_err(|message| Failure::new("unavailable", message))?;
    match plan(runs.len(), stop) {
        Plan::Blocked => {
            return Ok(Outcome {
                status: "blocked",
                runs,
                profile: id,
            });
        }
        Plan::StopRunsThenProceed => stop_runs(host)
            .await
            .map_err(|message| Failure::new("runs_not_stopped", message))?,
        Plan::Proceed => {}
    }

    let binary =
        sidecar::bundled_binary().map_err(|message| Failure::new("start_failed", message))?;
    let profile = id.clone();
    let launched = tauri::async_runtime::spawn_blocking(move || {
        sidecar::launch(&binary, Some(&profile), crate::VERSION)
    })
    .await
    .map_err(|error| {
        Failure::new(
            "start_failed",
            format!("could not start the runtime: {error}"),
        )
    })?;
    let fresh = launched.map_err(|error| match error {
        sidecar::LaunchError::Failed(reason) => Failure::new(
            "start_failed",
            format!("The runtime for '{id}' did not start: {reason}. Nothing was changed; the current profile is still in use."),
        ),
        sidecar::LaunchError::Mismatch { expected, found } => Failure::new(
            "start_failed",
            format!("The runtime reported {found}, not {expected}. Nothing was changed."),
        ),
    })?;

    // The run gate keeps new runs out; check once more right before committing.
    match active_runs(host).await {
        Ok(late) if late.is_empty() => {}
        Ok(late) => {
            discard(fresh);
            return Err(Failure::new(
                "runs_appeared",
                format!(
                    "{} run(s) started while switching. Nothing was changed; try again.",
                    late.len()
                ),
            ));
        }
        Err(message) => {
            discard(fresh);
            return Err(Failure::new("unavailable", message));
        }
    }

    // Commit, all or nothing: the active-profile write first. If it fails,
    // the new runtime is stopped and the old one keeps serving.
    if let Err(message) = call(
        host,
        &fresh.base_url(),
        &fresh.authorization(),
        reqwest::Method::PUT,
        &format!("/api/lightagent/v1/profiles/{id}/active"),
        None,
    )
    .await
    {
        discard(fresh);
        return Err(Failure::new(
            "persist_failed",
            format!(
                "Could not make '{id}' the active profile ({message}). Nothing was changed: the current profile and its runtime are still in use. Check that the Lightagent home folder is writable, then try again."
            ),
        ));
    }
    let previous = std::mem::replace(&mut *lock(&host.runtime), Runtime::Ready(fresh));
    end_streams(host);
    if let Runtime::Ready(previous) = previous {
        discard(previous);
    }
    Ok(Outcome {
        status: "switched",
        runs: Vec::new(),
        profile: id,
    })
}

/// Unattended switch scenarios for hosted end-to-end tests. Compiled only into
/// test builds (`--features e2e-hooks`), never into the bundled app.
/// `LIGHTAGENT_DESKTOP_E2E_SWITCHES` lists steps run in order after the
/// runtime is ready; `a&b` runs two switches at the same time.
#[cfg(feature = "e2e-hooks")]
pub fn run_e2e_switches(host: &Arc<Host>) {
    let Ok(plan) = std::env::var("LIGHTAGENT_DESKTOP_E2E_SWITCHES") else {
        return;
    };
    for step in plan.split(',').filter(|step| !step.is_empty()) {
        let ids: Vec<String> = step.split('&').map(str::to_owned).collect();
        tauri::async_runtime::block_on(async {
            let switches = ids.iter().map(|id| switch(host, id.clone(), false));
            futures_util::future::join_all(switches).await;
        });
    }
    host.e2e
        .note_once("SWITCHES_DONE", &format!("serving={}", serving(host)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_runs_block_a_switch_unless_asked_to_stop_them() {
        assert_eq!(plan(0, false), Plan::Proceed);
        assert_eq!(plan(0, true), Plan::Proceed);
        assert_eq!(plan(2, false), Plan::Blocked);
        assert_eq!(plan(1, true), Plan::StopRunsThenProceed);
    }

    #[test]
    fn work_cannot_start_on_a_runtime_being_replaced() {
        assert!(blocked_while_switching("runs.create"));
        assert!(blocked_while_switching("approvals.respond"));
        for reading in [
            "runs.get",
            "runs.events",
            "sessions.list",
            "approvals.list",
            "runs.cancel",
        ] {
            assert!(!blocked_while_switching(reading), "{reading}");
        }
    }

    #[test]
    fn run_lists_are_read_defensively() {
        assert_eq!(runs_of(&json!({ "runs": [{ "id": "a" }] })).len(), 1);
        assert!(runs_of(&json!({})).is_empty());
        assert!(runs_of(&Value::Null).is_empty());
    }

    #[test]
    fn only_one_lifecycle_change_runs_at_a_time() {
        let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
        let held = Arc::clone(&lifecycle).try_lock_owned().expect("first");
        assert!(Arc::clone(&lifecycle).try_lock_owned().is_err());
        drop(held);
        assert!(Arc::clone(&lifecycle).try_lock_owned().is_ok());
    }
}
