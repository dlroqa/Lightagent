"""Profile-switch scenarios for the desktop host, on a GitHub-hosted runner.

Usage: python switch.py <out-dir> <lightagent-cli> -- <host command...>

The host must be a test build with `--features e2e-hooks`, which runs the
switches named in LIGHTAGENT_DESKTOP_E2E_SWITCHES once its runtime is ready
and records each result. The bundled app is never built with that feature.
All scenarios share one isolated LIGHTAGENT_HOME (set by the caller):

  ok          switch to `work`: switched; the CLI's active profile is `work`
  start       switch to a missing profile: the new runtime cannot start;
              nothing changes (still serving `work`, the CLI still says `work`)
  persist     a directory occupies the temporary file the atomic active-profile
              write must create, so the commit fails for real: nothing changes,
              and the unused new runtime is stopped
  concurrent  two switches at once: one switches, the other is refused as busy
  pending     (driven host, fake provider; harness.py) a run waits for approval
              of a file write. While a switch holds its gate, starting a run,
              regenerating and answering the approval are refused. A switch
              that does not stop runs is blocked and the approval stays
              pending; one that stops them rejects it explicitly, and the new
              profile's runtime knows nothing of that run or its approval.
              The file is never written

After every scenario exactly one runtime may remain under the host. Writes
switch.json; exits non-zero when any expectation fails.
"""

import json
import os
import subprocess
import sys
import time

import psutil


def read(path):
    return open(path, encoding="utf-8").read() if os.path.exists(path) else ""


