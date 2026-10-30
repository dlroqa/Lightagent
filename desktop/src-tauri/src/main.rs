//! The Lightagent Desktop host (docs/desktop/DESKTOP_ARCHITECTURE.md).
//!
//! It owns the window and one bundled runtime. The UI has no network access:
//! it asks the host, over IPC, for operations from the contract, and the host
//! makes the HTTP request to its runtime with a credential the UI never sees.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod contract;
mod sidecar;
mod sse;
mod switch;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::StreamExt as _;
use serde::Serialize;
use tauri::Manager as _;
use tokio::sync::Notify;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The longest a single events poll waits for something to arrive.
const MAX_POLL: Duration = Duration::from_secs(25);
/// At most one automatic restart of a crashed runtime in this window; a
/// runtime that keeps failing stays down and says so.
const AUTO_RESTART_WINDOW: Duration = Duration::from_secs(60);
/// How often the host checks that its runtime is still alive.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

enum Runtime {
    Starting,
    Ready(sidecar::Sidecar),
    Failed(String),
    Mismatch {
        expected: String,
        found: String,
    },
    /// Another Lightagent Desktop already owns this app's data; this window
    /// starts no runtime of its own.
    Duplicate,
}

struct Host {
    contract: contract::Contract,
    runtime: Mutex<Runtime>,
    http: reqwest::Client,
    subscriptions: Mutex<HashMap<u64, Arc<Subscription>>>,
    next_subscription: AtomicU64,
    e2e: E2e,
    /// When the runtime was last restarted automatically.
    last_auto_restart: Mutex<Option<Instant>>,
    /// The output of the last runtime that stopped, kept for diagnostics.
    last_logs: Mutex<Vec<String>>,
    /// The single-instance lock, held for the app's lifetime.
    instance: Mutex<Option<std::fs::File>>,
    /// A profile switch is in progress (one at a time).
    switching: AtomicBool,
    /// The window was asked to close while runs were working; the UI asks.
    close_requested: AtomicBool,
}

#[derive(Default)]
struct Subscription {
    state: Mutex<SubscriptionState>,
    notify: Notify,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

#[derive(Default)]
struct SubscriptionState {
    events: VecDeque<sse::SseEvent>,
    closed: bool,
    error: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Host {
    /// The runtime's base URL and authorization, copied out so no lock is held
    /// across a request.
    fn connection(&self) -> Result<(String, String), String> {
        match &*lock(&self.runtime) {
            Runtime::Ready(sidecar) => Ok((sidecar.base_url(), sidecar.authorization())),
            Runtime::Starting => Err("the runtime is still starting".to_owned()),
            Runtime::Failed(reason) => Err(format!("the runtime is unavailable: {reason}")),
            Runtime::Mismatch { .. } => {
                Err("the runtime version does not match this app".to_owned())
            }
            Runtime::Duplicate => Err("another Lightagent window owns this data".to_owned()),
        }
    }

    fn stop_runtime(&self) {
        let previous = std::mem::replace(
            &mut *lock(&self.runtime),
            Runtime::Failed("stopped".to_owned()),
        );
        drop(previous);
    }
}

/// Unattended-test evidence: when `LIGHTAGENT_DESKTOP_E2E_REPORT` names a
/// file, milestones are appended to it. Nothing secret is ever written.
struct E2e {
    path: Option<PathBuf>,
    started: Instant,
    seen: Mutex<HashSet<String>>,
}

impl E2e {
    fn from_env() -> Self {
        Self {
            path: std::env::var_os("LIGHTAGENT_DESKTOP_E2E_REPORT").map(PathBuf::from),
            started: Instant::now(),
            seen: Mutex::default(),
        }
    }

