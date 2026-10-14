//! The provisioning HTTP contract, through the real handlers, against a
//! durable identity store in a scratch home. Built only with the
//! `provisioning-api` feature, which no release build enables. Synthetic
//! issuers, subjects and tokens only.

#![cfg(feature = "provisioning-api")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lightagent_api::provisioning::{MAX_BODY_BYTES, ProvisioningApi, routes};
use lightagent_api::{AuthConfig, IdentityAuthority};
use lightagent_core::identity::store::{IdentityStore, Transaction};
use lightagent_core::identity::{
    Caller, Capability, PrincipalId, PrincipalSource, Restriction, Role,
};
use lightagent_core::provisioning::ProvisioningService;
use lightagent_core::{ProfileId, ProfileStore, RunId};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const T0: u64 = 1_000_000;
const ISSUER: &str = "https://accounts.control.example";
const SUBJECT: &str = "synthetic-http-subject-01";
const LEGACY: &str = "legacy-operator-test-key";

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn principal(id: &str) -> PrincipalId {
    PrincipalId::new(id).unwrap()
}

fn operator() -> Caller {
    Caller::local_operator(PrincipalSource::LocalProcess)
}

fn change<R>(home: &Path, work: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
    let mut value = None;
    IdentityStore::new(home)
        .transact(&operator(), T0, None, |tx| {
            value = Some(work(tx));
            Ok(())
        })
        .unwrap();
    value.unwrap()
}

fn issue(home: &Path, id: &str, capabilities: Option<&[Capability]>) -> String {
    change(home, |tx| {
        tx.issue_credential(
            &principal(id),
            Restriction {
                capabilities: capabilities.map(|set| set.iter().copied().collect()),
                profiles: None,
            },
            None,
        )
        .unwrap()
        .1
        .expose()
        .to_owned()
    })
}

struct Server {
    home: Home,
    addr: String,
    site: String,
    ops: String,
    other: String,
}

/// A home with a trusted issuer, a registered provisioner (`control-site`),
/// a second one (`other-site`) and an unrestricted operator; served by the
/// provisioning routes over a persistent identity authority.
async fn server() -> Server {
    let root = std::env::temp_dir().join(format!(
        "lightagent-provision-http-{}",
        RunId::new().as_str()
    ));
    std::fs::create_dir_all(&root).unwrap();
    change(&root, |tx| {
        tx.trust_issuer(ISSUER).unwrap();
        for id in ["control-site", "other-site", "ops"] {
            tx.create_principal(principal(id), Role::Operator, None)
                .unwrap();
        }
    });
    let site = issue(&root, "control-site", Some(&[Capability::Provision]));
    let other = issue(&root, "other-site", Some(&[Capability::Provision]));
    let ops = issue(&root, "ops", None);
    let service = Arc::new(ProvisioningService::new(&root));
    for id in ["control-site", "other-site"] {
        service
            .register_provisioner(
                &operator(),
                &principal(id),
                [ISSUER.to_owned()].into(),
                ["assistant".to_owned()].into(),
                10,
            )
            .unwrap();
    }
    let identity = Arc::new(IdentityAuthority::persistent(Arc::new(IdentityStore::new(
        &root,
    ))));
    let api = ProvisioningApi {
        service,
        auth: Arc::new(AuthConfig::multi_user(identity, Some(LEGACY.to_owned()))),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, routes(api).into_make_service()).await;
    });
    Server {
        home: Home(root),
        addr,
        site,
        ops,
        other,
    }
}

