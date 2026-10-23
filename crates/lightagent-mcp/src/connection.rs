//! The two MCP transports behind one [`Connection`] trait.
//!
//! [`StdioConnection`] spawns the server as a subprocess and speaks
//! newline-delimited JSON-RPC over its stdin/stdout, correlating each reply to
//! its request by id through a background reader task. [`HttpConnection`] speaks
//! the streamable-HTTP transport: it POSTs a request and reads the reply from
//! either a JSON body or a `text/event-stream` body, carrying the server's
//! `Mcp-Session-Id` on later calls. Both hand the client a `result` or an
//! [`McpError`]; neither knows any MCP method.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot, watch};

use crate::jsonrpc::{self, McpError};

/// A live link to one MCP server.
#[async_trait]
pub trait Connection: Send + Sync {
    /// Send a request and await its correlated result.
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError>;
    /// Send a fire-and-forget notification.
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError>;
}

/// The MCP protocol version this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

// --- stdio -----------------------------------------------------------------

struct StdioInner {
    pending: StdMutex<HashMap<i64, oneshot::Sender<Value>>>,
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicI64,
    timeout: Duration,
    /// Set once the server's output has ended: nothing more will be answered.
    closed: AtomicBool,
    /// The last lines the server wrote to stderr, bounded and sanitized.
    stderr: StdMutex<VecDeque<String>>,
    /// True once the server's stderr has ended.
    stderr_done: watch::Receiver<bool>,
}

/// How much of a server's stderr is kept: the last lines, each shortened.
const STDERR_LINES: usize = 20;
const STDERR_LINE_CHARS: usize = 300;
/// A stderr line longer than this is omitted whole, never cut, so no part of
/// a secret in it can show.
const STDERR_LINE_BYTES: usize = 4096;

/// One stderr line, made safe to show: the values of the variables the server
/// was started with are replaced, and so is everything after a credential-like
/// key (`token=`, `Authorization:`, `Bearer`); control characters are dropped
/// and the line is shortened.
fn sanitize(line: &str, secrets: &[String]) -> String {
    let mut text = line.to_owned();
    for secret in secrets {
        text = text.replace(secret.as_str(), "[redacted]");
    }
    let mut words = Vec::new();
    for word in text.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if let Some(split) = word.find(['=', ':'])
            && credential_key(&lower[..split])
        {
            words.push(format!("{}[redacted]", &word[..=split]));
            break;
        }
        if matches!(lower.as_str(), "bearer" | "basic" | "token") {
            words.push(format!("{word} [redacted]"));
            break;
        }
        words.push(word.to_owned());
    }
    let clean: String = words.join(" ").chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > STDERR_LINE_CHARS {
        let short: String = clean.chars().take(STDERR_LINE_CHARS).collect();
        format!("{short}…")
    } else {
        clean
    }
}

fn credential_key(key: &str) -> bool {
    [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "api-key",
        "authorization",
        "credential",
        "key",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
}

impl StdioInner {
    fn keep_stderr(&self, line: String) {
        if let Ok(mut lines) = self.stderr.lock() {
            if lines.len() == STDERR_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }
}

/// A server run as a subprocess, spoken to over stdin/stdout.
pub struct StdioConnection {
    inner: std::sync::Arc<StdioInner>,
    // Held so the child lives as long as the connection; stopped on drop.
    child: AsyncMutex<Option<Child>>,
    /// The directory the server runs from, when one was given.
    cwd: Option<PathBuf>,
}

/// Stdio servers that were stopped (their connection dropped) and have not
/// exited yet, by the directory each runs from. A process keeps its working
/// directory in use until it is gone, and on Windows a directory in use
/// cannot be moved or removed, so a bundle a server ran from must wait for
/// it: see [`wait_for_exit_under`].
static STOPPING: LazyLock<StdMutex<HashMap<u64, PathBuf>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
/// Notified each time a stopped server has exited.
static EXITED: LazyLock<Notify> = LazyLock::new(Notify::new);
static NEXT_STOPPING: AtomicU64 = AtomicU64::new(0);

/// Wait until every stopped stdio server that ran from `dir`, or from a
/// directory below it, has exited, for at most `timeout`. On a timeout,
/// returns how many had not.
pub async fn wait_for_exit_under(dir: &Path, timeout: Duration) -> Result<(), usize> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // Registered before the count is read, so an exit in between is not
        // missed.
        let mut exited = std::pin::pin!(EXITED.notified());
        exited.as_mut().enable();
        let pending = STOPPING
            .lock()
            .map(|stopping| {
                stopping
                    .values()
                    .filter(|cwd| cwd.starts_with(&dir))
                    .count()
            })
            .unwrap_or(0);
        if pending == 0 {
            return Ok(());
        }
        if tokio::time::timeout_at(deadline, exited).await.is_err() {
            return Err(pending);
        }
    }
}

