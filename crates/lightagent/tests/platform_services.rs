//! Live compatibility checks for the externally provisioned RAG services.
//!
//! This is deliberately ignored by default: it downloads models and talks to
//! real HTTP services. Run it with the two URLs supplied by the platform
//! compose file or CI job:
//!
//! ```text
//! QDRANT_URL=http://127.0.0.1:6333 \
//! INFINITY_URL=http://127.0.0.1:7997 \
//! cargo test -p lightagent --test platform_services -- --ignored
//! ```

use std::time::{SystemTime, UNIX_EPOCH};

use lightagent_rag::{RagStore, embed::HashingEmbedder};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const EMBEDDING_MODEL: &str = "BAAI/bge-small-en-v1.5";
const RERANK_MODEL: &str = "mixedbread-ai/mxbai-rerank-xsmall-v1";

fn configured_url(name: &str, legacy_name: &str) -> String {
    std::env::var(name)
        .or_else(|_| std::env::var(legacy_name))
        .unwrap_or_else(|_| {
            panic!("{name} (or legacy {legacy_name}) must be set for this live test")
        })
        .trim_end_matches('/')
        .to_owned()
}

async fn response_json(response: reqwest::Response, operation: &str) -> Value {
    let status = response.status();
    let body = response.text().await.expect("read HTTP response body");
    assert!(
        status.is_success(),
        "{operation} failed with {status}: {body}"
    );
    serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("{operation} returned invalid JSON: {error}; body: {body}"))
}

fn embedding_vector(response: &Value, index: usize) -> Vec<f32> {
    response["data"][index]["embedding"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("Infinity response is missing data[{index}].embedding: {response}")
        })
        .iter()
        .map(|value| value.as_f64().expect("embedding values must be numbers") as f32)
        .collect()
}

