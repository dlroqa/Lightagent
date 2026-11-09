"""Trace research runs that call `web.search` and `rag.realtime` through the
real Lightagent harness, with a model whose context window is known.

  LIGHTWEIGHT_MOCK_GATEWAY=... LIGHTAGENT_BIN=... python3 tool-trace.py <out-dir>

Each scenario starts its own Lightweight mock gateway (`--ctx 8192`, so every
turn is budgeted and Lightagent sends `generation.report`), its own Lightagent
home and `lightagent serve`, and a loopback fixture that plays the search
provider and the pages it links to. The fixture is reached through
`web.trusted_networks` (127.0.0.0/8, operator runs only); the SSRF guard stays
on. Scenarios: retrieval with evidence, a search with zero results, pages
unrelated to the query, `web.search` with and without results, and a
provider that refuses with HTTP 429.

What is checked is the content and the order, not labels: the events
Lightagent emits (ids contiguous, `generation.report` before the tool events,
every name one the UI listens for), each call requested, started and finished
exactly once, exactly one search sent to the provider per call, the tool's
result text and progress outcome, and the saved transcript's tool entry
against the live event. Writes trace.json and prints a Markdown summary;
exits 1 on any failed check. Nothing here leaves the runner: the provider and
the pages are loopback fixtures.
"""

import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = pathlib.Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("stream_trace", HERE / "stream-trace.py")
trace = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(trace)

LIGHTAGENT = pathlib.Path(os.environ.get("LIGHTAGENT_BIN", ""))
LIGHTWEIGHT = pathlib.Path(os.environ.get("LIGHTWEIGHT_MOCK_GATEWAY", ""))
QUERY = "When did Zephyr ship"
FACT = "Zephyr shipped on September 10, 2026."
TERMINAL = {"run.completed", "run.cancelled", "run.failed"}
TOOL_EVENTS = ("tool.requested", "tool.started", "tool.progress", "tool.output", "tool.failed")

# Pages the fixture serves. The unrelated ones share no word with QUERY.
PAGES = {
    "zephyr-notes": ("Zephyr release notes", f"<h1>Zephyr release notes</h1><p>{FACT} The release followed a long beta.</p>"),
    "zephyr-blog": ("Zephyr launch post", f"<h1>Zephyr launch post</h1><p>{FACT} Thanks to every tester.</p>"),
    "garden-a": ("Tomato care", "<h1>Tomato care</h1><p>Water tomatoes deeply twice weekly and mulch the beds.</p>"),
    "garden-b": ("Compost basics", "<h1>Compost basics</h1><p>Mix green clippings with brown leaves and turn the heap monthly.</p>"),
}
SNIPPETS = {
    "zephyr-notes": FACT,
    "zephyr-blog": "The launch post for the new release.",
    "garden-a": "Watering and mulching advice for tomatoes.",
    "garden-b": "Green clippings, brown leaves, a monthly turn.",
}


class Fixture:
    """The search provider and its pages, on loopback, with a request log."""

    def __init__(self):
        self.mode = "evidence"
        self.log = []
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def send(self, status, content_type, body):
                data = body.encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                url = urllib.parse.urlsplit(self.path)
                with fixture.lock:
                    fixture.log.append({"path": url.path, "query": urllib.parse.parse_qs(url.query)})
                if url.path == "/search":
                    if fixture.mode == "ratelimit":
                        return self.send(429, "application/json", json.dumps({"error": "rate limited"}))
                    names = {"evidence": ["zephyr-notes", "zephyr-blog"], "zero": [],
                             "irrelevant": ["garden-a", "garden-b"]}[fixture.mode]
                    results = [{"title": PAGES[name][0], "url": f"{fixture.root}/page/{name}",
                                "content": SNIPPETS[name]} for name in names]
                    return self.send(200, "application/json", json.dumps({"results": results}))
                name = url.path.removeprefix("/page/")
                if url.path.startswith("/page/") and name in PAGES:
                    title, body = PAGES[name]
                    return self.send(200, "text/html; charset=utf-8",
                                     f"<!doctype html><html><head><title>{title}</title></head><body>{body}</body></html>")
                return self.send(404, "text/plain", "not found")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.root = f"http://127.0.0.1:{self.server.server_address[1]}"

    def reset(self, mode):
        with self.lock:
            self.mode, self.log = mode, []

    def requests(self):
        with self.lock:
            return list(self.log)


