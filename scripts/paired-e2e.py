#!/usr/bin/env python3
"""Black-box Lightagent <-> Lightweight contract test.

Build products separately, then communicate only through Lightweight's public
/health and OpenAI-compatible /v1 endpoints.  The test-only mock gateway gets
its deterministic replies at process startup; this driver never calls a
Lightweight control endpoint.
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIGHTAGENT = Path(os.environ.get("LIGHTAGENT_BIN", ROOT / "target/debug/lightagent"))
LIGHTWEIGHT = Path(os.environ["LIGHTWEIGHT_MOCK_GATEWAY"])


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(url: str, method: str = "GET", body: object | None = None) -> tuple[int, object]:
    payload = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=payload, method=method)
    if payload is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            raw = response.read().decode()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        return error.code, json.loads(raw) if raw else None


def wait_for(url: str, predicate, label: str, timeout: float = 20) -> object:
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            status, body = request(url)
            last = (status, body)
            if predicate(status, body):
                return body
        except OSError as error:
            last = error
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {label}: {last}")


def events(url: str) -> list[tuple[str, object]]:
    out: list[tuple[str, object]] = []
    with urllib.request.urlopen(url, timeout=15) as response:
        event = None
        for raw in response:
            line = raw.decode().rstrip("\r\n")
            if line.startswith("event: "):
                event = line[7:]
            elif line.startswith("data: ") and event:
                out.append((event, json.loads(line[6:])))
                event = None
    return out


def main() -> None:
    if not LIGHTAGENT.is_file() or not LIGHTWEIGHT.is_file():
        raise SystemExit("missing built Lightagent or Lightweight test binary")
    work = Path(tempfile.mkdtemp(prefix="lightagent-paired-e2e-"))
    gateway = agent = None
    try:
        script = work / "scripts.json"
        script.write_text(json.dumps([
            {"kind": "tool_call", "id": "call_write_1", "name": "fs.write",
             "argument_fragments": ["{\"path\":\"proof.txt\",", "\"content\":\"approved\"}"]},
            {"kind": "content", "fragments": ["approved completion"]},
        ]))
        gateway = subprocess.Popen(
            [str(LIGHTWEIGHT), "--port", "0", "--model", "paired-model",
             "--script-file", str(script)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        startup = json.loads(gateway.stdout.readline())
        gateway_root = f"http://127.0.0.1:{startup['port']}"
        assert request(f"{gateway_root}/health")[0] == 200
        models = request(f"{gateway_root}/v1/models")[1]
        model_id = models["data"][0]["id"]
        assert model_id.startswith("paired-model")

        env = {**os.environ, "LIGHTAGENT_HOME": str(work / "agent-home")}
        subprocess.run([str(LIGHTAGENT), "init", "--base-url", gateway_root, "--model", model_id],
                       check=True, env=env, stdout=subprocess.DEVNULL)
        port = free_port()
        agent = subprocess.Popen([str(LIGHTAGENT), "serve", "--host", "127.0.0.1", "--port", str(port)],
                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        api = f"http://127.0.0.1:{port}/api/lightagent/v1"
        wait_for(f"http://127.0.0.1:{port}/health", lambda status, _: status == 200, "Lightagent")
        status, settings = request(f"{api}/settings")
        assert status == 200
        settings.update({"filesystem_tools_enabled": True, "terminal_enabled": False,
                         "approval_policy": "strict"})
        assert request(f"{api}/settings", "PUT", settings)[0] == 200
        status, session = request(f"{api}/sessions", "POST", {})
        assert status == 201
        status, run = request(f"{api}/runs", "POST", {"message": "write proof.txt", "session_id": session["id"]})
        assert status == 202
        run_id = run["id"]
        paused = wait_for(f"{api}/runs/{run_id}",
                          lambda _, body: body["status"] == "awaiting_approval", "approval pause")
        pending = paused["pending_approval"]
        assert pending["approval_id"] and pending["tool_call_id"] == "call_write_1" and pending["tool"] == "fs.write"
        status, response = request(f"{api}/approvals/{run_id}", "POST", {"approve": True})
        assert status == 200 and response["delivered"] is True
        wait_for(f"{api}/runs/{run_id}", lambda _, body: body["status"] == "completed", "completion")
        stream = events(f"{api}/runs/{run_id}/events")
        names = [name for name, _ in stream]
        for required in ("tool.requested", "approval.required", "tool.started", "tool.output", "model.delta", "run.completed"):
            assert required in names, (required, names)
        approval = next(data for name, data in stream if name == "approval.required")
        assert approval["approval_id"] == pending["approval_id"] and approval["tool_call_id"] == "call_write_1"
        assert any(name == "model.delta" and "approved completion" in data.get("content", "") for name, data in stream)
        saved = wait_for(f"{api}/sessions/{session['id']}",
                         lambda status, body: status == 200 and len(body["runs"]) == 1, "saved session")
        assert any(message["role"] == "assistant" and "approved completion" in message["content"] for message in saved["messages"])
        assert (work / "agent-home/profiles/default/workspace/proof.txt").read_text() == "approved"
        print("paired Lightagent/Lightweight E2E passed")
    finally:
        for process, name in ((agent, "lightagent"), (gateway, "lightweight")):
            if process:
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill()
                if process.returncode not in (None, 0, -15):
                    print(f"== {name} stderr ==\n{process.stderr.read()}", file=sys.stderr)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
