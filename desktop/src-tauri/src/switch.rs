//! Switching profile: start the new runtime, then commit
//! (docs/desktop/DESKTOP_ARCHITECTURE.md §7).
//!
//! 1. While runs are working the switch is refused, unless the person chose
//!    to stop them: pending approvals are then rejected explicitly (recorded,
//!    not lost), the runs are cancelled, and the switch goes ahead only once
//!    none is left.
//! 2. A second runtime starts on the chosen profile with its own key while
//!    the first keeps serving. If it does not become ready it is stopped, and
//!    nothing has changed.
//! 3. Commit: make the profile active through the new runtime (the CLI sees
//!    the same choice), route the app to it, end the old runtime's streams
//!    with `profile_switch`, and stop the old runtime.
//!
//! The same steps stop runs before the app quits.

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
    pub persisted: bool,
    pub profile: String,
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

/// One switch at a time.
struct Switching<'a>(&'a Host);

impl<'a> Switching<'a> {
    fn begin(host: &'a Host) -> Result<Self, String> {
        if host.switching.swap(true, Ordering::AcqRel) {
            return Err("a profile switch is already in progress".to_owned());
        }
        Ok(Self(host))
    }
}

impl Drop for Switching<'_> {
    fn drop(&mut self) {
        self.0.switching.store(false, Ordering::Release);
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

pub async fn switch(host: &Arc<Host>, id: String, stop: bool) -> Result<Outcome, String> {
    // The id must be one the contract accepts as a path segment.
    host.contract
        .resolve("PUT", &format!("/api/lightagent/v1/profiles/{id}/active"))?;
    let _switching = Switching::begin(host)?;
    let runs = active_runs(host).await?;
    match plan(runs.len(), stop) {
        Plan::Blocked => {
            return Ok(Outcome {
                status: "blocked",
                runs,
                persisted: false,
                profile: id,
            });
        }
        Plan::StopRunsThenProceed => stop_runs(host).await?,
        Plan::Proceed => {}
    }

    let binary = sidecar::bundled_binary()?;
    let profile = id.clone();
    let launched = tauri::async_runtime::spawn_blocking(move || {
        sidecar::launch(&binary, Some(&profile), crate::VERSION)
    })
    .await
    .map_err(|error| format!("could not start the runtime: {error}"))?;
    let fresh = launched.map_err(|error| match error {
        sidecar::LaunchError::Failed(reason) => {
            format!("the runtime for '{id}' did not start: {reason}. Nothing was changed.")
        }
        sidecar::LaunchError::Mismatch { expected, found } => {
            format!("the runtime reported {found}, not {expected}. Nothing was changed.")
        }
    })?;

    let persisted = call(
        host,
        &fresh.base_url(),
        &fresh.authorization(),
        reqwest::Method::PUT,
        &format!("/api/lightagent/v1/profiles/{id}/active"),
        None,
    )
    .await
    .is_ok();
    let previous = std::mem::replace(&mut *lock(&host.runtime), Runtime::Ready(fresh));
    end_streams(host);
    // The old runtime's lifeline closes here; let its grace period pass off the UI's path.
    std::thread::spawn(move || drop(previous));
    host.e2e.note_once("PROFILE_SWITCHED", &id);
    Ok(Outcome {
        status: "switched",
        runs: Vec::new(),
        persisted,
        profile: id,
    })
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
    fn run_lists_are_read_defensively() {
        assert_eq!(runs_of(&json!({ "runs": [{ "id": "a" }] })).len(), 1);
        assert!(runs_of(&json!({})).is_empty());
        assert!(runs_of(&Value::Null).is_empty());
    }
}