def cli_active(cli):
    out = subprocess.run([cli, "profile", "list", "--json"], capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        return f"<cli failed: {out.stderr.strip()[:200]}>"
    return json.loads(out.stdout).get("active")


def runtimes(pid):
    try:
        root = psutil.Process(pid)
        return [p for p in root.children(recursive=True) if "sidecar" in p.name() and p.is_running()]
    except psutil.Error:
        return []


def scenario(out, name, command, switches):
    report = os.path.join(out, f"report-switch-{name}.txt")
    if os.path.exists(report):
        os.remove(report)
    env = dict(os.environ, LIGHTAGENT_DESKTOP_E2E_REPORT=report, LIGHTAGENT_DESKTOP_E2E_SWITCHES=switches)
    app = subprocess.Popen(command, env=env)
    deadline = time.time() + 120
    while time.time() < deadline and app.poll() is None and "SWITCHES_DONE" not in read(report):
        time.sleep(0.2)
    # A replaced or unused runtime stops within its grace period.
    settle = time.time() + 10
    while time.time() < settle and len(runtimes(app.pid)) > 1:
        time.sleep(0.2)
    remaining = len(runtimes(app.pid))
    lines = [line for line in read(report).splitlines() if line.startswith(("SWITCH_RESULT", "SWITCHES_DONE"))]
    app.terminate()
    try:
        app.wait(timeout=15)
    except subprocess.TimeoutExpired:
        app.kill()
    return lines, remaining


def has(lines, *parts):
    return any(line.startswith("SWITCH_RESULT") and all(part in line for part in parts) for line in lines)


def pending_approval(out, cli, command, home):
    """A pending approval across a profile switch (see the module docstring)."""
    import harness
    from harness import API

    provider = harness.FakeProvider()
    provider.scripts["switch-approve"] = lambda p, turn, index: (
        [("tool", "fs.write", {"path": "switch.txt", "content": "written across a switch\n", "append": True}),
         ("finish", "tool_calls")]
        if turn == 0 else [("text", "After the decision."), ("finish", "stop")]
    )
    harness.prepare_home(cli, home, provider)
    report = os.path.join(out, "report-switch-pending.txt")
    env = dict(
        os.environ,
        LIGHTAGENT_DESKTOP_E2E_DRIVER="1",
        LIGHTAGENT_DESKTOP_E2E_SWITCH_GATE="1",
        LIGHTAGENT_DESKTOP_E2E_REPORT=report,
    )
    host = harness.Host(command, env, os.path.join(out, "driver-switch-pending.log"))
    check = harness.Checks("pending")
    seen = {}
    try:
        status = host.ready()
        check.expect("starts on default", status.get("profile") == "default", status.get("profile"))
        path = os.path.join(host.workspace(), "switch.txt")
        session = host.new_session()
        run = host.start_run(session, "[e2e:switch-approve] Write the file.")
        stream = harness.Stream(host, run)
        check.expect("the run waits for approval", stream.until(lambda s: s.has("approval.required")), stream.names())

        switching = host.send("switch", profile="work", cancel_runs=False)
        host.wait_event("switch_held")
        during = {
            "runs.create": host.try_fetch("POST", f"{API}/runs", {"message": "[e2e:switch-approve] Again.", "session_id": session}),
            "sessions.regenerate": host.try_fetch("POST", f"{API}/sessions/{session}/regenerate", {"run_id": "r-any"}),
            "approvals.respond": host.try_fetch("POST", f"{API}/approvals/{run}", {"approve": True}),
        }
        host.call("release_switch")
        blocked = host.answer(switching)
        seen["during"] = during
        for operation, answer in during.items():
            check.expect(f"{operation} is refused during the switch", answer[0] == "refused" and "switch is in progress" in answer[1], answer)
        check.expect("without stopping runs the switch is blocked", blocked.get("status") == "blocked" and any(r.get("id") == run for r in blocked.get("runs", [])), blocked)
        status = host.call("status")
        _, approvals = host.fetch("GET", f"{API}/approvals")
        check.expect("still serving default", status.get("profile") == "default", status.get("profile"))
        check.expect("the approval is still pending", any(r.get("run") == run and r.get("pending") for r in approvals.get("approvals", [])), approvals)
        check.expect("nothing written while blocked", harness.read(path) is None, harness.read(path))

        switching = host.send("switch", profile="work", cancel_runs=True)
        host.wait_event("switch_held")
        host.call("release_switch")
        switched = host.answer(switching)
        stream.finish()
        status = host.call("status")
        check.expect("stopping the runs lets the switch happen", switched.get("status") == "switched", switched)
        check.expect("now serving work", status.get("profile") == "work", status.get("profile"))
        denied = [o for o in stream.data("tool.output") + stream.data("tool.failed") if "denied" in json.dumps(o)]
        check.expect("the approval was rejected explicitly", bool(denied), stream.names())

        _, approvals = host.fetch("GET", f"{API}/approvals")
        crossed = host.fetch("POST", f"{API}/approvals/{run}", {"approve": True})
        old = host.fetch("GET", f"{API}/runs/{run}")
        check.expect("work's runtime has no pending approval", approvals.get("approvals") == [], approvals)
        check.expect("the old approval cannot be answered in work", crossed[0] == 404, crossed)
        check.expect("work's runtime does not know the old run", old[0] == 404, old)
        check.expect("the file was never written", harness.read(path) is None, harness.read(path))
        saved = harness.wait_until(lambda: (lambda s: s if s and s.get("runs") else None)(harness.session_file(home, session)), timeout=15)
        seen["saved_runs"] = (saved or {}).get("runs")
        check.expect("default's session records the stopped run", bool(saved), saved)
    except Exception as error:
        check.failures.append(f"pending: {error!r}")
    finally:
        host.stop()
        provider.close()
    seen["checks"] = check.results
    seen["provider"] = provider.log
    return seen, check.failures


def main():
    out = os.path.abspath(sys.argv[1])
    cli = sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    home = os.environ["LIGHTAGENT_HOME"]
    created = subprocess.run([cli, "profile", "create", "work"], capture_output=True, text=True, timeout=60)
    results = {"setup": created.returncode == 0}
    failures = [] if created.returncode == 0 else [f"setup: {created.stderr.strip()[:200]}"]

    lines, remaining = scenario(out, "ok", command, "work")
    active = cli_active(cli)
    results["ok"] = {"lines": lines, "runtimes_left": remaining, "cli_active": active}
    if not has(lines, "id=work", "status=switched", "serving=work"):
        failures.append("ok: the switch to work was not reported as switched and serving work")
    if active != "work":
        failures.append(f"ok: the CLI's active profile is {active!r}, expected 'work'")

    lines, remaining = scenario(out, "start", command, "missing")
    active = cli_active(cli)
    results["start"] = {"lines": lines, "runtimes_left": remaining, "cli_active": active}
    if not has(lines, "status=error", "kind=start_failed", "serving=work"):
        failures.append("start: a runtime that cannot start was not refused while still serving work")
    if active != "work":
        failures.append(f"start: the CLI's active profile changed to {active!r}")

    blocker = os.path.join(home, "active_profile.tmp")
    os.mkdir(blocker)
    try:
        lines, remaining = scenario(out, "persist", command, "default")
    finally:
        os.rmdir(blocker)
    active = cli_active(cli)
    results["persist"] = {"lines": lines, "runtimes_left": remaining, "cli_active": active}
    if not has(lines, "status=error", "kind=persist_failed", "serving=work"):
        failures.append("persist: a failed active-profile write did not leave work in service")
    if active != "work":
        failures.append(f"persist: the CLI's active profile changed to {active!r}")

    lines, remaining = scenario(out, "concurrent", command, "default&default")
    active = cli_active(cli)
    results["concurrent"] = {"lines": lines, "runtimes_left": remaining, "cli_active": active}
    if not (has(lines, "id=default", "status=switched") and has(lines, "status=error", "kind=busy")):
        failures.append("concurrent: expected exactly one switch and one busy refusal")
    if active != "default":
        failures.append(f"concurrent: the CLI's active profile is {active!r}, expected 'default'")

    for name in ("ok", "start", "persist", "concurrent"):
        entry = results.get(name)
        if isinstance(entry, dict) and entry["runtimes_left"] != 1:
            failures.append(f"{name}: {entry['runtimes_left']} runtimes remain under the host, expected exactly 1")

    results["pending"], pending_failures = pending_approval(out, cli, command, home)
    failures.extend(pending_failures)

    results["failures"] = failures
    with open(os.path.join(out, "switch.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    print(json.dumps(results, indent=1))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
