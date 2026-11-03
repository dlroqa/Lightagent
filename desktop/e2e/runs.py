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
  continue    with a one-second budget set through the host, a run started
              with on_time_budget pause pauses (run.paused, awaiting_continue)
              and sends nothing; continue resumes the same execution (the next
              turn carries the unread tool result): extend offers the tools,
              answer does not and ends on the budget; a second continue and a
              cancel while paused behave; a run that did not ask wraps up
  steering    the server side of the client-held steering queue: a steer sent
              while the session is busy is refused and not saved; two clients
              sending at once get exactly one admitted; queued turns then run
              in order
  crash       an approved write runs, then the runtime is killed while the
              model's next turn is in flight: the host reports the lost stream
              and restarts its runtime; the session records the run as
              Interrupted with no answer; neither the turn nor the write is
              repeated; the session takes the next run

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

    def slow_lookup(p, turn, index):
        # The tool's turn outlasts a one-second budget, so the budget runs
        # out with its result unread.
        if turn == 0:
            return [("tool", "datetime.now", {}), ("sleep", 1.5), ("finish", "tool_calls")]
        return [("text", "Found it."), ("finish", "stop")]

    for name in ("continue-extend", "continue-answer", "continue-cancel", "continue-default"):
        provider.scripts[name] = slow_lookup

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
        "crash": lambda p, turn, index: (
            [write("crash.txt", "once\n"), ("finish", "tool_calls")]
            if turn == 0 else [("text", "Still "), ("gate", "crash"), ("text", "working."), ("finish", "stop")]
        ),
        "steer-hold": lambda p, turn, index: [("text", "Working"), ("gate", "steer-hold"), ("text", " done."), ("finish", "stop")],
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


def scenario_continue(host, provider, workspace):
    check = Checks("continue")
    status, body = host.fetch("PUT", f"{API}/config/agent.wall_clock_secs", {"value": "1"})
    check.expect("a one-second time budget is set through the host", status == 200, (status, body))
    try:
        for mode in ("extend", "answer"):
            name = f"continue-{mode}"
            session = host.new_session()
            status, created = host.fetch(
                "POST", f"{API}/runs",
                {"message": f"[e2e:{name}] Look it up.", "session_id": session, "on_time_budget": "pause"},
            )
            check.expect(f"{mode}: the run starts", status == 202, (status, created))
            if status != 202:
                continue
            run = created["id"]
            stream = Stream(host, run)
            paused = stream.until(lambda s: s.has("run.paused"))
            _, info = host.fetch("GET", f"{API}/runs/{run}")
            waiting = len(provider.requests(name))
            status, answered = host.fetch("POST", f"{API}/runs/{run}/continue", {"mode": mode})
            stream.finish()
            again = host.fetch("POST", f"{API}/runs/{run}/continue", {"mode": mode})
            requests = provider.requests(name)
            final = (stream.data("run.completed") or [{}])[-1]
            check.expect(f"{mode}: the run paused on its budget", paused and (info or {}).get("status") == "awaiting_continue", (stream.names(), info))
            check.expect(f"{mode}: nothing was sent while paused", waiting == 1, waiting)
            check.expect(f"{mode}: the continue was taken", status == 200 and (answered or {}).get("continued") is True, (status, answered))
            # The tool result the model had not read is in the next turn's
            # conversation: the same execution went on, not a new turn.
            check.expect(f"{mode}: the same execution carried on", len(requests) == 2 and "tool" in requests[1]["roles"], requests)
            if mode == "extend":
                check.expect("extend: the tools are offered again", len(requests) == 2 and requests[1]["offered"] > 0, requests)
                check.expect("extend: the run completed", final.get("reason") == "EndTurn", final)
            else:
                check.expect("answer: the final turn has no tools", len(requests) == 2 and requests[1]["offered"] == 0, requests)
                check.expect("answer: it ended on the budget", final.get("reason") == "WallClockExceeded", final)
            check.expect(f"{mode}: the answer arrived", "Found it." in stream.text(), stream.text())
            check.expect(f"{mode}: a second continue is refused", again[0] == 409, again)

        session = host.new_session()
        status, created = host.fetch(
            "POST", f"{API}/runs",
            {"message": "[e2e:continue-cancel] Look it up.", "session_id": session, "on_time_budget": "pause"},
        )
        if status == 202:
            run = created["id"]
            stream = Stream(host, run)
            stream.until(lambda s: s.has("run.paused"))
            host.fetch("POST", f"{API}/runs/{run}/cancel", {})
            stream.finish()
            _, info = host.fetch("GET", f"{API}/runs/{run}")
            check.expect("cancel: a paused run is cancelled", (info or {}).get("status") == "cancelled" and stream.has("run.cancelled"), (info, stream.names()))
            check.expect("cancel: nothing more was sent", len(provider.requests("continue-cancel")) == 1, provider.requests("continue-cancel"))
        else:
            check.expect("cancel: the run starts", False, (status, created))

        session = host.new_session()
        stream = Stream(host, host.start_run(session, "[e2e:continue-default] Look it up.")).finish()
        requests = provider.requests("continue-default")
        final = (stream.data("run.completed") or [{}])[-1]
        check.expect("default: a run that did not ask never pauses", not stream.has("run.paused"), stream.names())
        check.expect("default: it wraps up without tools", final.get("reason") == "WallClockExceeded" and len(requests) == 2 and requests[1]["offered"] == 0, (final, requests))
    finally:
        host.fetch("PUT", f"{API}/config/agent.wall_clock_secs", {"value": ""})
    return check


