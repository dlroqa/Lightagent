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
declaration only: no older system is run.

User data (user_data.py): the account's own Lightagent home is seeded with
one of each kind of data (a profile, a setting, a conversation with an
attachment, a memory, a knowledge source, a provider key and a sealed vault
credential). The installed app is launched on it, the same installer is run
again over its installation and the app launched again, and the app is
uninstalled; after each step every seeded file must be unchanged and each
kind of data must read back, the credentials included. No Desktop release
exists, so "after the reinstall" is a simulated upgrade, reported as such.

Notices: THIRD_PARTY_NOTICES.txt and LICENSE.txt must be in each installed
app, identical to the ones the job generated and bundled.

On Linux the shipped AppImage's lifecycle is driven as well
(appimage_lifecycle.py): its actual host process is closed and killed, not
only the AppImage's wrapper.

Writes installers-<label>.json and SHA256SUMS-<label>.txt, prints a Markdown
summary, and exits non-zero when a step fails.
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
DATA = os.path.join(HERE, "user_data.py")
LINUX = sys.platform.startswith("linux")
MACOS = sys.platform == "darwin"
SMOKE_USER = "la-smoke"
EXT = ".exe" if os.name == "nt" else ""
# The real runtime this job built: the CLI that seeds and reads back the data.
CLI = os.path.join(os.path.dirname(os.path.dirname(HERE)), "target", "release", "lightagent" + EXT)
# What scripts/desktop-sbom.py --notices generated and the bundle carries.
NOTICES = os.path.join(os.path.dirname(HERE), "src-tauri", "notices")
NOTICE_FILES = ("THIRD_PARTY_NOTICES.txt", "LICENSE.txt")
STEPS = ("install", "launch", "notices", "data_seed", "data_after_launch", "data_after_reinstall", "uninstall",
         "data_after_uninstall", "lifecycle", "data_after_appimage")


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


def own_home_env():
    env = dict(os.environ)
    env.pop("LIGHTAGENT_HOME", None)
    return env


def local_launch(out, name, app):
    """run.py ready against the installed app with the account's own home (the seeded data)."""
    target = os.path.join(out, f"launch-{name}")
    os.makedirs(target, exist_ok=True)
    env = dict(own_home_env(), E2E_REQUIRE_NO_ORPHAN="1")
    code, tail = sh([sys.executable, RUN, target, "ready", "--", app], timeout=300, env=env)
    return judged(code, ready_report(target), tail, account="runner", home="the account's own, with the seeded data")


def user_data(action, key, description, baseline, prefix=(), script=DATA, cli=CLI, env=None):
    """user_data.py seed or verify; the step's result, with `ok`."""
    result_path = f"{baseline}.{key}.json"
    args = [*prefix, sys.executable, script, action, cli, baseline]
    if action == "verify":
        args += [key, description, result_path]
    code, tail = sh(args, timeout=900, env=env)
    if action == "seed":
        result = {"ok": code == 0, "description": description}
    else:
        try:
            with open(result_path, encoding="utf-8") as handle:
                result = json.load(handle)
        except (OSError, ValueError):
            result = {"description": description}
        result["ok"] = bool(result.get("ok")) and code == 0
    if not result["ok"]:
        result["tail"] = tail[-900:]
    return result


def local_data(action, key, description, baseline):
    return user_data(action, key, description, baseline, env=own_home_env())


def with_steps(result, **steps):
    result["steps"] = steps
    result["ok"] = bool(result.get("ok")) and all(steps.values())
    return result


def data_cycle(out, entry, name, app, reinstall, how, baseline=None):
    """Seed the account's own home (unless `baseline` names an earlier seed),
    launch the installed app on it, run the installer again over its
    installation and launch again, checking the data after each. Returns the
    baseline once seeded."""
    if baseline is None:
        if os.path.exists(os.path.join(os.path.expanduser("~"), ".lightagent")):
            entry["data_seed"] = {"ok": False, "seen": "this account already has a Lightagent home; it is left untouched"}
            return None
        baseline = os.path.join(tempfile.mkdtemp(prefix="la-user-data-"), "baseline.json")
        entry["data_seed"] = local_data("seed", "seed", "seeded in the account's own home (simulated earlier data)", baseline)
        if not entry["data_seed"]["ok"]:
            return None
    launched = local_launch(out, f"{name}-existing-data", app)
    entry["data_after_launch"] = with_steps(local_data("verify", f"{name}-after-launch", "after the installed app ran on it", baseline),
                                            launch=launched["ok"])
    code, tail = reinstall()
    relaunched = local_launch(out, f"{name}-reinstalled", app) if code == 0 else {"ok": False}
    entry["data_after_reinstall"] = with_steps(local_data("verify", f"{name}-after-reinstall", how, baseline),
                                               reinstall=code == 0, launch=relaunched["ok"])
    if code != 0:
        entry["data_after_reinstall"]["reinstall_tail"] = tail[-600:]
    return baseline


