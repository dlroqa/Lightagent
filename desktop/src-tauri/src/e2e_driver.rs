//! A test-only driver for hosted end-to-end runs. Compiled only into test
//! builds (`--features e2e-hooks`), never into the bundled app: the shell
//! job in desktop.yml checks every package for this module's markers.
//!
//! With `LIGHTAGENT_DESKTOP_E2E_DRIVER=1` the host reads one JSON command
//! per line on stdin and answers each on stdout as `E2E_DRIVER {json}`,
//! carrying the command's `id`. Most commands call the function the UI's
//! IPC command calls (`fetch`, `subscribe`, `poll`, `unsubscribe`, the
//! switch), so a harness drives the host's own contract check, transport,
//! relay and switch against the real runtime. Commands run concurrently.
//!
//! The native commands (D8) drive the rendered page itself. `eval` runs a
//! script in the main window's page; the page answers through the
//! `e2e_reply` IPC command with the single-use token the host gave that
//! script. `e2e_reply` is registered only in this build, and it accepts only
//! a token issued for a script still waiting: nothing in the page can obtain
//! one, and without the driver there are none. `geometry`, `focus`,
//! `resize` and `close` act on the main window as the OS would.
//!
//! Two hooks exist only here: `drop_relay` drops a relay's connection to the
//! runtime (the relay must resume it); and with
//! `LIGHTAGENT_DESKTOP_E2E_SWITCH_GATE=1` every switch, once it has raised
//! its run gate, announces `{"event":"switch_held"}` and waits for
//! `release_switch`, so operations during a switch are tried at a known
//! moment.

use std::fmt::Write as _;
use std::io::{BufRead as _, Write as _};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tauri::Manager as _;
use tokio::sync::{Notify, oneshot};

use crate::{Host, lock};

static SWITCH_GATE: Notify = Notify::const_new();
/// The app, for the window commands.
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// A script evaluated in the page and waiting for its reply: its token, and
/// where its (ok, JSON value) answer goes.
type Pending = (String, oneshot::Sender<(bool, String)>);
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// The largest reply a script may send (a page report or an axe result).
const MAX_REPLY: usize = 8 << 20;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Status,
    Fetch {
        method: String,
        target: String,
        #[serde(default)]
        body: Option<String>,
        /// A raw body (an attachment) and its file name, as `api_fetch` takes them.
        #[serde(default)]
        body_base64: Option<String>,
        #[serde(default)]
        filename: Option<String>,
    },
    Subscribe {
        target: String,
    },
    Poll {
        subscription: u64,
        wait_ms: u64,
    },
    Unsubscribe {
        subscription: u64,
    },
    DropRelay {
        subscription: u64,
    },
    Switch {
        profile: String,
        cancel_runs: bool,
    },
    ReleaseSwitch,
    /// Run `script` (an async function body) in the main window's page and
    /// answer with what it returns, as JSON.
    Eval {
        script: String,
        #[serde(default = "eval_timeout_ms")]
        timeout_ms: u64,
    },
    /// Where the window's content is on screen, in physical pixels.
    Geometry,
    /// Bring the window to the front with keyboard focus.
    Focus,
    /// Resize the window (logical pixels).
    Resize {
        width: f64,
        height: f64,
    },
    /// Ask the window to close, as its close button does.
    Close,
    /// The runtime's recent output (the running one, else the last that
    /// stopped), as the diagnostics view shows it.
    Logs {
        #[serde(default = "log_limit")]
        limit: usize,
    },
}

fn log_limit() -> usize {
    60
}

fn eval_timeout_ms() -> u64 {
    30_000
}

/// Start reading commands, when the driver is asked for.
pub fn start(host: &Arc<Host>, app: tauri::AppHandle) {
    if std::env::var_os("LIGHTAGENT_DESKTOP_E2E_DRIVER").is_none() {
        return;
    }
    let _ = APP.set(app);
    let host = Arc::clone(host);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let command: Value = match serde_json::from_str(&line) {
                Ok(command) => command,
                Err(error) => {
                    say(&json!({ "ok": false, "error": format!("unreadable command: {error}") }));
                    continue;
                }
            };
            let id = command["id"].clone();
            let host = Arc::clone(&host);
            tauri::async_runtime::spawn(async move {
                let answer = match serde_json::from_value::<Op>(command) {
                    Ok(op) => run(&host, op).await,
                    Err(error) => Err(format!("unknown command: {error}")),
                };
                say(&match answer {
                    Ok(result) => json!({ "id": id, "ok": true, "result": result }),
                    Err(error) => json!({ "id": id, "ok": false, "error": error }),
                });
            });
        }
    });
    say(&json!({ "event": "driver_ready" }));
}

