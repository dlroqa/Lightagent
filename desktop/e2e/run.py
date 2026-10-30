"""Unattended desktop end-to-end check on a GitHub-hosted runner.

Usage: python run.py <out-dir> <ready|mismatch> -- <app command...>

Launches the packaged app with LIGHTAGENT_DESKTOP_E2E_REPORT set, waits for
the host's milestones, screenshots the window, and checks that no 256-bit
hex value (the shape of the launch key) appears in any process command line
or environment of the app's tree, or in the report. Then it stops the host
the way a crash would (terminate) and records whether the runtime outlived
it (with E2E_REQUIRE_NO_ORPHAN=1, a surviving runtime fails the check).
Writes e2e.json; exits non-zero when an expectation is not met.
"""

import json
import os
import re
import subprocess
import sys
import time

import psutil

EXPECT = {
    "ready": ["RUNTIME_READY", "PAGE_LOADED", "UI_READY", "API_OK sessions.list"],
    "mismatch": ["HANDSHAKE_MISMATCH", "PAGE_LOADED", "UI_ERROR"],
}
KEY_SHAPE = re.compile(r"\b[0-9a-f]{64}\b")


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


def main():
    out = os.path.abspath(sys.argv[1])
    mode = sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    report = os.path.join(out, f"report-{mode}.txt")
    if os.path.exists(report):
        os.remove(report)
    env = dict(os.environ, LIGHTAGENT_DESKTOP_E2E_REPORT=report)
    started = time.time()
    app = subprocess.Popen(command, env=env)
    want = EXPECT[mode]
    seen = {}
    while time.time() - started < 90 and app.poll() is None:
        text = open(report, encoding="utf-8").read() if os.path.exists(report) else ""
        for tag in want:
            if tag not in seen and any(line.startswith(tag) for line in text.splitlines()):
                seen[tag] = round(time.time() - started, 3)
        if len(seen) == len(want):
            break
        time.sleep(0.1)
    time.sleep(2)
    result = {"mode": mode, "milestones_s": seen, "missing": [t for t in want if t not in seen]}
    result["screenshot"] = screenshot(os.path.join(out, f"screenshot-{mode}.png"))

    processes = tree(app.pid)
    result["processes"] = [p.name() for p in processes if p.is_running()]
    leaks = []
    for process in processes:
        try:
            if KEY_SHAPE.search(" ".join(process.cmdline())):
                leaks.append(f"argv of {process.name()}")
        except psutil.Error:
            pass
        try:
            if any(KEY_SHAPE.search(value) for value in process.environ().values()):
                leaks.append(f"environment of {process.name()}")
        except psutil.Error:
            pass  # not readable on every OS; recorded below
    report_text = open(report, encoding="utf-8").read() if os.path.exists(report) else ""
    if KEY_SHAPE.search(report_text):
        leaks.append("e2e report")
    result["key_leaks"] = leaks
    result["report"] = report_text.splitlines()

    sidecars = [p for p in processes if "sidecar" in p.name()]
    app.terminate()
    try:
        app.wait(timeout=15)
    except subprocess.TimeoutExpired:
        app.kill()
    # The runtime's lifeline closes with the host; give it time to finish.
    deadline = time.time() + 10
    while time.time() < deadline and any(p.is_running() for p in sidecars):
        time.sleep(0.2)
    survivors = [p for p in sidecars if p.is_running()]
    result["runtime_survived_host_termination"] = bool(survivors)
    for process in survivors:
        process.kill()

    with open(os.path.join(out, f"e2e-{mode}.json"), "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=1)
    print(json.dumps(result, indent=1))
    orphan = survivors and os.environ.get("E2E_REQUIRE_NO_ORPHAN") == "1"
    return 1 if result["missing"] or leaks or orphan else 0


if __name__ == "__main__":
    sys.exit(main())
