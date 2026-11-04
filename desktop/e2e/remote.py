"""Remote mode through the desktop host, on a GitHub-hosted runner (D8).

Usage: python remote.py <out-dir> <lightagent-cli> -- <host command...>

The host is a test build driven over its stdin (harness.Host), started with
LIGHTAGENT_API_KEY set as a person would start it. The remote is a real
`lightagent serve --key-env LIGHTAGENT_API_KEY` on loopback with its own
home; both homes point at harness.FakeProvider, so a run reveals where it
ran. Stubs stand in for a redirecting server (with a trap that records any
request reaching it), a server of another version and a server that is not
Lightagent. LIGHTAGENT_HOME, set by the caller, is this computer's home.

Checked: address rules before any connection; a wrong key, a closed port,
a redirect (the key never reaches the trap), another version, not a
Lightagent server and a cancelled key file are refused with their reason
and leave this computer's runtime in use; connected, the bundled runtime is
stopped and runs go to the remote only; the remote stopped fails requests
with its error and nothing runs here; the remote back, requests work
again; profile switching is refused; disconnect brings this computer's
runtime back. The key is in neither the bundled runtime's environment nor
any log, report or file of this computer's home. Writes remote.json.
"""

import json
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import harness
from harness import API, Checks, Stream


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def listening(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        return True
    except OSError:
        return False


def cli_in(cli, home, *args):
    env = dict(os.environ, LIGHTAGENT_HOME=home)
    out = subprocess.run([cli, *args], env=env, capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        raise RuntimeError(f"lightagent {' '.join(args)}: {out.stderr.strip()[:300]}")
    return out.stdout


class RemoteServer:
    """`lightagent serve --key-env LIGHTAGENT_API_KEY` with its own home."""

    def __init__(self, cli, home, key, port, log):
        self.args = [cli, "serve", "--host", "127.0.0.1", "--port", str(port), "--key-env", "LIGHTAGENT_API_KEY"]
        self.env = dict(os.environ, LIGHTAGENT_HOME=home, LIGHTAGENT_API_KEY=key)
        for name in ("LIGHTAGENT_DESKTOP_E2E_DRIVER", "LIGHTAGENT_DESKTOP_E2E_REPORT"):
            self.env.pop(name, None)
        self.port, self.log, self.process = port, log, None

    def start(self):
        self.process = subprocess.Popen(self.args, env=self.env, stdin=subprocess.DEVNULL,
                                        stdout=open(self.log, "a"), stderr=subprocess.STDOUT)
        if not harness.wait_until(lambda: listening(self.port), timeout=60):
            raise RuntimeError("the remote server did not start")

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        harness.wait_until(lambda: not listening(self.port), timeout=15)


def stub(answer):
    """A loopback HTTP server answering every GET with `answer(handler)`;
    it records the headers of every request it receives."""
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            seen.append({"path": self.path, "authorization": self.headers.get("Authorization")})
            code, headers, body = answer(self)
            self.send_response(code)
            for name, value in headers.items():
                self.send_header(name, value)
            data = json.dumps(body).encode() if body is not None else b""
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}", seen


def attempt(host, url, source="environment"):
    """A connection attempt: (error, outcome)."""
    try:
        return None, host.call("remote_connect", url=url, source=source, timeout=60)
    except harness.HostError as error:
        return str(error), None


def main():
    out = os.path.abspath(sys.argv[1])
    cli = sys.argv[2]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    local_home = os.environ["LIGHTAGENT_HOME"]
    remote_home = tempfile.mkdtemp(prefix="lightagent-e2e-remote-home-")
    key = "la-remote-" + secrets.token_urlsafe(24)
    report = os.path.join(out, "report-remote.txt")
    provider = harness.FakeProvider()
    provider.scripts["remote"] = lambda p, turn, index: [("text", "Answered remotely."), ("finish", "stop")]
    for home in (local_home, remote_home):
        cli_in(cli, home, "config", "set", "inference.base_url", provider.url)
    port = free_port()
    origin = f"http://127.0.0.1:{port}"
    remote = RemoteServer(cli, remote_home, key, port, os.path.join(out, "remote-server.log"))
    trap, trap_url, trapped = stub(lambda handler: (200, {}, {"service": "lightagent"}))
    redirect, redirect_url, _ = stub(lambda handler: (302, {"Location": f"{trap_url}/api/lightagent/v1/server"}, None))
    older, older_url, _ = stub(lambda handler: (200, {}, {"service": "lightagent", "version": "0.0.1", "protocol": 1}))
    other, other_url, _ = stub(lambda handler: (404, {}, {"error": "not found"}))
    check = Checks("remote")
    results = {}
    host = None
    try:
        remote.start()
        env = dict(os.environ, LIGHTAGENT_DESKTOP_E2E_DRIVER="1", LIGHTAGENT_DESKTOP_E2E_REPORT=report,
                   LIGHTAGENT_API_KEY=key)
        host = harness.Host(command, env, os.path.join(out, "driver-remote.log"))
        host.ready()
        bundled = harness.runtimes(host.process.pid)
        environments = []
        for process in bundled:
            try:
                environments.append(process.environ())
            except Exception as error:  # an unreadable environment is a failure to prove isolation
                environments.append({"<unreadable>": repr(error)})
        check.expect("the bundled runtime's environment has no LIGHTAGENT_API_KEY",
                     bundled and environments and not any("LIGHTAGENT_API_KEY" in e or "<unreadable>" in e for e in environments),
                     [sorted(e)[:5] for e in environments])
        local_sessions = len((host.fetch("GET", f"{API}/sessions")[1] or {}).get("sessions") or [])

        def still_local(label, error):
            status = host.call("status")
            check.expect(f"{label}: refused with its reason, this computer's runtime still in use",
                         error is not None and status.get("mode") == "local" and status["state"] == "ready"
                         and bool(harness.runtimes(host.process.pid)), (error, status.get("mode")))

        for address in ("http://192.0.2.10:8787", "https://user@agent.example.com", "https://agent.example.com/api",
                        "ftp://agent.example.com", "agent.example.com"):
            error, _ = attempt(host, address)
            still_local(f"the address {address}", error)
        with tempfile.TemporaryDirectory(prefix="lightagent-e2e-remote-key-") as keys:
            wrong = os.path.join(keys, "wrong.key")
            with open(wrong, "w", encoding="utf-8") as handle:
                handle.write("not-the-servers-key-123\n")
            host.call("pick_file", path=wrong)
            error, _ = attempt(host, origin, "file")
            still_local("a wrong key", error)
            check.expect("a wrong key says the server refused it", error is not None and "refused the key (401)" in error, error)
            host.call("pick_file", cancel=True)
            error, outcome = attempt(host, origin, "file")
            check.expect("a cancelled key file sends nothing", error is None and (outcome or {}).get("state") == "cancelled", (error, outcome))
            host.call("pick_file", native=True)
        error, _ = attempt(host, f"http://127.0.0.1:{free_port()}")
        still_local("a closed port", error)
        check.expect("a closed port says the connection failed", error is not None and "could not reach" in error, error)
        error, _ = attempt(host, redirect_url)
        still_local("a redirect", error)
        check.expect("a redirect is not followed and the key never reaches its target",
                     error is not None and "redirect" in error and not trapped, (error, trapped))
        error, _ = attempt(host, older_url)
        still_local("another version", error)
        check.expect("another version is named", error is not None and "runs Lightagent 0.0.1" in error, error)
        error, _ = attempt(host, other_url)
        still_local("not a Lightagent server", error)

        error, outcome = attempt(host, origin)
        status = host.call("status")
        check.expect("connected with the key from the environment, the versions equal",
                     error is None and (outcome or {}).get("state") == "connected" and status.get("mode") == "remote"
                     and status.get("remote") == origin and status.get("runtime_version") == status.get("desktop_version"),
                     (error, outcome, status))
        check.expect("the bundled runtime is stopped while remote",
                     harness.wait_until(lambda: not harness.runtimes(host.process.pid), timeout=20))
        session = host.new_session()
        run = host.start_run(session, "[e2e:remote] Hello there.")
        stream = Stream(host, run).finish()
        check.expect("a run goes to the remote server and is answered", stream.text() == "Answered remotely.", stream.text())
        check.expect("the conversation is kept by the remote, not on this computer",
                     harness.session_file(remote_home, session) is not None and harness.session_file(local_home, session) is None)

        remote.stop()
        failed = host.try_fetch("GET", f"{API}/sessions")
        refused_run = host.try_fetch("POST", f"{API}/runs", {"message": "[e2e:remote] While it is down.", "session_id": session})
        check.expect("with the remote down a request fails with its error",
                     failed[0] == "refused" and "remote server did not answer" in failed[1], failed)
        check.expect("with the remote down nothing runs on this computer",
                     refused_run[0] == "refused" and not harness.runtimes(host.process.pid)
                     and host.call("status").get("mode") == "remote", (refused_run, host.call("status").get("mode")))
        remote.start()
        back = harness.wait_until(lambda: host.try_fetch("GET", f"{API}/sessions")[0] == 200, timeout=30)
        listed = host.fetch("GET", f"{API}/sessions")[1] if back else None
        check.expect("the remote back, requests work again and its conversation is there",
                     back and session in json.dumps(listed), back)
        try:
            host.call("switch", profile="default", cancel_runs=False)
            switched = None
        except harness.HostError as error:
            switched = str(error)
        check.expect("profile switching is refused while remote", switched is not None and "remote" in switched, switched)

        host.call("remote_disconnect", timeout=90)
        host.wait_ready()
        status = host.call("status")
        local = (host.fetch("GET", f"{API}/sessions")[1] or {}).get("sessions") or []
        check.expect("disconnect brings this computer's runtime back",
                     status.get("mode") == "local" and bool(harness.runtimes(host.process.pid)), status)
        check.expect("this computer's conversations are as they were",
                     len(local) == local_sessions and session not in json.dumps(local), (local_sessions, len(local)))
    except Exception as error:
        check.failures.append(f"remote: {error!r}")
        results["error"] = repr(error)
    finally:
        if host:
            host.stop()
        remote.stop()
        provider.close()
        for server in (trap, redirect, older, other):
            server.shutdown()
    held = []
    for path in [os.path.join(out, "driver-remote.log"), report]:
        if key in (harness.read(path) or ""):
            held.append(path)
    for root, _dirs, files in os.walk(local_home):
        for name in files:
            try:
                with open(os.path.join(root, name), "rb") as handle:
                    if key.encode() in handle.read():
                        held.append(os.path.join(root, name))
            except OSError:
                pass
    check.expect("the key is in no log, report or file of this computer's home", not held, held)
    results["checks"] = check.results
    results["failures"] = check.failures
    with open(os.path.join(out, "remote.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    print(json.dumps({"failures": check.failures}, indent=1))
    return 1 if check.failures else 0


if __name__ == "__main__":
    sys.exit(main())
