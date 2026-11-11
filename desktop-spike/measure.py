"""D1 spike measurement on a GitHub-hosted runner (runner measurements only).

Usage: python measure.py <out-dir> -- <command...>

Launches the command with SPIKE_REPORT and SPIKE_EXIT_AFTER_SECS set, waits for
the app's SPIKE_PAGE_LOADED line, lets it idle for ten seconds, then sums the
resident memory of its process tree plus webview helper processes started
after launch (WebKit XPC services on macOS are not children), takes a
screenshot, and writes measure.json.
"""

import json
import os
import platform
import subprocess
import sys
import time

import psutil

HELPERS = ("WebKit", "msedgewebview2", "WebKitWebProcess", "WebKitNetworkProcess")


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
    except Exception as error:  # evidence is best-effort; the result says so
        print(f"screenshot failed: {error}")
        return False


def main():
    out = os.path.abspath(sys.argv[1])
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    report = os.path.join(out, "app-report.txt")
    if os.path.exists(report):
        os.remove(report)
    env = dict(os.environ, SPIKE_REPORT=report, SPIKE_EXIT_AFTER_SECS="45")
    started = time.time()
    process = subprocess.Popen(command, env=env)
    loaded = None
    while time.time() - started < 120:
        if os.path.exists(report) and "SPIKE_PAGE_LOADED" in open(report, encoding="utf-8").read():
            loaded = round(time.time() - started, 3)
            break
        if process.poll() is not None:
            break
        time.sleep(0.1)
    result = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "command": os.path.basename(command[0]),
        "page_loaded_wall_s": loaded,
        "exit_before_load": process.poll(),
    }
    if loaded is not None:
        time.sleep(10)
        processes = []
        try:
            root = psutil.Process(process.pid)
            processes = [root] + root.children(recursive=True)
        except psutil.Error:
            pass
        seen = {p.pid for p in processes}
        for candidate in psutil.process_iter(["name", "create_time"]):
            try:
                name = candidate.info["name"] or ""
                if candidate.pid not in seen and candidate.info["create_time"] >= started - 1 and any(h in name for h in HELPERS):
                    processes.append(candidate)
                    seen.add(candidate.pid)
            except psutil.Error:
                pass
        total = 0
        rows = []
        for p in processes:
            try:
                rss = p.memory_info().rss
                total += rss
                rows.append([p.name(), round(rss / 1048576, 1)])
            except psutil.Error:
                pass
        result["idle_rss_mb_after_10s"] = round(total / 1048576, 1)
        result["processes"] = rows
        result["screenshot"] = screenshot(os.path.join(out, "screenshot.png"))
    if os.path.exists(report):
        result["app_report"] = open(report, encoding="utf-8").read().splitlines()
    try:
        process.terminate()
        process.wait(timeout=15)
    except Exception:
        process.kill()
    with open(os.path.join(out, "measure.json"), "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=1)
    print(json.dumps(result, indent=1))
    return 0 if loaded is not None else 1


if __name__ == "__main__":
    sys.exit(main())
