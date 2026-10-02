//! `lightagent rag` — index documents and search them, and the `rag.search`
//! tool wired into a run.
//!
//! Retrieval is per-profile: the active profile's index lives at
//! `<profile>/rag/index.jsonl`. `add` chunks and embeds a file (or every file in
//! a directory), `search` returns the best passages, `list` shows what is
//! indexed, and `clear` empties it. The same store, opened read-only, backs the
//! `rag.search` tool a chat or served run is given when the index is non-empty.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::{
    Config, ConfigStore, LightagentPaths, ProfileStore, RiskClass, Scope, ToolOutcome,
};
use lightagent_provider_lightweight::EmbeddingClient;
use lightagent_rag::{
    HashingEmbedder, Hit, RagSearch, RagStore, RealtimeRag, SemanticEmbedder, chunk, index_path,
};
use lightagent_tools::{Tool, ToolCtx, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};

/// A semantic embedder backed by an OpenAI-compatible embeddings endpoint.
struct ProviderSemanticEmbedder {
    client: EmbeddingClient,
    model: String,
}

#[async_trait]
impl SemanticEmbedder for ProviderSemanticEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.client
            .embed(&self.model, texts)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Bounded client for the optional remote retrieval plane.  It is deliberately
/// best-effort: callers always retain the profile-local index as their source
/// of truth when a remote component is unavailable or returns malformed data.
#[derive(Clone)]
struct RemoteRag {
    client: reqwest::Client,
    qdrant_url: String,
    qdrant_key: Option<String>,
    infinity_url: String,
    infinity_key: Option<String>,
    collection: String,
    embedding_model: String,
    rerank_model: String,
    profile: String,
}

#[derive(Deserialize)]
struct QdrantResponse<T> {
    result: T,
}

#[derive(Deserialize)]
struct QdrantQueryResult {
    #[serde(default)]
    points: Vec<QdrantPoint>,
}

#[derive(Deserialize)]
struct QdrantPoint {
    score: f32,
    payload: Option<QdrantPayload>,
}

#[derive(Deserialize)]
struct QdrantPayload {
    source: String,
    text: String,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f32,
}

impl RemoteRag {
    fn from_config(config: &Config, profile: impl Into<String>) -> Option<Self> {
        let qdrant = &config.platform.qdrant;
        let infinity = &config.platform.infinity;
        if !qdrant.endpoint.enabled || !infinity.endpoint.enabled {
            return None;
        }
        let qdrant_url = qdrant.endpoint.base_url.clone()?;
        let infinity_url = infinity.endpoint.base_url.clone()?;
        // reqwest is built with rustls-no-provider workspace-wide; install the
        // repository's chosen ring provider before constructing this client.
        lightagent_provider_lightweight::ensure_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        Some(Self {
            client,
            qdrant_url: qdrant_url.trim_end_matches('/').to_owned(),
            qdrant_key: qdrant
                .endpoint
                .api_key
                .as_ref()
                .and_then(|key| key.resolve()),
            infinity_url: infinity_url.trim_end_matches('/').to_owned(),
            infinity_key: infinity
                .endpoint
                .api_key
                .as_ref()
                .and_then(|key| key.resolve()),
            collection: qdrant.collection.clone(),
            embedding_model: infinity.embedding_model.clone(),
            rerank_model: infinity.rerank_model.clone(),
            profile: profile.into(),
        })
    }

    fn qdrant(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.qdrant_key {
            Some(key) => request.header("api-key", key),
            None => request,
        }
    }

    fn infinity(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.infinity_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    async fn embeddings(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let response = self
            .infinity(
                self.client
                    .post(format!("{}/embeddings", self.infinity_url)),
            )
            .json(&json!({ "model": self.embedding_model, "input": inputs }))
            .send()
            .await
            .map_err(|error| format!("Infinity embeddings request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Infinity embeddings returned {}",
                response.status()
            ));
        }
        #[derive(Deserialize)]
        struct Embeddings {
            data: Vec<Embedding>,
        }
        #[derive(Deserialize)]
        struct Embedding {
            embedding: Vec<f32>,
        }
        let parsed: Embeddings = response.json().await.map_err(|error| error.to_string())?;
        let vectors: Vec<Vec<f32>> = parsed.data.into_iter().map(|item| item.embedding).collect();
        if vectors.len() != inputs.len()
            || vectors
                .iter()
                .any(|vector| vector.is_empty() || vector.iter().any(|v| !v.is_finite()))
        {
            return Err("Infinity returned invalid embeddings".to_owned());
        }
        Ok(vectors)
    }

