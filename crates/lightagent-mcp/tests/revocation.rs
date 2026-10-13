//! A remote MCP tool follows its run's authorization: a revoked run sends no
//! `tools/call`, and a call in flight is abandoned when the run is revoked.
//! The server is an in-process connection that signals each request; every
//! wait is on that signal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::identity::{Denial, RevocationCheck, RunAuthorization};
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{PolicyEngine, RiskClass, ToolCall, ToolInvoker};
use lightagent_mcp::{Connection, McpClient, McpError, McpTool, McpToolDef};
use lightagent_tools::{BoundedExecutor, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Sets a flag when dropped: proves the request future was torn down.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Answers `tools/call` at once, or holds it until torn down.
struct Server {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    hold: bool,
}

#[async_trait]
impl Connection for Server {
    async fn request(&self, method: &str, _params: Value) -> Result<Value, McpError> {
        if method != "tools/call" {
            return Ok(json!({}));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.hold {
            return Ok(json!({"content": [{"type": "text", "text": "pong"}], "isError": false}));
        }
        let _guard = DropFlag(self.dropped.clone());
        self.started.notify_one();
        std::future::pending::<Result<Value, McpError>>().await
    }

    async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
        Ok(())
    }
}

struct Fixture {
    executor: Arc<BoundedExecutor>,
    authorization: RunAuthorization,
    allowed: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

fn fixture(hold: bool) -> Fixture {
    let (calls, started, dropped) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(Notify::new()),
        Arc::new(AtomicBool::new(false)),
    );
    let server = Server {
        calls: calls.clone(),
        started: started.clone(),
        dropped: dropped.clone(),
        hold,
    };
    let tool = McpTool::new(
        "remote",
        Arc::new(McpClient::new(Box::new(server))),
        McpToolDef {
            name: "ping".to_owned(),
            description: "Ping.".to_owned(),
            input_schema: json!({"type": "object"}),
            risk: RiskClass::External,
        },
    );
    let allowed = Arc::new(AtomicBool::new(true));
    let flag = allowed.clone();
    let check: RevocationCheck = Arc::new(move || {
        if flag.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Denial::Revoked)
        }
    });
    let authorization = RunAuthorization::revocable(check, CancellationToken::new(), None);
    let executor = BoundedExecutor::new(
        ToolRegistry::new().with(Arc::new(tool)),
        PolicyEngine::new(ApprovalPolicy::permissive()),
        Duration::from_secs(60),
        65_536,
    )
    .with_authorization(authorization.clone());
    Fixture {
        executor: Arc::new(executor),
        authorization,
        allowed,
        calls,
        started,
        dropped,
    }
}

fn ping() -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: "mcp.remote.ping".to_owned(),
        arguments: "{}".to_owned(),
    }
}

#[tokio::test]
async fn a_revoked_run_sends_no_mcp_request() {
    let fixture = fixture(false);
    let cancel = fixture.authorization.cancel_token().clone();
    let outcome = fixture.executor.invoke(&ping(), cancel.clone()).await;
    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);

    fixture.allowed.store(false, Ordering::SeqCst);
    let outcome = fixture.executor.invoke(&ping(), cancel).await;
    assert!(outcome.is_error && outcome.content.contains("withdrawn"));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1, "nothing was sent");
}

#[tokio::test]
async fn revocation_during_an_mcp_request_abandons_it() {
    let fixture = fixture(true);
    let running = {
        let executor = fixture.executor.clone();
        let cancel = fixture.authorization.cancel_token().clone();
        tokio::spawn(async move { executor.invoke(&ping(), cancel).await })
    };
    tokio::time::timeout(Duration::from_secs(30), fixture.started.notified())
        .await
        .expect("the request never arrived");
    assert!(fixture.authorization.revoke(Denial::Revoked));
    let outcome = running.await.unwrap();
    assert!(outcome.is_error && outcome.content.contains("cancelled"));
    assert!(
        fixture.dropped.load(Ordering::SeqCst),
        "the request was dropped"
    );
    // And nothing more is sent.
    let outcome = fixture
        .executor
        .invoke(&ping(), fixture.authorization.cancel_token().clone())
        .await;
    assert!(outcome.is_error);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}
