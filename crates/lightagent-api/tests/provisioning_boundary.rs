//! A provisioning-only credential against the real production router in
//! multi-user mode: it reaches no run, session, tool, approval, Jev,
//! provider, profile or settings endpoint, and the provisioning routes
//! themselves are not served. Synthetic identities only.

// Not every binary that includes the fixtures uses all of them.
#![allow(dead_code, unused_imports)]

include!("support/fixtures.rs");

/// `site` holds an operator credential restricted to `provision`; `ops` an
/// unrestricted operator credential; `alice` owns `alice-bot`.
struct Boundary {
    addr: String,
    lab: Arc<Lab>,
    site: String,
    ops: String,
    identity: Arc<IdentityAuthority>,
}

async fn boundary() -> Boundary {
    let mut state = IdentityState::default();
    let site = state
        .credentials
        .issue(
            principal("control-site"),
            Role::Operator,
            Restriction {
                capabilities: Some([Capability::Provision].into()),
                profiles: None,
            },
            T0,
        )
        .unwrap()
        .expose()
        .to_owned();
    let ops = state
        .credentials
        .issue(principal("ops"), Role::Operator, Restriction::default(), T0)
        .unwrap()
        .expose()
        .to_owned();
    state
        .ownership
        .assign(profile("alice-bot"), principal("alice"))
        .unwrap();
    let identity = Arc::new(IdentityAuthority::new(state).with_clock(Arc::new(|| T0)));
    let lab = Arc::new(Lab::default());
    let manager = RunManager::new(lab.clone());
    let addr = serve(
        manager,
        AuthConfig::multi_user(identity.clone(), Some("legacy-operator-test-key".into())),
    )
    .await;
    Boundary {
        addr,
        lab,
        site,
        ops,
        identity,
    }
}

/// Every route of the production router, with a plausible body.
fn routes() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    let run = r#"{"message":"hello","profile":"alice-bot"}"#;
    vec![
        ("GET", "/api/lightagent/v1/tools", None),
        ("GET", "/api/lightagent/v1/tools?profile=alice-bot", None),
        ("GET", "/api/lightagent/v1/provider", None),
        ("GET", "/api/lightagent/v1/jev", None),
        ("GET", "/api/lightagent/v1/jev?profile=alice-bot", None),
        ("POST", "/api/lightagent/v1/jev/verify", Some("{}")),
        ("GET", "/api/lightagent/v1/skills", None),
        ("GET", "/api/lightagent/v1/profiles", None),
        ("GET", "/api/lightagent/v1/settings", None),
        ("PUT", "/api/lightagent/v1/settings", Some("{}")),
        ("POST", "/api/lightagent/v1/runs", Some(run)),
        ("GET", "/api/lightagent/v1/runs/0123456789abcdef", None),
        (
            "GET",
            "/api/lightagent/v1/runs/0123456789abcdef/events",
            None,
        ),
        (
            "POST",
            "/api/lightagent/v1/runs/0123456789abcdef/cancel",
            Some("{}"),
        ),
        ("GET", "/api/lightagent/v1/sessions", None),
        ("GET", "/api/lightagent/v1/sessions?profile=alice-bot", None),
        ("POST", "/api/lightagent/v1/sessions", Some("{}")),
        ("GET", "/api/lightagent/v1/sessions/search?q=x", None),
        ("GET", "/api/lightagent/v1/sessions/s1", None),
        ("PATCH", "/api/lightagent/v1/sessions/s1", Some("{}")),
        ("DELETE", "/api/lightagent/v1/sessions/s1", None),
        (
            "POST",
            "/api/lightagent/v1/sessions/s1/attachments",
            Some("{}"),
        ),
        ("GET", "/api/lightagent/v1/approvals", None),
        (
            "POST",
            "/api/lightagent/v1/approvals/0123456789abcdef",
            Some(r#"{"decision":"approve"}"#),
        ),
    ]
}

// A provisioning-only credential is refused by every route, before anything
// runs: no run starts, no session, catalog, diagnostic, profile list or
// setting is returned.
#[tokio::test]
async fn a_provisioning_credential_reaches_no_runtime_or_operator_endpoint() {
    let world = boundary().await;
    for (method, path, body) in routes() {
        let (status, response) = http(&world.addr, method, path, &world.site, body).await;
        assert!(
            matches!(status, 401 | 403 | 404 | 422),
            "{method} {path}: {status} {response}"
        );
        assert!(!(200..300).contains(&status), "{method} {path}");
        for leaked in ["alice-bot\"", "\"events\"", "\"sessions\"", "\"tools\""] {
            assert!(!response.contains(leaked), "{method} {path}: {response}");
        }
    }
    // The decisive ones are refused for the capability, exactly.
    for (method, path, body) in [
        (
            "POST",
            "/api/lightagent/v1/runs",
            Some(r#"{"message":"hello","profile":"alice-bot"}"#),
        ),
        ("GET", "/api/lightagent/v1/settings", None),
        ("GET", "/api/lightagent/v1/jev", None),
        ("GET", "/api/lightagent/v1/tools", None),
        ("GET", "/api/lightagent/v1/approvals", None),
        ("GET", "/api/lightagent/v1/sessions", None),
        ("GET", "/api/lightagent/v1/profiles", None),
        ("POST", "/api/lightagent/v1/jev/verify", Some("{}")),
    ] {
        let (status, response) = http(&world.addr, method, path, &world.site, body).await;
        assert_eq!(status, 403, "{method} {path}: {response}");
    }
    assert_eq!(
        world.lab.supervised.load(Ordering::SeqCst),
        0,
        "no run started"
    );
    assert_eq!(world.lab.plain.load(Ordering::SeqCst), 0, "no run started");
    // Every refusal was audited against the provisioning principal.
    assert!(world.identity.audit_log().iter().any(|record| {
        record.principal.as_ref() == Some(&principal("control-site"))
            && matches!(
                record.outcome,
                AuditOutcome::Denied(Denial::MissingCapability)
            )
    }));
}

// The production router does not serve the provisioning contract, for any
// caller: it is the ordinary unknown-endpoint answer.
#[tokio::test]
async fn the_production_router_does_not_serve_provisioning() {
    let world = boundary().await;
    for token in [
        world.site.as_str(),
        world.ops.as_str(),
        "legacy-operator-test-key",
        "",
    ] {
        for (method, path) in [
            (
                "PUT",
                "/api/lightagent/v1/provisioning/profiles/alice-research",
            ),
            (
                "GET",
                "/api/lightagent/v1/provisioning/operations/0123456789abcdef0123456789abcdef",
            ),
            (
                "POST",
                "/api/lightagent/v1/provisioning/operations/0123456789abcdef0123456789abcdef/revoke",
            ),
        ] {
            let (status, response) = http(&world.addr, method, path, token, Some("{}")).await;
            assert!(
                status == 404 || status == 405,
                "{method} {path}: {status} {response}"
            );
            assert!(!response.contains("operation"), "{response}");
        }
    }
}

// The single-user API is unchanged: no identity, no provisioning, the open
// loopback policy still serves the tool catalog.
#[tokio::test]
async fn single_user_mode_is_unchanged() {
    let lab = Arc::new(Lab::default());
    let addr = serve(RunManager::new(lab), AuthConfig::open()).await;
    let (status, _) = http(&addr, "GET", "/api/lightagent/v1/tools", "", None).await;
    assert_eq!(status, 200);
    let (status, _) = http(
        &addr,
        "PUT",
        "/api/lightagent/v1/provisioning/profiles/x",
        "",
        Some("{}"),
    )
    .await;
    assert!(status == 404 || status == 405);
}