def notices_check(paths):
    """The packaged notices: in the installed app, and identical to the generated ones."""
    beside = {os.path.dirname(p) for p in paths if os.path.basename(p) == NOTICE_FILES[0]}
    seen = {}
    for name in NOTICE_FILES:
        candidates = [p for p in paths if os.path.basename(p) == name and os.path.isfile(p)]
        found = next((p for p in candidates if os.path.dirname(p) in beside), candidates[0] if candidates else None)
        generated = os.path.join(NOTICES, name)
        seen[name] = {"packaged": found is not None,
                      "same_as_generated": bool(found) and os.path.isfile(generated) and sha256(found) == sha256(generated)}
    return {"ok": all(v["packaged"] and v["same_as_generated"] for v in seen.values()), **seen}


# Linux ----------------------------------------------------------------------

def linux_user():
    if sh(["id", SMOKE_USER])[0] != 0:
        sh(["sudo", "useradd", "-m", "-s", "/bin/bash", SMOKE_USER])
    tools = tempfile.mkdtemp(prefix="la-smoke-tools-")
    for name in ("run.py", "harness.py", "user_data.py", "appimage_lifecycle.py", "stubborn_mcp.py"):
        shutil.copy(os.path.join(HERE, name), tools)
        os.chmod(os.path.join(tools, name), 0o755)
    shutil.copy(CLI, os.path.join(tools, "lightagent"))
    os.chmod(os.path.join(tools, "lightagent"), 0o755)
    os.chmod(tools, 0o755)
    shared = tempfile.mkdtemp(prefix="la-smoke-out-")
    os.chmod(shared, 0o777)
    return tools, shared


def as_smoke(*extra):
    home = f"/home/{SMOKE_USER}"
    return ["sudo", "-u", SMOKE_USER, "-H", "env", f"PATH={os.environ['PATH']}", f"LIGHTAGENT_HOME={home}/.lightagent", *extra]


def as_smoke_user(out, name, app, tools, shared, orphan_check=True):
    target = os.path.join(shared, name)
    code, tail = sh(as_smoke(f"E2E_REQUIRE_NO_ORPHAN={1 if orphan_check else 0}", "APPIMAGE_EXTRACT_AND_RUN=1",
                             "xvfb-run", "-a", "-s", "-screen 0 1280x800x24", "dbus-run-session", "--",
                             sys.executable, os.path.join(tools, "run.py"), target, "ready", "--", app), timeout=300)
    report = ready_report(target)
    if os.path.isdir(target):
        shutil.copytree(target, os.path.join(out, f"launch-{name}"), dirs_exist_ok=True)
    return judged(code, report, tail, account=SMOKE_USER)


def smoke_data(action, key, description, tools, shared):
    return user_data(action, key, description, os.path.join(shared, "user-data-baseline.json"), prefix=as_smoke(),
                     script=os.path.join(tools, "user_data.py"), cli=os.path.join(tools, "lightagent"))


def appimage_files(image):
    place = tempfile.mkdtemp(prefix="la-appimage-contents-")
    sh([image, "--appimage-extract"], timeout=300, cwd=place)
    root = os.path.join(place, "squashfs-root")
    return [os.path.join(base, name) for base, _dirs, names in os.walk(root) for name in names if name in NOTICE_FILES]