def scenario_steering(host, provider, workspace):
    check = Checks("steering")
    session = host.new_session()
    first = host.start_run(session, "[e2e:steer-hold] Start.")
    stream = Stream(host, first)
    stream.until(lambda s: "Working" in s.text())
    busy = host.fetch("POST", f"{API}/runs", {"message": "[e2e:steer] Too early.", "session_id": session})
    provider.open("steer-hold")
    stream.finish()
    recorded(host, session, 1)
    check.expect("a steer sent while the session is busy is refused", busy[0] == 409, busy)

    # Two clients send their queued steer at once: exactly one is admitted.
    deadline = time.time() + 10
    while True:
        sent = [
            host.send("fetch", method="POST", target=f"{API}/runs",
                      body=json.dumps({"message": f"[e2e:steer] Steer {n}.", "session_id": session}))
            for n in (1, 2)
        ]
        answers = [host.answer(ident) for ident in sent]
        statuses = sorted(answer["status"] for answer in answers)
        if statuses != [409, 409] or time.time() > deadline:
            break
        time.sleep(0.1)
    check.expect("two clients at once: exactly one is admitted", statuses == [202, 409], statuses)
    if statuses == [202, 409]:
        won = 0 if answers[0]["status"] == 202 else 1
        Stream(host, json.loads(answers[won]["body"])["id"]).finish()
        recorded(host, session, 2)
        other = f"[e2e:steer] Steer {2 - won}."
        deadline, follow = time.time() + 10, None
        while time.time() < deadline:
            follow = host.fetch("POST", f"{API}/runs", {"message": other, "session_id": session})
            if follow[0] != 409:
                break
            time.sleep(0.1)
        if follow and follow[0] == 202:
            Stream(host, follow[1]["id"]).finish()
        saved = recorded(host, session, 3)
        prompts = [m.get("content") for m in saved.get("messages", []) if m.get("role") == "user"]
        expected = ["[e2e:steer-hold] Start.", f"[e2e:steer] Steer {won + 1}.", other]
        check.expect("the queued turns ran in the order admitted, the refused one never saved", prompts == expected, prompts)
    return check


def scenario_crash(host, provider, workspace, report):
    check = Checks("crash")
    path = os.path.join(workspace, "crash.txt")
    session = host.new_session()
    run = host.start_run(session, "[e2e:crash] Write the file, then keep working.")
    stream = Stream(host, run)
    stream.until(lambda s: s.has("approval.required"))
    host.fetch("POST", f"{API}/approvals/{run}", {"approve": True})
    held = provider.wait_for("gate", "crash", gate="crash")
    written = harness.read(path)
    victims = harness.runtimes(host.process.pid)
    for victim in victims:
        victim.kill()
    stream.finish()
    restarted = harness.wait_until(lambda: "RUNTIME_RESTARTED" in (harness.read(report) or ""))
    host.wait_ready()
    saved = host.session(session)
    gone = host.fetch("GET", f"{API}/runs/{run}")
    sent = len(provider.requests("crash"))
    follow = host.fetch("POST", f"{API}/runs", {"message": "[e2e:after-crash] Next.", "session_id": session})
    if follow[0] == 202:
        Stream(host, follow[1]["id"]).finish()
    runs = saved.get("runs", [])
    check.expect("the model's next turn was in flight", held, provider.log[-3:])
    check.expect("the approved write ran once before the crash", written == "once\n", written)
    check.expect("exactly one runtime was killed", len(victims) == 1, [v.pid for v in victims])
    check.expect("the host reported the lost stream", stream.error is not None, stream.error)
    check.expect("the host restarted its runtime", bool(restarted), None)
    check.expect("the session records the run as interrupted", bool(runs) and runs[-1].get("stop_reason") == "Interrupted", runs)
    check.expect("no answer was saved for it", harness.answers(saved) == [], harness.answers(saved))
    check.expect("the restarted runtime does not know the old run", gone[0] == 404, gone)
    check.expect("the turn was not sent again after the restart", sent == 2, provider.requests("crash"))
    check.expect("the session takes the next run", follow[0] == 202, follow)
    check.expect("the write was not repeated", harness.read(path) == "once\n" and len(provider.requests("crash")) == 2, harness.read(path))
    provider.open("crash")
    return check


