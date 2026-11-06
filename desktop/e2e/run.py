"""Unattended desktop end-to-end checks on a GitHub-hosted runner.

Usage: python run.py <out-dir> <mode> -- <app command...>

Modes:
  ready      the runtime starts, the UI loads and makes its first API call
  mismatch   a runtime reporting another version is refused, with the error screen
  crash      the runtime is killed under a ready app: the host reports it,
             restarts it once, and the UI is ready again
  duplicate  a second launch finds the first instance, starts no runtime, and
             says so; the first instance keeps running

Every mode screenshots the window and checks that no 256-bit hex value (the
shape of the launch key) appears in any argv or readable environment of the
app's process tree, or in the host's report. Then the host is terminated the
way a crash would end it, and the runtime must follow it within 10 s when
E2E_REQUIRE_NO_ORPHAN=1. Writes e2e-<mode>.json; exits non-zero when an
expectation is not met.
"""

import json
import os
import re
import subprocess
import sys
import time

import psutil

KEY_SHAPE = re.compile(r"\b[0-9a-f]{64}\b")
READY = ["RUNTIME_READY", "PAGE_LOADED", "UI_READY", "API_OK sessions.list"]


def screenshot(path):
    try:
        if sys.platform == "darwin":
            subprocess.run(["screencapture", "-x", path], check=True, timeout=30)
        elif sys.platform.startswith("linux"):
            subprocess.run(["import", "-window", "root", path], check=True, timeout=30)
        else:
            from PIL import ImageGrab

            ImageGrab.grab().save(path)
        return True
    except Exception as error:
        print(f"screenshot failed: {error}")
        return False


def tree(pid):
    try:
        root = psutil.Process(pid)
        return [root] + root.children(recursive=True)
    except psutil.Error:
        return []


def read(path):
    return open(path, encoding="utf-8").read() if os.path.exists(path) else ""


def launch(command, report):
    if os.path.exists(report):
        os.remove(report)
    return subprocess.Popen(command, env=dict(os.environ, LIGHTAGENT_DESKTOP_E2E_REPORT=report))


def wait_for(report, tags, process, started, seen, timeout=90):
    """Record when each tag first appears in the report."""
    deadline = time.time() + timeout
    while time.time() < deadline and process.poll() is None:
        lines = read(report).splitlines()
        for tag in tags:
            if tag not in seen and any(line.startswith(tag) for line in lines):
                seen[tag] = round(time.time() - started, 3)
        if all(tag in seen for tag in tags):
            return
        time.sleep(0.1)


def leaks_in(processes, *reports):
    found = []
    for process in processes:
        try:
            if KEY_SHAPE.search(" ".join(process.cmdline())):
                found.append(f"argv of {process.name()}")
        except psutil.Error:
            pass
        try:
            if any(KEY_SHAPE.search(value) for value in process.environ().values()):
                found.append(f"environment of {process.name()}")
        except psutil.Error:
            pass  # not readable on every OS
    for report in reports:
        if KEY_SHAPE.search(read(report)):
            found.append(os.path.basename(report))
    return found


def runtimes(processes):
    return [p for p in processes if "sidecar" in p.name()]


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()


def main():
    out = os.path.abspath(sys.argv[1])
    mode = sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    report = os.path.join(out, f"report-{mode}.txt")
    started = time.time()
    seen = {}
    want = list(READY)
    extra = {}

    app = launch(command, report)
    if mode == "mismatch":
        want = ["HANDSHAKE_MISMATCH", "PAGE_LOADED", "UI_ERROR"]
        wait_for(report, want, app, started, seen)
    else:
        wait_for(report, READY, app, started, seen)

    second = None
    second_report = os.path.join(out, "report-duplicate-second.txt")
    if mode == "crash" and all(tag in seen for tag in READY):
        victims = runtimes(tree(app.pid))
        extra["killed_runtime_pids"] = [p.pid for p in victims]
        for victim in victims:
            victim.kill()
        want += ["RUNTIME_EXITED", "RUNTIME_RESTARTED"]
        wait_for(report, ["RUNTIME_EXITED", "RUNTIME_RESTARTED"], app, started, seen)
    elif mode == "duplicate" and all(tag in seen for tag in READY):
        second = launch(command, second_report)
        second_seen = {}
        wait_for(second_report, ["DUPLICATE_INSTANCE", "PAGE_LOADED", "UI_ERROR"], second, started, second_seen)
        extra["second_instance"] = second_seen
        extra["second_started_a_runtime"] = bool(runtimes(tree(second.pid)))
        extra["first_still_running"] = app.poll() is None
        for tag in ("DUPLICATE_INSTANCE", "UI_ERROR"):
            if tag in second_seen:
                seen[f"second:{tag}"] = second_seen[tag]
        want += ["second:DUPLICATE_INSTANCE", "second:UI_ERROR"]

    time.sleep(2)
    result = {"mode": mode, "milestones_s": seen, "missing": [t for t in want if t not in seen], **extra}
    result["screenshot"] = screenshot(os.path.join(out, f"screenshot-{mode}.png"))
    processes = tree(app.pid) + (tree(second.pid) if second else [])
    result["processes"] = sorted({p.name() for p in processes if p.is_running()})
    result["key_leaks"] = leaks_in(processes, report, second_report)
    result["report"] = read(report).splitlines()

    owned = runtimes(processes)
    if second:
        stop(second)
    stop(app)
    deadline = time.time() + 10
    while time.time() < deadline and any(p.is_running() for p in owned):
        time.sleep(0.2)
    survivors = [p for p in owned if p.is_running()]
    result["runtime_survived_host_termination"] = bool(survivors)
    for process in survivors:
        process.kill()

    with open(os.path.join(out, f"e2e-{mode}.json"), "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=1)
    print(json.dumps(result, indent=1))
    failed = bool(result["missing"] or result["key_leaks"])
    failed |= bool(survivors) and os.environ.get("E2E_REQUIRE_NO_ORPHAN") == "1"
    failed |= mode == "duplicate" and (extra.get("second_started_a_runtime") or not extra.get("first_still_running"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
