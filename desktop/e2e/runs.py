"""Run scenarios through the desktop host, on a GitHub-hosted runner (D5a).

Usage: python runs.py <out-dir> <lightagent-cli> -- <host command...>

The host is a test build (`--features e2e-hooks`) driven over its stdin
(harness.Host). Its runtime is the real `lightagent serve --desktop`; its
provider is harness.FakeProvider, so every turn is deterministic. Scenarios
synchronize on what the host relayed or the provider received, not on time.
LIGHTAGENT_HOME, set by the caller, is isolated.

  stream      deltas stream, a tool that needs no approval runs, the answer is saved
  approve     a file write waits for approval; granted, it runs exactly once
  deny        denied, it never runs, and the model is told so
  cancel      a run cancelled mid-stream ends cancelled; the session takes the next run
  relay       the host's relay is dropped twice mid-run; it resumes, and the
              events it delivers are every event of the run, once each, in order
  regenerate  R1 through the host: a finished answer replaces the saved one;
              a provider failure, a refusal, an empty answer and a cancel keep
              it; a stale answer id and a busy session are refused

This is host-level evidence (host transport and relay plus the real
runtime), not browser or native WebView evidence. Writes runs.json; exits
non-zero when any expectation fails.
"""

import json
import os
import sys
import time

import harness
from harness import API, Checks, Stream


def scripts(provider):
    def write(name, content):
        return ("tool", "fs.write", {"path": name, "content": content, "append": True})

    provider.scripts.update({
        "stream": lambda p, turn, index: (
            [("text", "Checking "), ("text", "the time. "), ("tool", "datetime.now", {}), ("finish", "tool_calls")]
            if turn == 0 else [("text", "It is "), ("text", "now."), ("finish", "stop")]
        ),
        "approve": lambda p, turn, index: (
            [write("approved.txt", "granted\n"), ("finish", "tool_calls")]
            if turn == 0 else [("text", "Wrote it."), ("finish", "stop")]
        ),
        "deny": lambda p, turn, index: (
            [write("denied.txt", "should not exist\n"), ("finish", "tool_calls")]
            if turn == 0 else [("text", "Not written."), ("finish", "stop")]
        ),
        "cancel": lambda p, turn, index: [("text", "Working"), ("gate", "cancel"), ("text", " never"), ("finish", "stop")],
        "relay": lambda p, turn, index: (
            [("text", "one "), ("gate", "relay"), ("text", "two "), ("text", "three "),
             ("tool", "datetime.now", {}), ("finish", "tool_calls")]
            if turn == 0 else [("gate", "relay-2"), ("text", "four."), ("finish", "stop")]
        ),
        "regen": lambda p, turn, index: {
            "first": [("text", "First answer."), ("finish", "stop")],
            "second": [("text", "Second answer."), ("finish", "stop")],
            "fail": [("status", 500)],
            "refusal": [("refusal", "I can't help with that."), ("finish", "stop")],
            "empty": [("finish", "stop")],
            "hold": [("text", "Partial"), ("gate", "regen-hold"), ("text", " never"), ("finish", "stop")],
        }[p.modes.get("regen", "first")],
    })


def recorded(host, session, runs):
    """Wait until the session has `runs` recorded runs; return it."""
    return harness.wait_until(lambda: (lambda s: s if len(s.get("runs", [])) >= runs else None)(host.session(session)))


def scenario_stream(host, provider, workspace):
    check = Checks("stream")
    session = host.new_session()
    run = host.start_run(session, "[e2e:stream] What time is it?")
    stream = Stream(host, run).finish()
    saved = recorded(host, session, 1)
    names = stream.names()
    outputs = stream.data("tool.output")
    check.expect("deltas streamed before the tool", names.count("model.delta") >= 3, names)
    check.expect("the tool ran without approval", bool(outputs) and not stream.has("approval.required"), outputs)
    check.expect("the run completed", names[-1:] == ["run.completed"] and stream.error is None, (names[-1:], stream.error))
    check.expect("the streamed text is the answer", stream.text() == "Checking the time. It is now.", stream.text())
    check.expect("the answer is saved", (harness.answers(saved) or [""])[-1].endswith("It is now."), harness.answers(saved))
    check.expect("the provider got two turns", len(provider.requests("stream")) == 2, provider.requests("stream"))
    return check


