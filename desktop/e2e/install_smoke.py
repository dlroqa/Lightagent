"""Install, launch and uninstall the desktop installers on a GitHub-hosted runner (D9).

Usage: python install_smoke.py <out-dir> <label> <bundle-dir>

Each installer the runner built is used as a person would use it:

  linux    the .deb installed through apt, launched by a new user account
           (la-smoke, with its own home), and removed; the AppImage run by
           that user and deleted
  macos    the .dmg mounted, its app copied to /Applications, launched with
           a fresh home, and removed (a GUI session for a second account is
           not available on the runner)
  windows  the NSIS setup installed silently for the current user and the
           MSI for the machine, each launched and uninstalled silently (the
           app's data folder is the runner account's; Windows resolves it
           through the known-folder API, not the environment)

A launch is run.py's `ready` check against the installed binary: the
bundled runtime starts, the UI loads and makes its first API call, no launch
key shows in any process's arguments or environment, and the runtime leaves
with the app. Also recorded: each installer's SHA-256, the signature state
(unsigned is expected), and the minimum OS the binaries declare, which is a
declaration only: no older system is run. Writes installers-<label>.json and
SHA256SUMS-<label>.txt, prints a Markdown summary, and exits non-zero when
an install, a launch or an uninstall fails.
"""

import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
RUN = os.path.join(HERE, "run.py")
LINUX = sys.platform.startswith("linux")
MACOS = sys.platform == "darwin"
SMOKE_USER = "la-smoke"