    /// Verify Qdrant is ready and create the named collection if it does not
    /// exist. A vector-size mismatch is intentionally an error, not a silent
    /// destructive collection recreation.
    async fn ensure_collection(&self, vector_size: usize) -> Result<(), String> {
        let ready = self
            .qdrant(self.client.get(format!("{}/readyz", self.qdrant_url)))
            .send()
            .await
            .map_err(|error| format!("Qdrant readiness request failed: {error}"))?;
        if !ready.status().is_success() {
            return Err(format!("Qdrant is not ready ({})", ready.status()));
        }
        let collection_url = format!("{}/collections/{}", self.qdrant_url, self.collection);
        let existing = self
            .qdrant(self.client.get(&collection_url))
            .send()
            .await
            .map_err(|error| format!("Qdrant collection check failed: {error}"))?;
        if existing.status().is_success() {
            return Ok(());
        }
        if existing.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "Qdrant collection check returned {}",
                existing.status()
            ));
        }
        let created = self
            .qdrant(self.client.put(&collection_url))
            .json(&json!({ "vectors": { "size": vector_size, "distance": "Cosine" } }))
            .send()
            .await
            .map_err(|error| format!("Qdrant collection creation failed: {error}"))?;
        if !created.status().is_success() {
            return Err(format!(
                "Qdrant collection creation returned {}",
                created.status()
            ));
        }
        Ok(())
    }

    async fn upsert(
        &self,
        source: &str,
        text: &str,
        max_chars: usize,
        overlap: usize,
    ) -> Result<(), String> {
        let chunks = chunk(text, max_chars, overlap);
        if chunks.is_empty() {
            return Ok(());
        }
        let vectors = self.embeddings(&chunks).await?;
        let size = vectors.first().map_or(0, Vec::len);
        if vectors.iter().any(|vector| vector.len() != size) {
            return Err("Infinity returned embeddings with inconsistent dimensions".to_owned());
        }
        self.ensure_collection(size).await?;
        // Mirror RagStore::add's replacement semantics. Point ids include text
        // for deterministic retries, so remove stale chunks when a source is
        // re-indexed with changed content.
        let deleted = self
            .qdrant(self.client.post(format!(
                "{}/collections/{}/points/delete?wait=true",
                self.qdrant_url, self.collection
            )))
            .json(&json!({ "filter": { "must": [
                { "key": "profile", "match": { "value": self.profile } },
                { "key": "source", "match": { "value": source } }
            ] } }))
            .send()
            .await
            .map_err(|error| format!("Qdrant source replacement failed: {error}"))?;
        if !deleted.status().is_success() {
            return Err(format!(
                "Qdrant source replacement returned {}",
                deleted.status()
            ));
        }
        let points: Vec<Value> = chunks.into_iter().zip(vectors).enumerate().map(|(chunk_index, (text, vector))| {
            let mut hasher = DefaultHasher::new();
            self.profile.hash(&mut hasher);
            source.hash(&mut hasher);
            chunk_index.hash(&mut hasher);
            text.hash(&mut hasher);
            json!({
                "id": hasher.finish(),
                "vector": vector,
                "payload": { "profile": self.profile, "source": source, "text": text, "chunk": chunk_index }
            })
        }).collect();
        let response = self
            .qdrant(self.client.put(format!(
                "{}/collections/{}/points?wait=true",
                self.qdrant_url, self.collection
            )))
            .json(&json!({ "points": points }))
            .send()
            .await
            .map_err(|error| format!("Qdrant upsert failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("Qdrant upsert returned {}", response.status()));
        }
        Ok(())
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<Hit>, String> {
        let vector = self
            .embeddings(&[query.to_owned()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "Infinity returned no query embedding".to_owned())?;
        // Request a modestly expanded candidate set, bounded before passing
        // documents to a cross encoder.
        let candidates = top_k.saturating_mul(4).clamp(top_k, 50);
        let response = self
            .qdrant(self.client.post(format!(
                "{}/collections/{}/points/query",
                self.qdrant_url, self.collection
            )))
            .json(&json!({
                "query": vector,
                "limit": candidates,
                "with_payload": true,
                "filter": { "must": [{ "key": "profile", "match": { "value": self.profile } }] }
            }))
            .send()
            .await
            .map_err(|error| format!("Qdrant query failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("Qdrant query returned {}", response.status()));
        }
        let parsed: QdrantResponse<QdrantQueryResult> =
            response.json().await.map_err(|error| error.to_string())?;
        let hits: Vec<Hit> = parsed
            .result
            .points
            .into_iter()
            .filter_map(|point| {
                (point.score.is_finite())
                    .then_some(point.payload)
                    .flatten()
                    .and_then(|payload| {
                        (!payload.source.is_empty() && !payload.text.is_empty()).then_some(Hit {
                            score: point.score,
                            source: payload.source,
                            text: payload.text,
                        })
                    })
            })
            .collect();
        if hits.is_empty() {
            return Ok(hits);
        }
        self.rerank(query, hits, top_k).await
    }

    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<Hit>,
        top_k: usize,
    ) -> Result<Vec<Hit>, String> {
        let documents: Vec<String> = candidates.iter().map(|hit| hit.text.clone()).collect();
        let response = self
            .infinity(self.client.post(format!("{}/rerank", self.infinity_url)))
            .json(&json!({ "model": self.rerank_model, "query": query, "documents": documents }))
            .send()
            .await
            .map_err(|error| format!("Infinity rerank failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("Infinity rerank returned {}", response.status()));
        }
        let parsed: RerankResponse = response.json().await.map_err(|error| error.to_string())?;
        let mut used = std::collections::HashSet::new();
        let mut reranked = Vec::with_capacity(parsed.results.len());
        for result in parsed.results {
            if result.index >= candidates.len()
                || !result.relevance_score.is_finite()
                || !used.insert(result.index)
            {
                return Err("Infinity rerank returned invalid result indices or scores".to_owned());
            }
            let mut hit = candidates[result.index].clone();
            hit.score = result.relevance_score;
            reranked.push(hit);
        }
        if reranked.is_empty() {
            return Err("Infinity rerank returned no results".to_owned());
        }
        reranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        reranked.truncate(top_k);
        Ok(reranked)
    }
}

/// Build the semantic embedder when hybrid retrieval is configured, else `None`.
pub(crate) fn semantic_embedder(config: &Config) -> Option<Arc<dyn SemanticEmbedder>> {
    if config.platform.infinity.endpoint.enabled {
        let infinity = &config.platform.infinity;
        let base_url = infinity.endpoint.base_url.clone()?;
        let api_key = infinity
            .endpoint
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve());
        let client = EmbeddingClient::infinity(base_url, api_key).ok()?;
        return Some(Arc::new(ProviderSemanticEmbedder {
            client,
            model: infinity.embedding_model.clone(),
        }));
    }
    let semantic = &config.rag.semantic;
    if !semantic.enabled {
        return None;
    }
    let base_url = semantic.base_url.clone()?;
    let model = semantic.model.clone()?;
    let api_key = semantic
        .api_key
        .as_ref()
        .and_then(|secret| secret.resolve());
    let client = EmbeddingClient::new(base_url, api_key).ok()?;
    Some(Arc::new(ProviderSemanticEmbedder { client, model }))
}

/// The `rag.search` tool for a run, or `None` when nothing is indexed.
pub(crate) fn rag_tool(profile_dir: &Path, config: &Config) -> Option<Arc<dyn Tool>> {
    let store = RagStore::open(index_path(profile_dir)).ok()?;
    if store.is_empty() {
        return None;
    }
    let semantic = semantic_embedder(config);
    // The local profile index remains authoritative. Remote RAG is an optional
    // acceleration/scale-out path and every error falls through to this store.
    // `profile_dir` is the validated `<profiles>/<profile-id>` directory. Use
    // its final component so interactive runs and the CLI share one remote
    // profile namespace (rather than leaking a machine-specific path).
    let profile = profile_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| profile_dir.display().to_string());
    if let Some(remote) = RemoteRag::from_config(config, profile) {
        return Some(Arc::new(RemoteRagSearch::new(
            Arc::new(store),
            semantic,
            remote,
            config.rag.top_k,
        )));
    }
    Some(Arc::new(RagSearch::new(
        Arc::new(store),
        semantic,
        config.rag.top_k,
    )))
}

struct RemoteRagSearch {
    definition: ToolDefinition,
    store: Arc<RagStore>,
    semantic: Option<Arc<dyn SemanticEmbedder>>,
    remote: RemoteRag,
    top_k: usize,
}

impl RemoteRagSearch {
    fn new(
        store: Arc<RagStore>,
        semantic: Option<Arc<dyn SemanticEmbedder>>,
        remote: RemoteRag,
        top_k: usize,
    ) -> Self {
        Self {
            definition: ToolDefinition::new(
                RagSearch::NAME,
                "Search the indexed documents and return the most relevant passages.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What to look for." },
                        "top_k": { "type": "integer", "minimum": 1, "description": "How many passages to return." }
                    },
                    "required": ["query"], "additionalProperties": false,
                }),
                RiskClass::Observe,
                vec![Scope::new("rag:search")],
            ),
            store,
            semantic,
            remote,
            top_k: top_k.max(1),
        }
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    top_k: Option<usize>,
}