impl Drop for StdioConnection {
    fn drop(&mut self) {
        let Some(mut child) = self.child.get_mut().take() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Stop it now; it would be killed on drop in any case.
        let _ = child.start_kill();
        let (Some(cwd), Ok(runtime)) = (self.cwd.clone(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let id = NEXT_STOPPING.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut stopping) = STOPPING.lock() {
            stopping.insert(id, cwd);
        }
        runtime.spawn(async move {
            let _ = child.wait().await;
            if let Ok(mut stopping) = STOPPING.lock() {
                stopping.remove(&id);
            }
            EXITED.notify_waiters();
        });
    }
}

impl StdioConnection {
    /// Spawn `command` and complete no handshake — the caller drives that.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        Self::spawn_in(command, args, env, None, timeout).await
    }

    /// Spawn a server with an optional working directory for extension-local assets.
    pub async fn spawn_in(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut cmd = tokio::process::Command::new(command);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.args(args)
            .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|error| {
            McpError::Transport(format!("could not start {command:?}: {error}"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdout".to_owned()))?;
        let stderr = child.stderr.take();
        let (stderr_finished, stderr_done) = watch::channel(false);

        let inner = std::sync::Arc::new(StdioInner {
            pending: StdMutex::new(HashMap::new()),
            stdin: AsyncMutex::new(stdin),
            next_id: AtomicI64::new(1),
            timeout,
            closed: AtomicBool::new(false),
            stderr: StdMutex::new(VecDeque::new()),
            stderr_done,
        });

        // Keep reading stderr so the server never blocks on it, holding only
        // its last lines, sanitized, for a failure report.
        let stderr_inner = std::sync::Arc::clone(&inner);
        let secrets: Vec<String> = env
            .iter()
            .map(|(_, value)| value.clone())
            .filter(|value| value.len() >= 4)
            .collect();
        tokio::spawn(async move {
            if let Some(mut stderr) = stderr {
                let mut chunk = [0u8; 1024];
                let mut line = Vec::new();
                let mut overlong = false;
                while let Ok(read) = stderr.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    for &byte in &chunk[..read] {
                        if byte != b'\n' {
                            if line.len() < STDERR_LINE_BYTES {
                                line.push(byte);
                            } else {
                                overlong = true;
                            }
                            continue;
                        }
                        let kept = if overlong {
                            "[a longer line was omitted]".to_owned()
                        } else {
                            sanitize(&String::from_utf8_lossy(&line), &secrets)
                        };
                        if !kept.is_empty() {
                            stderr_inner.keep_stderr(kept);
                        }
                        line.clear();
                        overlong = false;
                    }
                }
                if !line.is_empty() && !overlong {
                    stderr_inner.keep_stderr(sanitize(&String::from_utf8_lossy(&line), &secrets));
                }
            }
            let _ = stderr_finished.send(true);
        });

        let reader_inner = std::sync::Arc::clone(&inner);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue; // a line that is not JSON is not ours to route
                };
                if let Some(id) = jsonrpc::response_id(&message) {
                    let sender = reader_inner
                        .pending
                        .lock()
                        .ok()
                        .and_then(|mut pending| pending.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(message);
                    }
                }
                // Server-initiated requests and notifications are ignored: this
                // client exposes tools and needs no server-driven callbacks.
            }
            // The output ended: nothing waiting will be answered, so fail every
            // pending request now rather than at its deadline.
            reader_inner.closed.store(true, Ordering::SeqCst);
            if let Ok(mut pending) = reader_inner.pending.lock() {
                pending.clear();
            }
        });

        Ok(Self {
            inner,
            child: AsyncMutex::new(Some(child)),
            cwd: cwd.map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())),
        })
    }
}

impl StdioConnection {
    /// Why the server can no longer answer: it closed its output (it exited,
    /// or stopped speaking), with its exit status when it has exited and the
    /// last lines it wrote to stderr. Waits for its stderr to end, at most the
    /// request timeout, so those lines are complete.
    async fn closed(&self) -> McpError {
        let mut stderr_done = self.inner.stderr_done.clone();
        let _ = tokio::time::timeout(self.inner.timeout, stderr_done.wait_for(|done| *done)).await;
        let status = match self.child.lock().await.as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => format!(" ({status})"),
            _ => String::new(),
        };
        let stderr = self
            .inner
            .stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
            .unwrap_or_default();
        let stderr = if stderr.is_empty() {
            "; it wrote nothing to stderr".to_owned()
        } else {
            format!("; its last stderr: {stderr}")
        };
        McpError::Transport(format!(
            "the server closed its output before answering{status}{stderr}"
        ))
    }

    /// A write to the server failed: report why it can no longer answer if
    /// it has closed or exited, else the write error.
    async fn write_failed(&self, error: std::io::Error) -> McpError {
        let exited = matches!(
            self.child.lock().await.as_mut().map(Child::try_wait),
            Some(Ok(Some(_)))
        );
        if exited || self.inner.closed.load(Ordering::SeqCst) {
            self.closed().await
        } else {
            McpError::Transport(error.to_string())
        }
    }
}

