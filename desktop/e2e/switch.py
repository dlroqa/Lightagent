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

    results["failures"] = failures
    with open(os.path.join(out, "switch.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    print(json.dumps(results, indent=1))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
