"""Trace one reasoning-then-answer run across the boundaries its final
answer crosses: Lightweight's gateway (its real gateway and wire format over
the mock backend), the stream Lightagent receives, the events Lightagent
emits, its saved transcript, and the Web UI's event contract.

  LIGHTWEIGHT_MOCK_GATEWAY=... LIGHTAGENT_BIN=... python3 stream-trace.py <out-dir>

A recording proxy sits between Lightagent and the gateway, so the provider
stream recorded is the exact bytes Lightagent read. Redacted: no request
header is recorded (the Authorization header included), and of each request
body only its shape (model, stream, max_tokens, reasoning_effort, message
roles), not the prompt. The scripted text holds nothing secret.

Writes trace.json and prints a Markdown summary. Exits 1 when the answer or
its completion is lost at a boundary, or when Lightagent emits an event the
UI does not listen for (frontend/src/hooks/useRunEvents.ts): a browser
EventSource drops a named event that has no listener, and the hook's
ordered-id check then reads the next event as a gap and reconnects.
"""

import http.client
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = pathlib.Path(__file__).resolve().parent.parent
LIGHTAGENT = pathlib.Path(os.environ.get("LIGHTAGENT_BIN", ""))
LIGHTWEIGHT = pathlib.Path(os.environ.get("LIGHTWEIGHT_MOCK_GATEWAY", ""))
REASONING = ["Considering the question. ", "Done considering."]
CONTENT = ["The traced ", "final answer."]
ANSWER = "".join(CONTENT)
TERMINAL = {"run.completed", "run.cancelled", "run.failed"}


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(url, method="GET", body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method, headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw)
        except ValueError:
            return error.code, raw.decode("utf-8", "replace")


def wait_for(url, predicate, label, timeout=30):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            status, body = request(url)
            last = (status, body)
            if predicate(status, body):
                return body
        except OSError as error:
            last = str(error)
        time.sleep(0.2)
    raise SystemExit(f"{label}: not reached within {timeout}s (last: {last!r})")


def sse_frames(raw):
    """Every frame of an SSE body: its id, event name and data, in order."""
    frames, frame = [], {"id": None, "event": "message", "data": []}
    for line in raw.splitlines():
        if line == "":
            if frame["data"] or frame["event"] != "message":
                frames.append({"id": frame["id"], "event": frame["event"], "data": "\n".join(frame["data"])})
            frame = {"id": frame["id"], "event": "message", "data": []}
        elif line.startswith(":"):
            continue
        else:
            field, _, value = line.partition(":")
            value = value[1:] if value.startswith(" ") else value
            if field == "data":
                frame["data"].append(value)
            elif field in ("event", "id"):
                frame[field] = value
    return frames


class Recorder:
    """A proxy to the gateway that records each exchange, redacted."""

    def __init__(self, upstream_port):
        self.exchanges = []
        self.lock = threading.Lock()
        recorder = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def forward(self):
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                headers = {k: v for k, v in self.headers.items()
                           if k.lower() not in ("host", "connection", "content-length", "accept-encoding")}
                upstream = http.client.HTTPConnection("127.0.0.1", upstream_port, timeout=60)
                upstream.request(self.command, self.path, body=body or None, headers=headers)
                response = upstream.getresponse()
                self.send_response(response.status)
                for name, value in response.getheaders():
                    if name.lower() not in ("connection", "content-length", "transfer-encoding", "keep-alive"):
                        self.send_header(name, value)
                self.send_header("Connection", "close")
                self.end_headers()
                received = bytearray()
                # Passed on as it arrives, so Lightagent still reads a stream.
                while True:
                    chunk = response.read1(65536) if hasattr(response, "read1") else response.read(65536)
                    if not chunk:
                        break
                    received.extend(chunk)
                    self.wfile.write(chunk)
                    self.wfile.flush()
                upstream.close()
                self.close_connection = True
                shape = None
                if body:
                    try:
                        sent = json.loads(body)
                        shape = {k: sent.get(k) for k in ("model", "stream", "max_tokens", "reasoning_effort")}
                        shape["roles"] = [m.get("role") for m in sent.get("messages") or []]
                        shape["tools_offered"] = len(sent.get("tools") or [])
                    except ValueError:
                        shape = {"unparsed_bytes": len(body)}
                with recorder.lock:
                    recorder.exchanges.append({"method": self.command, "path": self.path, "request": shape,
                                               "status": response.status,
                                               "body": received.decode("utf-8", "replace")})

            do_GET = forward
            do_POST = forward

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.root = f"http://127.0.0.1:{self.server.server_address[1]}"