#[async_trait]
impl Connection for StdioConnection {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.insert(id, sender);
        }
        // Checked after registering, so an end of output either is seen here
        // or drops this request's sender.
        if self.inner.closed.load(Ordering::SeqCst) {
            if let Ok(mut pending) = self.inner.pending.lock() {
                pending.remove(&id);
            }
            return Err(self.closed().await);
        }
        let line = format!("{}\n", jsonrpc::request(id, method, params));
        let written = {
            let mut stdin = self.inner.stdin.lock().await;
            match stdin.write_all(line.as_bytes()).await {
                Ok(()) => stdin.flush().await,
                Err(error) => Err(error),
            }
        };
        if let Err(error) = written {
            if let Ok(mut pending) = self.inner.pending.lock() {
                pending.remove(&id);
            }
            return Err(self.write_failed(error).await);
        }
        match tokio::time::timeout(self.inner.timeout, receiver).await {
            Ok(Ok(message)) => jsonrpc::read_result(message),
            Ok(Err(_)) => Err(self.closed().await),
            Err(_) => {
                if let Ok(mut pending) = self.inner.pending.lock() {
                    pending.remove(&id);
                }
                Err(McpError::Timeout)
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let line = format!("{}\n", jsonrpc::notification(method, params));
        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| McpError::Transport(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| McpError::Transport(error.to_string()))
    }
}

// --- streamable HTTP -------------------------------------------------------

/// A server reached over the streamable-HTTP transport.
pub struct HttpConnection {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    bearer: Option<String>,
    /// When the bearer is a profile's own grant, re-checked before every
    /// request: a refusal sends nothing.
    credential: Option<std::sync::Arc<lightagent_core::credentials::CredentialUse>>,
    session: StdMutex<Option<String>>,
    next_id: AtomicI64,
}

impl HttpConnection {
    /// Build a connection over an already-constructed client (redirects and TLS
    /// are the caller's to set), targeting `url`.
    pub fn new(
        client: reqwest::Client,
        url: String,
        headers: Vec<(String, String)>,
        bearer: Option<String>,
    ) -> Self {
        Self {
            client,
            url,
            headers,
            bearer,
            credential: None,
            session: StdMutex::new(None),
            next_id: AtomicI64::new(1),
        }
    }

    /// Re-check `credential` (the grant the bearer came from) before every
    /// request.
    pub fn with_credential(
        mut self,
        credential: std::sync::Arc<lightagent_core::credentials::CredentialUse>,
    ) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Refuse a request the grant no longer covers, before anything is sent.
    /// A tool call is checked against the grant's operations by tool name.
    fn permitted(&self, method: &str, params: &Value) -> Result<(), McpError> {
        let Some(credential) = &self.credential else {
            return Ok(());
        };
        let operation = (method == "tools/call")
            .then(|| params.get("name").and_then(Value::as_str).unwrap_or(""));
        credential
            .authorize(operation, &self.url)
            .map_err(|denial| {
                McpError::Transport(format!("credential refused ({})", denial.code()))
            })
    }

    fn builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .post(&self.url)
            .header(ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(body);
        for (name, value) in &self.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                builder = builder.header(name, value);
            }
        }
        if let Some(bearer) = &self.bearer {
            builder = builder.bearer_auth(bearer);
        }
        if let Some(session) = self.session.lock().ok().and_then(|s| s.clone()) {
            builder = builder.header("Mcp-Session-Id", session);
        }
        builder
    }

    fn capture_session(&self, response: &reqwest::Response) {
        if let Some(value) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            && let Ok(mut session) = self.session.lock()
        {
            *session = Some(value.to_owned());
        }
    }
}

