"""Shared pieces of the desktop host's end-to-end scenarios (runs.py, switch.py).

FakeProvider is a deterministic OpenAI-compatible provider on loopback. It
streams scripted turns, holds a turn at a named gate until the harness opens
it, and records every request, so a scenario synchronizes on what reached
the provider rather than on elapsed time.

Host drives a test build of the desktop host (`--features e2e-hooks`)
through its stdin driver (desktop/src-tauri/src/e2e_driver.rs). Each command
calls the function the UI's IPC command calls, so this is evidence about the
host and the real runtime behind it, not about the WebView.
"""

import json
import os
import re
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The longest one step waits. A hang fails that step; it never stalls the job.
WAIT = 120
API = "/api/lightagent/v1"


def normal(name):
    """A tool name without separators, so `fs.write` matches `fs_write`."""
    return re.sub(r"[^a-z0-9]", "", name.lower())


def content_of(message):
    content = message.get("content")
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


class FakeProvider:
    """GET /v1/models, and POST /v1/chat/completions streamed from a script.

    The scenario is the `[e2e:<name>]` marker in the conversation's latest
    user message; the turn is how many assistant messages follow it.
    `scripts[name](provider, turn, index)` returns the steps: ("text", s),
    ("refusal", s), ("tool", internal_name, arguments), ("finish", reason),
    ("gate", name) to wait until the harness opens that gate, or a first
    ("status", code) to fail the request with that HTTP status.
    """

    def __init__(self):
        self.log = []
        self.condition = threading.Condition()
        self.gates = {}
        self.modes = {}
        self.scripts = {}
        provider = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path.startswith("/v1/models"):
                    provider.reply(self, 200, {"object": "list", "data": [{"id": "fake-model"}]})
                else:
                    provider.reply(self, 404, {"error": "not found"})

            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                try:
                    request = json.loads(self.rfile.read(length) or b"{}")
                except ValueError:
                    request = {}
                provider.serve(self, request)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"

    def record(self, **entry):
        with self.condition:
            entry["t"] = round(time.time(), 3)
            self.log.append(entry)
            self.condition.notify_all()

    def requests(self, scenario):
        with self.condition:
            return [e for e in self.log if e["event"] == "request" and e["scenario"] == scenario]

    def wait_for(self, event, scenario, timeout=WAIT, **match):
        """Wait until the log holds `event` for `scenario` (matching `match`)."""
        deadline = time.time() + timeout

        def found():
            return any(
                e["event"] == event and e["scenario"] == scenario and all(e.get(k) == v for k, v in match.items())
                for e in self.log
            )

        with self.condition:
            while not found():
                left = deadline - time.time()
                if left <= 0:
                    return False
                self.condition.wait(left)
            return True

    def gate(self, name):
        with self.condition:
            return self.gates.setdefault(name, threading.Event())

    def open(self, name):
        self.gate(name).set()

    def close(self):
        with self.condition:
            gates = list(self.gates.values())
        for gate in gates:
            gate.set()
        self.server.shutdown()

    def reply(self, handler, code, value):
        body = json.dumps(value).encode()
        handler.send_response(code)
        handler.send_header("Content-Type", "application/json")
        handler.send_header("Content-Length", str(len(body)))
        handler.end_headers()
        handler.wfile.write(body)

    def chunk(self, handler, delta, finish=None):
        choice = {"index": 0, "delta": delta}
        if finish:
            choice["finish_reason"] = finish
        handler.wfile.write(f"data: {json.dumps({'choices': [choice]})}\n\n".encode())
        handler.wfile.flush()

    def serve(self, handler, request):
        messages = request.get("messages") or []
        users = [i for i, m in enumerate(messages) if m.get("role") == "user"]
        last = users[-1] if users else -1
        found = re.search(r"\[e2e:([a-z0-9-]+)\]", content_of(messages[last]) if users else "")
        scenario = found.group(1) if found else "none"
        turn = sum(1 for m in messages[last + 1:] if m.get("role") == "assistant")
        tools = {}
        for tool in request.get("tools") or []:
            name = (tool.get("function") or {}).get("name")
            if name:
                tools[normal(name)] = name
        index = len(self.requests(scenario))
        self.record(
            event="request", scenario=scenario, turn=turn, index=index, tools=sorted(tools.values()),
            roles=[m.get("role") for m in messages], offered=len(request.get("tools") or []),
        )
        script = self.scripts.get(scenario)
        steps = script(self, turn, index) if script else [("text", "ok"), ("finish", "stop")]
        if steps and steps[0][0] == "status":
            self.reply(handler, steps[0][1], {"error": {"message": "the fake provider failed on purpose"}})
            self.record(event="failed", scenario=scenario, turn=turn)
            return
        handler.send_response(200)
        handler.send_header("Content-Type", "text/event-stream")
        handler.send_header("Connection", "close")
        handler.end_headers()
        handler.close_connection = True
        calls = 0
        try:
            self.chunk(handler, {"role": "assistant"})
            for step in steps:
                kind = step[0]
                if kind == "text":
                    self.chunk(handler, {"content": step[1]})
                elif kind == "refusal":
                    self.chunk(handler, {"refusal": step[1]})
                elif kind == "tool":
                    call = {
                        "index": calls,
                        "id": f"call_{scenario}_{index}_{calls}",
                        "type": "function",
                        "function": {"name": tools.get(normal(step[1]), step[1]), "arguments": json.dumps(step[2])},
                    }
                    self.chunk(handler, {"tool_calls": [call]})
                    calls += 1
                elif kind == "gate":
                    self.record(event="gate", scenario=scenario, gate=step[1])
                    if not self.gate(step[1]).wait(WAIT):
                        self.record(event="gate-timeout", scenario=scenario, gate=step[1])
                        return
                elif kind == "sleep":
                    # Only for the time budget, which is measured in time.
                    time.sleep(step[1])
                elif kind == "finish":
                    self.chunk(handler, {}, finish=step[1])
            handler.wfile.write(b"data: [DONE]\n\n")
            handler.wfile.flush()
            self.record(event="finished", scenario=scenario, turn=turn)
        except OSError:
            self.record(event="disconnected", scenario=scenario, turn=turn)


