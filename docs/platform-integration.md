# Qdrant and Infinity integration environment

Lightagent's remote-RAG path treats Qdrant and Infinity as optional external
dependencies. The harness remains the policy and approval boundary. If either
dependency is disabled or unavailable, the application must fall back to its
profile-local RAG store.

The pinned local stack starts Qdrant `v1.13.6` and Infinity `v0.0.77-cpu` with
the embedding model `BAAI/bge-small-en-v1.5` and reranker
`mixedbread-ai/mxbai-rerank-xsmall-v1`:

```sh
bash scripts/test-platform-integration.sh
```

The first startup downloads public Hugging Face model artifacts and can take a
few minutes. It exposes services only on loopback:

| Service | URL | Readiness endpoint |
| --- | --- | --- |
| Qdrant | `http://127.0.0.1:6333` | `/readyz` |
| Infinity | `http://127.0.0.1:7997` | `/health` |

The test creates an isolated `lightagent-platform-contract` collection, obtains
an Infinity vector, creates the collection at the returned dimension, upserts a
profile-tagged point, performs a filtered Qdrant `/points/query`, and verifies
Infinity's `/rerank` result shape and ranking.

Set `LIGHTAGENT_RUN_RUST_PLATFORM_TESTS=1` to run the ignored Rust live adapter
test against the same services. It receives these environment variables:

```sh
QDRANT_URL=http://127.0.0.1:6333
INFINITY_URL=http://127.0.0.1:7997
INFINITY_EMBEDDING_MODEL=BAAI/bge-small-en-v1.5
INFINITY_RERANK_MODEL=mixedbread-ai/mxbai-rerank-xsmall-v1
```

The Rust test uses `LIGHTAGENT_QDRANT_URL` and `LIGHTAGENT_INFINITY_URL`; the
wrapper maps the shorter variables above to those names automatically.

Docker is intentionally not a normal Lightagent prerequisite. If Docker Engine
or Compose v2 is unavailable, `scripts/test-platform-integration.sh` exits with
status 2 and instructions; ordinary unit tests continue to work without it.
Use separately managed services with the same API contracts when containers are
not appropriate for a deployment.