def appimage_lifecycle(out, image, tools, shared):
    target = os.path.join(shared, "appimage-lifecycle")
    os.makedirs(target, exist_ok=True)
    os.chmod(target, 0o777)
    code, tail = sh(["sudo", "-u", SMOKE_USER, "-H", "env", f"PATH={os.environ['PATH']}",
                     f"LIGHTAGENT_HOME=/home/{SMOKE_USER}/lifecycle-home", "APPIMAGE_EXTRACT_AND_RUN=1",
                     "xvfb-run", "-a", "-s", "-screen 0 1280x800x24", "dbus-run-session", "--",
                     sys.executable, os.path.join(tools, "appimage_lifecycle.py"), target, image,
                     os.path.join(tools, "lightagent")], timeout=1200)
    try:
        with open(os.path.join(target, "appimage-lifecycle.json"), encoding="utf-8") as handle:
            report = json.load(handle)
    except (OSError, ValueError):
        report = {}
    shutil.copytree(target, os.path.join(out, "appimage-lifecycle"), dirs_exist_ok=True)
    ok = code == 0 and bool(report.get("checks")) and not report.get("failures")
    return {"ok": ok, "checks": report.get("checks"), "failures": report.get("failures"), "tail": None if ok else tail[-1200:]}


def glibc_needed(paths):
    found = set()
    for path in paths:
        _, text = sh(["objdump", "-T", path])
        found.update(re.findall(r"GLIBC_([0-9]+(?:\.[0-9]+)+)", text))
    return max(found, key=lambda v: tuple(int(x) for x in v.split(".")), default=None)


def linux(out, bundle, entries):
    tools, shared = linux_user()
    seeded = False
    for deb in sorted(glob.glob(os.path.join(bundle, "deb", "*.deb"))):
        package = sh(["dpkg-deb", "-f", deb, "Package"])[1].strip()
        entry = {"installer": os.path.basename(deb), "sha256": sha256(deb), "package": package,
                 "depends": sh(["dpkg-deb", "-f", deb, "Depends"])[1].strip(), "signature": "unsigned (Debian packages are not signed here)"}
        app = "/usr/bin/lightagent-desktop"
        code, tail = sh(["sudo", "apt-get", "install", "-y", os.path.abspath(deb)], timeout=900)
        entry["install"] = {"ok": code == 0 and os.path.exists(app), "tail": None if code == 0 else tail[-900:]}
        listed = sh(["dpkg", "-L", package])[1].splitlines()
        entry["installed_files"] = [line for line in listed if "/bin/" in line]
        if entry["install"]["ok"]:
            entry["launch"] = as_smoke_user(out, "deb", app, tools, shared)
            entry["min_os"] = {"glibc_needed": glibc_needed([app, "/usr/bin/lightagent-sidecar"]),
                               "declared_by": "the highest GLIBC symbol version the binaries need, and the package's Depends"}
            entry["notices"] = notices_check(listed)
            entry["data_seed"] = smoke_data("seed", "seed", "seeded in la-smoke's own home (simulated earlier data)", tools, shared)
            seeded = entry["data_seed"]["ok"]
            if seeded:
                launched = as_smoke_user(out, "deb-existing-data", app, tools, shared)
                entry["data_after_launch"] = with_steps(
                    smoke_data("verify", "after-launch", "after the installed app ran on it", tools, shared), launch=launched["ok"])
                code, tail = sh(["sudo", "apt-get", "install", "--reinstall", "-y", os.path.abspath(deb)], timeout=900)
                relaunched = as_smoke_user(out, "deb-reinstalled", app, tools, shared) if code == 0 else {"ok": False}
                entry["data_after_reinstall"] = with_steps(
                    smoke_data("verify", "after-reinstall", "simulated upgrade: apt-get install --reinstall over the installed package, then a launch",
                               tools, shared), reinstall=code == 0, launch=relaunched["ok"])
        code, tail = sh(["sudo", "apt-get", "remove", "-y", package], timeout=600)
        gone = not os.path.exists(app) and sh(["dpkg", "-s", package])[0] != 0
        entry["uninstall"] = {"ok": code == 0 and gone, "tail": None if code == 0 else tail[-600:],
                              "user_data_left": sh(["sudo", "test", "-e", f"/home/{SMOKE_USER}/.lightagent"])[0] == 0}
        if seeded:
            entry["data_after_uninstall"] = smoke_data("verify", "after-uninstall", "after apt-get remove", tools, shared)
        entries.append(entry)
    for image in sorted(glob.glob(os.path.join(bundle, "appimage", "*.AppImage"))):
        entry = {"installer": os.path.basename(image), "sha256": sha256(image), "signature": "unsigned"}
        place = tempfile.mkdtemp(prefix="la-appimage-")
        os.chmod(place, 0o755)
        copy = os.path.join(place, os.path.basename(image))
        shutil.copy(image, copy)
        os.chmod(copy, 0o755)
        entry["install"] = {"ok": os.access(copy, os.X_OK), "how": "copied and made executable (an AppImage needs no install)"}
        # run.py ends the process it started: here the AppImage's wrapper,
        # not the desktop host. The lifecycle below follows the host itself.
        entry["launch"] = as_smoke_user(out, "appimage", copy, tools, shared, orphan_check=False)
        entry["launch"]["wrapper_ended_host_kept"] = (entry["launch"].get("report") or {}).get("runtime_survived_host_termination")
        sh(["sudo", "pkill", "-u", SMOKE_USER, "-f", "lightagent"])
        time.sleep(2)
        entry["launch"]["left_running_after_cleanup"] = sh(["pgrep", "-u", SMOKE_USER, "-f", "lightagent"])[0] == 0
        entry["notices"] = notices_check(appimage_files(copy))
        entry["lifecycle"] = appimage_lifecycle(out, copy, tools, shared)
        os.remove(copy)
        entry["uninstall"] = {"ok": not os.path.exists(copy), "how": "the file deleted"}
        if seeded:
            entry["data_after_appimage"] = smoke_data("verify", "after-appimage", "after the AppImage ran on it and was deleted", tools, shared)
        entries.append(entry)