/// (status, parsed body) of one request.
async fn call(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let payload = body.unwrap_or("");
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\n");
    if !token.is_empty() {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if body.is_some() {
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            request.push_str("Content-Type: application/json\r\n");
        }
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    request.push_str("\r\n");
    request.push_str(payload);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn body(subject: &str) -> String {
    json!({
        "issuer": ISSUER,
        "subject": subject,
        "display_name": "Research assistant",
        "template": "assistant",
    })
    .to_string()
}

const PROFILE: &str = "/api/lightagent/v1/provisioning/profiles/alice-research";

fn operation_path(id: &str) -> String {
    format!("/api/lightagent/v1/provisioning/operations/{id}")
}

#[tokio::test]
async fn provision_replay_status_and_revoke_over_http() {
    let server = server().await;
    let key = [("Idempotency-Key", "http-key-0001")];
    let (status, created) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let operation = &created["operation"];
    assert_eq!(created["replayed"], false);
    assert_eq!(operation["state"], "completed");
    assert_eq!(operation["owner_bound"], true);
    assert_eq!(operation["profile_id"], "alice-research");
    assert_eq!(operation["template"], "assistant");
    assert!(
        operation["principal_id"]
            .as_str()
            .unwrap()
            .starts_with("ext-")
    );
    // The response names no subject, issuer or token.
    let text = created.to_string();
    assert!(!text.contains(SUBJECT) && !text.contains(ISSUER) && !text.contains(&server.site));
    let id = operation["operation_id"].as_str().unwrap().to_owned();

    // Same key, same request: the same operation, 200.
    let (status, replayed) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["operation"], created["operation"]);
    // Same key, different request: 409.
    let (status, conflict) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&body("someone-else-0001")),
    )
    .await;
    assert_eq!(
        (status, conflict["code"].as_str()),
        (409, Some("idempotency_conflict"))
    );
    // Another key for a taken profile: 409, nothing changed.
    let (status, taken) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &[("Idempotency-Key", "http-key-0002")],
        Some(&body("someone-else-0001")),
    )
    .await;
    assert_eq!(
        (status, taken["code"].as_str()),
        (409, Some("profile_unavailable"))
    );

    let (status, read) = call(
        &server.addr,
        "GET",
        &operation_path(&id),
        &server.site,
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(read["operation"], created["operation"]);
    // Another provisioner sees nothing: the same 404 as a missing operation.
    for path in [
        operation_path(&id),
        operation_path(&"0".repeat(32)),
        operation_path("..%2F.."),
    ] {
        let (status, missing) = call(&server.addr, "GET", &path, &server.other, &[], None).await;
        assert_eq!(
            (status, missing["code"].as_str()),
            (404, Some("unknown_operation"))
        );
    }
    let (status, _) = call(
        &server.addr,
        "POST",
        &format!("{}/revoke", operation_path(&id)),
        &server.other,
        &[],
        None,
    )
    .await;
    assert_eq!(status, 404);

    let (status, revoked) = call(
        &server.addr,
        "POST",
        &format!("{}/revoke", operation_path(&id)),
        &server.site,
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(revoked["operation"]["state"], "revoked");
    assert_eq!(revoked["operation"]["owner_bound"], false);
    let (status, again) = call(
        &server.addr,
        "POST",
        &format!("{}/revoke", operation_path(&id)),
        &server.site,
        &[],
        None,
    )
    .await;
    assert_eq!((status, &again["operation"]), (200, &revoked["operation"]));
    // The durable store agrees: no owner.
    let store = IdentityStore::new(&server.home.0);
    assert!(
        store
            .effective_ownership(&store.load().unwrap())
            .get(&ProfileId::new("alice-research").unwrap())
            .is_none()
    );
    assert!(
        ProfileStore::new(&server.home.0)
            .list()
            .unwrap()
            .iter()
            .any(|p| p.as_str() == "alice-research")
    );
}

// Only a registered provisioning-only credential is admitted.
#[tokio::test]
async fn invalid_and_over_privileged_credentials_are_refused() {
    let server = server().await;
    let key = [("Idempotency-Key", "http-key-0003")];
    let unrestricted_site = issue(&server.home.0, "control-site", None);
    change(&server.home.0, |tx| {
        tx.create_principal(principal("mallory"), Role::User, None)
            .unwrap();
    });
    let user = issue(&server.home.0, "mallory", None);
    for (token, status, code) in [
        ("", 401, "unauthenticated"),
        (
            "la_0000000000000000_0000000000000000000000000000000000000000000000000000000000000000",
            401,
            "unauthenticated",
        ),
        ("not-a-token", 401, "unauthenticated"),
        // The legacy --key-env key is an operator key, not a provisioning one.
        (LEGACY, 401, "unauthenticated"),
        (server.ops.as_str(), 403, "credential_not_provisioning_only"),
        (
            unrestricted_site.as_str(),
            403,
            "credential_not_provisioning_only",
        ),
        (user.as_str(), 403, "not_a_provisioner"),
    ] {
        let (got, response) = call(
            &server.addr,
            "PUT",
            PROFILE,
            token,
            &key,
            Some(&body(SUBJECT)),
        )
        .await;
        assert_eq!(
            (got, response["code"].as_str()),
            (status, Some(code)),
            "{token}"
        );
    }
    // Revoking the credential stops the next request.
    let caller = IdentityStore::new(&server.home.0)
        .load()
        .unwrap()
        .credentials()
        .authenticate_at(&server.site, T0)
        .unwrap();
    let PrincipalSource::Credential { key_id } = caller.principal.source else {
        panic!("credential");
    };
    change(&server.home.0, |tx| tx.revoke_credential(&key_id).unwrap());
    let (status, _) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(status, 401);
    assert!(
        !server
            .home
            .0
            .join("profiles")
            .join("alice-research")
            .exists()
    );
}

// Bounded, validated bodies with stable codes; configuration is refused.
#[tokio::test]
async fn requests_are_bounded_and_validated() {
    let server = server().await;
    let key = [("Idempotency-Key", "http-key-0004")];
    let huge = json!({
        "issuer": ISSUER,
        "subject": SUBJECT,
        "display_name": "x".repeat(MAX_BODY_BYTES),
        "template": "assistant",
    })
    .to_string();
    let (status, response) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&huge),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (413, Some("payload_too_large"))
    );
    for extra in [
        json!({"toolsets": ["all"]}),
        json!({"approval_policy": "permissive"}),
        json!({"model": "large"}),
        json!({"owner": "control-site"}),
        json!({"principal_id": "alice"}),
        json!({"path": "/etc"}),
        json!({"profile_id": "other"}),
    ] {
        let mut request: Value = serde_json::from_str(&body(SUBJECT)).unwrap();
        for (name, value) in extra.as_object().unwrap() {
            request[name] = value.clone();
        }
        let (status, response) = call(
            &server.addr,
            "PUT",
            PROFILE,
            &server.site,
            &key,
            Some(&request.to_string()),
        )
        .await;
        assert_eq!(
            (status, response["code"].as_str()),
            (400, Some("invalid_request")),
            "{extra}"
        );
    }
    let (status, response) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some("{not json"),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (400, Some("invalid_request"))
    );
    let (status, response) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &[
            ("Idempotency-Key", "http-key-0004"),
            ("Content-Type", "text/plain"),
        ],
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (415, Some("unsupported_media_type"))
    );
    let (status, response) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &[],
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (400, Some("missing_idempotency_key"))
    );
    for (path, code) in [
        (
            "/api/lightagent/v1/provisioning/profiles/Alice",
            "invalid_profile_id",
        ),
        (
            "/api/lightagent/v1/provisioning/profiles/default",
            "reserved_profile_id",
        ),
        (
            "/api/lightagent/v1/provisioning/profiles/a%2F..%2Fb",
            "invalid_profile_id",
        ),
    ] {
        let (status, response) = call(
            &server.addr,
            "PUT",
            path,
            &server.site,
            &key,
            Some(&body(SUBJECT)),
        )
        .await;
        assert_eq!(
            (status, response["code"].as_str()),
            (400, Some(code)),
            "{path}"
        );
    }
    let mut google = serde_json::from_str::<Value>(&body(SUBJECT)).unwrap();
    google["issuer"] = json!("https://accounts.google.com");
    let (status, response) = call(
        &server.addr,
        "PUT",
        PROFILE,
        &server.site,
        &key,
        Some(&google.to_string()),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (403, Some("issuer_not_allowed"))
    );
    assert!(
        !server.home.0.join("profiles").exists()
            || ProfileStore::new(&server.home.0).list().unwrap().is_empty()
    );
}

// Without an identity authority, nothing is served.
#[tokio::test]
async fn provisioning_requires_the_identity_store() {
    let root = std::env::temp_dir().join(format!(
        "lightagent-provision-http-{}",
        RunId::new().as_str()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let home = Home(root);
    let api = ProvisioningApi {
        service: Arc::new(ProvisioningService::new(&home.0)),
        auth: Arc::new(AuthConfig::open()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, routes(api).into_make_service()).await;
    });
    let (status, response) = call(
        &addr,
        "PUT",
        PROFILE,
        "",
        &[("Idempotency-Key", "http-key-0005")],
        Some(&body(SUBJECT)),
    )
    .await;
    assert_eq!(
        (status, response["code"].as_str()),
        (503, Some("identity_unavailable"))
    );
    assert!(!home.0.join("provisioning").exists());
}