def scenario_approval(host, provider, workspace, approve):
    name = "approve" if approve else "deny"
    path = os.path.join(workspace, "approved.txt" if approve else "denied.txt")
    check = Checks(name)
    session = host.new_session()
    run = host.start_run(session, f"[e2e:{name}] Write the file.")
    stream = Stream(host, run)
    stream.until(lambda s: s.has("approval.required"))
    status, listing = host.fetch("GET", f"{API}/approvals")
    pending = [row for row in (listing or {}).get("approvals", []) if row.get("run") == run and row.get("pending")]
    before = harness.read(path)
    status, answered = host.fetch("POST", f"{API}/approvals/{run}", {"approve": approve})
    stream.finish()
    check.expect("the approval is pending and listed", bool(pending), listing)
    check.expect("nothing was written before the decision", before is None, before)
    check.expect("the decision was delivered", status == 200 and (answered or {}).get("delivered") is True, (status, answered))
    check.expect("the run completed", stream.has("run.completed") and stream.error is None, (stream.names()[-1:], stream.error))
    after = harness.read(path)
    if approve:
        check.expect("the file was written exactly once", after == "granted\n", after)
    else:
        told = [o for o in stream.data("tool.output") + stream.data("tool.failed") if "denied" in json.dumps(o)]
        check.expect("the file was never written", after is None, after)
        check.expect("the model was told it was denied", bool(told), stream.data("tool.output") + stream.data("tool.failed"))
    check.expect("the provider got two turns", len(provider.requests(name)) == 2, provider.requests(name))
    return check


def scenario_cancel(host, provider, workspace):
    check = Checks("cancel")
    session = host.new_session()
    run = host.start_run(session, "[e2e:cancel] Work for a while.")
    stream = Stream(host, run)
    stream.until(lambda s: "Working" in s.text())
    held = provider.wait_for("gate", "cancel", gate="cancel")
    status, _ = host.fetch("POST", f"{API}/runs/{run}/cancel", {})
    stream.finish()
    _, info = host.fetch("GET", f"{API}/runs/{run}")
    saved = recorded(host, session, 1)
    check.expect("the provider was mid-turn", held, provider.log[-3:])
    check.expect("the cancel was accepted", status == 200, status)
    check.expect("the run is cancelled", (info or {}).get("status") == "cancelled", info)
    check.expect("the saved run says cancelled", saved["runs"][-1].get("stop_reason") == "Cancelled", saved["runs"][-1])
    # The session's lease went with the run: the next run on it is admitted.
    deadline, follow = time.time() + 10, None
    while time.time() < deadline:
        follow = host.fetch("POST", f"{API}/runs", {"message": "[e2e:after-cancel] Next.", "session_id": session})
        if follow[0] != 409:
            break
        time.sleep(0.1)
    if follow and follow[0] == 202:
        Stream(host, follow[1]["id"]).finish()
    check.expect("the next run on the session is admitted", follow and follow[0] == 202, follow)
    provider.open("cancel")
    check.expect("the cancelled turn was not sent again", len(provider.requests("cancel")) == 1, provider.requests("cancel"))
    return check


def scenario_relay(host, provider, workspace, report):
    check = Checks("relay")
    session = host.new_session()
    run = host.start_run(session, "[e2e:relay] Count to four.")
    stream = Stream(host, run)
    stream.until(lambda s: "one" in s.text())
    first = provider.wait_for("gate", "relay", gate="relay")
    stream.drop()
    provider.open("relay")
    stream.until(lambda s: s.has("tool.output"))
    second = provider.wait_for("gate", "relay", gate="relay-2")
    stream.drop()
    provider.open("relay-2")
    stream.finish()
    _, info = host.fetch("GET", f"{API}/runs/{run}")
    total = (info or {}).get("events")
    ids = stream.ids()
    ends = [line for line in harness.read(report).splitlines() if line.startswith("RELAY_END") and f"/runs/{run}/" in line]
    check.expect("both drops happened mid-run", first and second, (first, second))
    check.expect("every event once, in order", isinstance(total, int) and ids == list(range(total)), {"ids": ids, "events": total})
    check.expect("the relay ended without an error", stream.error is None, stream.error)
    check.expect("the host resumed twice", any("resumes=2" in line and "error=false" in line for line in ends), ends)
    check.expect("the text arrived whole", stream.text() == "one two three four.", stream.text())
    check.expect("the provider got two turns", len(provider.requests("relay")) == 2, provider.requests("relay"))
    return check


