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

## Advisory routing and execution settings

Settings exposes Jev's permitted models, permitted inference profiles, confidence
threshold, and routing timeout. An empty pair of allowlists disables advisory
routing. An explicit caller-selected model bypasses Jev. Selecting a session
profile retains that profile's execution identity while permitting advisory
inference routing. A permitted
profile supplies only inference model routing; the initiating profile retains
its persona, knowledge sources, tools, permissions, workspace and approvals.
Profiles using a different provider URL fall back to the original route so a
provider credential cannot be forwarded to another endpoint.
The adapter follows [TypeSafe's native API contract](https://docs.typesafe.ai/api):
`state` plus a typed routing choice question, with allowlisted options and a
default route, followed by validation of the corresponding choice answer.

Qdrant and Infinity each expose a request timeout (default 5 seconds, maximum
300). Remote failures retain the profile-local RAG fallback.

Open Terminal exposes request timeout (30 seconds), execution timeout (60
seconds), polling interval (250 milliseconds), and output cap (32768 bytes).
The run's overall time budget still applies. Commands remain executable tools
subject to the harness approval policy. The adapter uses authenticated,
session-scoped `POST /execute?wait=0`, polls `GET /execute/{id}/status`, and
kills cancelled or timed-out processes through `DELETE /execute/{id}?force=true`.
Its contract tests follow Open Terminal 0.14.0's `running`, `done` and `killed`
statuses. The session header scopes processes and working-directory tracking;
deploy the service in a separate container or machine for filesystem isolation.

The `Open Terminal live contract` GitHub Actions job provisions the pinned
Python service and tests command execution and cancellation with the actual
Lightagent adapter. Run the same ignored test locally against a disposable
service with `LIGHTAGENT_OPEN_TERMINAL_URL` and
`LIGHTAGENT_OPEN_TERMINAL_TOKEN` set:

```sh
cargo test -p lightagent-tools live_open_terminal -- --ignored --nocapture
```

Secrets remain configuration references resolved only in memory. Settings
returns only whether a secret is configured, and preserves references on save.