class HostError(Exception):
    pass


class Host:
    """A host test build, driven over its stdin (e2e_driver.rs)."""

    def __init__(self, command, env, log_path):
        self.log = open(log_path, "w", encoding="utf-8")
        self.logging = threading.Lock()
        self.process = subprocess.Popen(
            command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1
        )
        self.answers = {}
        self.events = []
        self.condition = threading.Condition()
        self.sending = threading.Lock()
        self.next_id = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _note(self, direction, text):
        with self.logging:
            if not self.log.closed:
                self.log.write(f"{time.time():.3f} {direction} {text}\n")
                self.log.flush()

    def _read(self):
        for line in self.process.stdout:
            if not line.startswith("E2E_DRIVER "):
                continue
            text = line[len("E2E_DRIVER "):].strip()
            self._note("<", text)
            try:
                message = json.loads(text)
            except ValueError:
                continue
            with self.condition:
                if message.get("id") is not None:
                    self.answers[message["id"]] = message
                else:
                    self.events.append(message)
                self.condition.notify_all()
        with self.condition:
            self.events.append({"event": "exited"})
            self.condition.notify_all()

    def send(self, op, **args):
        with self.sending:
            self.next_id += 1
            ident = self.next_id
            line = json.dumps({"id": ident, "op": op, **args})
            self._note(">", line)
            self.process.stdin.write(line + "\n")
            self.process.stdin.flush()
        return ident

    def answer(self, ident, timeout=WAIT):
        deadline = time.time() + timeout
        with self.condition:
            while ident not in self.answers:
                left = deadline - time.time()
                if left <= 0 or self.process.poll() is not None:
                    raise HostError(f"no answer to command {ident}")
                self.condition.wait(min(left, 1.0))
            message = self.answers.pop(ident)
        if not message.get("ok"):
            raise HostError(message.get("error") or "the command failed")
        return message.get("result")

    def call(self, op, timeout=WAIT, **args):
        return self.answer(self.send(op, **args), timeout)

    def wait_event(self, name, timeout=WAIT):
        """Wait for, and take, the oldest unsolicited driver event `name`."""
        deadline = time.time() + timeout
        with self.condition:
            while True:
                for position, event in enumerate(self.events):
                    if event.get("event") == name:
                        return self.events.pop(position)
                left = deadline - time.time()
                if left <= 0 or self.process.poll() is not None:
                    raise HostError(f"no {name} event")
                self.condition.wait(min(left, 1.0))

    def ready(self, timeout=WAIT):
        self.wait_event("driver_ready", timeout)
        return self.wait_ready(timeout)

    def wait_ready(self, timeout=WAIT):
        """Wait until the host serves a ready runtime (after a start or a restart)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            status = self.call("status")
            if status["state"] == "ready":
                return status
            if status["state"] in ("failed", "mismatch", "duplicate"):
                raise HostError(f"the runtime is {status['state']}: {status.get('detail')}")
            time.sleep(0.2)
        raise HostError("the runtime did not become ready")

    def fetch(self, method, target, body=None):
        result = self.call("fetch", method=method, target=target, body=None if body is None else json.dumps(body))
        try:
            value = json.loads(result["body"]) if result["body"] else None
        except ValueError:
            value = result["body"]
        return result["status"], value

    def try_fetch(self, method, target, body=None):
        """A fetch, or ("refused", message) when the host itself refuses it."""
        try:
            return self.fetch(method, target, body)
        except HostError as error:
            return "refused", str(error)

    def new_session(self):
        status, body = self.fetch("POST", f"{API}/sessions", {})
        if status != 201:
            raise HostError(f"sessions.create: {status} {body}")
        return body["id"]

    def start_run(self, session, message):
        status, body = self.fetch("POST", f"{API}/runs", {"message": message, "session_id": session})
        if status != 202:
            raise HostError(f"runs.create: {status} {body}")
        return body["id"]

    def session(self, session):
        status, body = self.fetch("GET", f"{API}/sessions/{session}")
        if status != 200:
            raise HostError(f"sessions.get: {status} {body}")
        return body

    def workspace(self):
        status, body = self.fetch("GET", f"{API}/workspace")
        if status != 200:
            raise HostError(f"workspace.get: {status} {body}")
        return body["workspace"]["effective"]

    def stop(self):
        try:
            self.process.stdin.close()
        except OSError:
            pass
        self.process.terminate()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        with self.logging:
            self.log.close()


class Stream:
    """A run's events as the host relays them (subscribe, then poll)."""

    def __init__(self, host, run):
        self.host = host
        self.run = run
        self.subscription = host.call("subscribe", target=f"{API}/runs/{run}/events")
        self.events = []
        self.closed = False
        self.error = None

    def poll(self, wait_ms=2000):
        batch = self.host.call("poll", subscription=self.subscription, wait_ms=wait_ms)
        self.events.extend(batch["events"])
        if batch["closed"]:
            self.closed, self.error = True, batch["error"]

    def until(self, predicate, timeout=WAIT):
        deadline = time.time() + timeout
        while not predicate(self) and not self.closed and time.time() < deadline:
            self.poll()
        return predicate(self)

    def finish(self, timeout=WAIT):
        self.until(lambda stream: stream.closed, timeout)
        return self

    def drop(self):
        self.host.call("drop_relay", subscription=self.subscription)

    def names(self):
        return [event["event"] for event in self.events]

    def has(self, name):
        return name in self.names()

    def ids(self):
        return [int(event["id"]) for event in self.events if event.get("id") not in (None, "")]

    def data(self, name):
        found = []
        for event in self.events:
            if event["event"] == name:
                try:
                    found.append(json.loads(event["data"]))
                except ValueError:
                    found.append(event["data"])
        return found

    def text(self):
        return "".join(item.get("content") or "" for item in self.data("model.delta") if isinstance(item, dict))