fn format_hits(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "No relevant passages found.".to_owned();
    }
    let mut output = String::new();
    for (rank, hit) in hits.iter().enumerate() {
        output.push_str(&format!(
            "[{}] {} (score {:.3})\n{}\n\n",
            rank + 1,
            hit.source,
            hit.score,
            hit.text
        ));
    }
    output.trim_end().to_owned()
}

#[async_trait]
impl Tool for RemoteRagSearch {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    async fn call(&self, args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        let Ok(args) = serde_json::from_value::<SearchArgs>(args.clone()) else {
            return ToolOutcome::error("could not read rag.search arguments");
        };
        let k = args.top_k.unwrap_or(self.top_k).clamp(1, 50);
        let hits = match self.remote.search(&args.query, k).await {
            Ok(hits) if !hits.is_empty() => hits,
            // No remote result is treated like an unavailable remote corpus so
            // users never lose their durable profile-local answer path.
            _ => {
                self.store
                    .search(&args.query, &HashingEmbedder, self.semantic.as_deref(), k)
                    .await
            }
        };
        ToolOutcome::ok(format_hits(&hits))
    }
}

/// The one-call realtime web retriever, available with a configured search backend.
pub(crate) fn realtime_rag_tool(config: &Config) -> Option<Arc<dyn Tool>> {
    if !config.rag.realtime_enabled || !config.web.enabled || config.web.search.endpoint.is_none() {
        return None;
    }
    Some(Arc::new(RealtimeRag::new(
        semantic_embedder(config),
        config.rag.top_k,
        config.web.search.max_results,
        config.rag.max_chunk_chars,
        config.rag.chunk_overlap_chars,
    )))
}