def scenario_extensions(host, cli):
    """D6 through the host transport: install, list, show, disable, refuse and
    uninstall an extension; the CLI sees each change; an environment value
    of its MCP server never crosses the host; MCP stays off unasked."""
    import shutil
    import tempfile

    check = Checks("extensions")
    source = tempfile.mkdtemp(prefix="lightagent-e2e-extension-")
    bundle = os.path.join(source, "host-probe")
    secret = "e2e-extension-env-value"

    def cli_list():
        return json.loads(harness.run_cli(cli, "--json", "extensions", "list"))

    try:
        os.makedirs(os.path.join(bundle, "skills", "greet"))
        with open(os.path.join(bundle, "extension.json"), "w", encoding="utf-8") as handle:
            json.dump({
                "name": "host-probe", "version": "1.0.0", "description": "Host probe",
                "mcp_servers": [{"transport": "stdio", "name": "probe", "command": "probe-server",
                                 "env": {"TOKEN": secret}}],
            }, handle)
        with open(os.path.join(bundle, "skills", "greet", "SKILL.md"), "w", encoding="utf-8") as handle:
            handle.write("---\nname: greet\ndescription: Say hello\n---\nSay hello.\n")
        status, installed = host.fetch("POST", f"{API}/extensions", {"source": bundle})
        installed = installed or {}
        check.expect("installed through the host, MCP left off",
                     status == 201 and installed.get("name") == "host-probe" and installed.get("mcp_enabled") is False,
                     (status, installed))
        status, listed = host.fetch("GET", f"{API}/extensions")
        check.expect("the host and the CLI list the same", status == 200 and listed == cli_list(), (status, listed))
        status, shown = host.fetch("GET", f"{API}/extensions/host-probe")
        commands = [server.get("command") for server in (shown or {}).get("mcp", [])]
        check.expect("shown with its server's command, never an environment value",
                     status == 200 and commands == ["probe-server"] and secret not in json.dumps(shown),
                     (status, shown))
        status, toggled = host.fetch("PUT", f"{API}/extensions/host-probe/enabled", {"enabled": False})
        by_cli = [ext.get("active") for ext in cli_list().get("extensions", [])]
        check.expect("disabled through the host, inactive for the CLI",
                     status == 200 and (toggled or {}).get("active") is False and by_cli == [False],
                     (status, toggled, by_cli))
        status, refused = host.fetch("POST", f"{API}/extensions", {"source": "relative/folder"})
        check.expect("a relative source is refused", status == 400, (status, refused))
        status, off = host.fetch("PUT", f"{API}/config/extensions.enabled", {"value": "false"})
        by_cli = json.loads(harness.run_cli(cli, "--json", "extensions", "list"))
        check.expect("the master switch turned off through the host is off for the CLI",
                     status == 200 and by_cli.get("enabled") is False, (status, off, by_cli))
        harness.run_cli(cli, "config", "set", "extensions.enabled", "true")
        status, listed = host.fetch("GET", f"{API}/extensions")
        check.expect("turned on by the CLI, on through the host",
                     status == 200 and (listed or {}).get("enabled") is True, (status, listed))
        status, removed = host.fetch("DELETE", f"{API}/extensions/host-probe?scope=global")
        left = cli_list().get("extensions")
        check.expect("uninstalled through the host, gone for the CLI", status == 200 and left == [],
                     (status, removed, left))
    finally:
        shutil.rmtree(source, ignore_errors=True)
    return check