#[async_trait]
impl Connection for HttpConnection {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.permitted(method, &params)?;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = jsonrpc::request(id, method, params);
        let response = self
            .builder(&body)
            .send()
            .await
            .map_err(|error| McpError::Transport(error.without_url().to_string()))?;
        self.capture_session(&response);
        if response.status().is_redirection() {
            // Never followed: credentials and headers stay with the
            // configured origin.
            return Err(McpError::Transport(format!(
                "redirect refused (HTTP {}); configure the server's final URL",
                response.status().as_u16()
            )));
        }
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {}",
                response.status().as_u16()
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        // Bounded, so a remote server (which may be reached over the network and
        // is not fully trusted) cannot exhaust memory with an unbounded body.
        let text = read_bounded_text(response, MAX_HTTP_BODY_BYTES).await?;
        let message = if content_type.contains("text/event-stream") {
            sse_message_for(&text, id)
                .ok_or_else(|| McpError::Protocol("no response in the event stream".to_owned()))?
        } else {
            serde_json::from_str::<Value>(&text)
                .map_err(|error| McpError::Protocol(format!("response was not JSON: {error}")))?
        };
        jsonrpc::read_result(message)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.permitted(method, &params)?;
        let body = jsonrpc::notification(method, params);
        let response = self
            .builder(&body)
            .send()
            .await
            .map_err(|error| McpError::Transport(error.without_url().to_string()))?;
        self.capture_session(&response);
        Ok(())
    }
}

/// The most bytes an HTTP MCP reply may be before it is refused.
///
/// A JSON-RPC message is small; this is a generous ceiling that only a runaway
/// or hostile server reaches. Chosen to match the 8 MiB frame cap the provider
/// adapter's SSE decoder uses.
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Read a response body as text, refusing one larger than `cap`.
///
/// `reqwest::Response::text` would buffer the whole body however large it is;
/// this streams it and stops with an error once `cap` is exceeded, so a server
/// cannot exhaust memory with an unbounded reply.
async fn read_bounded_text(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<String, McpError> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| McpError::Transport(error.without_url().to_string()))?
    {
        if buffer.len() + chunk.len() > cap {
            return Err(McpError::Protocol(format!(
                "the server's reply exceeded {cap} bytes"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    String::from_utf8(buffer)
        .map_err(|error| McpError::Protocol(format!("the reply was not UTF-8: {error}")))
}

/// Find the JSON-RPC response with `id` among an SSE body's `data:` events.
fn sse_message_for(body: &str, id: i64) -> Option<Value> {
    let mut fallback = None;
    for block in body.split("\n\n") {
        let mut data = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.trim_start());
            }
        }
        if data.is_empty() {
            continue;
        }
        if let Ok(message) = serde_json::from_str::<Value>(&data) {
            if jsonrpc::response_id(&message) == Some(id) {
                return Some(message);
            }
            if fallback.is_none() && jsonrpc::response_id(&message).is_some() {
                fallback = Some(message);
            }
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn stderr_lines_are_sanitized_and_bounded() {
        let secrets = vec!["s3cr3t-value".to_owned()];
        assert_eq!(
            sanitize("connecting with s3cr3t-value now", &secrets),
            "connecting with [redacted] now"
        );
        assert_eq!(
            sanitize("failed: token=abc123 and more", &secrets),
            "failed: token=[redacted]"
        );
        assert_eq!(
            sanitize("sent Authorization: Bearer abc", &secrets),
            "sent Authorization:[redacted]"
        );
        assert_eq!(
            sanitize("header bearer abc123", &secrets),
            "header bearer [redacted]"
        );
        assert_eq!(sanitize("api_key:xyz", &secrets), "api_key:[redacted]");
        assert_eq!(sanitize("plain\u{7}text", &secrets), "plaintext");
        let long = "x".repeat(STDERR_LINE_CHARS + 10);
        assert_eq!(
            sanitize(&long, &secrets).chars().count(),
            STDERR_LINE_CHARS + 1
        );
    }

    #[test]
    fn sse_body_yields_the_matching_response() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"ok\":true}}\n\n";
        let message = sse_message_for(body, 5).unwrap();
        assert_eq!(message["result"]["ok"], json!(true));
        assert!(
            sse_message_for(body, 99).is_some(),
            "falls back to a response"
        );
        assert!(sse_message_for("data: not json\n\n", 5).is_none());
    }

    /// A one-shot loopback server that answers with a body of `body_len` bytes.
    async fn serve_body(body_len: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let body = "a".repeat(body_len);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn a_body_within_the_cap_is_read() {
        lightagent_provider_lightweight::ensure_provider();
        let url = serve_body(500).await;
        let client = reqwest::Client::builder().build().unwrap();
        let response = client.get(&url).send().await.unwrap();
        let text = read_bounded_text(response, 8192).await.unwrap();
        assert_eq!(text.len(), 500);
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_refused_not_buffered() {
        lightagent_provider_lightweight::ensure_provider();
        let url = serve_body(4096).await;
        let client = reqwest::Client::builder().build().unwrap();
        let response = client.get(&url).send().await.unwrap();
        let error = read_bounded_text(response, 1024).await.unwrap_err();
        match error {
            McpError::Protocol(message) => assert!(message.contains("exceeded")),
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }
}