/// Resolve the active profile's index path and the loaded config.
fn active_index() -> Result<(PathBuf, Config, String), String> {
    let paths = LightagentPaths::resolve().map_err(|error| error.to_string())?;
    let config = ConfigStore::at(&paths)
        .load()
        .map_err(|error| error.to_string())?;
    let store = ProfileStore::new(paths.root());
    let active = store
        .active()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no active profile — run `lightagent init` first".to_owned())?;
    let dir = store.handle(&active).dir().to_path_buf();
    Ok((index_path(&dir), config, active.to_string()))
}

/// `rag add <path>` — index a file, or every file in a directory.
pub async fn add(path: PathBuf, source: Option<String>, json: bool) -> Result<(), String> {
    let (index, config, profile) = active_index()?;
    let mut store = RagStore::open(&index).map_err(|error| error.to_string())?;
    let embedder = HashingEmbedder;
    let semantic = semantic_embedder(&config);

    let mut targets = Vec::new();
    if path.is_dir() {
        let entries = std::fs::read_dir(&path).map_err(|error| error.to_string())?;
        for entry in entries.flatten() {
            if entry.path().is_file() {
                targets.push(entry.path());
            }
        }
        targets.sort();
    } else {
        targets.push(path.clone());
    }

    let mut total = 0;
    let mut indexed = Vec::new();
    for target in targets {
        let text = match std::fs::read_to_string(&target) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("· skipping {}: {error}", target.display());
                continue;
            }
        };
        let name = source.clone().unwrap_or_else(|| {
            target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.display().to_string())
        });
        let added = store
            .add(
                &name,
                &text,
                &embedder,
                semantic.as_deref(),
                config.rag.max_chunk_chars,
                config.rag.chunk_overlap_chars,
            )
            .await
            .map_err(|error| error.to_string())?;
        // The local write succeeds independently. A remote outage must never
        // make an indexed document disappear from this profile's RAG store.
        if let Some(remote) = RemoteRag::from_config(&config, &profile) {
            if let Err(error) = remote
                .upsert(
                    &name,
                    &text,
                    config.rag.max_chunk_chars,
                    config.rag.chunk_overlap_chars,
                )
                .await
            {
                eprintln!("· remote RAG sync skipped for {name}: {error}");
            }
        }
        total += added;
        indexed.push((name, added));
    }

    if json {
        let value = serde_json::json!({
            "indexed": indexed.iter().map(|(n, c)| serde_json::json!({ "source": n, "chunks": c })).collect::<Vec<_>>(),
            "total_chunks": total,
        });
        println!("{value:#}");
    } else {
        for (name, added) in &indexed {
            println!("indexed {name} ({added} chunks)");
        }
        println!("{total} chunk(s) added.");
    }
    Ok(())
}

