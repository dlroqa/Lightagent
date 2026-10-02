//! `open_terminal.run` — execute one approved shell command in an isolated
//! Open Terminal service.
//!
//! Open Terminal deliberately accepts a shell command, so this tool is always
//! [`RiskClass::Executable`](lightagent_core::RiskClass::Executable). The
//! normal policy/approval path remains the authority; this adapter neither
//! auto-approves calls nor forwards host credentials. Every request is bound to
//! the Lightagent run through `X-Session-Id`, output is bounded by the executor,
//! and a started background process is killed if this future is cancelled or
//! dropped by the executor timeout.

use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{RiskClass, Scope, ToolOutcome};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::{OpenTerminalContext, ToolCtx};
use crate::definition::{Tool, ToolDefinition};

const MAX_COMMAND_BYTES: usize = 4 * 1024;
const HEALTH_PATH: &str = "/health";
const CONFIG_PATH: &str = "/api/config";

/// Run one command inside Open Terminal's separately deployed environment.
pub struct OpenTerminalRun {
    definition: ToolDefinition,
}

impl OpenTerminalRun {
    pub const NAME: &'static str = "open_terminal.run";

    pub fn new() -> Self {
        Self {
            definition: ToolDefinition::new(
                Self::NAME,
                "Run an approved shell command in the configured isolated Open Terminal service.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to run inside the remote isolated environment."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional working directory inside the remote environment."
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
                RiskClass::Executable,
                vec![Scope::new("open-terminal:exec")],
            ),
        }
    }
}

