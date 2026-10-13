//! Cross-process revocation: the operator CLI (`lightagent identity`, run as
//! a real child process) changes the durable identity store while a
//! supervised run, in this process, is inside a long remote MCP call. The
//! revalidation monitor observes the change at its next sweep and tears the
//! call down. Synthetic identities; the MCP server is in-process.

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("../../lightagent-api/tests/support/fixtures.rs");

use lightagent_mcp::{Connection, McpClient, McpError, McpTool, McpToolDef};

/// Holds every `tools/call` until torn down, signalling its arrival.
struct HeldServer {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Connection for HeldServer {
    async fn request(&self, method: &str, _params: Value) -> Result<Value, McpError> {
        if method != "tools/call" {
            return Ok(json!({}));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _guard = DropFlag(self.dropped.clone());
        self.started.notify_one();
        std::future::pending().await
    }

    async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
        Ok(())
    }
}

/// Run `lightagent identity …` against `home` and return its JSON output.
fn cli(home: &std::path::Path, args: &[&str]) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lightagent"))
        .env("LIGHTAGENT_HOME", home)
        .arg("--json")
        .arg("identity")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn a_cli_revocation_in_another_process_stops_a_long_mcp_call() {
    let home = std::env::temp_dir().join(format!("lightagent-xproc-{}", RunId::new().as_str()));
    std::fs::create_dir_all(&home).unwrap();
    ProfileStore::new(&home)
        .create(&AgentProfile::new(
            profile("research"),
            "Research",
            "Persona",
            "fast",
        ))
        .unwrap();

    // Provisioned by the operator CLI, in other processes.
    cli(&home, &["user", "create", "alice"]);
    cli(&home, &["owner", "assign", "research", "alice"]);
    let secret = home.join("alice.token");
    let issued = cli(
        &home,
        &[
            "credential",
            "issue",
            "alice",
            "--secret-file",
            secret.to_str().unwrap(),
        ],
    );
    let key = issued["issued"].as_str().unwrap().to_owned();
    assert!(issued.get("secret").is_none(), "no secret in JSON output");
    let token = std::fs::read_to_string(&secret).unwrap().trim().to_owned();

    // This process: a durable authority and a supervised run.
    let identity = Arc::new(IdentityAuthority::persistent(Arc::new(IdentityStore::new(
        &home,
    ))));
    let auth = AuthConfig::multi_user(identity.clone(), None);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let caller = auth.caller(&headers).unwrap();
    let server = HeldServer {
        calls: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
    };
    let (calls, started, dropped) = (
        server.calls.clone(),
        server.started.clone(),
        server.dropped.clone(),
    );
    let tool = McpTool::new(
        "remote",
        Arc::new(McpClient::new(Box::new(server))),
        McpToolDef {
            name: "work".to_owned(),
            description: "Long remote work.".to_owned(),
            input_schema: json!({"type": "object"}),
            risk: RiskClass::External,
        },
    );
    let lab = Arc::new(Lab::default());
    let (paced, ticks) = manual_clock();
    let manager = RunManager::new(lab.clone()).with_revalidation(paced);
    let model = Model::new(vec![
        Turn::Call("mcp.remote.work", "{}"),
        Turn::Say("never"),
    ]);
    let mut script = Script::new(model.clone());
    script.tools.insert(Arc::new(tool));
    lab.script("remote work", script);
    let owner = RunOwner::new(
        principal("alice"),
        Some(profile("research")),
        RunTrust::RemoteUser,
    )
    .with_credential(key.clone());
    let run = manager
        .start_supervised(
            StartRun {
                message: "remote work".to_owned(),
                history: Vec::new(),
                profile: Some("research".to_owned()),
                model: None,
                cwd: None,
            },
            owner.clone(),
            identity.run_check(caller, owner),
            identity.generation(),
        )
        .await
        .unwrap();
    signalled(&started).await;

    // A sweep with nothing changed leaves the run alone.
    tick(&manager, &ticks).await;
    assert_eq!(run.status().await, RunStatus::Running);

    // The operator revokes from another process.
    let revoked = cli(&home, &["credential", "revoke", &key]);
    assert_eq!(revoked["revoked"], json!(key));
    let state = tick(&manager, &ticks).await;
    assert_eq!(state.revoked, 1);
    assert_eq!(finished(&run).await, RunStatus::Cancelled);
    assert_eq!(run.revocation(), Some(Denial::Revoked));
    assert!(dropped.load(Ordering::SeqCst), "the MCP call was torn down");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no new MCP request");
    assert_eq!(model.requests(), 1, "no new model request");

    // The CLI's audit trail records the revocation, without the secret.
    let audit = cli(&home, &["audit"]).to_string();
    assert!(audit.contains("credential_revoked"));
    assert!(!audit.contains(&token));
    std::fs::remove_dir_all(&home).ok();
}