/// `rag search <query>` — the best passages for a query.
pub async fn search(query: String, top_k: Option<usize>, json: bool) -> Result<(), String> {
    let (index, config, profile) = active_index()?;
    let store = RagStore::open(&index).map_err(|error| error.to_string())?;
    let k = top_k.unwrap_or(config.rag.top_k).max(1);
    let semantic = semantic_embedder(&config);
    let hits = match RemoteRag::from_config(&config, profile) {
        Some(remote) => match remote.search(&query, k).await {
            Ok(hits) if !hits.is_empty() => hits,
            _ => {
                store
                    .search(&query, &HashingEmbedder, semantic.as_deref(), k)
                    .await
            }
        },
        None => {
            store
                .search(&query, &HashingEmbedder, semantic.as_deref(), k)
                .await
        }
    };

    if json {
        let value = serde_json::json!({
            "query": query,
            "hits": hits.iter().map(|hit| serde_json::json!({
                "source": hit.source, "score": hit.score, "text": hit.text,
            })).collect::<Vec<_>>(),
        });
        println!("{value:#}");
        return Ok(());
    }
    if hits.is_empty() {
        println!("No relevant passages found.");
        return Ok(());
    }
    for (rank, hit) in hits.iter().enumerate() {
        println!("[{}] {} (score {:.2})", rank + 1, hit.source, hit.score);
        println!("{}\n", hit.text);
    }
    Ok(())
}

/// `rag list` — the indexed sources and their chunk counts.
pub fn list(json: bool) -> Result<(), String> {
    let (index, _, _) = active_index()?;
    let store = RagStore::open(&index).map_err(|error| error.to_string())?;
    let sources = store.sources();
    if json {
        let value = serde_json::json!({
            "sources": sources.iter().map(|(n, c)| serde_json::json!({ "source": n, "chunks": c })).collect::<Vec<_>>(),
            "total_chunks": store.len(),
        });
        println!("{value:#}");
        return Ok(());
    }
    if sources.is_empty() {
        println!("Nothing indexed. Add documents with `lightagent rag add <path>`.");
        return Ok(());
    }
    for (name, count) in sources {
        println!("{name}  ({count} chunks)");
    }
    Ok(())
}

