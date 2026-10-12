//! The client against a loopback HTTP server: bounds, cancellation, retries,
//! secrets and both response formats. No test reaches the real service.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightagent_jev::{
    Answer, EvaluationRequest, JevClient, JevError, Limits, Question, Response, ResponseFormat,
    RetryPolicy,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// One scripted reply.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
    delay: Duration,
    content_length: bool,
}

impl Reply {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
            content_length: true,
        }
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// A server answering each connection with the next reply, recording every
/// raw request. Returns its base URL and the shared request log.
async fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let request = read_request(&mut stream).await;
            log.lock().unwrap().push(request);
            tokio::time::sleep(reply.delay).await;
            let mut head = format!(
                "HTTP/1.1 {} Test\r\ncontent-type: application/json\r\nconnection: close\r\n",
                reply.status
            );
            if reply.content_length {
                head.push_str(&format!("content-length: {}\r\n", reply.body.len()));
            }
            for (name, value) in &reply.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(reply.body.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (format!("http://{address}"), seen)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    while let Ok(count) = stream.read(&mut buf).await {
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..count]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..offset]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= offset + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn route_request() -> EvaluationRequest {
    EvaluationRequest::new(
        "jev-1.13.0",
        "a synthetic task",
        BTreeMap::from([(
            "route".to_owned(),
            Question::choice(
                "Pick a route.",
                [
                    ("default".to_owned(), "Keep it.".to_owned()),
                    ("model:fast".to_owned(), "Use fast.".to_owned()),
                ],
            ),
        )]),
    )
}

const TYPED: &str = r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"model:fast","confidence":0.9,"probabilities":{"default":0.05,"model:fast":0.95}}},"usage":{"input_tokens":42,"output_tokens":4}}"#;

fn client(base_url: &str, timeout: Duration, retry: RetryPolicy) -> JevClient {
    JevClient::new(
        base_url,
        None,
        Limits {
            timeout,
            ..Limits::default()
        },
        retry,
    )
    .unwrap()
}

#[tokio::test]
async fn a_typed_answer_reports_its_model_version_and_usage() {
    let (url, seen) = serve(vec![Reply::json(200, TYPED)]).await;
    let response = client(&url, Duration::from_secs(2), RetryPolicy::none())
        .evaluate(&route_request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.format(), ResponseFormat::TypeSafe);
    let Response::TypeSafe(evaluation) = response else {
        unreachable!()
    };
    assert_eq!(evaluation.model.as_deref(), Some("jev-1.13.0"));
    let usage = evaluation.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(42), Some(4))
    );
    assert!(
        matches!(&evaluation.answers["route"], Answer::Choice { choice, .. } if choice == "model:fast")
    );
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.starts_with("POST /v1/systemone HTTP/1.1"));
    assert!(request.contains("\"model\":\"jev-1.13.0\""));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
}

#[tokio::test]
async fn the_legacy_envelope_is_reported_as_such() {
    let (url, _) = serve(vec![Reply::json(
        200,
        r#"{"output":{"choice":"fast","score":0.9}}"#,
    )])
    .await;
    let response = client(&url, Duration::from_secs(2), RetryPolicy::none())
        .evaluate(&route_request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(response.format(), ResponseFormat::LegacyOutput);
}

#[tokio::test]
async fn cancellation_ends_an_in_flight_call_promptly() {
    let (url, _) = serve(vec![
        Reply::json(200, TYPED).delayed(Duration::from_secs(5)),
    ])
    .await;
    let client = client(&url, Duration::from_secs(10), RetryPolicy::none());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let start = std::time::Instant::now();
    let result = client.evaluate(&route_request(), &cancel).await;
    assert_eq!(result, Err(JevError::Cancelled));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn an_already_cancelled_call_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = client(&url, Duration::from_secs(2), RetryPolicy::none())
        .evaluate(&route_request(), &cancel)
        .await;
    assert_eq!(result, Err(JevError::Cancelled));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "a cancelled call must not connect"
    );
}