async fn run(host: &Arc<Host>, op: Op) -> Result<Value, String> {
    Ok(match op {
        Op::Status => json!(crate::status(host)),
        Op::Fetch {
            method,
            target,
            body,
            body_base64,
            filename,
        } => json!(crate::fetch(host, method, target, body, body_base64, filename).await?),
        Op::Subscribe { target } => json!(crate::subscribe(host, target)?),
        Op::Poll {
            subscription,
            wait_ms,
        } => json!(crate::poll(host, subscription, wait_ms).await?),
        Op::Unsubscribe { subscription } => {
            crate::unsubscribe(host, subscription);
            Value::Null
        }
        Op::DropRelay { subscription } => {
            let entry = lock(&host.subscriptions)
                .get(&subscription)
                .cloned()
                .ok_or("unknown subscription")?;
            entry.interrupt.notify_one();
            Value::Null
        }
        Op::Switch {
            profile,
            cancel_runs,
        } => json!(crate::switch::switch(host, profile, cancel_runs).await?),
        Op::ReleaseSwitch => {
            SWITCH_GATE.notify_one();
            Value::Null
        }
        Op::Eval { script, timeout_ms } => eval(&script, timeout_ms).await?,
        Op::Geometry => {
            let window = main_window()?;
            let position = window.inner_position().map_err(|error| error.to_string())?;
            let size = window.inner_size().map_err(|error| error.to_string())?;
            let scale = window.scale_factor().map_err(|error| error.to_string())?;
            json!({
                "x": position.x,
                "y": position.y,
                "width": size.width,
                "height": size.height,
                "scale": scale,
            })
        }
        Op::Focus => {
            let window = main_window()?;
            let _ = window.unminimize();
            window.show().map_err(|error| error.to_string())?;
            window.set_focus().map_err(|error| error.to_string())?;
            Value::Null
        }
        Op::Resize { width, height } => {
            main_window()?
                .set_size(tauri::LogicalSize::new(width, height))
                .map_err(|error| error.to_string())?;
            Value::Null
        }
        Op::Close => {
            main_window()?.close().map_err(|error| error.to_string())?;
            Value::Null
        }
        Op::Logs { limit } => {
            let current: Option<Vec<String>> = match &*lock(&host.runtime) {
                crate::Runtime::Ready(sidecar) => {
                    Some(lock(&sidecar.logs).iter().cloned().collect())
                }
                _ => None,
            };
            let lines = current.unwrap_or_else(|| lock(&host.last_logs).clone());
            let skip = lines.len().saturating_sub(limit.min(500));
            json!(&lines[skip..])
        }
    })
}

fn main_window() -> Result<tauri::WebviewWindow, String> {
    APP.get()
        .and_then(|app| app.get_webview_window("main"))
        .ok_or_else(|| "the main window is not open".to_owned())
}

fn take_pending(token: &str) -> Option<oneshot::Sender<(bool, String)>> {
    let mut pending = lock(&PENDING);
    let index = pending.iter().position(|(issued, _)| issued == token)?;
    Some(pending.swap_remove(index).1)
}

/// Evaluate `script` in the page and wait for its answer.
async fn eval(script: &str, timeout_ms: u64) -> Result<Value, String> {
    let window = main_window()?;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "the OS random source is unavailable".to_owned())?;
    let mut token = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    let (sender, receiver) = oneshot::channel();
    lock(&PENDING).push((token.clone(), sender));
    let wrapped = [
        "(async () => { let ok = true, value = null;\ntry { value = await (async () => {\n",
        script,
        "\n})(); } catch (error) { ok = false; value = String((error && error.stack) || error); }\n",
        "let text; try { text = JSON.stringify(value === undefined ? null : value); }\n",
        "catch (error) { ok = false; text = JSON.stringify(String(error)); }\n",
        "window.__TAURI__.core.invoke(\"e2e_reply\", { token: \"",
        token.as_str(),
        "\", ok, value: text }); })();",
    ]
    .concat();
    if let Err(error) = window.eval(&wrapped) {
        take_pending(&token);
        return Err(format!("the page could not run the script: {error}"));
    }
    let answer = tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await;
    take_pending(&token);
    match answer {
        Ok(Ok((true, text))) => {
            serde_json::from_str(&text).map_err(|error| format!("unreadable reply: {error}"))
        }
        Ok(Ok((false, text))) => Err(format!(
            "the script failed: {}",
            serde_json::from_str::<String>(&text).unwrap_or(text)
        )),
        Ok(Err(_)) => Err("the reply was refused (too large)".to_owned()),
        Err(_) => Err(format!(
            "no reply within {timeout_ms} ms (the page reloaded, or the script hung)"
        )),
    }
}

/// The page's answer to a script the driver evaluated. Registered only in
/// test builds; a token not issued for a waiting script is refused.
#[tauri::command]
pub fn e2e_reply(token: String, ok: bool, value: String) -> Result<(), String> {
    let sender = take_pending(&token).ok_or("unknown token")?;
    if value.len() > MAX_REPLY {
        return Err("reply too large".to_owned());
    }
    let _ = sender.send((ok, value));
    Ok(())
}

/// Hold a switch, its run gate raised, until `release_switch`, when asked to.
pub async fn switch_gate(profile: &str) {
    if std::env::var_os("LIGHTAGENT_DESKTOP_E2E_SWITCH_GATE").is_none() {
        return;
    }
    say(&json!({ "event": "switch_held", "profile": profile }));
    SWITCH_GATE.notified().await;
}

fn say(message: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "E2E_DRIVER {message}");
    let _ = out.flush();
}