/// `rag clear` — empty the index.
pub fn clear(json: bool) -> Result<(), String> {
    let (index, _, _) = active_index()?;
    let mut store = RagStore::open(&index).map_err(|error| error.to_string())?;
    store.clear().map_err(|error| error.to_string())?;
    if json {
        println!("{}", serde_json::json!({ "cleared": true }));
    } else {
        println!("Index cleared.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_tools::ToolCtx;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    fn remote(qdrant_url: String, infinity_url: String) -> RemoteRag {
        lightagent_provider_lightweight::ensure_provider();
        RemoteRag {
            client: reqwest::Client::builder().build().unwrap(),
            qdrant_url,
            qdrant_key: Some("qdrant-secret".to_owned()),
            infinity_url,
            infinity_key: Some("infinity-secret".to_owned()),
            collection: "docs".to_owned(),
            embedding_model: "embed-test".to_owned(),
            rerank_model: "rerank-test".to_owned(),
            profile: "profile-a".to_owned(),
        }
    }

    async fn write_response(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
        stream
            .write_all(format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn remote_contract_covers_readiness_upsert_filtered_query_and_rerank() {
        let qdrant_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let qdrant_address = qdrant_listener.local_addr().unwrap();
        let qdrant_server = tokio::spawn(async move {
            for step in 0..6 {
                let (mut stream, _) = qdrant_listener.accept().await.unwrap();
                let mut bytes = vec![0_u8; 16_384];
                let read = stream.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..read]);
                assert!(request.contains("api-key: qdrant-secret"));
                match step {
                    0 => {
                        assert!(request.starts_with("GET /readyz "));
                        write_response(&mut stream, "200 OK", "{}").await;
                    }
                    1 => {
                        assert!(request.starts_with("GET /collections/docs "));
                        write_response(&mut stream, "404 Not Found", "{}").await;
                    }
                    2 => {
                        assert!(
                            request.starts_with("PUT /collections/docs "),
                            "expected collection creation, received: {request}"
                        );
                        assert!(request.contains("\"size\":2"));
                        write_response(&mut stream, "200 OK", "{}").await;
                    }
                    3 => {
                        assert!(
                            request.starts_with("POST /collections/docs/points/delete?wait=true ")
                        );
                        assert!(request.contains("\"source\""));
                        write_response(&mut stream, "200 OK", "{}").await;
                    }
                    4 => {
                        assert!(request.starts_with("PUT /collections/docs/points?wait=true "));
                        assert!(request.contains("\"profile\":\"profile-a\""));
                        write_response(&mut stream, "200 OK", "{}").await;
                    }
                    _ => {
                        assert!(request.starts_with("POST /collections/docs/points/query "));
                        assert!(request.contains("\"key\":\"profile\""));
                        assert!(request.contains("\"value\":\"profile-a\""));
                        write_response(&mut stream, "200 OK", r#"{"result":{"points":[{"score":0.4,"payload":{"source":"manual","text":"remote matching passage"}}]}}"#).await;
                    }
                }
            }
        });
        let infinity_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let infinity_address = infinity_listener.local_addr().unwrap();
        let infinity_server = tokio::spawn(async move {
            for step in 0..3 {
                let (mut stream, _) = infinity_listener.accept().await.unwrap();
                let mut bytes = vec![0_u8; 16_384];
                let read = stream.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..read]);
                assert!(request.contains("authorization: Bearer infinity-secret"));
                if step == 2 {
                    assert!(request.starts_with("POST /rerank "));
                    assert!(request.contains("\"documents\""));
                    write_response(
                        &mut stream,
                        "200 OK",
                        r#"{"results":[{"index":0,"relevance_score":0.93}]}"#,
                    )
                    .await;
                } else {
                    assert!(request.starts_with("POST /embeddings "));
                    write_response(
                        &mut stream,
                        "200 OK",
                        r#"{"data":[{"embedding":[0.1,0.2]}]}"#,
                    )
                    .await;
                }
            }
        });
        let remote = remote(
            format!("http://{qdrant_address}"),
            format!("http://{infinity_address}"),
        );
        remote
            .upsert("manual", "remote matching passage", 256, 0)
            .await
            .unwrap();
        let hits = remote.search("matching", 3).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, "manual");
        assert_eq!(hits[0].score, 0.93);
        qdrant_server.await.unwrap();
        infinity_server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_failure_falls_back_to_the_profile_store() {
        let path = std::env::temp_dir().join(format!("rag-fallback-{}.jsonl", std::process::id()));
        let mut store = RagStore::open(&path).unwrap();
        store
            .add(
                "local",
                "local profile answer",
                &HashingEmbedder,
                None,
                256,
                0,
            )
            .await
            .unwrap();
        let remote = remote(
            "http://127.0.0.1:1".to_owned(),
            "http://127.0.0.1:1".to_owned(),
        );
        let tool = RemoteRagSearch::new(Arc::new(store), None, remote, 3);
        let outcome = tool
            .call(
                &json!({"query":"local profile"}),
                &ToolCtx::new(CancellationToken::new()),
            )
            .await;
        assert!(!outcome.is_error);
        assert!(outcome.content.contains("local profile answer"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn malformed_rerank_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0_u8; 4096];
            let _ = stream.read(&mut bytes).await.unwrap();
            write_response(
                &mut stream,
                "200 OK",
                r#"{"results":[{"index":9,"relevance_score":0.9}]}"#,
            )
            .await;
        });
        let remote = remote("http://127.0.0.1:1".to_owned(), format!("http://{address}"));
        let error = remote
            .rerank(
                "q",
                vec![Hit {
                    score: 0.1,
                    source: "s".to_owned(),
                    text: "t".to_owned(),
                }],
                1,
            )
            .await
            .unwrap_err();
        assert!(error.contains("invalid result"));
        server.await.unwrap();
    }
}