def scenario_workers(host, cli):
    """D7 through the host transport: a role assigned and revoked through the
    host; readiness through the host equals the CLI's before and after."""
    check = Checks("workers")

    def cli_status():
        return json.loads(harness.run_cli(cli, "--json", "workers", "status"))

    status, listed = host.fetch("GET", f"{API}/workers")
    check.expect("readiness through the host equals the CLI's",
                 status == 200 and (listed or {}).get("workers") == cli_status(), (status, listed))
    status, assigned = host.fetch("PUT", f"{API}/workers/reviewer", {})
    _, listed = host.fetch("GET", f"{API}/workers")
    check.expect("assigned through the host, as the CLI reports it",
                 status == 200 and isinstance((assigned or {}).get("id"), str)
                 and (listed or {}).get("workers") == cli_status(), (status, assigned))
    status, revoked = host.fetch("DELETE", f"{API}/workers/reviewer")
    check.expect("revoked through the host",
                 status == 200 and (revoked or {}).get("revoked") is True, (status, revoked))

    # Another profile's workers (--profile), assigned to a profile (--worker).
    def cli_status_of(profile):
        return json.loads(harness.run_cli(cli, "--json", "workers", "status", "--profile", profile))

    harness.run_cli(cli, "profile", "create", "e2e-helper")
    _, catalog = host.fetch("GET", f"{API}/profiles")
    active = (catalog or {}).get("active_profile")
    status, assigned = host.fetch("PUT", f"{API}/workers/coder", {"worker": active, "profile": "e2e-helper"})
    _, helper = host.fetch("GET", f"{API}/workers?profile=e2e-helper")
    coder = next((w for w in (helper or {}).get("workers", []) if w.get("role") == "coder"), {})
    check.expect("a role of the profile named assigned through the host to a profile, as the CLI reports it",
                 status == 200 and isinstance((assigned or {}).get("id"), str)
                 and str(coder.get("worker", "")).startswith("assigned:")
                 and (helper or {}).get("workers") == cli_status_of("e2e-helper"), (status, assigned, helper))
    _, listed = host.fetch("GET", f"{API}/workers")
    check.expect("the active profile's workers are untouched",
                 (listed or {}).get("workers") == cli_status(), listed)
    status, revoked = host.fetch("DELETE", f"{API}/workers/coder?profile=e2e-helper")
    _, helper = host.fetch("GET", f"{API}/workers?profile=e2e-helper")
    check.expect("revoked through the host for the profile named, as the CLI reports it",
                 status == 200 and (revoked or {}).get("revoked") is True
                 and (helper or {}).get("workers") == cli_status_of("e2e-helper"), (status, revoked, helper))
    return check


def scenario_remote_credentials(host, cli):
    """D7 through the host transport: the vault's status (with its key's
    source), a grant added from a secret file (its secret never in an
    answer), the listing, a rotation, a revocation and the audit equal the
    CLI's; an add without a secret file is refused and adds nothing."""
    import shutil
    import tempfile

    check = Checks("remote_credentials")
    work = tempfile.mkdtemp(prefix="lightagent-e2e-credentials-")

    def cli_json(*args):
        return json.loads(harness.run_cli(cli, "--json", *args))

    try:
        _, catalog = host.fetch("GET", f"{API}/profiles")
        active = (catalog or {}).get("active_profile")
        status, shown = host.fetch("GET", f"{API}/remote-credentials/vault")
        check.expect("status through the host equals the CLI's, the key from a file",
                     status == 200 and shown == cli_json("remote-credential", "status")
                     and (shown or {}).get("key_source") == "file", (status, shown))
        secret = "e2e-host-secret-4d5e6f7a8b9c"
        secret_file = os.path.join(work, "host.secret")
        with open(secret_file, "w", encoding="utf-8") as handle:
            handle.write(secret + "\n")
        body = {"profile": active, "service": "mcp:e2e-docs", "destinations": ["https://docs.e2e.invalid"]}
        status, refused = host.fetch("POST", f"{API}/remote-credentials", body)
        check.expect("an add without a secret file is refused and adds nothing",
                     status == 400 and cli_json("remote-credential", "list").get("credentials") == [],
                     (status, refused))
        status, added = host.fetch("POST", f"{API}/remote-credentials", dict(body, secret_file=secret_file))
        grant = (added or {}).get("added")
        check.expect("added through the host from the file, the secret not in the answer",
                     status == 200 and isinstance(grant, str) and secret not in json.dumps(added), (status, added))
        _, listed = host.fetch("GET", f"{API}/remote-credentials?profile={active}")
        check.expect("grants through the host equal the CLI's, with no secret",
                     listed == cli_json("remote-credential", "list", "--profile", active)
                     and secret not in json.dumps(listed), listed)
        rotated_file = os.path.join(work, "host-rotated.secret")
        with open(rotated_file, "w", encoding="utf-8") as handle:
            handle.write("e2e-host-secret-rotated-1a2b3c\n")
        status, rotated = host.fetch("POST", f"{API}/remote-credentials/{grant}/rotate", {"secret_file": rotated_file})
        versions = [g.get("version") for g in cli_json("remote-credential", "list").get("credentials", [])]
        check.expect("rotated through the host, version 2 for the CLI",
                     status == 200 and (rotated or {}).get("version") == 2 and versions == [2], (status, rotated))
        status, revoked = host.fetch("POST", f"{API}/remote-credentials/{grant}/revoke")
        states = [g.get("state") for g in cli_json("remote-credential", "list").get("credentials", [])]
        check.expect("revoked through the host, revoked for the CLI",
                     status == 200 and states == ["revoked"], (status, revoked, states))
        status, audit = host.fetch("GET", f"{API}/remote-credentials/audit?limit=3")
        check.expect("the audit through the host equals the CLI's, and holds no secret",
                     status == 200 and audit == cli_json("remote-credential", "audit", "--limit", "3")
                     and secret not in json.dumps(audit), (status, audit))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return check


