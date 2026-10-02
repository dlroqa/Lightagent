#!/usr/bin/env python3
"""Live contract check for the pinned Qdrant and Infinity test services.

This is intentionally dependency-free so it runs on GitHub-hosted runners and
developer machines with only Python 3 and the services from the compose file.
It validates the HTTP contracts the Rust adapters rely on, rather than mocking
either upstream server.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request


QDRANT_URL = os.environ.get("QDRANT_URL", "http://127.0.0.1:6333").rstrip("/")
INFINITY_URL = os.environ.get("INFINITY_URL", "http://127.0.0.1:7997").rstrip("/")
EMBEDDING_MODEL = os.environ.get("INFINITY_EMBEDDING_MODEL", "BAAI/bge-small-en-v1.5")
RERANK_MODEL = os.environ.get(
    "INFINITY_RERANK_MODEL", "mixedbread-ai/mxbai-rerank-xsmall-v1"
)
COLLECTION = os.environ.get("QDRANT_COLLECTION", "lightagent-platform-contract")


def request(method, url, body=None):
    headers = {"Accept": "application/json"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read().decode("utf-8")
            return response.status, json.loads(raw) if raw else {}
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8", errors="replace")
        raise AssertionError(f"{method} {url} returned {error.code}: {raw}") from error


def wait_ready(url, label):
    deadline = time.monotonic() + 90
    last_error = None
    while time.monotonic() < deadline:
        try:
            status, _ = request("GET", url)
            if status == 200:
                return
        except (AssertionError, OSError, urllib.error.URLError) as error:
            last_error = error
        time.sleep(2)
    raise AssertionError(f"{label} did not become ready at {url}: {last_error}")


def main():
    # Qdrant documents /readyz as its readiness probe; Infinity exposes /health.
    wait_ready(f"{QDRANT_URL}/readyz", "Qdrant")
    wait_ready(f"{INFINITY_URL}/health", "Infinity")

    _, embedded = request(
        "POST",
        f"{INFINITY_URL}/embeddings",
        {"model": EMBEDDING_MODEL, "input": ["Lightagent platform integration contract"]},
    )
    vector = embedded.get("data", [{}])[0].get("embedding")
    assert isinstance(vector, list) and vector and all(
        isinstance(value, (int, float)) for value in vector
    ), "Infinity /embeddings did not return a non-empty float vector"

    status, created = request(
        "PUT",
        f"{QDRANT_URL}/collections/{COLLECTION}",
        {"vectors": {"size": len(vector), "distance": "Cosine"}},
    )
    assert status == 200 and created.get("status") == "ok", "Qdrant collection creation failed"

    status, upserted = request(
        "PUT",
        f"{QDRANT_URL}/collections/{COLLECTION}/points?wait=true",
        {
            "points": [
                {
                    "id": 1,
                    "vector": vector,
                    "payload": {
                        "profile": "platform-contract",
                        "source": "contract-test",
                        "text": "Lightagent routes remote RAG through Qdrant and Infinity.",
                    },
                }
            ]
        },
    )
    assert status == 200 and upserted.get("status") == "ok", "Qdrant point upsert failed"

    status, queried = request(
        "POST",
        f"{QDRANT_URL}/collections/{COLLECTION}/points/query",
        {
            "query": vector,
            "filter": {
                "must": [{"key": "profile", "match": {"value": "platform-contract"}}]
            },
            "limit": 3,
            "with_payload": True,
        },
    )
    points = queried.get("result", {}).get("points", [])
    assert status == 200 and any(point.get("id") == 1 for point in points), (
        "Qdrant filtered /points/query did not return the upserted point"
    )

    _, reranked = request(
        "POST",
        f"{INFINITY_URL}/rerank",
        {
            "model": RERANK_MODEL,
            "query": "Which document describes Lightagent remote RAG?",
            "documents": [
                "The sky is blue.",
                "Lightagent routes remote RAG through Qdrant and Infinity.",
            ],
        },
    )
    results = reranked.get("results", [])
    assert results and all(
        isinstance(item.get("index"), int)
        and isinstance(item.get("relevance_score"), (int, float))
        for item in results
    ), "Infinity /rerank did not return indexed numeric scores"
    assert results[0]["index"] == 1, "Infinity /rerank did not rank the relevant document first"

    print(
        f"platform contracts passed (dimension={len(vector)}, collection={COLLECTION})",
        flush=True,
    )


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, OSError, urllib.error.URLError, ValueError, KeyError) as error:
        print(f"platform contract test failed: {error}", file=sys.stderr)
        sys.exit(1)
