"""The shipped AppImage's lifecycle, driven as a person drives it (D9 acceptance).

  python appimage_lifecycle.py <out-dir> <app.AppImage> <cli>

Run as the smoke account inside its own X display and session bus, with its
own Lightagent home, against the packaged AppImage (no test hooks). The
AppImage's runtime starts the desktop host as its child, so every check
follows the actual processes: the host, the runtime the host started, and
the MCP server a run starts; the AppImage's wrapper is checked last.

  normal_quit     the window is asked to close (WM_DELETE_WINDOW, what a
                  window manager's close button sends) with nothing working:
                  the host, its runtime and the wrapper exit
  quit_with_work  a run is working (its MCP server running, its model call
                  held): the close request leaves everything running and the
                  app asks; Tab then Enter choose "Stop runs and quit", and the
                  host, its runtime and the MCP server end
  host_crash      the host is killed (SIGKILL) while a run works: its runtime
                  follows it, and the run's MCP server does not outlive them

The message is typed into the composer with xdotool, as OS key presses. The
MCP server (stubborn_mcp.py) ignores the end of its input on purpose, so it
ends only if the runtime ends it. Writes appimage-lifecycle.json and exits 1
when a check fails.
"""

import ctypes
import ctypes.util
import json
import os
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import harness  # noqa: E402
import psutil  # noqa: E402

READY = ["RUNTIME_READY", "PAGE_LOADED", "UI_READY", "API_OK sessions.list"]
FIXTURE = os.path.join(HERE, "stubborn_mcp.py")


def sh(args, timeout=60):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return out.returncode, (out.stdout or "") + (out.stderr or "")
    except (OSError, subprocess.SubprocessError) as error:
        return -1, repr(error)


def running(pid):
    try:
        return pid is not None and psutil.Process(pid).status() != psutil.STATUS_ZOMBIE
    except psutil.Error:
        return False


def binary_of(process):
    try:
        return os.path.basename(process.exe())
    except psutil.Error:
        return ""


def below(pid, binary):
    """The first process under `pid` running `binary` (by its executable, not
    its name: the AppImage starts the host through a link)."""
    try:
        for process in psutil.Process(pid).children(recursive=True):
            if binary_of(process) == binary:
                return process.pid
    except psutil.Error:
        pass
    return None


def marked(marker):
    found = []
    for process in psutil.process_iter(["pid", "cmdline"]):
        try:
            if process.pid != os.getpid() and marker in " ".join(process.info["cmdline"] or []) and running(process.pid):
                found.append(process.pid)
        except psutil.Error:
            pass
    return found


def ended(pids, seconds):
    return bool(harness.wait_until(lambda: not [pid for pid in pids if running(pid)], timeout=seconds, every=0.25))


def app_windows():
    _, text = sh(["xdotool", "search", "--name", "^Lightagent$"])
    return [int(word) for word in text.split() if word.isdigit()]


class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p), ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]


class XEvent(ctypes.Union):
    _fields_ = [("client", ClientMessage), ("pad", ctypes.c_long * 24)]