def tool_call(name, call_id):
    arguments = json.dumps({"query": QUERY})
    half = len(arguments) // 2
    return {"kind": "tool_call", "id": call_id, "name": name,
            "argument_fragments": [arguments[:half], arguments[half:]]}


SCENARIOS = [
    {"name": "rag-evidence", "tool": "rag.realtime", "mode": "evidence", "error": False, "outcome": "evidence",
     "contains": ["CURRENT WEB EVIDENCE", FACT, "/page/zephyr-notes"], "pages": 2,
     "answer": ["Zephyr shipped on September 10, 2026 ", "(release notes)."]},
    {"name": "rag-zero-results", "tool": "rag.realtime", "mode": "zero", "error": False,
     "outcome": "no_relevant_evidence", "contains": ["No current web results found"], "pages": 0,
     "answer": ["I found no current sources ", "for that."]},
    # Pages that share no word with the query still rank: with no lexical
    # match and no semantic endpoint, the search engine's order stands
    # (store.rs), so the outcome is evidence, not no_relevant_evidence.
    {"name": "rag-unrelated-pages", "tool": "rag.realtime", "mode": "irrelevant", "error": False,
     "outcome": "evidence", "contains": ["CURRENT WEB EVIDENCE", "/page/garden-a"], "pages": 2,
     "answer": ["The sources found ", "do not answer that."]},
    {"name": "search-results", "tool": "web.search", "mode": "evidence", "error": False, "outcome": None,
     "contains": ["/page/zephyr-notes", "/page/zephyr-blog"], "pages": 0,
     "answer": ["Two sources ", "were found."]},
    {"name": "search-zero-results", "tool": "web.search", "mode": "zero", "error": False, "outcome": None,
     "contains": ["No results for"], "pages": 0, "answer": ["The search ", "found nothing."]},
    {"name": "provider-429", "tool": "rag.realtime", "mode": "ratelimit", "error": True, "outcome": "failed",
     "contains": ["HTTP 429"], "pages": 0, "answer": ["The search provider ", "refused the request."]},
]