    fn note_once(&self, tag: &str, detail: &str) {
        let Some(path) = &self.path else { return };
        if !lock(&self.seen).insert(format!("{tag} {detail}")) {
            return;
        }
        let line = format!(
            "{tag} {detail} t_ms={}\n",
            self.started.elapsed().as_millis()
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[derive(Serialize)]
struct RuntimeStatus {
    state: &'static str,
    detail: Option<String>,
    desktop_version: &'static str,
    runtime_version: Option<String>,
    profile: Option<String>,
    capabilities: Vec<String>,
    close_requested: bool,
}

#[tauri::command]
fn runtime_status(host: tauri::State<'_, Arc<Host>>) -> RuntimeStatus {
    let mut status = RuntimeStatus {
        state: "starting",
        detail: None,
        desktop_version: VERSION,
        runtime_version: None,
        profile: None,
        capabilities: Vec::new(),
        close_requested: host.close_requested.load(Ordering::Acquire),
    };
    match &*lock(&host.runtime) {
        Runtime::Starting => {}
        Runtime::Ready(sidecar) => {
            status.state = "ready";
            status.runtime_version = Some(sidecar.handshake.version.clone());
            status.profile = sidecar.handshake.profile.clone();
            status.capabilities = sidecar.handshake.capabilities.clone();
        }
        Runtime::Failed(reason) => {
            status.state = "failed";
            status.detail = Some(reason.clone());
        }
        Runtime::Mismatch { expected, found } => {
            status.state = "mismatch";
            status.detail = Some(format!(
                "this app needs runtime {expected}; the bundled runtime reported {found}"
            ));
            status.runtime_version = Some(found.clone());
        }
        Runtime::Duplicate => {
            status.state = "duplicate";
            status.detail = Some(
                "Lightagent Desktop is already open. Use that window; this one does not start a second runtime on the same data."
                    .to_owned(),
            );
        }
    }
    status
}

/// Switch the bundled runtime to another profile (see [`switch`]).
#[tauri::command]
async fn profile_switch(
    host: tauri::State<'_, Arc<Host>>,
    id: String,
    cancel_runs: bool,
) -> Result<switch::Outcome, String> {
    switch::switch(host.inner(), id, cancel_runs).await
}

/// Quit after the person chose to, stopping working runs first if asked.
#[tauri::command]
async fn app_quit(
    app: tauri::AppHandle,
    host: tauri::State<'_, Arc<Host>>,
    cancel_runs: bool,
) -> Result<(), String> {
    if cancel_runs {
        switch::stop_runs(host.inner()).await?;
    }
    host.e2e.note_once("QUIT_CONFIRMED", "");
    app.exit(0);
    Ok(())
}

/// Keep the window open after a close request.
#[tauri::command]
fn app_close_cancel(host: tauri::State<'_, Arc<Host>>) {
    host.close_requested.store(false, Ordering::Release);
}

/// Restart the runtime on request (a crashed or failed one, or a running one).
#[tauri::command]
fn runtime_restart(host: tauri::State<'_, Arc<Host>>) -> Result<(), String> {
    let previous = {
        let mut runtime = lock(&host.runtime);
        if matches!(*runtime, Runtime::Starting | Runtime::Duplicate) {
            return Err("the runtime cannot be restarted right now".to_owned());
        }
        std::mem::replace(&mut *runtime, Runtime::Starting)
    };
    let host = Arc::clone(host.inner());
    std::thread::spawn(move || {
        // Stopping the old runtime may take its grace period; not on the UI's thread.
        drop(previous);
        start_runtime(&host, true);
    });
    Ok(())
}

/// The runtime's most recent output lines (already redacted), for diagnostics.
#[tauri::command]
fn runtime_logs(host: tauri::State<'_, Arc<Host>>, limit: usize) -> Vec<String> {
    match &*lock(&host.runtime) {
        Runtime::Ready(sidecar) => {
            let logs = lock(&sidecar.logs);
            let skip = logs.len().saturating_sub(limit.min(500));
            logs.iter().skip(skip).cloned().collect()
        }
        _ => {
            let logs = lock(&host.last_logs);
            let skip = logs.len().saturating_sub(limit.min(500));
            logs.iter().skip(skip).cloned().collect()
        }
    }
}

#[derive(Serialize)]
struct FetchResult {
    status: u16,
    content_type: String,
    body: String,
}

fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
}

#[tauri::command]
async fn api_fetch(
    host: tauri::State<'_, Arc<Host>>,
    method: String,
    target: String,
    body: Option<String>,
    body_base64: Option<String>,
    filename: Option<String>,
) -> Result<FetchResult, String> {
    let operation = host.contract.resolve(&method, &target)?.clone();
    if operation.stream {
        return Err(format!(
            "{} is a stream; subscribe to it instead",
            operation.id
        ));
    }
    let (base, authorization) = host.connection()?;
    let verb =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid method".to_owned())?;
    let mut request = host
        .http
        .request(verb, format!("{base}{target}"))
        .header(reqwest::header::AUTHORIZATION, authorization);
    if operation.raw_body {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body_base64.unwrap_or_default())
            .map_err(|_| "the attachment is not valid base64".to_owned())?;
        let filename = filename
            .filter(|name| valid_filename(name))
            .ok_or("an attachment needs a plain file name")?;
        request = request
            .header("X-Lightagent-Filename", filename)
            .body(bytes);
    } else if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("the runtime did not answer: {}", error.without_url()))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = response
        .text()
        .await
        .map_err(|error| format!("the runtime's answer was cut off: {}", error.without_url()))?;
    if (200..300).contains(&status) {
        host.e2e.note_once("API_OK", &operation.id);
    }
    Ok(FetchResult {
        status,
        content_type,
        body,
    })
}

#[tauri::command]
async fn api_subscribe(host: tauri::State<'_, Arc<Host>>, target: String) -> Result<u64, String> {
    let operation = host.contract.resolve("GET", &target)?;
    if !operation.stream {
        return Err(format!("{} is not a stream", operation.id));
    }
    let (base, authorization) = host.connection()?;
    let request = host
        .http
        .get(format!("{base}{target}"))
        .header(reqwest::header::AUTHORIZATION, authorization)
        .header(reqwest::header::ACCEPT, "text/event-stream");
    let id = host.next_subscription.fetch_add(1, Ordering::Relaxed);
    let subscription = Arc::new(Subscription::default());
    lock(&host.subscriptions).insert(id, Arc::clone(&subscription));
    let relay = Arc::clone(&subscription);
    let task = tauri::async_runtime::spawn(async move {
        let outcome = relay_stream(request, &relay).await;
        {
            let mut state = lock(&relay.state);
            state.closed = true;
            state.error = outcome.err();
        }
        relay.notify.notify_waiters();
    });
    *lock(&subscription.task) = Some(task);
    Ok(id)
}

async fn relay_stream(
    request: reqwest::RequestBuilder,
    subscription: &Subscription,
) -> Result<(), String> {
    let response = request
        .send()
        .await
        .map_err(|error| format!("the runtime did not answer: {}", error.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "the runtime refused the stream ({})",
            response.status().as_u16()
        ));
    }
    let mut parser = sse::SseParser::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("the stream broke: {}", error.without_url()))?;
        let events = parser.push(&chunk);
        if !events.is_empty() {
            lock(&subscription.state).events.extend(events);
            subscription.notify.notify_waiters();
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PollResult {
    events: Vec<sse::SseEvent>,
    closed: bool,
    error: Option<String>,
}

fn drain(state: &mut SubscriptionState) -> PollResult {
    let events: Vec<_> = state.events.drain(..).collect();
    PollResult {
        events,
        closed: state.closed,
        error: state.error.clone(),
    }
}

#[tauri::command]
async fn api_events_poll(
    host: tauri::State<'_, Arc<Host>>,
    subscription: u64,
    wait_ms: u64,
) -> Result<PollResult, String> {
    let entry = lock(&host.subscriptions)
        .get(&subscription)
        .cloned()
        .ok_or("unknown subscription")?;
    // Register for a wake-up before looking, so an event that lands in
    // between is never missed.
    let mut notified = std::pin::pin!(entry.notify.notified());
    notified.as_mut().enable();
    let ready = {
        let state = lock(&entry.state);
        !state.events.is_empty() || state.closed
    };
    if !ready {
        let _ = tokio::time::timeout(Duration::from_millis(wait_ms).min(MAX_POLL), notified).await;
    }
    let result = drain(&mut lock(&entry.state));
    if result.closed {
        lock(&host.subscriptions).remove(&subscription);
    }
    Ok(result)
}

#[tauri::command]
fn api_unsubscribe(host: tauri::State<'_, Arc<Host>>, subscription: u64) {
    if let Some(entry) = lock(&host.subscriptions).remove(&subscription)
        && let Some(task) = lock(&entry.task).take()
    {
        task.abort();
    }
}

#[tauri::command]
fn e2e_note(host: tauri::State<'_, Arc<Host>>, note: String) -> Result<(), String> {
    match note.as_str() {
        "ui_ready" => host.e2e.note_once("UI_READY", ""),
        "ui_error" => host.e2e.note_once("UI_ERROR", ""),
        _ => return Err("unknown note".to_owned()),
    }
    Ok(())
}

fn start_runtime(host: &Host, restart: bool) {
    let launched = sidecar::bundled_binary()
        .map_err(sidecar::LaunchError::Failed)
        .and_then(|binary| sidecar::launch(&binary, None, VERSION));
    let next = match launched {
        Ok(sidecar) => {
            host.e2e.note_once(
                if restart {
                    "RUNTIME_RESTARTED"
                } else {
                    "RUNTIME_READY"
                },
                &format!("version={}", sidecar.handshake.version),
            );
            Runtime::Ready(sidecar)
        }
        Err(sidecar::LaunchError::Failed(reason)) => {
            host.e2e.note_once("RUNTIME_FAILED", "");
            Runtime::Failed(reason)
        }
        Err(sidecar::LaunchError::Mismatch { expected, found }) => {
            host.e2e.note_once(
                "HANDSHAKE_MISMATCH",
                &format!("expected={expected} found={found}"),
            );
            Runtime::Mismatch { expected, found }
        }
    };
    *lock(&host.runtime) = next;
}

/// Crash detection: a runtime that exits while the host still wants it is
/// reported, its output kept, and it is restarted at most once per
/// [`AUTO_RESTART_WINDOW`].
fn watch_runtime(host: &Host) {
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        let exited = match &mut *lock(&host.runtime) {
            Runtime::Ready(sidecar) => sidecar.exit_status().map(|status| {
                let logs: Vec<String> = lock(&sidecar.logs).iter().cloned().collect();
                (status, logs)
            }),
            _ => None,
        };
        let Some((status, logs)) = exited else {
            continue;
        };
        host.e2e.note_once("RUNTIME_EXITED", "");
        *lock(&host.last_logs) = logs;
        *lock(&host.runtime) =
            Runtime::Failed(format!("the runtime stopped unexpectedly ({status})"));
        let allowed = {
            let mut last = lock(&host.last_auto_restart);
            let allowed = last.is_none_or(|at| at.elapsed() >= AUTO_RESTART_WINDOW);
            if allowed {
                *last = Some(Instant::now());
            }
            allowed
        };
        if allowed {
            *lock(&host.runtime) = Runtime::Starting;
            start_runtime(host, true);
        }
    }
}

/// Claim this app's single-instance lock. `Ok(None)`: another instance holds it.
fn claim_instance(dir: Option<PathBuf>) -> Result<Option<std::fs::File>, String> {
    let dir = dir.ok_or("the app has no data directory")?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))
        .map_err(|error| error.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.to_string()),
    }
}