def scenario_identity(host, cli):
    """D7 through the host transport: identity status, a user, a credential
    issued to a new file (its secret never in an answer), credentials,
    revocation, ownership and the audit through the host equal the CLI's;
    an issue without a secret file is refused and issues nothing."""
    import shutil
    import tempfile

    check = Checks("identity")
    work = tempfile.mkdtemp(prefix="lightagent-e2e-identity-")

    def cli_json(*args):
        return json.loads(harness.run_cli(cli, "--json", *args))

    try:
        status, shown = host.fetch("GET", f"{API}/identity")
        check.expect("status through the host equals the CLI's",
                     status == 200 and shown == cli_json("identity", "status"), (status, shown))
        status, created = host.fetch("POST", f"{API}/identity/users", {"id": "e2e-alice"})
        _, users = host.fetch("GET", f"{API}/identity/users")
        check.expect("a user created through the host, listed alike",
                     status == 200 and (created or {}).get("created") == "e2e-alice"
                     and users == cli_json("identity", "user", "list"), (status, created, users))
        status, refused = host.fetch("POST", f"{API}/identity/credentials", {"principal": "e2e-alice"})
        listed = cli_json("identity", "credential", "list", "--principal", "e2e-alice")
        check.expect("an issue without a secret file is refused and issues nothing",
                     status == 400 and listed.get("credentials") == [], (status, refused, listed))
        secret_file = os.path.join(work, "e2e-alice.key")
        status, issued = host.fetch("POST", f"{API}/identity/credentials", {
            "principal": "e2e-alice", "expires_in": 600, "capabilities": ["runs_read"], "secret_file": secret_file})
        key_id = (issued or {}).get("issued")
        token = ""
        if os.path.exists(secret_file):
            with open(secret_file, encoding="utf-8") as handle:
                token = handle.read().strip()
        secret = token.rsplit("_", 1)[-1] if token else "<no secret was delivered>"
        check.expect("issued through the host: the secret only in the new file",
                     status == 200 and isinstance(key_id, str) and token.startswith(f"la_{key_id}_")
                     and secret not in json.dumps(issued), (status, issued))
        _, listed = host.fetch("GET", f"{API}/identity/credentials?principal=e2e-alice")
        check.expect("credentials through the host equal the CLI's, with no secret",
                     listed == cli_json("identity", "credential", "list", "--principal", "e2e-alice")
                     and secret not in json.dumps(listed), listed)
        status, revoked = host.fetch("POST", f"{API}/identity/credentials/{key_id}/revoke")
        states = [c.get("state") for c in
                  cli_json("identity", "credential", "list", "--principal", "e2e-alice").get("credentials", [])]
        check.expect("revoked through the host, revoked for the CLI",
                     status == 200 and states == ["revoked"], (status, revoked, states))
        _, catalog = host.fetch("GET", f"{API}/profiles")
        active = (catalog or {}).get("active_profile")
        status, owned = host.fetch("PUT", f"{API}/identity/owners/{active}", {"principal": "e2e-alice"})
        _, grants = host.fetch("GET", f"{API}/identity/grants?profile={active}")
        check.expect("ownership through the host equals the CLI's",
                     status == 200 and (owned or {}).get("owner") == "e2e-alice"
                     and grants == cli_json("identity", "grants", "list", "--profile", active),
                     (status, owned, grants))
        status, removed = host.fetch("DELETE", f"{API}/identity/owners/{active}")
        check.expect("ownership removed through the host",
                     status == 200 and cli_json("identity", "grants", "list", "--profile", active).get("profiles") == [],
                     (status, removed))
        status, audit = host.fetch("GET", f"{API}/identity/audit?limit=4")
        check.expect("the audit through the host equals the CLI's, and holds no secret",
                     status == 200 and audit == cli_json("identity", "audit", "--limit", "4")
                     and secret not in json.dumps(audit), (status, audit))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return check


def scenario_sessions(host, cli):
    """D7 through the host transport: the session list and a saved session
    through the host equal the CLI's; a delete through the host is seen by
    the CLI."""
    check = Checks("sessions")

    def cli_json(*args):
        return json.loads(harness.run_cli(cli, "--json", *args))

    session = host.new_session()
    status, listed = host.fetch("GET", f"{API}/sessions")
    check.expect("the host lists what the CLI lists",
                 status == 200 and (listed or {}).get("sessions") == cli_json("sessions", "list"), (status, listed))
    status, shown = host.fetch("GET", f"{API}/sessions/{session}")
    check.expect("a session through the host is what the CLI shows",
                 status == 200 and shown == cli_json("sessions", "show", session), (status, shown))
    status, _ = host.fetch("DELETE", f"{API}/sessions/{session}")
    remaining = [s.get("id") for s in cli_json("sessions", "list")]
    check.expect("deleted through the host, gone for the CLI",
                 status in (200, 204) and session not in remaining, (status, remaining))
    return check


