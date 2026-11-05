"""The user data a Desktop install, upgrade or uninstall must leave alone (D9 acceptance).

  python user_data.py seed <cli> <baseline.json>
  python user_data.py verify <cli> <baseline.json> <key> <description> <result.json>

`seed` creates one of each kind of user data with the real CLI and runtime
(`<cli>` is the `lightagent` binary the job built), in the Lightagent home
the app uses (LIGHTAGENT_HOME, else ~/.lightagent): a second profile, a
setting, a saved conversation with an attachment, a memory, an indexed
knowledge source, a provider API key in the secrets store and a sealed
remote credential in the vault (its key file outside the home, as a person
keeps it). It records every file of the home with its SHA-256.

`verify` checks that every recorded file is still there, unchanged, and
reads each kind of data back through the CLI and a keyed `lightagent serve`,
which also shows that both protected credentials still open. It writes
`<result.json>` (no secret, no absolute path) and exits 1 on any difference.

No Lightagent Desktop release has been published, so there is no earlier
version to upgrade from. The installers' checks therefore simulate one: the
home is seeded before the data checks, and the same installer is then run
again over its own installation. That is reported as a simulated upgrade,
never as an upgrade from a release.
"""

import hashlib
import json
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import harness  # noqa: E402

API = "/api/lightagent/v1"
PROFILE = "upgrade-desk"
PERSONA = "You keep the field notes for the upgrade check."
MEMORY = "The heron nests by the north pier."
ANSWER = "Kept across the upgrade."
SOURCE = "heron-field-guide"
SERVICE = "mcp:upgrade-check"
ATTACHMENT = b"heron field notes\n\x00\xff page two\n"


def lightagent_home():
    return os.environ.get("LIGHTAGENT_HOME") or os.path.join(os.path.expanduser("~"), ".lightagent")


def manifest(root):
    """Every file of the home with its SHA-256. Lock files are left out: they
    are created and released as the app works, and hold no data."""
    files = {}
    for base, _dirs, names in os.walk(root):
        for name in names:
            if name.endswith(".lock"):
                continue
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            try:
                with open(path, "rb") as handle:
                    files[rel] = hashlib.sha256(handle.read()).hexdigest()
            except OSError as error:
                files[rel] = f"unreadable ({type(error).__name__})"
    return files


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Server:
    """A keyed `lightagent serve` on loopback, for what the CLI does not show."""

    def __init__(self, cli):
        self.key = "la-data-" + secrets.token_urlsafe(24)
        self.port = free_port()
        env = dict(os.environ, LIGHTAGENT_API_KEY=self.key)
        self.process = subprocess.Popen(
            [cli, "serve", "--host", "127.0.0.1", "--port", str(self.port), "--key-env", "LIGHTAGENT_API_KEY"],
            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if not harness.wait_until(self.listening, timeout=60) or self.process.poll() is not None:
            self.stop()
            raise RuntimeError("lightagent serve did not start")

    def listening(self):
        if self.process.poll() is not None:
            return True
        try:
            with socket.create_connection(("127.0.0.1", self.port), timeout=1):
                return True
        except OSError:
            return False

    def request(self, method, path, body=None, headers=None):
        data = body if isinstance(body, bytes) or body is None else json.dumps(body).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}{API}{path}", data=data, method=method)
        request.add_header("Authorization", f"Bearer {self.key}")
        if body is not None and not isinstance(body, bytes):
            request.add_header("Content-Type", "application/json")
        for name, value in (headers or {}).items():
            request.add_header(name, value)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            return error.code, error.read().decode("utf-8", "replace")[:300]

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()


def ids_in(value):
    if isinstance(value, dict):
        for key in ("sessions", "items", "data"):
            if isinstance(value.get(key), list):
                return ids_in(value[key])
        return []
    if isinstance(value, list):
        return [item["id"] for item in value if isinstance(item, dict) and "id" in item]
    return []


def answered(body):
    messages = (body or {}).get("messages", []) if isinstance(body, dict) else []
    return any(ANSWER in harness.content_of(m) for m in messages if isinstance(m, dict) and m.get("role") == "assistant")


def find_session(server):
    status, listing = server.request("GET", "/sessions")
    for session in ids_in(listing):
        found, body = server.request("GET", f"/sessions/{session}")
        if found == 200 and answered(body):
            return session
    raise RuntimeError(f"the seeded conversation is not listed (sessions.list {status})")


def active_profile(cli):
    listing = json.loads(harness.run_cli(cli, "--json", "profile", "list"))
    if isinstance(listing, dict):
        if isinstance(listing.get("active"), str):
            return listing["active"]
        listing = listing.get("profiles", [])
    for row in listing if isinstance(listing, list) else []:
        if isinstance(row, dict) and row.get("active"):
            return row.get("id") or row.get("name")
    return "default"