# macOS ------------------------------------------------------------------------

def plist_value(app, key):
    code, text = sh(["/usr/libexec/PlistBuddy", "-c", f"Print :{key}", os.path.join(app, "Contents", "Info.plist")])
    return text.strip() if code == 0 else None


def macos(out, bundle, entries):
    baseline = None
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
            entry["notices"] = notices_check([os.path.join(target, "Contents", "Resources", name) for name in NOTICE_FILES])

            def replace_app():
                shutil.rmtree(target, ignore_errors=True)
                return sh(["ditto", apps[0], target])

            baseline = data_cycle(out, entry, "dmg", binary, replace_app,
                                  "simulated upgrade: the app replaced from the image (as Finder's Replace does), then a launch", baseline)
            shutil.rmtree(target, ignore_errors=True)
        sh(["hdiutil", "detach", mount])
        entry["uninstall"] = {"ok": bool(target) and not os.path.exists(target), "how": "the app removed from /Applications"}
        if baseline:
            entry["data_after_uninstall"] = local_data("verify", "dmg-after-uninstall", "after the app was removed from /Applications", baseline)
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


def app_data_folders():
    """Whether the WebView's own data folders (named by the bundle identifier) are there."""
    return {var: os.path.isdir(os.path.join(os.environ[var], "dev.lightagent.desktop"))
            for var in ("APPDATA", "LOCALAPPDATA") if os.environ.get(var)}


def windows(out, bundle, entries):
    if installed_exe():
        entries.append({"installer": "(any)", "install": {"ok": False, "tail": f"already installed at {installed_exe()}; not touched"}})
        return
    baseline = None
    for setup in sorted(glob.glob(os.path.join(bundle, "nsis", "*-setup.exe"))):
        entry = {"installer": os.path.basename(setup), "sha256": sha256(setup), "signature": authenticode(setup)}
        code, tail = sh([setup, "/S"], timeout=900)
        exe = installed_exe()
        entry["install"] = {"ok": code == 0 and exe is not None, "where": exe, "how": "silent (/S), current user",
                            "tail": None if code == 0 else tail[-600:]}
        if exe:
            entry["min_os"] = {**pe_versions(exe), "declared_by": "the PE header (declared); WebView2 is required"}
            entry["launch"] = launch(out, "nsis", exe)
            entry["notices"] = notices_check([os.path.join(os.path.dirname(exe), name) for name in NOTICE_FILES])
            baseline = data_cycle(out, entry, "nsis", exe, lambda: sh([setup, "/S"], timeout=900),
                                  "simulated upgrade: the setup run again (/S) over its installation, then a launch", baseline)
            uninstaller = os.path.join(os.path.dirname(exe), "uninstall.exe")
            code, tail = sh([uninstaller, "/S"], timeout=600) if os.path.exists(uninstaller) else (-1, "no uninstall.exe")
            entry["uninstall"] = {"ok": code == 0 and wait_gone(exe), "how": "uninstall.exe /S", "tail": None if code == 0 else tail[-600:]}
            entry["app_data_after_uninstall"] = app_data_folders()
            if baseline:
                entry["data_after_uninstall"] = local_data("verify", "nsis-after-uninstall", "after uninstall.exe /S", baseline)
        entries.append(entry)
    for msi in sorted(glob.glob(os.path.join(bundle, "msi", "*.msi"))):
        entry = {"installer": os.path.basename(msi), "sha256": sha256(msi), "signature": authenticode(msi)}
        log = os.path.join(out, "msi-install.log")
        code, tail = sh(["msiexec", "/i", os.path.abspath(msi), "/qn", "/norestart", "/l*v", log], timeout=900)
        exe = installed_exe()
        entry["install"] = {"ok": code == 0 and exe is not None, "where": exe, "how": "msiexec /i /qn, machine", "exit": code}
        if exe:
            entry["launch"] = launch(out, "msi", exe)
            entry["notices"] = notices_check([os.path.join(os.path.dirname(exe), name) for name in NOTICE_FILES])
            repair_log = os.path.join(out, "msi-reinstall.log")
            baseline = data_cycle(out, entry, "msi", exe,
                                  lambda: sh(["msiexec", "/fvomus", os.path.abspath(msi), "/qn", "/norestart", "/l*v", repair_log], timeout=900),
                                  "simulated upgrade: msiexec /fvomus (every file reinstalled over the installation), then a launch", baseline)
            code, tail = sh(["msiexec", "/x", os.path.abspath(msi), "/qn", "/norestart", "/l*v",
                             os.path.join(out, "msi-uninstall.log")], timeout=900)
            entry["uninstall"] = {"ok": code == 0 and wait_gone(exe), "how": "msiexec /x /qn", "exit": code}
            entry["app_data_after_uninstall"] = app_data_folders()
            if baseline:
                entry["data_after_uninstall"] = local_data("verify", "msi-after-uninstall", "after msiexec /x", baseline)
        entries.append(entry)


