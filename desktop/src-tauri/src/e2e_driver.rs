//! A test-only driver for hosted end-to-end runs. Compiled only into test
//! builds (`--features e2e-hooks`), never into the bundled app.
//!
//! With `LIGHTAGENT_DESKTOP_E2E_DRIVER=1` the host reads one JSON command
//! per line on stdin and answers each on stdout as `E2E_DRIVER {json}`,
//! carrying the command's `id`. Each command calls the function the UI's IPC
//! command calls (`fetch`, `subscribe`, `poll`, `unsubscribe`, the switch),
//! so a harness drives the host's own contract check, transport, relay and
//! switch against the real runtime, without a WebView. Commands run
//! concurrently.
//!
//! Two hooks exist only here: `drop_relay` drops a relay's connection to the
//! runtime (the relay must resume it); and with
//! `LIGHTAGENT_DESKTOP_E2E_SWITCH_GATE=1` every switch, once it has raised
//! its run gate, announces `{"event":"switch_held"}` and waits for
//! `release_switch`, so operations during a switch are tried at a known
//! moment.

use std::io::{BufRead as _, Write as _};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::{Host, lock};

static SWITCH_GATE: Notify = Notify::const_new();

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
}

/// Start reading commands, when the driver is asked for.
pub fn start(host: &Arc<Host>) {
    if std::env::var_os("LIGHTAGENT_DESKTOP_E2E_DRIVER").is_none() {
        return;
    }
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
    })
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