/// Verifies the exact wire contracts Lightagent relies on against real service
/// containers: Qdrant readiness/create/upsert/filtered-query and Infinity
/// health/embedding/rerank. It also proves that a down remote path leaves the
/// persisted local RAG index usable.
#[tokio::test]
#[ignore = "requires provisioned Qdrant and Infinity services"]
async fn qdrant_and_infinity_live_contract_and_local_fallback() {
    // The workspace deliberately builds reqwest with rustls-no-provider; the
    // production clients install `ring` before building their HTTP clients.
    // This direct live-contract client must do the same.
    lightagent_provider_lightweight::ensure_provider();
    let qdrant = configured_url("QDRANT_URL", "LIGHTAGENT_QDRANT_URL");
    let infinity = configured_url("INFINITY_URL", "LIGHTAGENT_INFINITY_URL");
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .expect("build HTTP client");

    // Qdrant exposes Kubernetes readiness at /readyz. Infinity's documented
    // readiness equivalent is /health (it has no /readyz endpoint).
    assert_eq!(
        client
            .get(format!("{qdrant}/readyz"))
            .send()
            .await
            .expect("request Qdrant /readyz")
            .status(),
        StatusCode::OK,
        "Qdrant must be ready before collection operations"
    );
    let infinity_health = response_json(
        client
            .get(format!("{infinity}/health"))
            .send()
            .await
            .expect("request Infinity /health"),
        "Infinity /health",
    )
    .await;
    assert!(
        infinity_health["unix"].is_number(),
        "Infinity /health contract changed: {infinity_health}"
    );

    let inputs = vec![
        "Lightagent stores retrieval chunks in Qdrant.".to_owned(),
        "The unrelated bakery sells sourdough every morning.".to_owned(),
    ];
    let embeddings = response_json(
        client
            .post(format!("{infinity}/embeddings"))
            .json(&json!({"model": EMBEDDING_MODEL, "input": inputs}))
            .send()
            .await
            .expect("request Infinity embeddings"),
        "Infinity /embeddings",
    )
    .await;
    let first = embedding_vector(&embeddings, 0);
    let second = embedding_vector(&embeddings, 1);
    assert!(!first.is_empty(), "Infinity returned an empty embedding");
    assert_eq!(first.len(), second.len(), "embedding dimensions must agree");
    assert!(first.iter().all(|value| value.is_finite()));

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let collection = format!("lightagent-contract-{nonce}");
    let collection_url = format!("{qdrant}/collections/{collection}");
    response_json(
        client
            .put(&collection_url)
            .json(&json!({"vectors": {"size": first.len(), "distance": "Cosine"}}))
            .send()
            .await
            .expect("create Qdrant collection"),
        "Qdrant collection creation",
    )
    .await;

    response_json(
        client
            .put(format!("{collection_url}/points?wait=true"))
            .json(&json!({"points": [
                {"id": 1, "vector": first, "payload": {"source": "qdrant.md", "text": inputs[0], "profile": "integration"}},
                {"id": 2, "vector": second, "payload": {"source": "bakery.md", "text": inputs[1], "profile": "other"}}
            ]}))
            .send()
            .await
            .expect("upsert Qdrant points"),
        "Qdrant point upsert",
    )
    .await;

    let query_embedding = response_json(
        client
            .post(format!("{infinity}/embeddings"))
            .json(&json!({"model": EMBEDDING_MODEL, "input": ["Where are Lightagent retrieval chunks stored?"]}))
            .send()
            .await
            .expect("embed query"),
        "Infinity query embedding",
    )
    .await;
    let query_vector = embedding_vector(&query_embedding, 0);
    let query = response_json(
        client
            .post(format!("{collection_url}/points/query"))
            .json(&json!({
                "query": query_vector,
                "limit": 4,
                "with_payload": true,
                "filter": {"must": [{"key": "profile", "match": {"value": "integration"}}]}
            }))
            .send()
            .await
            .expect("query Qdrant points"),
        "Qdrant filtered point query",
    )
    .await;
    let points = query["result"]["points"]
        .as_array()
        .or_else(|| query["result"].as_array())
        .unwrap_or_else(|| panic!("Qdrant query contract changed: {query}"));
    assert_eq!(
        points.len(),
        1,
        "profile filter must exclude other profiles"
    );
    assert_eq!(points[0]["payload"]["source"], "qdrant.md");

    let reranked = response_json(
        client
            .post(format!("{infinity}/rerank"))
            .json(&json!({
                "model": RERANK_MODEL,
                "query": "Where are Lightagent retrieval chunks stored?",
                "documents": ["The unrelated bakery sells sourdough every morning.", "Lightagent stores retrieval chunks in Qdrant."],
                "top_n": 1
            }))
            .send()
            .await
            .expect("request Infinity rerank"),
        "Infinity /rerank",
    )
    .await;
    let best = reranked["results"]
        .as_array()
        .and_then(|results| results.first())
        .unwrap_or_else(|| panic!("Infinity rerank response has no results: {reranked}"));
    assert_eq!(
        best["index"].as_u64(),
        Some(1),
        "reranker chose wrong document"
    );
    assert!(
        best["relevance_score"].as_f64().is_some(),
        "reranker must return a numeric relevance_score: {best}"
    );

    // A failed remote call must not corrupt or disable profile-local RAG.
    let missing_remote = Client::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .expect("build unavailable remote client")
        .get("http://127.0.0.1:9/embeddings")
        .send()
        .await;
    assert!(
        missing_remote.is_err(),
        "test requires an unavailable remote endpoint"
    );
    let local_path = std::env::temp_dir().join(format!("lightagent-local-fallback-{nonce}.jsonl"));
    let mut local = RagStore::open(&local_path).expect("open local profile RAG");
    local
        .add(
            "fallback.md",
            "The local profile RAG remains available when remote retrieval is down.",
            &HashingEmbedder,
            None,
            600,
            80,
        )
        .await
        .expect("add local fallback document");
    let hits = local
        .search(
            "Which retrieval works when remote is down?",
            &HashingEmbedder,
            None,
            1,
        )
        .await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, "fallback.md");
    let _ = std::fs::remove_file(local_path);

    // The test owns this unique collection; best-effort cleanup preserves the
    // original failure as the useful assertion when a service is unavailable.
    let _ = client.delete(&collection_url).send().await;
}