def sh(args, timeout=600, **kw):
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout, **kw)
        return out.returncode, (out.stdout or "") + (out.stderr or "")
    except (OSError, subprocess.SubprocessError) as error:
        return -1, repr(error)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def ready_report(directory):
    try:
        with open(os.path.join(directory, "e2e-ready.json"), encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    return {key: data.get(key) for key in ("milestones_s", "missing", "key_leaks", "runtime_survived_host_termination")}


def judged(code, report, tail, **extra):
    ok = code == 0 and report is not None and not report["missing"] and not report["key_leaks"]
    return {"ok": ok, "exit": code, "report": report, "tail": None if ok else tail[-900:], **extra}


def launch(out, name, app, env=None):
    """run.py ready against an installed app, as this account, with a fresh
    Lightagent home (and, when given, other environment)."""
    target = os.path.join(out, f"launch-{name}")
    os.makedirs(target, exist_ok=True)
    home = tempfile.mkdtemp(prefix="la-install-home-")
    run_env = dict(os.environ, LIGHTAGENT_HOME=os.path.join(home, "lightagent"), E2E_REQUIRE_NO_ORPHAN="1")
    run_env.update(env or {})
    code, tail = sh([sys.executable, RUN, target, "ready", "--", app], timeout=300, env=run_env)
    return judged(code, ready_report(target), tail, account="runner")


def free_space_note():
    return None


# Linux ----------------------------------------------------------------------

def linux_user():
    if sh(["id", SMOKE_USER])[0] != 0:
        sh(["sudo", "useradd", "-m", "-s", "/bin/bash", SMOKE_USER])
    tools = tempfile.mkdtemp(prefix="la-smoke-tools-")
    shutil.copy(RUN, tools)
    os.chmod(tools, 0o755)
    os.chmod(os.path.join(tools, "run.py"), 0o755)
    shared = tempfile.mkdtemp(prefix="la-smoke-out-")
    os.chmod(shared, 0o777)
    return os.path.join(tools, "run.py"), shared


def as_smoke_user(out, name, app, run, shared):
    target = os.path.join(shared, name)
    home = f"/home/{SMOKE_USER}"
    code, tail = sh(["sudo", "-u", SMOKE_USER, "-H", "env", f"PATH={os.environ['PATH']}",
                     f"LIGHTAGENT_HOME={home}/.lightagent", "E2E_REQUIRE_NO_ORPHAN=1", "APPIMAGE_EXTRACT_AND_RUN=1",
                     "xvfb-run", "-a", "-s", "-screen 0 1280x800x24", "dbus-run-session", "--",
                     sys.executable, run, target, "ready", "--", app], timeout=300)
    report = ready_report(target)
    if os.path.isdir(target):
        shutil.copytree(target, os.path.join(out, f"launch-{name}"), dirs_exist_ok=True)
    return judged(code, report, tail, account=SMOKE_USER)


def glibc_needed(paths):
    found = set()
    for path in paths:
        _, text = sh(["objdump", "-T", path])
        found.update(re.findall(r"GLIBC_([0-9]+(?:\.[0-9]+)+)", text))
    return max(found, key=lambda v: tuple(int(x) for x in v.split(".")), default=None)


def linux(out, bundle, entries):
    run, shared = linux_user()
    for deb in sorted(glob.glob(os.path.join(bundle, "deb", "*.deb"))):
        package = sh(["dpkg-deb", "-f", deb, "Package"])[1].strip()
        entry = {"installer": os.path.basename(deb), "sha256": sha256(deb), "package": package,
                 "depends": sh(["dpkg-deb", "-f", deb, "Depends"])[1].strip(), "signature": "unsigned (Debian packages are not signed here)"}
        app = "/usr/bin/lightagent-desktop"
        code, tail = sh(["sudo", "apt-get", "install", "-y", os.path.abspath(deb)], timeout=900)
        entry["install"] = {"ok": code == 0 and os.path.exists(app), "tail": None if code == 0 else tail[-900:]}
        entry["installed_files"] = [line for line in sh(["dpkg", "-L", package])[1].splitlines() if "/bin/" in line]
        if entry["install"]["ok"]:
            entry["launch"] = as_smoke_user(out, "deb", app, run, shared)
            entry["min_os"] = {"glibc_needed": glibc_needed([app, "/usr/bin/lightagent-sidecar"]),
                               "declared_by": "the highest GLIBC symbol version the binaries need, and the package's Depends"}
        code, tail = sh(["sudo", "apt-get", "remove", "-y", package], timeout=600)
        gone = not os.path.exists(app) and sh(["dpkg", "-s", package])[0] != 0
        entry["uninstall"] = {"ok": code == 0 and gone, "tail": None if code == 0 else tail[-600:],
                              "user_data_left": sh(["sudo", "test", "-e", f"/home/{SMOKE_USER}/.lightagent"])[0] == 0}
        entries.append(entry)
    for image in sorted(glob.glob(os.path.join(bundle, "appimage", "*.AppImage"))):
        entry = {"installer": os.path.basename(image), "sha256": sha256(image), "signature": "unsigned"}
        place = tempfile.mkdtemp(prefix="la-appimage-")
        os.chmod(place, 0o755)
        copy = os.path.join(place, os.path.basename(image))
        shutil.copy(image, copy)
        os.chmod(copy, 0o755)
        entry["install"] = {"ok": os.access(copy, os.X_OK), "how": "copied and made executable (an AppImage needs no install)"}
        entry["launch"] = as_smoke_user(out, "appimage", copy, run, shared)
        os.remove(copy)
        entry["uninstall"] = {"ok": not os.path.exists(copy), "how": "the file deleted"}
        entries.append(entry)


# macOS ------------------------------------------------------------------------

def plist_value(app, key):
    code, text = sh(["/usr/libexec/PlistBuddy", "-c", f"Print :{key}", os.path.join(app, "Contents", "Info.plist")])
    return text.strip() if code == 0 else None


def macos(out, bundle, entries):
    for dmg in sorted(glob.glob(os.path.join(bundle, "dmg", "*.dmg"))):
        entry = {"installer": os.path.basename(dmg), "sha256": sha256(dmg)}
        mount = tempfile.mkdtemp(prefix="la-dmg-")
        for attempt in (1, 2, 3):
            code, tail = sh(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mount, dmg], timeout=300)
            if code == 0 or "Resource busy" not in tail:
                break
            entry.setdefault("attach_retries", []).append(attempt)
            time.sleep(15)
        apps = glob.glob(os.path.join(mount, "*.app"))
        target = os.path.join("/Applications", os.path.basename(apps[0])) if apps else None
        if target and os.path.exists(target):
            entry["install"] = {"ok": False, "tail": f"{target} already exists; it is not overwritten"}
            sh(["hdiutil", "detach", mount])
            entries.append(entry)
            continue
        copied = sh(["ditto", apps[0], target]) if target else (-1, "no app in the image")
        sh(["hdiutil", "detach", mount])
        binary = os.path.join(target or "", "Contents", "MacOS", "lightagent-desktop")
        entry["install"] = {"ok": code == 0 and copied[0] == 0 and os.path.exists(binary),
                            "how": "mounted, the app copied to /Applications", "tail": None if copied[0] == 0 else copied[1][-600:]}
        if entry["install"]["ok"]:
            entry["signature"] = sh(["codesign", "-dv", "--verbose=2", target])[1].strip()[-500:]
            entry["gatekeeper"] = sh(["spctl", "--assess", "--type", "execute", "-v", target])[1].strip()[-300:]
            entry["min_os"] = {"LSMinimumSystemVersion": plist_value(target, "LSMinimumSystemVersion"),
                               "declared_by": "Info.plist and each binary's minos (declared, not run on older systems)"}
            fresh = tempfile.mkdtemp(prefix="la-macos-home-")
            entry["launch"] = launch(out, "dmg", binary, env={"HOME": fresh})
            shutil.rmtree(target, ignore_errors=True)
        entry["uninstall"] = {"ok": bool(target) and not os.path.exists(target), "how": "the app removed from /Applications"}
        entries.append(entry)


# Windows ----------------------------------------------------------------------

def pe_versions(path):
    with open(path, "rb") as handle:
        data = handle.read(4096)
    header = int.from_bytes(data[0x3C:0x40], "little")
    optional = header + 24

    def word(offset):
        return int.from_bytes(data[optional + offset:optional + offset + 2], "little")

    return {"os_version": f"{word(40)}.{word(42)}", "subsystem_version": f"{word(48)}.{word(50)}"}