def seed(cli, baseline_path):
    root = lightagent_home()
    work = tempfile.mkdtemp(prefix="la-user-data-")
    provider = harness.FakeProvider()
    provider.scripts["upgrade"] = lambda p, turn, index: [("text", ANSWER), ("finish", "stop")]
    try:
        harness.run_cli(cli, "config", "set", "inference.base_url", provider.url)
        harness.run_cli(cli, "config", "set", "web.enabled", "true")
        harness.run_cli(cli, "profile", "create", PROFILE, "--persona", PERSONA)
        harness.run_cli_input(cli, "[e2e:upgrade] Remember the heron.\n/exit\n", "chat")
        harness.run_cli(cli, "memory", "add", MEMORY)
        source = os.path.join(work, f"{SOURCE}.md")
        with open(source, "w", encoding="utf-8") as handle:
            handle.write("# Heron field guide\n\nHerons wade in shallow water and nest in colonies.\n")
        harness.run_cli(cli, "rag", "add", source)
        key_file = os.path.join(work, "credential.key")
        harness.run_cli(cli, "remote-credential", "keygen", key_file)
        os.environ["LIGHTAGENT_CREDENTIAL_KEY_FILE"] = key_file
        secret_file = os.path.join(work, "vault-secret.txt")
        with open(secret_file, "w", encoding="utf-8") as handle:
            handle.write("la-vault-" + secrets.token_hex(12) + "\n")
        profile = active_profile(cli)
        harness.run_cli(cli, "remote-credential", "add", profile, "--service", SERVICE,
                        "--destination", "https://upgrade.e2e.invalid", "--secret-file", secret_file)
        os.remove(secret_file)
        server = Server(cli)
        try:
            session = find_session(server)
            status, uploaded = server.request("POST", f"/sessions/{session}/attachments", ATTACHMENT,
                                              {"x-lightagent-filename": "heron notes.txt",
                                               "Content-Type": "application/octet-stream"})
            if status != 200 or not isinstance(uploaded, dict):
                raise RuntimeError(f"attachment upload: {status} {uploaded}")
            status, saved = server.request("PUT", "/provider/credential", {"key": "la-provider-" + secrets.token_hex(16)})
            if status != 200:
                raise RuntimeError(f"provider key: {status} {saved}")
        finally:
            server.stop()
    finally:
        provider.close()
    attachment = os.path.relpath(uploaded["path"], root).replace(os.sep, "/")
    baseline = {"home": root, "files": manifest(root), "profile": profile, "session": session,
                "attachment": attachment, "attachment_sha256": hashlib.sha256(ATTACHMENT).hexdigest(),
                "credential_key_file": key_file}
    with open(baseline_path, "w", encoding="utf-8") as handle:
        json.dump(baseline, handle, indent=1)
    print(f"seeded {len(baseline['files'])} files: a profile, a setting, a conversation with an attachment, "
          "a memory, a knowledge source, a provider key and a vault credential")
    return 0


def verify(cli, baseline_path, key, description, result_path):
    with open(baseline_path, encoding="utf-8") as handle:
        baseline = json.load(handle)
    root = baseline["home"]
    os.environ["LIGHTAGENT_CREDENTIAL_KEY_FILE"] = baseline["credential_key_file"]
    now, before = manifest(root), baseline["files"]
    missing = sorted(set(before) - set(now))
    changed = sorted(name for name in before if name in now and now[name] != before[name])
    added = sorted(set(now) - set(before))
    checks = {}

    def check(name, probe):
        try:
            ok, seen = probe()
        except Exception as error:  # a failed read is a failed check, with why
            ok, seen = False, repr(error)[:300]
        checks[name] = {"ok": bool(ok), "seen": seen}

    def listed(args, wanted):
        return wanted in harness.run_cli(cli, "--json", *args), None

    def attachment():
        with open(os.path.join(root, baseline["attachment"]), "rb") as handle:
            return hashlib.sha256(handle.read()).hexdigest() == baseline["attachment_sha256"], None

    check("the second profile is listed", lambda: listed(["profile", "list"], PROFILE))
    check("the setting is kept", lambda: (harness.run_cli(cli, "config", "get", "web.enabled").strip() == "true", None))
    check("the memory is listed", lambda: listed(["memory", "list"], MEMORY))
    check("the knowledge source is listed", lambda: listed(["rag", "list"], SOURCE))
    check("the vault credential is listed", lambda: listed(["remote-credential", "list"], SERVICE))
    check("the attachment is byte for byte the same", attachment)
    try:
        server = Server(cli)
    except RuntimeError as error:
        server = None
        checks["the runtime starts on the home"] = {"ok": False, "seen": str(error)}
    if server:
        try:
            def conversation():
                status, body = server.request("GET", f"/sessions/{baseline['session']}")
                return status == 200 and answered(body), status

            def provider_key():
                status, body = server.request("GET", "/provider/credential")
                state = body.get("state") if isinstance(body, dict) else None
                return status == 200 and state == "ready", state

            def vault_opens():
                status = json.loads(harness.run_cli(cli, "--json", "remote-credential", "status"))
                seen = {k: status.get(k) for k in ("vault", "key_configured", "grants", "unopenable_with_configured_key")}
                return (status.get("key_configured") is True and (status.get("grants") or {}).get("active") == 1
                        and status.get("unopenable_with_configured_key") == 0), seen

            check("the conversation and its answer are kept", conversation)
            check("the provider key is still usable", provider_key)
            check("the vault still opens with its key", vault_opens)
        finally:
            server.stop()
    ok = not missing and not changed and all(c["ok"] for c in checks.values())
    result = {"key": key, "description": description, "ok": ok, "files": len(before),
              "missing": missing[:20], "changed": changed[:20], "added": added[:20], "checks": checks}
    with open(result_path, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=1)
    print(json.dumps({"key": key, "ok": ok, "missing": len(missing), "changed": len(changed),
                      "failed": {name: c["seen"] for name, c in checks.items() if not c["ok"]}}))
    return 0 if ok else 1


def main():
    if sys.argv[1] == "seed":
        return seed(sys.argv[2], sys.argv[3])
    return verify(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6])


if __name__ == "__main__":
    sys.exit(main())