impl Default for OpenTerminalRun {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct RunArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct Health {
    status: String,
}

#[derive(Deserialize)]
struct Capabilities {
    features: Features,
}

#[derive(Deserialize)]
struct Features {
    terminal: bool,
}

#[derive(Deserialize)]
struct Process {
    id: String,
    status: String,
    exit_code: Option<i32>,
}

#[derive(Deserialize)]
struct ProcessOutput {
    #[serde(flatten)]
    process: Process,
    #[serde(default)]
    output: Vec<OutputPart>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct OutputPart {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: String,
    #[serde(default)]
    data: String,
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn process_path(id: &str) -> String {
    // IDs are generated UUID-like identifiers. Reject separators rather than
    // ever interpolating a malicious value into a URL path.
    format!("/processes/{}", id)
}

fn valid_process_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn session_id(ctx: &ToolCtx) -> Result<&str, ToolOutcome> {
    ctx.run
        .as_ref()
        .map(|run| run.as_str())
        .ok_or_else(|| ToolOutcome::error("open_terminal.run requires a run-scoped executor"))
}

fn request(
    remote: &OpenTerminalContext,
    method: Method,
    path: &str,
    session: &str,
) -> reqwest::RequestBuilder {
    let builder = remote
        .client
        .request(method, endpoint(&remote.policy.base_url, path))
        .header("X-Session-Id", session);
    match &remote.policy.api_key {
        Some(key) => builder.bearer_auth(key),
        None => builder,
    }
}

async fn error_detail(response: reqwest::Response, operation: &str) -> ToolOutcome {
    let status = response.status();
    let detail = response
        .json::<Value>()
        .await
        .ok()
        .and_then(|body| {
            body.get("detail")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| status.to_string());
    ToolOutcome::error(format!(
        "Open Terminal {operation} failed ({status}): {detail}"
    ))
}

async fn health_check(remote: &OpenTerminalContext, session: &str) -> Result<(), ToolOutcome> {
    let response = request(remote, Method::GET, HEALTH_PATH, session)
        .send()
        .await
        .map_err(|error| {
            ToolOutcome::error(format!("Open Terminal health check failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(error_detail(response, "health check").await);
    }
    let health = response.json::<Health>().await.map_err(|_| {
        ToolOutcome::error("Open Terminal health check returned an invalid response")
    })?;
    if health.status != "ok" {
        return Err(ToolOutcome::error("Open Terminal is not healthy"));
    }

    let response = request(remote, Method::GET, CONFIG_PATH, session)
        .send()
        .await
        .map_err(|error| {
            ToolOutcome::error(format!("Open Terminal capability check failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(error_detail(response, "capability check").await);
    }
    let capabilities = response.json::<Capabilities>().await.map_err(|_| {
        ToolOutcome::error("Open Terminal capability check returned an invalid response")
    })?;
    if !capabilities.features.terminal {
        return Err(ToolOutcome::error(
            "Open Terminal has terminal execution disabled",
        ));
    }
    Ok(())
}

/// Kills a remote process if a future is aborted by cancellation or timeout.
/// `Drop` cannot await, so it schedules the idempotent cleanup on the current
/// Tokio runtime. Failure remains best-effort: the original timeout/cancel
/// result must never be hidden or replaced by a network error.
struct ProcessCleanup {
    remote: OpenTerminalContext,
    session: String,
    id: Option<String>,
}

impl ProcessCleanup {
    fn new(remote: OpenTerminalContext, session: String, id: String) -> Self {
        Self {
            remote,
            session,
            id: Some(id),
        }
    }

    fn disarm(&mut self) {
        self.id = None;
    }

    async fn cleanup_now(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let _ = request(
            &self.remote,
            Method::DELETE,
            &process_path(&id),
            &self.session,
        )
        .send()
        .await;
    }
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let remote = self.remote.clone();
        let session = self.session.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = request(&remote, Method::DELETE, &process_path(&id), &session)
                    .send()
                    .await;
            });
        }
    }
}

#[async_trait]
impl Tool for OpenTerminalRun {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(remote) = ctx.open_terminal.as_ref() else {
            return ToolOutcome::error("Open Terminal is not enabled for this run");
        };
        let session = match session_id(ctx) {
            Ok(session) => session.to_owned(),
            Err(error) => return error,
        };
        let Ok(args) = serde_json::from_value::<RunArgs>(args.clone()) else {
            return ToolOutcome::error("could not read open_terminal.run arguments");
        };
        if args.command.trim().is_empty() || args.command.len() > MAX_COMMAND_BYTES {
            return ToolOutcome::error("Open Terminal command must be between 1 and 4096 bytes");
        }
        if args.cwd.as_ref().is_some_and(|cwd| cwd.len() > 1024) {
            return ToolOutcome::error("Open Terminal cwd exceeds the review limit");
        }
        if let Err(error) = health_check(remote, &session).await {
            return error;
        }

        let response = match request(remote, Method::POST, "/execute", &session)
            .json(&json!({ "command": args.command, "cwd": args.cwd }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return ToolOutcome::error(format!("Open Terminal execute failed: {error}"));
            }
        };
        if !response.status().is_success() {
            return error_detail(response, "execute").await;
        }
        let process = match response.json::<Process>().await {
            Ok(process) if valid_process_id(&process.id) => process,
            Ok(_) => return ToolOutcome::error("Open Terminal returned an invalid process id"),
            Err(_) => {
                return ToolOutcome::error("Open Terminal execute returned an invalid response");
            }
        };
        let mut cleanup = ProcessCleanup::new(remote.clone(), session.clone(), process.id.clone());

        loop {
            if ctx.cancel.is_cancelled() {
                cleanup.cleanup_now().await;
                return ToolOutcome::error("open_terminal.run was cancelled");
            }
            let response = match request(remote, Method::GET, &process_path(&process.id), &session)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    return ToolOutcome::error(format!(
                        "Open Terminal process poll failed: {error}"
                    ));
                }
            };
            if !response.status().is_success() {
                return error_detail(response, "process poll").await;
            }
            let status = match response.json::<ProcessOutput>().await {
                Ok(status) => status,
                Err(_) => {
                    return ToolOutcome::error(
                        "Open Terminal process poll returned an invalid response",
                    );
                }
            };
            if status.process.id != process.id {
                return ToolOutcome::error("Open Terminal process poll returned the wrong process");
            }
            if status.process.status != "running" {
                cleanup.disarm();
                let output = status
                    .output
                    .into_iter()
                    .map(|part| part.data)
                    .collect::<String>();
                let suffix = status
                    .truncated
                    .then_some("\n[remote output truncated]")
                    .unwrap_or("");
                return ToolOutcome {
                    content: format!(
                        "exit: {}\n{}{}",
                        status
                            .process
                            .exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| status.process.status.clone()),
                        output,
                        suffix,
                    )
                    .trim_end()
                    .to_owned(),
                    is_error: status.process.exit_code.is_some_and(|code| code != 0)
                        || status.process.status == "killed",
                };
            }
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    cleanup.cleanup_now().await;
                    return ToolOutcome::error("open_terminal.run was cancelled");
                }
                _ = tokio::time::sleep(remote.policy.poll_interval.max(Duration::from_millis(10))) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_ids_are_path_safe() {
        assert!(valid_process_id("job-123_abc"));
        assert!(!valid_process_id("../../etc/passwd"));
        assert!(!valid_process_id(""));
    }

    #[test]
    fn definition_is_executable_and_has_no_environment_input() {
        let tool = OpenTerminalRun::new();
        assert_eq!(tool.definition.risk, RiskClass::Executable);
        let properties = tool
            .definition
            .parameters
            .get("properties")
            .and_then(Value::as_object)
            .expect("Open Terminal parameters must declare properties");
        assert!(properties.contains_key("command"));
        assert!(properties.contains_key("cwd"));
        assert!(!properties.contains_key("env"));
    }
}