def close_request(window):
    """WM_DELETE_WINDOW to `window`, as a window manager's close button sends it."""
    x11 = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XInternAtom.restype = ctypes.c_ulong
    x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
    x11.XFlush.argtypes = [ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = x11.XOpenDisplay(None)
    if not display:
        return False
    event = XEvent()
    event.client.type = 33  # ClientMessage
    event.client.send_event = 1
    event.client.display = display
    event.client.window = window
    event.client.message_type = x11.XInternAtom(display, b"WM_PROTOCOLS", 0)
    event.client.format = 32
    event.client.data[0] = x11.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
    event.client.data[1] = 0  # CurrentTime
    sent = x11.XSendEvent(display, window, 0, 0, ctypes.byref(event))
    x11.XFlush(display)
    x11.XCloseDisplay(display)
    return bool(sent)


def close_app():
    return [close_request(window) for window in app_windows()]


def type_message(text):
    for window in app_windows():
        sh(["xdotool", "windowfocus", str(window)])
    time.sleep(0.5)
    sh(["xdotool", "type", "--delay", "30", text], timeout=120)
    time.sleep(0.3)
    sh(["xdotool", "key", "Return"])


def screenshot(out, name):
    sh(["import", "-window", "root", os.path.join(out, f"appimage-{name}.png")], timeout=30)


class App:
    """The AppImage started as a person starts it, and its actual processes."""

    def __init__(self, out, image, case):
        self.report = os.path.join(out, f"report-{case}.txt")
        if os.path.exists(self.report):
            os.remove(self.report)
        self.log = open(os.path.join(out, f"app-{case}.log"), "w", encoding="utf-8")
        self.wrapper = subprocess.Popen([image], env=dict(os.environ, LIGHTAGENT_DESKTOP_E2E_REPORT=self.report),
                                        stdin=subprocess.DEVNULL, stdout=self.log, stderr=subprocess.STDOUT)

        def ready():
            lines = (harness.read(self.report) or "").splitlines()
            return self.wrapper.poll() is not None or all(any(line.startswith(tag) for line in lines) for tag in READY)

        self.ready = bool(harness.wait_until(ready, timeout=150)) and self.wrapper.poll() is None
        try:
            own = binary_of(psutil.Process(self.wrapper.pid))
        except psutil.Error:
            own = ""
        self.host = self.wrapper.pid if own == "lightagent-desktop" else below(self.wrapper.pid, "lightagent-desktop")
        self.runtime = below(self.host, "lightagent-sidecar") if self.host else None

    def facts(self):
        return {"ready": self.ready, "host_found": bool(self.host), "host_is_the_wrapper": self.host == self.wrapper.pid,
                "runtime_found": bool(self.runtime)}

    def end(self):
        for pid in (self.runtime, self.host):
            if running(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
        if self.wrapper.poll() is None:
            self.wrapper.kill()
        try:
            self.wrapper.wait(timeout=30)
        except subprocess.TimeoutExpired:
            pass
        self.log.close()


def wrapper_exits(app):
    return bool(harness.wait_until(lambda: app.wrapper.poll() is not None, timeout=30))


def normal_quit(app, provider, marker, check, out):
    sent = close_app()
    check.expect("the window takes a close request (WM_DELETE_WINDOW)", any(sent), sent)
    check.expect("with nothing working the host exits", ended([app.host], 30))
    check.expect("its runtime exits with it", ended([app.runtime], 15))
    check.expect("the AppImage's wrapper exits after its host", wrapper_exits(app), app.wrapper.poll())


def working(app, provider, scenario, marker, check, out):
    type_message(f"[e2e:{scenario}] Hold on, please.")
    reached = provider.wait_for("gate", scenario, timeout=90, gate=scenario)
    mcp = harness.wait_until(lambda: marked(marker), timeout=30, every=0.5) or []
    screenshot(out, f"{scenario}-working")
    check.expect("a run works: its model call is held and its MCP server runs", reached and bool(mcp),
                 {"model_call_held": reached, "mcp_servers": len(mcp)})
    return mcp


def quit_with_work(app, provider, marker, check, out):
    mcp = working(app, provider, "appimage-quit", marker, check, out)
    sent = close_app()
    time.sleep(5)
    kept = running(app.host) and running(app.runtime) and bool(mcp) and all(running(pid) for pid in mcp)
    screenshot(out, "quit-asked")
    check.expect("a close request with a run working keeps the app, its runtime and the run (it asks)",
                 any(sent) and kept, {"sent": sent, "host": running(app.host), "runtime": running(app.runtime)})
    sh(["xdotool", "key", "Tab"])
    time.sleep(0.5)
    sh(["xdotool", "key", "Return"])
    check.expect("Stop runs and quit (Tab, Enter) ends the host", ended([app.host], 45))
    check.expect("its runtime exits", ended([app.runtime], 15))
    check.expect("the run's MCP server does not outlive them", ended(mcp, 20),
                 {"left": len([pid for pid in mcp if running(pid)])})
    check.expect("the AppImage's wrapper exits", wrapper_exits(app), app.wrapper.poll())


def host_crash(app, provider, marker, check, out):
    mcp = working(app, provider, "appimage-crash", marker, check, out)
    os.kill(app.host, signal.SIGKILL)
    check.expect("the host is gone (SIGKILL)", ended([app.host], 10))
    check.expect("its runtime exits after its host", ended([app.runtime], 20))
    check.expect("the run's MCP server does not outlive its runtime", bool(mcp) and ended(mcp, 20),
                 {"left": len([pid for pid in mcp if running(pid)])})
    check.expect("the AppImage's wrapper exits", wrapper_exits(app), app.wrapper.poll())


def configure(cli, home, provider, marker):
    harness.run_cli(cli, "config", "set", "inference.base_url", provider.url)
    path = os.path.join(home, "config.json")
    with open(path, encoding="utf-8") as handle:
        config = json.load(handle)
    config.setdefault("tools", {})["enabled"] = True
    config["mcp"] = {"enabled": True, "timeout_secs": 30, "servers": [
        {"transport": "stdio", "name": "stubborn", "command": sys.executable, "args": [FIXTURE, marker]}]}
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(config, handle, indent=2)


def main():
    out, image, cli = os.path.abspath(sys.argv[1]), sys.argv[2], sys.argv[3]
    os.makedirs(out, exist_ok=True)
    home = os.environ["LIGHTAGENT_HOME"]
    provider = harness.FakeProvider()
    for scenario in ("appimage-quit", "appimage-crash"):
        provider.scripts[scenario] = lambda p, turn, index, gate=scenario: [("gate", gate), ("text", "Late."), ("finish", "stop")]
    results = {"checks": {}, "failures": []}
    for name, case in (("normal_quit", normal_quit), ("quit_with_work", quit_with_work), ("host_crash", host_crash)):
        marker = f"la-appimage-{name.replace('_', '-')}-{os.getpid()}"
        check = harness.Checks(f"appimage_{name}")
        app = None
        try:
            configure(cli, home, provider, marker)
            app = App(out, image, name)
            usable = bool(app.ready and app.host and app.runtime)
            check.expect("the app is ready: its runtime started, the UI loaded and called the API", usable, app.facts())
            if usable:
                case(app, provider, marker, check, out)
        except Exception as error:  # a broken case is a failure; the others still run
            check.expect("the case ran to the end", False, repr(error)[:300])
        finally:
            for pid in marked(marker):
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
            if app:
                app.end()
        results["checks"][name] = check.results
        results["failures"].extend(check.failures)
    provider.close()
    with open(os.path.join(out, "appimage-lifecycle.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    for name, checks in results["checks"].items():
        print(f"- {name}: " + "; ".join(f"{c} {'ok' if v['ok'] else 'FAILED'}" for c, v in checks.items()))
    for failure in results["failures"]:
        print(f"- FAILED {failure}")
    return 1 if results["failures"] else 0


if __name__ == "__main__":
    sys.exit(main())