def provider_view(exchanges):
    """What the chat-completion streams told Lightagent."""
    streams = []
    for exchange in exchanges:
        if not exchange["path"].startswith("/v1/chat/completions"):
            continue
        reasoning, content, finish, done, chunks = [], [], [], False, 0
        for frame in sse_frames(exchange["body"]):
            if frame["data"] == "[DONE]":
                done = True
                continue
            try:
                chunk = json.loads(frame["data"])
            except ValueError:
                continue
            chunks += 1
            for choice in chunk.get("choices") or []:
                delta = choice.get("delta") or {}
                if delta.get("reasoning_content") or delta.get("reasoning"):
                    reasoning.append(delta.get("reasoning_content") or delta.get("reasoning"))
                if delta.get("content"):
                    content.append(delta["content"])
                if choice.get("finish_reason"):
                    finish.append(choice["finish_reason"])
        streams.append({"request": exchange["request"], "status": exchange["status"], "chunks": chunks,
                        "reasoning": "".join(reasoning), "content": "".join(content),
                        "finish_reasons": finish, "terminated_with_done": done})
    return streams


def ui_listens_for():
    hook = (ROOT / "frontend" / "src" / "hooks" / "useRunEvents.ts").read_text(encoding="utf-8")
    listed = hook.split("const EVENT_NAMES = [", 1)[1].split("];", 1)[0]
    return set(re.findall(r'"([^"]+)"', listed))