def authenticode(path):
    return sh(["powershell", "-NoProfile", "-Command", f"(Get-AuthenticodeSignature -LiteralPath '{path}').Status"])[1].strip()


def installed_exe():
    roots = [os.environ.get("LOCALAPPDATA", ""), os.path.join(os.environ.get("LOCALAPPDATA", ""), "Programs"),
             os.environ.get("ProgramFiles", ""), os.environ.get("ProgramFiles(x86)", "")]
    for root in filter(None, roots):
        for found in glob.glob(os.path.join(root, "Lightagent*", "lightagent-desktop.exe")):
            return found
    return None


def wait_gone(path, seconds=120):
    deadline = time.time() + seconds
    while time.time() < deadline:
        if not os.path.exists(path):
            return True
        time.sleep(1)
    return False


def windows(out, bundle, entries):
    if installed_exe():
        entries.append({"installer": "(any)", "install": {"ok": False, "tail": f"already installed at {installed_exe()}; not touched"}})
        return
    for setup in sorted(glob.glob(os.path.join(bundle, "nsis", "*-setup.exe"))):
        entry = {"installer": os.path.basename(setup), "sha256": sha256(setup), "signature": authenticode(setup)}
        code, tail = sh([setup, "/S"], timeout=900)
        exe = installed_exe()
        entry["install"] = {"ok": code == 0 and exe is not None, "where": exe, "how": "silent (/S), current user",
                            "tail": None if code == 0 else tail[-600:]}
        if exe:
            entry["min_os"] = {**pe_versions(exe), "declared_by": "the PE header (declared); WebView2 is required"}
            entry["launch"] = launch(out, "nsis", exe)
            uninstaller = os.path.join(os.path.dirname(exe), "uninstall.exe")
            code, tail = sh([uninstaller, "/S"], timeout=600) if os.path.exists(uninstaller) else (-1, "no uninstall.exe")
            entry["uninstall"] = {"ok": code == 0 and wait_gone(exe), "how": "uninstall.exe /S", "tail": None if code == 0 else tail[-600:]}
        entries.append(entry)
    for msi in sorted(glob.glob(os.path.join(bundle, "msi", "*.msi"))):
        entry = {"installer": os.path.basename(msi), "sha256": sha256(msi), "signature": authenticode(msi)}
        log = os.path.join(out, "msi-install.log")
        code, tail = sh(["msiexec", "/i", os.path.abspath(msi), "/qn", "/norestart", "/l*v", log], timeout=900)
        exe = installed_exe()
        entry["install"] = {"ok": code == 0 and exe is not None, "where": exe, "how": "msiexec /i /qn, machine", "exit": code}
        if exe:
            entry["launch"] = launch(out, "msi", exe)
            code, tail = sh(["msiexec", "/x", os.path.abspath(msi), "/qn", "/norestart", "/l*v",
                             os.path.join(out, "msi-uninstall.log")], timeout=900)
            entry["uninstall"] = {"ok": code == 0 and wait_gone(exe), "how": "msiexec /x /qn", "exit": code}
        entries.append(entry)


def main():
    out = os.path.abspath(sys.argv[1])
    label, bundle = sys.argv[2], os.path.abspath(sys.argv[3])
    os.makedirs(out, exist_ok=True)
    entries = []
    if LINUX:
        linux(out, bundle, entries)
    elif MACOS:
        macos(out, bundle, entries)
    else:
        windows(out, bundle, entries)
    with open(os.path.join(out, f"SHA256SUMS-{label}.txt"), "w", encoding="utf-8") as handle:
        for entry in entries:
            if entry.get("sha256"):
                handle.write(f"{entry['sha256']}  {entry['installer']}\n")
    failures = [f"{entry.get('installer')}: {step}" for entry in entries for step in ("install", "launch", "uninstall")
                if step in entry and not entry[step].get("ok")]
    if not entries:
        failures.append("no installer was found")
    with open(os.path.join(out, f"installers-{label}.json"), "w", encoding="utf-8") as handle:
        json.dump({"label": label, "entries": entries, "failures": failures}, handle, indent=1)
    print(f"### installers {label}")
    for entry in entries:
        steps = ", ".join(f"{step} {'ok' if entry[step].get('ok') else 'FAILED'}" for step in ("install", "launch", "uninstall") if step in entry)
        account = (entry.get("launch") or {}).get("account")
        print(f"- `{entry.get('installer')}` sha256 `{entry.get('sha256', '?')}`: {steps}"
              + (f" (launched as {account})" if account else ""))
        if entry.get("min_os"):
            print(f"  - minimum OS (declared): {json.dumps(entry['min_os'])}")
        if entry.get("signature"):
            print(f"  - signature: {str(entry['signature']).splitlines()[-1][:160] if entry['signature'] else '?'}")
    for failure in failures:
        print(f"- FAILED {failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