def run_scenario(scenario, fixture, work):
    problems = []
    fixture.reset(scenario["mode"])
    call_id = f"call_{scenario['name'].replace('-', '_')}"
    home = work / scenario["name"]
    home.mkdir()
    script = home / "scripts.json"
    script.write_text(json.dumps([
        tool_call(scenario["tool"], call_id),
        {"kind": "reasoning", "reasoning": ["Reading the tool result. "], "content": scenario["answer"]},
    ]))
    answer = "".join(scenario["answer"])
    gateway = agent = None
    try:
        gateway = subprocess.Popen([str(LIGHTWEIGHT), "--port", "0", "--model", "tool-trace-model", "--ctx", "8192",
                                    "--script-file", str(script)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        startup = json.loads(gateway.stdout.readline())
        recorder = trace.Recorder(startup["port"])
        model_id = trace.request(f"{recorder.root}/v1/models")[1]["data"][0]["id"]

        env = {**os.environ, "LIGHTAGENT_HOME": str(home / "agent-home")}
        run = lambda *args: subprocess.run([str(LIGHTAGENT), *args], check=True, env=env, stdout=subprocess.DEVNULL)
        run("init", "--base-url", recorder.root, "--model", model_id)
        run("config", "set", "web.enabled", "true")
        run("config", "set", "web.search.endpoint", f"{fixture.root}/search")
        run("config", "set", "rag.realtime_enabled", "true")
        # The fixture is loopback: name its network instead of dropping the guard.
        config_path = home / "agent-home" / "config.json"
        config = json.loads(config_path.read_text())
        config.setdefault("web", {})["trusted_networks"] = ["127.0.0.0/8"]
        config["web"]["block_private_addresses"] = True
        config_path.write_text(json.dumps(config, indent=2))

        port = trace.free_port()
        agent = subprocess.Popen([str(LIGHTAGENT), "serve", "--host", "127.0.0.1", "--port", str(port)],
                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        api = f"http://127.0.0.1:{port}/api/lightagent/v1"
        trace.wait_for(f"http://127.0.0.1:{port}/health", lambda status, _: status == 200, "Lightagent")
        status, session = trace.request(f"{api}/sessions", "POST", {})
        assert status == 201, (status, session)
        status, created = trace.request(f"{api}/runs", "POST", {"message": f"{QUERY}?", "session_id": session["id"]})
        assert status == 202, (status, created)
        run_id = created["id"]

        approvals = 0
        deadline = time.time() + 90
        finished = None
        while time.time() < deadline:
            status, body = trace.request(f"{api}/runs/{run_id}")
            if isinstance(body, dict) and body.get("status") == "awaiting_approval":
                approvals += 1
                trace.request(f"{api}/approvals/{run_id}", "POST", {"approve": True})
            elif isinstance(body, dict) and body.get("status") in ("completed", "failed", "cancelled"):
                finished = body
                break
            time.sleep(0.2)
        if finished is None:
            raise SystemExit(f"{scenario['name']}: the run did not end within 90s")

        with trace.urllib.request.urlopen(f"{api}/runs/{run_id}/events", timeout=30) as response:
            frames = trace.sse_frames(response.read().decode("utf-8"))
        saved = trace.wait_for(f"{api}/sessions/{session['id']}",
                               lambda status, body: status == 200 and len(body.get("runs") or []) == 1,
                               "the saved session")

        events = []
        for frame in frames:
            try:
                data = json.loads(frame["data"])
            except ValueError:
                data = {"unparsed": frame["data"]}
            events.append({"id": frame["id"], "event": frame["event"], "data": data})
        names = [event["event"] for event in events]
        ids = [int(event["id"]) for event in events if (event["id"] or "").isdigit()]
        heard = trace.ui_listens_for()
        of_call = [event for event in events if event["event"] in TOOL_EVENTS and event["data"].get("id") == call_id]
        count = lambda name: sum(1 for event in of_call if event["event"] == name)
        ended = [event for event in of_call if event["event"] in ("tool.output", "tool.failed")]
        progress = [event["data"] for event in of_call if event["event"] == "tool.progress"]
        content = "".join(str(event["data"].get("content") or "") for event in ended)
        searches = [entry for entry in fixture.requests() if entry["path"] == "/search"]
        pages = [entry["path"] for entry in fixture.requests() if entry["path"].startswith("/page/")]
        streams = trace.provider_view(recorder.exchanges)
        offered = []
        for exchange in recorder.exchanges:
            if exchange["path"].startswith("/v1/chat/completions"):
                offered.append((exchange["request"] or {}).get("tools_offered"))
        streamed_answer = "".join(str(event["data"].get("content") or "") for event in events if event["event"] == "model.delta")
        tools_saved = [tool for run in saved.get("runs") or [] for tool in run.get("tools") or []]

        # The event stream, as the UI receives it.
        if ids != list(range(len(ids))) or len(ids) != len(events):
            problems.append(f"event ids are not contiguous from 0: {[event['id'] for event in events]}")
        unheard = sorted({name for name in names if name not in heard})
        if unheard:
            problems.append(f"emitted but not listened for by the UI: {unheard}")
        reports = [index for index, name in enumerate(names) if name == "generation.report"]
        first_tool = next((index for index, name in enumerate(names) if name in TOOL_EVENTS), None)
        if not reports:
            problems.append("no generation.report: the model's window was not known, so this is not the Lightweight case")
        elif first_tool is None or reports[0] > first_tool:
            problems.append(f"expected a generation.report before the first tool event (reports at {reports}, first tool event at {first_tool})")
        if not any(name in TERMINAL for name in names):
            problems.append("no terminal event")
        # The call: once, with its real result.
        for name in ("tool.requested", "tool.started"):
            if count(name) != 1:
                problems.append(f"{name} for {call_id}: {count(name)} times, expected once")
        if len(ended) != 1:
            problems.append(f"{call_id} finished {len(ended)} times, expected once")
        requested = next((event["data"] for event in of_call if event["event"] == "tool.requested"), {})
        if requested.get("name") != scenario["tool"]:
            problems.append(f"requested tool {requested.get('name')!r}, expected {scenario['tool']!r}")
        if ended and bool(ended[0]["data"].get("is_error")) != scenario["error"]:
            problems.append(f"is_error {ended[0]['data'].get('is_error')!r}, expected {scenario['error']}")
        for needle in scenario["contains"]:
            if needle not in content:
                problems.append(f"tool result lacks {needle!r}: {content[:300]!r}")
        outcomes = [entry.get("outcome") for entry in progress if entry.get("stage") == "completed"]
        if scenario["outcome"] is not None and outcomes != [scenario["outcome"]]:
            problems.append(f"progress outcome {outcomes}, expected [{scenario['outcome']!r}]")
        # The provider: exactly one search per call, each page read at most once.
        if len(searches) != 1:
            problems.append(f"the search provider received {len(searches)} searches, expected 1")
        elif searches[0]["query"].get("q") != [QUERY]:
            problems.append(f"the search provider received q={searches[0]['query'].get('q')!r}, expected [{QUERY!r}]")
        if len(pages) != scenario["pages"] or len(set(pages)) != len(pages):
            problems.append(f"pages read {pages}, expected {scenario['pages']} distinct")
        if approvals > 1:
            problems.append(f"{approvals} approvals asked for one call")
        # The model saw the result, and the answer arrived once.
        if len(streams) != 2:
            problems.append(f"expected 2 chat completions (tool call, answer), saw {len(streams)}")
        if streamed_answer != answer:
            problems.append(f"streamed answer {streamed_answer!r}, expected {answer!r}")
        answers = [m.get("content") for m in saved.get("messages") or [] if m.get("role") == "assistant" and m.get("content")]
        if answers.count(answer) != 1:
            problems.append(f"saved answers {answers!r}, expected {answer!r} once")
        # The transcript's tool entry is the live result.
        if len(tools_saved) != 1:
            problems.append(f"saved tool entries {len(tools_saved)}, expected 1")
        else:
            excerpt = tools_saved[0].get("result_excerpt") or ""
            head = excerpt.strip()[:60]
            if tools_saved[0].get("tool") != scenario["tool"] or not head or head not in content:
                problems.append(f"saved tool entry {tools_saved[0]!r} does not match the live result")
            if (tools_saved[0].get("outcome") == "error") != scenario["error"]:
                problems.append(f"saved tool outcome {tools_saved[0].get('outcome')!r}")

        return {
            "scenario": scenario["name"], "tool": scenario["tool"], "provider_mode": scenario["mode"],
            "run": {"status": finished.get("status"), "stop_reason": finished.get("stop_reason"), "approvals": approvals},
            "events": [f"{event['id']}:{event['event']}" for event in events],
            "tool_events": of_call,
            "tool_result": content,
            "progress_outcomes": outcomes,
            "provider_requests": fixture.requests(),
            "chat_completions": [{"roles": (e["request"] or {}).get("roles"), "tools_offered": (e["request"] or {}).get("tools_offered")}
                                 for e in recorder.exchanges if e["path"].startswith("/v1/chat/completions")],
            "saved_tool_entries": tools_saved,
            "problems": problems,
        }
    finally:
        for process in (agent, gateway):
            if process:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()


def main():
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "tool-trace")
    out.mkdir(parents=True, exist_ok=True)
    if not LIGHTAGENT.is_file() or not LIGHTWEIGHT.is_file():
        raise SystemExit("missing built Lightagent or Lightweight test binary")
    work = pathlib.Path(tempfile.mkdtemp(prefix="lightagent-tool-trace-"))
    fixture = Fixture()
    results = []
    try:
        for scenario in SCENARIOS:
            try:
                results.append(run_scenario(scenario, fixture, work))
            except (AssertionError, SystemExit, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
                results.append({"scenario": scenario["name"], "problems": [f"did not complete: {error!r}"]})
    finally:
        fixture.server.shutdown()
        shutil.rmtree(work, ignore_errors=True)
    (out / "trace.json").write_text(json.dumps(results, indent=1))
    lines = ["### Tool trace: web.search and rag.realtime through Lightagent, model window known", "",
             "| Scenario | Provider | Searches | Pages | Outcome | Tool result | Events | Result |",
             "|---|---|---|---|---|---|---|---|"]
    for result in results:
        requests = result.get("provider_requests") or []
        tool_result = (result.get("tool_result") or "").replace("\n", " ").replace("|", "\\|")[:90]
        lines.append(
            f"| {result['scenario']} | {result.get('provider_mode', '')} "
            f"| {sum(1 for r in requests if r['path'] == '/search')} "
            f"| {sum(1 for r in requests if r['path'].startswith('/page/'))} "
            f"| {', '.join(map(str, result.get('progress_outcomes') or [])) or '—'} | `{tool_result}` "
            f"| {len(result.get('events') or [])} | {'FAIL' if result['problems'] else 'ok'} |")
    for result in results:
        lines += [f"- FAILED {result['scenario']}: {problem}" for problem in result["problems"]]
    if not any(result["problems"] for result in results):
        lines.append(f"- all {len(results)} scenarios hold: one search per call, the result in the events and the transcript, every event heard")
    print("\n".join(lines))
    return 1 if any(result["problems"] for result in results) else 0


if __name__ == "__main__":
    sys.exit(main())