def main():
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "stream-trace")
    out.mkdir(parents=True, exist_ok=True)
    if not LIGHTAGENT.is_file() or not LIGHTWEIGHT.is_file():
        raise SystemExit("missing built Lightagent or Lightweight test binary")
    work = pathlib.Path(tempfile.mkdtemp(prefix="lightagent-stream-trace-"))
    gateway = agent = None
    problems = []
    try:
        script = work / "scripts.json"
        script.write_text(json.dumps([{"kind": "reasoning", "reasoning": REASONING, "content": CONTENT}]))
        gateway = subprocess.Popen([str(LIGHTWEIGHT), "--port", "0", "--model", "trace-model", "--ctx", "8192",
                                    "--script-file", str(script)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        startup = json.loads(gateway.stdout.readline())
        recorder = Recorder(startup["port"])
        model_id = request(f"{recorder.root}/v1/models")[1]["data"][0]["id"]

        env = {**os.environ, "LIGHTAGENT_HOME": str(work / "agent-home")}
        subprocess.run([str(LIGHTAGENT), "init", "--base-url", recorder.root, "--model", model_id],
                       check=True, env=env, stdout=subprocess.DEVNULL)
        port = free_port()
        agent = subprocess.Popen([str(LIGHTAGENT), "serve", "--host", "127.0.0.1", "--port", str(port)],
                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        api = f"http://127.0.0.1:{port}/api/lightagent/v1"
        wait_for(f"http://127.0.0.1:{port}/health", lambda status, _: status == 200, "Lightagent")
        status, session = request(f"{api}/sessions", "POST", {})
        assert status == 201, (status, session)
        status, run = request(f"{api}/runs", "POST", {"message": "Trace the final answer.", "session_id": session["id"]})
        assert status == 202, (status, run)
        run_id = run["id"]
        finished = wait_for(f"{api}/runs/{run_id}", lambda _, body: isinstance(body, dict)
                            and body.get("status") in ("completed", "failed", "cancelled"), "the run's end")
        with urllib.request.urlopen(f"{api}/runs/{run_id}/events", timeout=30) as response:
            emitted = sse_frames(response.read().decode("utf-8"))
        saved = wait_for(f"{api}/sessions/{session['id']}",
                         lambda status, body: status == 200 and len(body.get("runs") or []) == 1, "the saved session")

        streams = provider_view(recorder.exchanges)
        names = [frame["event"] for frame in emitted]
        ids = [int(frame["id"]) for frame in emitted if (frame["id"] or "").isdigit()]
        data = []
        for frame in emitted:
            try:
                data.append(json.loads(frame["data"]))
            except ValueError:
                data.append({})
        lightagent = {
            "status": finished.get("status"), "stop_reason": finished.get("stop_reason"),
            "events": [{"id": frame["id"], "event": frame["event"],
                        "data_keys": sorted(d) if isinstance(d, dict) else None} for frame, d in zip(emitted, data)],
            "reasoning": "".join(d.get("reasoning") or "" for n, d in zip(names, data) if n == "model.delta"),
            "content": "".join(d.get("content") or "" for n, d in zip(names, data) if n == "model.delta"),
            "ids_contiguous": ids == list(range(len(ids))),
        }
        answers = [m.get("content") for m in saved.get("messages") or [] if m.get("role") == "assistant"]
        transcript = {"assistant_messages": answers, "runs": [{k: r.get(k) for k in ("run_id", "stop_reason")}
                                                               for r in saved.get("runs") or []]}

        heard = ui_listens_for()
        unheard = [{"id": frame["id"], "event": frame["event"]} for frame in emitted if frame["event"] not in heard]
        ui = {"listens_for": sorted(heard), "emitted_but_not_listened_for": unheard}
        if unheard:
            first = unheard[0]
            following = next((frame for frame in emitted
                              if (frame["id"] or "").isdigit() and first["id"] and int(frame["id"]) > int(first["id"])
                              and frame["event"] in heard), None)
            ui["consequence"] = (
                f"a browser EventSource drops `{first['event']}` (id {first['id']}); the hook then reads "
                + (f"`{following['event']}` (id {following['id']}) as a gap and reconnects after id "
                   f"{int(first['id']) - 1}, whose replay starts with the same dropped event, so "
                   f"{', '.join(sorted({f['event'] for f in emitted if f['event'] in TERMINAL}))} never reaches the UI"
                   if following else "nothing after it"))

        # What each boundary must hold.
        if len(streams) != 1:
            problems.append(f"provider: expected one chat-completion stream, saw {len(streams)}")
        for stream in streams[:1]:
            if stream["reasoning"] != "".join(REASONING):
                problems.append(f"provider: reasoning {stream['reasoning']!r}")
            if stream["content"] != ANSWER:
                problems.append(f"provider: content {stream['content']!r}")
            if "stop" not in stream["finish_reasons"] or not stream["terminated_with_done"]:
                problems.append(f"provider: finish {stream['finish_reasons']}, [DONE] {stream['terminated_with_done']}")
        if lightagent["reasoning"] != "".join(REASONING) or lightagent["content"] != ANSWER:
            problems.append(f"lightagent events: reasoning {lightagent['reasoning']!r}, content {lightagent['content']!r}")
        if not any(name in TERMINAL for name in names) or not lightagent["ids_contiguous"]:
            problems.append(f"lightagent events: terminal {[n for n in names if n in TERMINAL]}, ids contiguous {lightagent['ids_contiguous']}")
        if answers.count(ANSWER) != 1 or len(answers) != 1:
            problems.append(f"transcript: assistant messages {answers!r}")
        if unheard:
            problems.append(f"ui contract: {ui['consequence']}")

        trace = {"provider_streams": streams, "lightagent": lightagent, "transcript": transcript, "ui": ui,
                 "exchanges": [{k: e[k] for k in ("method", "path", "status", "request")} for e in recorder.exchanges],
                 "problems": problems}
        (out / "trace.json").write_text(json.dumps(trace, indent=1))
        lines = ["### Stream trace: Lightweight mock gateway → Lightagent → UI event contract", "",
                 "| Boundary | What it held |", "|---|---|"]
        for stream in streams:
            lines.append(f"| provider stream ({stream['chunks']} chunks) | reasoning `{stream['reasoning']}`; content "
                         f"`{stream['content']}`; finish {stream['finish_reasons']}; `[DONE]` {stream['terminated_with_done']} |")
        lines.append("| Lightagent events | " + " → ".join(f"{f['id']}:{f['event']}" for f in emitted) + " |")
        lines.append(f"| Lightagent content | reasoning `{lightagent['reasoning']}`; content `{lightagent['content']}`; "
                     f"run {lightagent['status']} ({lightagent['stop_reason']}) |")
        lines.append(f"| saved transcript | {answers!r} |")
        lines.append(f"| UI listens for | {', '.join(sorted(heard))} |")
        lines.append("| emitted, not listened for | " + (", ".join(f"{u['id']}:{u['event']}" for u in unheard) or "none") + " |")
        if ui.get("consequence"):
            lines.append(f"| consequence in the UI | {ui['consequence']} |")
        lines += [f"- FAILED {problem}" for problem in problems] or ["- every boundary holds the answer and its completion"]
        print("\n".join(lines))
        return 1 if problems else 0
    finally:
        for process, name in ((agent, "lightagent"), (gateway, "lightweight")):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