def one_line(text, limit=600):
    return " / ".join(line.strip() for line in str(text).splitlines() if line.strip())[-limit:]


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
    failures = [f"{entry.get('installer')}: {step}" for entry in entries for step in STEPS
                if step in entry and not entry[step].get("ok")]
    if not entries:
        failures.append("no installer was found")
    with open(os.path.join(out, f"installers-{label}.json"), "w", encoding="utf-8") as handle:
        json.dump({"label": label, "entries": entries, "failures": failures}, handle, indent=1)
    print(f"### installers {label}")
    for entry in entries:
        steps = ", ".join(f"{step} {'ok' if entry[step].get('ok') else 'FAILED'}" for step in STEPS if step in entry)
        account = (entry.get("launch") or {}).get("account")
        print(f"- `{entry.get('installer')}` sha256 `{entry.get('sha256', '?')}`: {steps}"
              + (f" (launched as {account})" if account else ""))
        if entry.get("min_os"):
            print(f"  - minimum OS (declared): {json.dumps(entry['min_os'])}")
        if entry.get("signature"):
            print(f"  - signature: {str(entry['signature']).splitlines()[-1][:160] if entry['signature'] else '?'}")
        if entry.get("notices"):
            print(f"  - packaged notices: {json.dumps(entry['notices'])}")
        for step in STEPS:
            result = entry.get(step)
            if not step.startswith("data_after") or not result:
                continue
            checks = ", ".join(f"{name} {'ok' if c.get('ok') else 'FAILED'}" for name, c in (result.get("checks") or {}).items())
            print(f"  - {step} ({result.get('description', '')}): {result.get('files', '?')} seeded files; "
                  f"missing {result.get('missing')}, changed {result.get('changed')}, added {len(result.get('added') or [])}; "
                  f"{checks}" + (f"; steps {json.dumps(result['steps'])}" if result.get("steps") else ""))
        if entry.get("lifecycle"):
            for name, checks in (entry["lifecycle"].get("checks") or {}).items():
                print(f"  - AppImage lifecycle {name}: " + "; ".join(f"{c} {'ok' if v.get('ok') else 'FAILED'}" for c, v in checks.items()))
        if entry.get("app_data_after_uninstall") is not None:
            print(f"  - the WebView's own data folders (dev.lightagent.desktop) after the uninstall: {json.dumps(entry['app_data_after_uninstall'])}")
        for step in STEPS:
            result = entry.get(step) or {}
            if step in entry and not result.get("ok"):
                print(f"  - {step} FAILED: {one_line(result.get('tail') or result.get('seen') or result.get('failures') or '')}")
    for failure in failures:
        print(f"- FAILED {failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
