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
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, ()> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

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
    next_offset: u64,
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
    format!("/execute/{}", id)
}

fn valid_process_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn bounded_output(mut content: String, maximum: usize) -> String {
    if content.len() <= maximum {
        return content;
    }
    let notice = "\n[output truncated]";
    let mut end = maximum.saturating_sub(notice.len()).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    content.truncate(end);
    if maximum >= notice.len() {
        content.push_str(notice);
    }
    content
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
        .timeout(remote.policy.request_timeout)
        .header("X-Session-Id", session);
    match &remote.policy.api_key {
        Some(key) => builder.bearer_auth(key),
        None => builder,
    }
}

async fn error_detail(response: reqwest::Response, operation: &str) -> ToolOutcome {
    let status = response.status();
    // Untrusted server diagnostics can echo the Authorization header.
    ToolOutcome::error(format!("Open Terminal {operation} failed ({status})"))
}

async fn health_check(remote: &OpenTerminalContext, session: &str) -> Result<(), ToolOutcome> {
    let response = request(remote, Method::GET, HEALTH_PATH, session)
        .send()
        .await
        .map_err(|_| ToolOutcome::error("Open Terminal health check request failed"))?;
    if !response.status().is_success() {
        return Err(error_detail(response, "health check").await);
    }
    let health = bounded_json::<Health>(response).await.map_err(|_| {
        ToolOutcome::error("Open Terminal health check returned an invalid response")
    })?;
    if health.status != "ok" {
        return Err(ToolOutcome::error("Open Terminal is not healthy"));
    }

    let response = request(remote, Method::GET, CONFIG_PATH, session)
        .send()
        .await
        .map_err(|_| ToolOutcome::error("Open Terminal capability check request failed"))?;
    if !response.status().is_success() {
        return Err(error_detail(response, "capability check").await);
    }
    let capabilities = bounded_json::<Capabilities>(response).await.map_err(|_| {
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
        .query(&[("force", "true")])
        .timeout(
            self.remote
                .policy
                .request_timeout
                .min(Duration::from_secs(5)),
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
                    .query(&[("force", "true")])
                    .timeout(remote.policy.request_timeout.min(Duration::from_secs(5)))
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
        tokio::select! {
            _ = ctx.cancel.cancelled() => ToolOutcome::error("open_terminal.run was cancelled"),
            result = tokio::time::timeout(remote.policy.execution_timeout, self.run(args, ctx)) => {
                match result {
                    Ok(outcome) => outcome,
                    Err(_) => ToolOutcome::error("Open Terminal execution timed out"),
                }
            }
        }
    }
}

impl OpenTerminalRun {
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
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
        if ctx.cancel.is_cancelled() {
            return ToolOutcome::error("open_terminal.run was cancelled");
        }

        // Keep ownership of the startup handshake even if the awaiting call is
        // cancelled before the server's process id arrives. Dropping a JoinHandle
        // detaches this bounded request; its returned cleanup guard is then
        // dropped when the task finishes and kills the newly discovered process.
        let startup_remote = remote.clone();
        let startup_session = session.clone();
        let startup = tokio::spawn(async move {
            let response = request(&startup_remote, Method::POST, "/execute", &startup_session)
                .query(&[("wait", "0")])
                .json(&json!({ "command": args.command, "cwd": args.cwd }))
                .send()
                .await
                .map_err(|_| ToolOutcome::error("Open Terminal execute request failed"))?;
            if !response.status().is_success() {
                return Err(error_detail(response, "execute").await);
            }
            let body = bounded_json::<Value>(response).await.map_err(|_| {
                ToolOutcome::error("Open Terminal execute returned an invalid response")
            })?;
            let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
            if !valid_process_id(id) {
                return Err(ToolOutcome::error(
                    "Open Terminal returned an invalid process id",
                ));
            }
            // A known process must be cleaned up even when the rest of the
            // startup response is malformed.
            let cleanup = ProcessCleanup::new(startup_remote, startup_session, id.to_owned());
            let process = serde_json::from_value::<Process>(body).map_err(|_| {
                ToolOutcome::error("Open Terminal execute returned an invalid response")
            })?;
            if !matches!(process.status.as_str(), "running" | "done" | "killed")
                || (process.status == "done" && process.exit_code.is_none())
            {
                return Err(ToolOutcome::error(
                    "Open Terminal execute returned an invalid status",
                ));
            }
            Ok((process, cleanup))
        });
        let (process, mut cleanup) = match startup.await {
            Ok(Ok(started)) => started,
            Ok(Err(error)) => return error,
            Err(_) => return ToolOutcome::error("Open Terminal execute task failed"),
        };
        let mut offset = 0_u64;
        let mut collected = String::new();
        let mut output_truncated = false;

        loop {
            if ctx.cancel.is_cancelled() {
                cleanup.cleanup_now().await;
                return ToolOutcome::error("open_terminal.run was cancelled");
            }
            let response = match request(
                remote,
                Method::GET,
                &format!("{}/status", process_path(&process.id)),
                &session,
            )
            .query(&[("wait", "0".to_owned()), ("offset", offset.to_string())])
            .send()
            .await
            {
                Ok(response) => response,
                Err(_) => {
                    return ToolOutcome::error("Open Terminal process poll request failed");
                }
            };
            if !response.status().is_success() {
                return error_detail(response, "process poll").await;
            }
            let status = match bounded_json::<ProcessOutput>(response).await {
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
            if !matches!(
                status.process.status.as_str(),
                "running" | "done" | "killed"
            ) {
                return ToolOutcome::error("Open Terminal process poll returned an invalid status");
            }
            let expected_offset = offset.checked_add(status.output.len() as u64);
            if expected_offset.is_none_or(|expected| {
                status.next_offset < expected
                    || (!status.truncated && status.next_offset != expected)
            }) || (status.process.status == "done" && status.process.exit_code.is_none())
            {
                return ToolOutcome::error(
                    "Open Terminal process poll returned invalid completion or cursor",
                );
            }
            offset = status.next_offset;
            output_truncated |= status.truncated;
            for part in status.output {
                // Lookahead recognizes a bearer split across output chunks or
                // the byte ceiling before final redaction and truncation.
                let raw_limit = remote
                    .policy
                    .max_output_bytes
                    .saturating_add(remote.policy.api_key.as_ref().map_or(0, String::len));
                let remaining = raw_limit.saturating_sub(collected.len());
                let mut end = remaining.min(part.data.len());
                while !part.data.is_char_boundary(end) {
                    end -= 1;
                }
                collected.push_str(&part.data[..end]);
                output_truncated |= end < part.data.len();
            }
            if status.process.status != "running" {
                cleanup.disarm();
                if output_truncated && let Some(key) = remote.policy.api_key.as_deref() {
                    // A final partial token may be caused by the raw read cap.
                    for length in (1..key.len()).rev() {
                        if key.is_char_boundary(length) && collected.ends_with(&key[..length]) {
                            collected.truncate(collected.len() - length);
                            break;
                        }
                    }
                }
                let suffix = if output_truncated {
                    "\n[remote output truncated]"
                } else {
                    ""
                };
                let content = format!(
                    "exit: {}\n{}{}",
                    status
                        .process
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| status.process.status.clone()),
                    collected,
                    suffix,
                )
                .trim_end()
                .to_owned();
                let content = match remote.policy.api_key.as_deref() {
                    Some(key) if !key.is_empty() => content.replace(key, "<redacted>"),
                    _ => content,
                };
                return ToolOutcome {
                    content: bounded_output(content, remote.policy.max_output_bytes),
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
    use crate::context::OpenTerminalPolicy;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    const SECRET: &str = "terminal-contract-secret";

    struct Reply {
        method: &'static str,
        path: &'static str,
        status: u16,
        body: Value,
        delay: Duration,
    }

    fn reply(method: &'static str, path: &'static str, body: Value) -> Reply {
        Reply {
            method,
            path,
            status: 200,
            body,
            delay: Duration::ZERO,
        }
    }

    fn checks() -> Vec<Reply> {
        vec![
            reply("GET", "/health", json!({"status":"ok"})),
            reply("GET", "/api/config", json!({"features":{"terminal":true}})),
        ]
    }

    fn process(status: &str, output: &str) -> Value {
        let parts = if output.is_empty() {
            vec![]
        } else {
            vec![json!({"type":"stdout", "data":output})]
        };
        json!({"id":"job-1", "status":status, "exit_code":if status == "done" { Some(0) } else { None }, "next_offset":parts.len(), "output":parts})
    }

    async fn server(
        replies: Vec<Reply>,
    ) -> (
        ToolCtx,
        mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let run = lightagent_core::RunId::new();
        let session = run.as_str().to_owned();
        let task = tokio::spawn(async move {
            for response in replies {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let header_end = loop {
                    let size = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let size = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                }
                assert!(headers.starts_with(&format!(
                    "{} {} http/1.1",
                    response.method.to_ascii_lowercase(),
                    response.path
                )));
                assert!(headers.contains(&format!("authorization: bearer {SECRET}")));
                assert!(
                    headers.contains(&format!("x-session-id: {}", session.to_ascii_lowercase()))
                );
                if response.path == "/execute?wait=0" {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&bytes[header_end..]).unwrap(),
                        json!({"command":"printf hello", "cwd":null})
                    );
                }
                sender.send(response.path.to_owned()).unwrap();
                tokio::time::sleep(response.delay).await;
                let body = response.body.to_string();
                let message = format!(
                    "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    body.len(),
                    body
                );
                let _ = socket.write_all(message.as_bytes()).await;
            }
        });
        let ctx = ToolCtx::new(CancellationToken::new())
            .with_run(run)
            .with_open_terminal(OpenTerminalContext {
                client: reqwest::Client::new(),
                policy: Arc::new(OpenTerminalPolicy {
                    base_url: format!("http://{address}"),
                    api_key: Some(SECRET.to_owned()),
                    poll_interval: Duration::from_millis(10),
                    request_timeout: Duration::from_secs(1),
                    execution_timeout: Duration::from_secs(2),
                    max_output_bytes: 64,
                }),
            });
        (ctx, receiver, task)
    }

    async fn call(ctx: &ToolCtx) -> ToolOutcome {
        OpenTerminalRun::new()
            .call(&json!({"command":"printf hello"}), ctx)
            .await
    }

    #[tokio::test]
    async fn http_contract_checks_auth_session_capabilities_and_async_output_cap() {
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        replies.push(reply(
            "GET",
            "/execute/job-1/status?wait=0&offset=0",
            process("running", ""),
        ));
        replies.push(reply(
            "GET",
            "/execute/job-1/status?wait=0&offset=0",
            process("done", &format!("{SECRET}{}", "é".repeat(100))),
        ));
        let (ctx, _requests, task) = server(replies).await;
        let result = call(&ctx).await;
        task.await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.len() <= 64);
        assert!(result.content.contains("truncated"));
        assert!(!result.content.contains(SECRET));
        assert!(!format!("{:?}", ctx.open_terminal.unwrap().policy).contains(SECRET));
    }

    #[tokio::test]
    async fn capability_and_health_failures_prevent_execution() {
        for body in [
            json!({"features":{"terminal":false}}),
            json!({"features":{}}),
        ] {
            let mut replies = checks();
            replies[1].body = body;
            let (ctx, _requests, task) = server(replies).await;
            assert!(call(&ctx).await.is_error);
            task.await.unwrap();
        }
        for body in [json!({"status":"unhealthy"}), json!({"unexpected":true})] {
            let (ctx, _requests, task) = server(vec![reply("GET", "/health", body)]).await;
            assert!(call(&ctx).await.is_error);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn polling_advances_cursor_and_preserves_incremental_output() {
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        let mut first = process("running", "first ");
        first["next_offset"] = json!(1);
        replies.push(reply("GET", "/execute/job-1/status?wait=0&offset=0", first));
        let mut last = process("done", "second");
        last["next_offset"] = json!(2);
        replies.push(reply("GET", "/execute/job-1/status?wait=0&offset=1", last));
        let (ctx, _requests, task) = server(replies).await;
        let result = call(&ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "exit: 0\nfirst second");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_poll_is_rejected_and_process_is_deleted() {
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        replies.push(reply(
            "GET",
            "/execute/job-1/status?wait=0&offset=0",
            process("done", &"x".repeat(MAX_RESPONSE_BYTES + 1)),
        ));
        replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
        let (ctx, _requests, task) = server(replies).await;
        assert!(call(&ctx).await.is_error);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn token_split_across_parts_and_output_ceiling_is_redacted() {
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        let mut first = process("running", &SECRET[..10]);
        first["next_offset"] = json!(1);
        replies.push(reply("GET", "/execute/job-1/status?wait=0&offset=0", first));
        let mut last = process("done", &format!("{}{}", &SECRET[10..], "x".repeat(100)));
        last["next_offset"] = json!(2);
        replies.push(reply("GET", "/execute/job-1/status?wait=0&offset=1", last));
        let (mut ctx, _requests, task) = server(replies).await;
        Arc::make_mut(&mut ctx.open_terminal.as_mut().unwrap().policy).max_output_bytes = 24;
        let result = call(&ctx).await;
        assert!(!result.content.contains("terminal"));
        assert!(result.content.len() <= 24);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_log_cursor_deletes_started_process() {
        for next_offset in [0, 2] {
            let mut replies = checks();
            replies.push(reply("POST", "/execute?wait=0", process("running", "")));
            let mut status = process("done", "one entry");
            status["next_offset"] = json!(next_offset);
            replies.push(reply(
                "GET",
                "/execute/job-1/status?wait=0&offset=0",
                status,
            ));
            replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
            let (ctx, _requests, task) = server(replies).await;
            assert!(call(&ctx).await.is_error);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_during_startup_deletes_eventually_returned_process() {
        let mut replies = checks();
        let mut execute = reply("POST", "/execute?wait=0", process("running", ""));
        execute.delay = Duration::from_millis(150);
        replies.push(execute);
        replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
        let (ctx, mut requests, task) = server(replies).await;
        let token = ctx.cancel.clone();
        let execution = tokio::spawn(async move { call(&ctx).await });
        while requests.recv().await.unwrap() != "/execute?wait=0" {}
        token.cancel();
        assert!(execution.await.unwrap().is_error);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_startup_with_known_id_deletes_process() {
        for body in [
            json!({"id":"job-1"}),
            json!({"id":"job-1", "status":"unexpected"}),
            json!({"id":"job-1", "status":"done"}),
        ] {
            let mut replies = checks();
            replies.push(reply("POST", "/execute?wait=0", body));
            replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
            let (ctx, _requests, task) = server(replies).await;
            assert!(call(&ctx).await.is_error);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn malformed_execute_and_echoed_server_errors_are_safe() {
        for body in [
            json!({"unexpected":true}),
            json!({"id":"../secret", "status":"running"}),
        ] {
            let mut replies = checks();
            replies.push(reply("POST", "/execute?wait=0", body));
            let (ctx, _requests, task) = server(replies).await;
            assert!(call(&ctx).await.is_error);
            task.await.unwrap();
        }
        let mut response = reply("GET", "/health", json!({"detail":SECRET}));
        response.status = 401;
        let (ctx, _requests, task) = server(vec![response]).await;
        let result = call(&ctx).await;
        assert!(result.is_error);
        assert!(!result.content.contains(SECRET));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_poll_and_http_failure_delete_started_process() {
        for (body, status) in [
            (json!({"unexpected":true}), 200),
            (json!({"id":"other", "status":"running"}), 200),
            (process("surprise", ""), 200),
            (json!({"id":"job-1", "status":"done", "next_offset":0}), 200),
            (json!({"detail":SECRET}), 500),
        ] {
            let mut replies = checks();
            replies.push(reply("POST", "/execute?wait=0", process("running", "")));
            let mut poll = reply("GET", "/execute/job-1/status?wait=0&offset=0", body);
            poll.status = status;
            replies.push(poll);
            replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
            let (ctx, _requests, task) = server(replies).await;
            let result = call(&ctx).await;
            assert!(result.is_error);
            assert!(!result.content.contains(SECRET));
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_and_execution_timeout_delete_started_process() {
        for cancel in [false, true] {
            let mut replies = checks();
            replies.push(reply("POST", "/execute?wait=0", process("running", "")));
            let mut poll = reply(
                "GET",
                "/execute/job-1/status?wait=0&offset=0",
                process("running", ""),
            );
            poll.delay = Duration::from_millis(750);
            replies.push(poll);
            replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
            let (mut ctx, mut requests, task) = server(replies).await;
            Arc::make_mut(&mut ctx.open_terminal.as_mut().unwrap().policy).execution_timeout =
                Duration::from_millis(500);
            let token = ctx.cancel.clone();
            let execution = tokio::spawn(async move { call(&ctx).await });
            while requests.recv().await.unwrap() != "/execute/job-1/status?wait=0&offset=0" {}
            if cancel {
                token.cancel();
            }
            let result = execution.await.unwrap();
            assert!(result.is_error);
            assert!(
                result
                    .content
                    .contains(if cancel { "cancelled" } else { "timed out" })
            );
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn request_timeout_returns_error_and_deletes_process() {
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        let mut poll = reply(
            "GET",
            "/execute/job-1/status?wait=0&offset=0",
            process("running", ""),
        );
        poll.delay = Duration::from_millis(750);
        replies.push(poll);
        replies.push(reply("DELETE", "/execute/job-1?force=true", json!({})));
        let (mut ctx, _requests, task) = server(replies).await;
        Arc::make_mut(&mut ctx.open_terminal.as_mut().unwrap().policy).request_timeout =
            Duration::from_millis(500);
        assert!(call(&ctx).await.is_error);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn executor_requires_approval_before_remote_execution() {
        use lightagent_core::{
            ApprovalDecision, ApprovalNeed, PolicyEngine, ToolCall, ToolInvoker,
        };
        let mut replies = checks();
        replies.push(reply("POST", "/execute?wait=0", process("running", "")));
        replies.push(reply(
            "GET",
            "/execute/job-1/status?wait=0&offset=0",
            process("done", "hello"),
        ));
        let (ctx, mut requests, task) = server(replies).await;
        let executor = crate::BoundedExecutor::new(
            crate::ToolRegistry::builtin(),
            PolicyEngine::new(lightagent_core::ApprovalPolicy::Strict.into()),
            Duration::from_millis(1),
            1024,
        )
        .with_run(ctx.run.unwrap())
        .with_open_terminal(ctx.open_terminal.unwrap());
        let call = ToolCall {
            id: "terminal-approval".into(),
            name: OpenTerminalRun::NAME.into(),
            arguments: json!({"command":"printf hello"}).to_string(),
        };
        assert!(executor.invoke(&call, ctx.cancel.clone()).await.is_error);
        assert!(requests.try_recv().is_err());
        let ApprovalNeed::Require(request) = executor.approval_for(&call) else {
            panic!("executable remote command must request approval")
        };
        executor.remember(&ApprovalDecision::grant(request.id), &call);
        assert!(!executor.invoke(&call, ctx.cancel.clone()).await.is_error);
        task.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a real isolated Open Terminal service"]
    async fn live_open_terminal_execution_and_cancellation_cleanup() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let base_url =
            std::env::var("LIGHTAGENT_OPEN_TERMINAL_URL").expect("live Open Terminal URL");
        let token =
            std::env::var("LIGHTAGENT_OPEN_TERMINAL_TOKEN").expect("live service test token");
        let remote = OpenTerminalContext {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            policy: Arc::new(OpenTerminalPolicy {
                base_url,
                api_key: Some(token),
                poll_interval: Duration::from_millis(20),
                request_timeout: Duration::from_secs(3),
                execution_timeout: Duration::from_secs(10),
                max_output_bytes: 128,
            }),
        };
        let ctx = ToolCtx::new(CancellationToken::new())
            .with_run(lightagent_core::RunId::new())
            .with_open_terminal(remote.clone());
        let output = OpenTerminalRun::new()
            .call(&json!({"command":"printf lightagent-live-terminal"}), &ctx)
            .await;
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("lightagent-live-terminal"));
        let token = ctx.cancel.clone();
        let session = ctx.run.as_ref().unwrap().as_str().to_owned();
        let run = tokio::spawn(async move {
            OpenTerminalRun::new()
                .call(&json!({"command":"sleep 30"}), &ctx)
                .await
        });
        let id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let processes = request(&remote, Method::GET, "/execute", &session)
                    .send()
                    .await
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap();
                if let Some(id) = processes
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|process| {
                        process["command"] == "sleep 30" && process["status"] == "running"
                    })
                    .and_then(|process| process["id"].as_str())
                {
                    break id.to_owned();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("live command should start");
        token.cancel();
        assert!(run.await.unwrap().is_error);
        // A forced cancellation is asynchronous. Open Terminal may either keep
        // the record with a terminal status or compact it from `/execute`;
        // neither outcome may leave the command running.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                // The process list exposes authoritative state without reading
                // logs while upstream concurrently closes the killed runner.
                let processes = request(&remote, Method::GET, "/execute", &session)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap();
                if !processes
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|process| process["id"] == id && process["status"] == "running")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cancellation DELETE should leave no live process");
    }

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