def scenario_knowledge(host, cli):
    """D7 through the host transport: a document indexed through the host is
    listed by the CLI; list, search and a skipped path through the host
    equal the CLI's; a clear through the host leaves the CLI nothing."""
    import shutil
    import tempfile

    check = Checks("knowledge")
    work = tempfile.mkdtemp(prefix="lightagent-e2e-knowledge-")

    def cli_json(*args):
        return json.loads(harness.run_cli(cli, "--json", *args))

    try:
        doc = os.path.join(work, "host-rivers.md")
        with open(doc, "w", encoding="utf-8") as handle:
            handle.write("Host rivers carry water from glaciers to the sea.\n")
        status, added = host.fetch("POST", f"{API}/knowledge", {"path": doc})
        check.expect("indexed through the host",
                     status == 200 and [i.get("source") for i in (added or {}).get("indexed", [])] == ["host-rivers.md"],
                     (status, added))
        named = os.path.join(work, "host-lakes.md")
        with open(named, "w", encoding="utf-8") as handle:
            handle.write("Host lakes hold still water between hills.\n")
        status, added = host.fetch("POST", f"{API}/knowledge", {"path": named, "source": "host-named-source"})
        cli_names = [s.get("source") for s in cli_json("rag", "list").get("sources", [])]
        check.expect("a source name through the host names the document, as --source does for the CLI",
                     status == 200
                     and [i.get("source") for i in (added or {}).get("indexed", [])] == ["host-named-source"]
                     and "host-named-source" in cli_names and "host-lakes.md" not in cli_names,
                     (status, added, cli_names))
        status, listed = host.fetch("GET", f"{API}/knowledge")
        check.expect("the host lists what the CLI lists", status == 200 and listed == cli_json("rag", "list"),
                     (status, listed))
        status, hits = host.fetch("GET", f"{API}/knowledge/search?q=glaciers&top_k=2")
        check.expect("search through the host equals the CLI's",
                     status == 200 and hits == cli_json("rag", "search", "glaciers", "--top-k", "2"), (status, hits))
        missing = os.path.join(work, "missing.md")
        status, skipped = host.fetch("POST", f"{API}/knowledge", {"path": missing})
        check.expect("a missing path skipped alike through the host and the CLI",
                     status == 200 and len((skipped or {}).get("skipped", [])) == 1
                     and skipped == cli_json("rag", "add", missing), (status, skipped))
        status, cleared = host.fetch("DELETE", f"{API}/knowledge")
        check.expect("cleared through the host, nothing left for the CLI",
                     status == 200 and (cleared or {}).get("cleared") is True
                     and cli_json("rag", "list").get("sources") == [], (status, cleared))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return check


def scenario_memory(host, cli):
    """D7 through the host transport: a memory added through the host is
    listed by the CLI; list, search and candidates through the host equal
    the CLI's; a forget through the host is seen by the CLI."""
    check = Checks("memory")

    def cli_json(*args):
        return json.loads(harness.run_cli(cli, "--json", *args))

    status, added = host.fetch("POST", f"{API}/memories",
                               {"text": "Host probe prefers tabs", "kind": "preference", "tags": ["host"]})
    memory = (added or {}).get("id")
    check.expect("added through the host", status == 200 and bool(memory), (status, added))
    status, listed = host.fetch("GET", f"{API}/memories")
    check.expect("the host lists what the CLI lists", status == 200 and listed == cli_json("memory", "list"),
                 (status, listed))
    status, hits = host.fetch("GET", f"{API}/memories/search?q=tabs&top_k=3")
    check.expect("search through the host equals the CLI's",
                 status == 200 and hits == cli_json("memory", "search", "tabs", "--top-k", "3"), (status, hits))
    session = host.new_session()
    status, found = host.fetch("GET", f"{API}/memories/candidates?session={session}")
    check.expect("candidates through the host equal the CLI's",
                 status == 200 and found == cli_json("memory", "candidates", session), (status, found))
    status, page = host.fetch("GET", f"{API}/memories/reflect")
    check.expect("reflect through the host equals the CLI's",
                 status == 200 and page == cli_json("memory", "reflect"), (status, page))
    status, updated = host.fetch("PUT", f"{API}/memories/items/{memory}", {"text": "Host probe prefers tabs always"})
    texts = [m.get("text") for m in cli_json("memory", "list").get("memories", [])]
    check.expect("updated through the host, as the CLI lists it",
                 status == 200 and (updated or {}).get("updated") is True and "Host probe prefers tabs always" in texts,
                 (status, updated, texts))
    # A conversation an earlier scenario saved: its first message is the user's.
    _, sessions = host.fetch("GET", f"{API}/sessions")
    saved = next((s.get("id") for s in (sessions or {}).get("sessions", []) if s.get("id") != session), None)
    status, kept = host.fetch("POST", f"{API}/memories/promote",
                              {"session": saved, "message": 1, "text": "Host kept fact", "kind": "decision", "tags": ["host"]})
    listed = cli_json("memory", "list").get("memories", [])
    kept_memory = next((m for m in listed if m.get("text") == "Host kept fact"), {})
    check.expect("a conversation's message kept through the host, with its source, as the CLI lists it",
                 status == 200 and kept_memory.get("kind") == "decision" and "host" in (kept_memory.get("tags") or [])
                 and (kept_memory.get("source") or {}).get("session_id") == saved,
                 (status, kept, saved, kept_memory))
    status, gone = host.fetch("DELETE", f"{API}/memories/items/{memory}")
    left = cli_json("memory", "list").get("memories", [])
    check.expect("forgotten through the host, gone for the CLI",
                 status == 200 and (gone or {}).get("removed") is True and all(m.get("id") != memory for m in left),
                 (status, gone, left))
    status, cleared = host.fetch("DELETE", f"{API}/memories")
    check.expect("cleared through the host, nothing left for the CLI",
                 status == 200 and (cleared or {}).get("cleared") is True
                 and cli_json("memory", "list").get("memories") == [], (status, cleared))
    return check