class Checks:
    """Named expectations for one scenario; a failure says what was seen."""

    def __init__(self, scenario):
        self.scenario = scenario
        self.results = {}
        self.failures = []

    def expect(self, name, ok, seen=None):
        self.results[name] = {"ok": bool(ok), "seen": seen}
        if not ok:
            self.failures.append(f"{self.scenario}: {name} (seen: {seen!r})")


def run_cli(cli, *args):
    out = subprocess.run([cli, *args], capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        raise RuntimeError(f"lightagent {' '.join(args)}: {out.stderr.strip()[:300]}")
    return out.stdout


def prepare_home(cli, home, provider):
    """Point the home at the fake provider and enable the tools. The approval
    policy stays the default (balanced): a file write asks first."""
    run_cli(cli, "config", "set", "inference.base_url", provider.url)
    path = os.path.join(home, "config.json")
    with open(path, encoding="utf-8") as handle:
        config = json.load(handle)
    config.setdefault("tools", {})["enabled"] = True
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(config, handle, indent=2)


def session_file(home, session):
    """A session as saved on disk, whichever profile's store holds it."""
    for root, _dirs, files in os.walk(home):
        if f"{session}.json" in files:
            with open(os.path.join(root, f"{session}.json"), encoding="utf-8") as handle:
                return json.load(handle)
    return None


def answers(session):
    return [m.get("content") for m in (session or {}).get("messages", []) if m.get("role") == "assistant"]


def read(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read()
    except FileNotFoundError:
        return None


def wait_until(probe, timeout=WAIT, every=0.2):
    """Re-read observable state until `probe()` is truthy; returns its last value."""
    deadline = time.time() + timeout
    while True:
        value = probe()
        if value or time.time() >= deadline:
            return value
        time.sleep(every)


def runtimes(pid):
    """The runtimes running under the host process `pid`."""
    import psutil

    try:
        return [p for p in psutil.Process(pid).children(recursive=True) if "sidecar" in p.name() and p.is_running()]
    except psutil.Error:
        return []
