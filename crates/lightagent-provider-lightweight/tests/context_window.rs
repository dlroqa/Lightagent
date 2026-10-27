//! Discovering the served context window over real loopback HTTP: the
//! versioned capabilities contract first, then Lightweight's model row, and
//! nothing from any other server or from a model's name.
use lightagent_provider_lightweight::{LightweightProvider, ProviderConfig};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Answer each connection with the next `(status, body)`, recording the
/// request lines.
async fn server(replies: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut lines = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            let head = String::from_utf8_lossy(&bytes).to_string();
            lines.push(head.lines().next().unwrap_or_default().to_owned());
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        lines
    });
    (format!("http://{address}"), task)
}

fn capabilities(protocol: &str, id: &str, context_length: u32) -> (u16, String) {
    (
        200,
        json!({
            "object": "capability.list",
            "protocol": {"name": protocol, "version": 1},
            "state": {"model_loaded": true, "model": {"id": id, "context_length": context_length}},
        })
        .to_string(),
    )
}

fn provider(base: String) -> LightweightProvider {
    LightweightProvider::new(ProviderConfig::new(base, "default")).unwrap()
}

#[tokio::test]
async fn the_capabilities_contract_reports_the_loaded_window() {
    let (base, task) = server(vec![capabilities(
        "lightweight-public-inference",
        "lfm2@8k",
        8_192,
    )])
    .await;
    assert_eq!(provider(base).context_window("lfm2@8k").await, Some(8_192));
    let lines = task.await.unwrap();
    assert!(lines[0].starts_with("GET /v1/capabilities "));
}

#[tokio::test]
async fn an_older_gateway_reports_the_window_on_its_model_row() {
    let row = json!({"data": [{
        "id": "lfm2@4k", "context_length": 4_096, "n_ctx": 4_096,
        "max_tokens": 4_096, "max_output_tokens": 4_096, "hermes": {"state": "loaded"},
    }]});
    let (base, task) = server(vec![(404, "{}".into()), (200, row.to_string())]).await;
    assert_eq!(provider(base).context_window("lfm2@4k").await, Some(4_096));
    let lines = task.await.unwrap();
    assert!(lines[1].starts_with("GET /v1/models "));
}

#[tokio::test]
async fn another_servers_figures_and_model_names_are_never_taken_for_a_window() {
    // A different contract, then a model row without Lightweight's namespace.
    let foreign = json!({"data": [{"id": "big@32k", "n_ctx": 32_768, "context_length": 32_768}]});
    let (base, _task) = server(vec![
        capabilities("someone-else", "big@32k", 32_768),
        (200, foreign.to_string()),
    ])
    .await;
    assert_eq!(provider(base).context_window("big@32k").await, None);
}

#[tokio::test]
async fn a_different_resident_model_reports_no_window_for_this_one() {
    let other = json!({"data": [{"id": "other@8k", "n_ctx": 8_192, "hermes": {}}]});
    let (base, _task) = server(vec![
        capabilities("lightweight-public-inference", "other@8k", 8_192),
        (200, other.to_string()),
    ])
    .await;
    assert_eq!(provider(base).context_window("lfm2@8k").await, None);
}

#[tokio::test]
async fn an_unreachable_gateway_reports_no_window() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{address}");
    assert_eq!(provider(base).context_window("lfm2@8k").await, None);
}