def scenario_skills(host, cli):
    """D6-6 (new scope) through the host transport: a skills folder imports
    inactive, the CLI sees the extension inactive, and it is removed."""
    import shutil
    import tempfile

    check = Checks("skills")
    work = tempfile.mkdtemp(prefix="lightagent-e2e-skills-")
    created = []
    try:
        skill = os.path.join(work, "host-pack", "skills", "host-skill")
        os.makedirs(skill)
        with open(os.path.join(skill, "SKILL.md"), "w", encoding="utf-8") as handle:
            handle.write("---\nname: host-skill\ndescription: Host\n---\nDo it.\n")
        status, imported = host.fetch("POST", f"{API}/skills/imports", {"source": os.path.join(work, "host-pack")})
        imported = imported or {}
        check.expect("imported through the host, inactive",
                     status == 201 and imported.get("skills") == ["host-skill"] and imported.get("active") is False,
                     (status, imported))
        listed = json.loads(harness.run_cli(cli, "--json", "extensions", "list")).get("extensions", [])
        check.expect("the CLI sees it inactive",
                     [(e.get("name"), e.get("active")) for e in listed] == [("host-pack", False)], listed)
        # GET /skills through the host: each skill with its scope and source.
        home = os.environ["LIGHTAGENT_HOME"]
        active = json.loads(harness.run_cli(cli, "profile", "list", "--json")).get("active") or "default"
        for root, name in ((os.path.join(home, "skills"), "host-global"),
                           (os.path.join(home, "profiles", active, "skills"), "host-profile")):
            os.makedirs(os.path.join(root, name), exist_ok=True)
            created.append(os.path.join(root, name))
            with open(os.path.join(root, name, "SKILL.md"), "w", encoding="utf-8") as handle:
                handle.write(f"---\nname: {name}\ndescription: Host\n---\nDo it.\n")
        status, _ = host.fetch("PUT", f"{API}/extensions/host-pack/enabled", {"enabled": True})
        status, skills = host.fetch("GET", f"{API}/skills")
        by_name = {s.get("name"): s for s in (skills or {}).get("skills", [])}
        check.expect("GET /skills through the host lists each skill with its scope and source",
                     status == 200
                     and by_name.get("host-global", {}).get("source") == {"scope": "global"}
                     and by_name.get("host-profile", {}).get("source") == {"scope": "profile"}
                     and by_name.get("host-skill", {}).get("source") == {"scope": "extension", "extension": "host-pack"}
                     and all(isinstance(s.get("description"), str) for s in by_name.values()),
                     (status, skills))
        status, _ = host.fetch("DELETE", f"{API}/extensions/host-pack")
        check.expect("removed through the host", status == 200, status)
    finally:
        shutil.rmtree(work, ignore_errors=True)
        for path in created:
            shutil.rmtree(path, ignore_errors=True)
    return check


def scenario_tools(host, cli):
    """D6-5 through the host transport: the catalog with permissions, built
    without connecting MCP, is what `lightagent tools list` reports."""
    check = Checks("tools")

    def rows(tools):
        return sorted((t.get("name"), t.get("risk"), t.get("permission")) for t in tools or [])

    status, catalog = host.fetch("GET", f"{API}/tools?mcp=skip")
    by_cli = json.loads(harness.run_cli(cli, "--json", "tools", "list"))
    check.expect("the host's catalog is the CLI's, permissions included",
                 status == 200 and rows((catalog or {}).get("tools")) == rows(by_cli)
                 and all(t.get("permission") in ("auto", "ask", "block") for t in (catalog or {}).get("tools", [])),
                 (status, catalog, by_cli))
    return check