def scenario_regenerate(host, provider, workspace):
    check = Checks("regenerate")
    session = host.new_session()
    provider.modes["regen"] = "first"
    Stream(host, host.start_run(session, "[e2e:regen] Say something.")).finish()
    saved = recorded(host, session, 1)
    check.expect("the first answer is saved", harness.answers(saved) == ["First answer."], harness.answers(saved))

    def regenerate(mode, answer):
        provider.modes["regen"] = mode
        return host.fetch("POST", f"{API}/sessions/{session}/regenerate", {"run_id": answer})

    def settled():
        # A stale answer id is refused as "not the latest" only once the
        # session is free again (a busy one is refused as busy first).
        deadline = time.time() + 30
        while time.time() < deadline:
            status, body = host.fetch("POST", f"{API}/sessions/{session}/regenerate", {"run_id": "r-stale"})
            if status == 409 and "latest" in json.dumps(body):
                return True
            time.sleep(0.1)
        return False

    status, created = regenerate("second", saved["runs"][-1]["run_id"])
    replaced = Stream(host, created["id"]).finish() if status == 202 else None
    current = harness.wait_until(lambda: (lambda s: s if harness.answers(s) == ["Second answer."] else None)(host.session(session)), timeout=30)
    check.expect("a finished answer replaces the saved one", bool(current), host.session(session))
    if not current:
        return check
    check.expect("one run record, the new one", len(current["runs"]) == 1 and current["runs"][-1]["run_id"] != saved["runs"][-1]["run_id"], current["runs"])
    check.expect("the replacement streamed", replaced is not None and replaced.text() == "Second answer.", replaced and replaced.text())
    check.expect("a stale answer id is refused", settled(), None)
    answer = current["runs"][-1]["run_id"]

    for mode in ("fail", "refusal", "empty"):
        before = host.session(session)
        status, created = regenerate(mode, answer)
        stream = Stream(host, created["id"]).finish() if status == 202 else None
        free = settled()
        after = host.session(session)
        check.expect(f"{mode}: accepted and run", status == 202 and stream is not None, (status, created))
        check.expect(f"{mode}: the saved answer is kept", free and after["messages"] == before["messages"] and after["runs"] == before["runs"], harness.answers(after))

    before = host.session(session)
    status, created = regenerate("hold", answer)
    stream = Stream(host, created["id"])
    stream.until(lambda s: "Partial" in s.text())
    busy = host.fetch("POST", f"{API}/sessions/{session}/regenerate", {"run_id": answer})
    host.fetch("POST", f"{API}/runs/{created['id']}/cancel", {})
    stream.finish()
    free = settled()
    after = host.session(session)
    check.expect("a busy session refuses another regeneration", busy[0] == 409 and "active run" in json.dumps(busy[1]), busy)
    check.expect("cancel: the saved answer is kept", free and after["messages"] == before["messages"] and after["runs"] == before["runs"], harness.answers(after))
    check.expect("the saved answer is still the replacement", harness.answers(after) == ["Second answer."], harness.answers(after))
    provider.open("regen-hold")
    return check


def main():
    out = os.path.abspath(sys.argv[1])
    cli = sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    home = os.environ["LIGHTAGENT_HOME"]
    report = os.path.join(out, "report-runs.txt")
    provider = harness.FakeProvider()
    scripts(provider)
    harness.prepare_home(cli, home, provider)
    env = dict(os.environ, LIGHTAGENT_DESKTOP_E2E_DRIVER="1", LIGHTAGENT_DESKTOP_E2E_REPORT=report)
    host = harness.Host(command, env, os.path.join(out, "driver-runs.log"))
    results, failures = {}, []
    try:
        host.ready()
        workspace = host.workspace()
        results["workspace"] = workspace
        scenarios = [
            ("stream", lambda: scenario_stream(host, provider, workspace)),
            ("approve", lambda: scenario_approval(host, provider, workspace, True)),
            ("deny", lambda: scenario_approval(host, provider, workspace, False)),
            ("cancel", lambda: scenario_cancel(host, provider, workspace)),
            ("relay", lambda: scenario_relay(host, provider, workspace, report)),
            ("regenerate", lambda: scenario_regenerate(host, provider, workspace)),
        ]
        for name, scenario in scenarios:
            try:
                check = scenario()
                results[name] = check.results
                failures.extend(check.failures)
            except Exception as error:  # a scenario that breaks is a failure, and the rest still run
                results[name] = {"error": repr(error)}
                failures.append(f"{name}: {error!r}")
    except Exception as error:
        failures.append(f"setup: {error!r}")
    finally:
        host.stop()
        provider.close()
    results["provider"] = provider.log
    results["failures"] = failures
    with open(os.path.join(out, "runs.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    print(json.dumps({"failures": failures}, indent=1))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