#[tokio::test]
async fn the_deadline_bounds_a_slow_server() {
    let (url, _) = serve(vec![
        Reply::json(200, TYPED).delayed(Duration::from_secs(5)),
    ])
    .await;
    let start = std::time::Instant::now();
    let result = client(&url, Duration::from_millis(300), RetryPolicy::bounded(3))
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(result, Err(JevError::Timeout));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn retry_after_is_honored_within_the_deadline() {
    let (url, seen) = serve(vec![
        Reply::json(429, r#"{"error":"slow down"}"#).header("retry-after-ms", "50"),
        Reply::json(200, TYPED),
    ])
    .await;
    let result = client(&url, Duration::from_secs(2), RetryPolicy::bounded(1))
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_retry_after_beyond_the_deadline_fails_without_waiting() {
    let (url, seen) = serve(vec![
        Reply::json(529, "{}").header("retry-after", "30"),
        Reply::json(200, TYPED),
    ])
    .await;
    let start = std::time::Instant::now();
    let result = client(&url, Duration::from_secs(1), RetryPolicy::bounded(3))
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(
        result,
        Err(JevError::Overloaded {
            retry_after: Some(Duration::from_secs(30))
        })
    );
    assert!(start.elapsed() < Duration::from_millis(800));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn retries_never_exceed_the_hard_cap() {
    let busy = Reply::json(503, "{}").header("retry-after-ms", "10");
    let (url, seen) = serve(vec![busy; 10]).await;
    let result = client(&url, Duration::from_secs(5), RetryPolicy::bounded(50))
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(result.unwrap_err().kind(), "status");
    assert_eq!(
        seen.lock().unwrap().len(),
        lightagent_jev::MAX_RETRIES as usize + 1
    );
}

#[tokio::test]
async fn permanent_failures_are_not_retried() {
    for (status, kind) in [(401, "unauthorized"), (422, "rejected"), (302, "status")] {
        let (url, seen) = serve(vec![Reply::json(status, "{}"), Reply::json(200, TYPED)]).await;
        let result = client(&url, Duration::from_secs(2), RetryPolicy::bounded(3))
            .evaluate(&route_request(), &CancellationToken::new())
            .await;
        assert_eq!(result.unwrap_err().kind(), kind);
        assert_eq!(seen.lock().unwrap().len(), 1, "HTTP {status}");
    }
    let (url, seen) = serve(vec![Reply::json(200, "not json"), Reply::json(200, TYPED)]).await;
    let result = client(&url, Duration::from_secs(2), RetryPolicy::bounded(3))
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(result.unwrap_err().kind(), "malformed");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_requests_are_never_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let client = JevClient::new(
        &url,
        None,
        Limits {
            max_request_bytes: 64,
            ..Limits::default()
        },
        RetryPolicy::none(),
    )
    .unwrap();
    let result = client
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(result, Err(JevError::RequestTooLarge { limit: 64 }));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn oversized_responses_are_abandoned_with_or_without_a_length() {
    let big = "x".repeat(lightagent_jev::MAX_RESPONSE_BYTES + 1);
    let mut streamed = Reply::json(200, big.clone());
    streamed.content_length = false;
    for reply in [Reply::json(200, big), streamed] {
        let (url, _) = serve(vec![reply]).await;
        let result = client(&url, Duration::from_secs(2), RetryPolicy::none())
            .evaluate(&route_request(), &CancellationToken::new())
            .await;
        assert_eq!(result.unwrap_err().kind(), "response_too_large");
    }
}

#[tokio::test]
async fn the_key_is_sent_only_as_a_bearer_header_and_never_echoed() {
    let secret = "jev-contract-private-secret";
    let (url, seen) = serve(vec![Reply::json(401, format!(r#"{{"error":"{secret}"}}"#))]).await;
    let client = JevClient::new(
        &url,
        Some(secret.to_owned()),
        Limits::default(),
        RetryPolicy::none(),
    )
    .unwrap();
    let error = client
        .evaluate(&route_request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(!format!("{error} {error:?} {client:?}").contains(secret));
    let request = seen.lock().unwrap()[0].clone();
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains(&format!("Bearer {secret}")));
    assert!(!body.contains(secret));
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_connect_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    // Generous: a refused loopback connection can take seconds on Windows.
    let result = client(&url, Duration::from_secs(10), RetryPolicy::none())
        .evaluate(&route_request(), &CancellationToken::new())
        .await;
    assert_eq!(result, Err(JevError::Connect));
}