def scenario_mcp(host):
    """D6-4 through the host transport: a stdio server is added and listed
    without starting; a check starts it once, reports why it failed with no
    environment value, and its process is gone afterwards; MCP stays off."""
    import shutil
    import tempfile

    check = Checks("mcp")
    work = tempfile.mkdtemp(prefix="lightagent-e2e-mcp-")
    pidfile = os.path.join(work, "pid")
    secret = "e2e-mcp-env-value-5c1e"
    script = ("import os, sys, time; open(sys.argv[1], 'w').write(str(os.getpid())); "
              "sys.stderr.write('token ' + os.environ.get('TOKEN', '') + '\\n'); sys.stderr.flush(); time.sleep(300)")

    def gone(pid):
        deadline = time.time() + 15
        while time.time() < deadline:
            try:
                with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
                    state = handle.read().rsplit(")", 1)[1].split()[0]
            except OSError:
                return True
            if state == "Z":
                return True
            time.sleep(0.1)
        return False

    try:
        status, _ = host.fetch("PUT", f"{API}/mcp", {"timeout_secs": 2})
        check.expect("timeout set through the host", status == 200, status)
        server = {"transport": "stdio", "name": "hostprobe", "command": sys.executable,
                  "args": ["-c", script, pidfile], "env": {"TOKEN": secret}}
        status, added = host.fetch("POST", f"{API}/mcp/servers", {"server": server})
        check.expect("added through the host, value withheld", status == 201 and secret not in json.dumps(added),
                     (status, added))
        status, listed = host.fetch("GET", f"{API}/mcp")
        check.expect("listing starts nothing", status == 200 and not os.path.exists(pidfile),
                     (status, os.path.exists(pidfile)))
        status, result = host.fetch("POST", f"{API}/mcp/servers/hostprobe/check")
        started = os.path.exists(pidfile)
        check.expect("a check starts it and reports the failure without the value",
                     status == 200 and started and (result or {}).get("ok") is False and secret not in json.dumps(result),
                     (status, result, started))
        pid = None
        if started:
            with open(pidfile, encoding="utf-8") as handle:
                pid = handle.read().strip()
        check.expect("its process is gone after the check", pid is not None and gone(pid), pid)
        status, view = host.fetch("GET", f"{API}/mcp")
        check.expect("a check does not turn MCP on", ((view or {}).get("mcp") or {}).get("enabled") is False, view)
        fields = {"transport": "http", "name": "hostauth", "url": "https://mcp.example.invalid/mcp",
                  "auth": {"kind": "env", "var": "HOST_MCP_TOKEN"},
                  "untrusted_profiles": ["research"], "credential_profiles": ["research"]}
        status, _ = host.fetch("POST", f"{API}/mcp/servers", {"server": fields})
        _, view = host.fetch("GET", f"{API}/mcp")
        saved = next((s for s in ((view or {}).get("mcp") or {}).get("servers", []) if s.get("name") == "hostauth"), {})
        check.expect("auth and profile fields saved through the host and shown by name",
                     status == 201 and saved.get("auth") == {"kind": "env", "var": "HOST_MCP_TOKEN"}
                     and saved.get("untrusted_profiles") == ["research"]
                     and saved.get("credential_profiles") == ["research"], (status, saved))
        host.fetch("DELETE", f"{API}/mcp/servers/hostauth")
        status, _ = host.fetch("DELETE", f"{API}/mcp/servers/hostprobe")
        check.expect("removed through the host", status == 200, status)
    finally:
        shutil.rmtree(work, ignore_errors=True)
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
    # The vault's key, outside the home; the host's runtime and the CLI
    # calls here both inherit it.
    import tempfile
    key_dir = tempfile.mkdtemp(prefix="lightagent-e2e-credential-key-")
    os.environ["LIGHTAGENT_CREDENTIAL_KEY_FILE"] = os.path.join(key_dir, "credential.key")
    harness.run_cli(cli, "remote-credential", "keygen", os.environ["LIGHTAGENT_CREDENTIAL_KEY_FILE"])
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
            ("continue", lambda: scenario_continue(host, provider, workspace)),
            ("steering", lambda: scenario_steering(host, provider, workspace)),
            ("extensions", lambda: scenario_extensions(host, cli)),
            ("mcp", lambda: scenario_mcp(host)),
            ("tools", lambda: scenario_tools(host, cli)),
            ("skills", lambda: scenario_skills(host, cli)),
            ("memory", lambda: scenario_memory(host, cli)),
            ("knowledge", lambda: scenario_knowledge(host, cli)),
            ("sessions", lambda: scenario_sessions(host, cli)),
            ("workers", lambda: scenario_workers(host, cli)),
            ("identity", lambda: scenario_identity(host, cli)),
            ("remote_credentials", lambda: scenario_remote_credentials(host, cli)),
            # Last: it kills the runtime, and the host restarts one at most once a minute.
            ("crash", lambda: scenario_crash(host, provider, workspace, report)),
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