fn main() {
    let contract = match contract::Contract::load() {
        Ok(contract) => contract,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    // Loopback only: never through a proxy from the environment, and never
    // following a redirect that would carry the credential elsewhere.
    let http = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
    {
        Ok(http) => http,
        Err(error) => {
            eprintln!("could not build the HTTP client: {error}");
            std::process::exit(1);
        }
    };
    let host = Arc::new(Host {
        contract,
        runtime: Mutex::new(Runtime::Starting),
        http,
        subscriptions: Mutex::default(),
        next_subscription: AtomicU64::new(1),
        e2e: E2e::from_env(),
        last_auto_restart: Mutex::new(None),
        last_logs: Mutex::default(),
        instance: Mutex::new(None),
        switching: AtomicBool::new(false),
        close_requested: AtomicBool::new(false),
    });
    let app = tauri::Builder::default()
        .manage(Arc::clone(&host))
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            runtime_logs,
            runtime_restart,
            profile_switch,
            app_quit,
            app_close_cancel,
            api_fetch,
            api_subscribe,
            api_events_poll,
            api_unsubscribe,
            e2e_note
        ])
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
                && let Some(host) = webview.try_state::<Arc<Host>>()
            {
                host.e2e.note_once("PAGE_LOADED", "");
            }
        })
        .setup(|app| {
            let host = Arc::clone(app.state::<Arc<Host>>().inner());
            match claim_instance(app.path().app_local_data_dir().ok()) {
                Ok(Some(lock_file)) => *lock(&host.instance) = Some(lock_file),
                Ok(None) => {
                    host.e2e.note_once("DUPLICATE_INSTANCE", "");
                    *lock(&host.runtime) = Runtime::Duplicate;
                    return Ok(());
                }
                // Without a lock the app still runs; the runtime's own
                // atomic writes remain the guard against a second writer.
                Err(error) => eprintln!("single-instance lock unavailable: {error}"),
            }
            std::thread::spawn(move || {
                start_runtime(&host, false);
                watch_runtime(&host);
            });
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            eprintln!("could not start the window: {error}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| match event {
        // Closing with runs at work: keep the window and let the UI ask
        // (stop them and quit, or keep working). With none, close as usual.
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            let Some(host) = app.try_state::<Arc<Host>>() else {
                return;
            };
            let host = Arc::clone(host.inner());
            if host.connection().is_err() {
                return;
            }
            api.prevent_close();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                match switch::active_runs(&host).await {
                    Ok(runs) if !runs.is_empty() => {
                        host.close_requested.store(true, Ordering::Release);
                        host.e2e
                            .note_once("CLOSE_HELD", &format!("runs={}", runs.len()));
                    }
                    _ => handle.exit(0),
                }
            });
        }
        tauri::RunEvent::Exit => {
            if let Some(host) = app.try_state::<Arc<Host>>() {
                host.stop_runtime();
            }
        }
        _ => {}
    });
}
