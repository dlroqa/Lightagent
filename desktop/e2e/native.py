"""Native WebView verification on a GitHub-hosted runner (D8).

Usage: python native.py <out-dir> <lightagent-cli> <full|offline> -- <host command...>
       python native.py --summary <out-dir>

The host is a test build (`--features e2e-hooks`); its window is the real
WebView of the target: WKWebView on macOS, WebView2 on Windows, WebKitGTK
on Linux. Scripts run in that page through the driver's `eval` command
(desktop/src-tauri/src/e2e_driver.rs) with the helpers in native-prelude.js:
they find controls by role and accessible name and click, type and choose
through the page's own event handlers, as the rendered UI receives them.
Screenshots are captures of the screen. The OS clipboard is read with the
platform's own tool. Keyboard checks send real key presses through the OS
where the runner allows it (xdotool; System Events; SendKeys) and say
exactly what happened where it does not.

  full     streaming, approvals, Stop, Continue and a paused run recovered
           after a reload, every screen (with an in-page axe scan when
           E2E_AXE names axe.min.js) plus Memory and MCP forms, response
           actions (R2-R5), keyboard, a narrow window, a conversation-scoped
           profile, the profile switch, closing with a working run, crash
           recovery, and quitting with a working run
  offline  startup without internet: the bundled UI, local conversations and
           settings work; a model call and an MCP check against unreachable
           services show errors. With E2E_NETNS=1 the run is inside a network
           namespace that has only loopback (Linux), which is checked first

Gated expectations fail the run. Platform facts (what the WebView offers,
whether OS input reached the window) are recorded, never silently skipped.
Writes native-<mode>.json.
"""

import glob
import json
import os
import platform
import re
import socket
import subprocess
import sys
import time

import harness
import runs
from harness import API, Checks

HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(HERE, "native-prelude.js"), encoding="utf-8") as _handle:
    PRELUDE = _handle.read()
LINUX = sys.platform.startswith("linux")
MACOS = sys.platform == "darwin"
WINDOWS = os.name == "nt"
OFFLINE_URL = "http://lightagent-offline.invalid/v1"
ANSWER = "Native answer streamed."
SENTINEL = "lightagent-native-sentinel"
PERSONA = "You answer as the native research desk."
BUNDLED_ORIGINS = ("tauri://localhost", "http://tauri.localhost", "https://tauri.localhost")
SCREENS = [
    ("Tools", "#/tools", ["Enabled tools", "Extensions", "MCP servers"]),
    ("Memory", "#/memory", ["Remember something", "Memories"]),
    ("Knowledge", "#/knowledge", ["Add documents", "Indexed sources"]),
    ("Identity", "#/admin/identity", ["Identity store", "Users"]),
    ("Remote credentials", "#/admin/credentials", ["Vault", "Add a credential"]),
    ("Diagnostics", "#/diagnostics", ["Doctor", "Architecture"]),
    ("Setup", "#/setup", ["Initialise or reconfigure", "Import from Hermes"]),
    ("Profiles", "#/profiles", ["Saved profiles", "Create a profile"]),
    ("Settings", "#/settings", ["Agent runtime", "Subagents"]),
]


def T(template, **values):
    """A script with $NAME replaced by the JSON of each value."""
    for name, value in values.items():
        template = template.replace("$" + name, json.dumps(value))
    return template


def screenshot(path):
    try:
        if MACOS:
            subprocess.run(["screencapture", "-x", path], check=True, timeout=30)
        elif LINUX:
            subprocess.run(["import", "-window", "root", path], check=True, timeout=30)
        else:
            from PIL import ImageGrab

            ImageGrab.grab().save(path)
        return True
    except Exception as error:
        print(f"screenshot failed: {error}")
        return False


def clipboard():
    """The OS clipboard's text, read with the platform's own tool."""
    try:
        if MACOS:
            out = subprocess.run(["pbpaste"], capture_output=True, text=True, timeout=10)
        elif LINUX:
            out = subprocess.run(["xclip", "-o", "-selection", "clipboard"], capture_output=True, text=True, timeout=10)
        else:
            out = subprocess.run(["powershell", "-NoProfile", "-Command", "Get-Clipboard -Raw"],
                                 capture_output=True, text=True, timeout=30)
        if out.returncode != 0:
            return f"<unreadable: {out.stderr.strip()[:200]}>"
        return out.stdout.rstrip("\r\n")
    except Exception as error:
        return f"<unreadable: {error}>"


def set_clipboard(value):
    try:
        if MACOS:
            subprocess.run(["pbcopy"], input=value, text=True, timeout=10, check=True)
        elif LINUX:
            # xclip keeps serving the selection from the background until replaced.
            subprocess.Popen(["xclip", "-selection", "clipboard"], stdin=subprocess.PIPE, text=True).communicate(value, timeout=10)
        else:
            subprocess.run(["powershell", "-NoProfile", "-Command", f"Set-Clipboard -Value '{value}'"], timeout=30, check=True)
    except Exception as error:
        print(f"could not set the clipboard: {error}")


KEYS = {
    "Tab": ("Tab", 48, "{TAB}"),
    "Return": ("Return", 36, "{ENTER}"),
    "Escape": ("Escape", 53, "{ESC}"),
    "Right": ("Right", 124, "{RIGHT}"),
    "Left": ("Left", 123, "{LEFT}"),
}


def os_keys(keys):
    """Press `keys` through the OS. None when sent; otherwise why not."""
    try:
        if LINUX:
            for name in keys:
                subprocess.run(["xdotool", "key", "--clearmodifiers", KEYS[name][0]], check=True, timeout=10)
        elif MACOS:
            for name in keys:
                out = subprocess.run(["osascript", "-e", f'tell application "System Events" to key code {KEYS[name][1]}'],
                                     capture_output=True, text=True, timeout=30)
                if out.returncode != 0:
                    return f"System Events refused the key press: {out.stderr.strip()[:240]}"
        else:
            sends = "; ".join(f"[System.Windows.Forms.SendKeys]::SendWait('{KEYS[name][2]}')" for name in keys)
            out = subprocess.run(["powershell", "-NoProfile", "-Command", f"Add-Type -AssemblyName System.Windows.Forms; {sends}"],
                                 capture_output=True, text=True, timeout=60)
            if out.returncode != 0:
                return f"SendKeys failed: {out.stderr.strip()[:240]}"
        return None
    except Exception as error:
        return f"{type(error).__name__}: {error}"


class View:
    """The rendered page of the host's main window."""

    def __init__(self, host, out, mode):
        self.host, self.out, self.mode = host, out, mode
        self.shots = []
        self.axe_source = None
        axe = os.environ.get("E2E_AXE")
        if axe and os.path.isfile(axe):
            with open(axe, encoding="utf-8") as handle:
                self.axe_source = handle.read()

    def js(self, body, timeout=30):
        return self.host.call("eval", timeout=timeout + 15, script=PRELUDE + "\n" + body, timeout_ms=int(timeout * 1000))

    def try_js(self, body, timeout=30):
        try:
            return True, self.js(body, timeout)
        except harness.HostError as error:
            return False, str(error)

    def wait_ui(self, timeout=120):
        """Until the page answers and shows the app's shell."""
        deadline, last = time.time() + timeout, None
        while time.time() < deadline:
            ok, value = self.try_js('return document.readyState === "complete" && !!one("button", "Open account menu");', 5)
            if ok and value:
                return True
            last = value
            time.sleep(0.5)
        raise harness.HostError(f"the page did not show the app ({last})")

    def reload(self):
        self.js("setTimeout(() => location.reload(), 50); return true;")
        time.sleep(1.5)
        self.wait_ui()

    def shot(self, name):
        path = os.path.join(self.out, f"native-{self.mode}-{name}.png")
        if screenshot(path):
            self.shots.append(os.path.basename(path))

    def go(self, item, route):
        """Open a screen from the account menu, as a person does. The chat
        screen's menu lists fewer screens; one it lacks is reached from
        Settings, whose menu lists them all."""
        opened = self.js(T("""
          press(await need("the account menu", () => one("button", "Open account menu")));
          const entry = await until(() => all("menuitem").find((el) => nameOf(el) === $ITEM), 3000);
          if (!entry) { key(document.activeElement || document.body, "Escape"); await sleep(300); return false; }
          press(entry);
          await need("the route", () => location.hash.split("?")[0] === $ROUTE);
          return true;""", ITEM=item, ROUTE=route))
        if not opened:
            if item == "Settings":
                raise harness.HostError("Settings is not in the account menu")
            self.go("Settings", "#/settings")
            self.go(item, route)

    def new_chat(self):
        self.js("""
          if (!location.hash.startsWith("#/") || location.hash.split("?")[0] !== "#/") location.hash = "#/";
          press(await need("New chat", () => one("button", "New chat")));
          await need("the composer", () => one("textbox", "Ask Lightagent"));
          return true;""")

    def send(self, message):
        """Type and send `message`, and confirm it reached the conversation;
        a send pressed before the composer was live is tried once more."""
        for attempt in (1, 2):
            reached = self.js(T("""
              const box = await need("the composer", () => all("textbox", "Ask Lightagent").find((el) => !el.disabled));
              fill(box, $MESSAGE);
              const button = await need("Send", () => all("button").filter((el) => /^(Send message|Queue steer)$/.test(nameOf(el)) && !el.disabled).pop());
              press(button);
              return !!(await until(() => [...document.querySelectorAll('[aria-label="Your message"]')]
                .some((el) => el.textContent.includes($MESSAGE)), 10000));""", MESSAGE=message), 40)
            if reached:
                return
            self.shot(f"send-not-shown-{attempt}")
        raise harness.HostError(f"the message never reached the conversation; the page ends: {self.js('return text().slice(-400);')!r}")

    def wait_text(self, wanted, seconds=60):
        return bool(self.js(T("return !!(await until(() => text().includes($WANTED), $MS));", WANTED=wanted, MS=seconds * 1000), seconds + 10))

    def idle(self, seconds=60):
        return bool(self.js(T('return !!(await until(() => !one("button", "Stop"), $MS));', MS=seconds * 1000), seconds + 10))

    def axe(self):
        """The page's axe violations, or None when axe-core is not available."""
        if not self.axe_source:
            return None
        if not self.js("return !!window.axe;"):
            self.host.call("eval", timeout=90, script=self.axe_source + "\nreturn !!window.axe;", timeout_ms=60000)
        return self.js("""
          const result = await axe.run(document, { resultTypes: ["violations"] });
          return result.violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help,
            nodes: v.nodes.slice(0, 4).map((n) => n.target.join(" ")) }));""", 90)


def focus_window(host):
    try:
        host.call("focus")
    except harness.HostError as error:
        return f"focus: {error}"
    try:
        if LINUX:
            found = subprocess.run(["xdotool", "search", "--name", "^Lightagent$"], capture_output=True, text=True, timeout=10)
            for window in found.stdout.split():
                subprocess.run(["xdotool", "windowfocus", window], capture_output=True, timeout=10)
        elif MACOS:
            subprocess.run(["osascript", "-e", 'tell application "System Events" to set frontmost of '
                            f"(first process whose unix id is {host.process.pid}) to true"], capture_output=True, timeout=30)
        else:
            subprocess.run(["powershell", "-NoProfile", "-Command",
                            f"(New-Object -ComObject WScript.Shell).AppActivate({host.process.pid}) | Out-Null"],
                           capture_output=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as error:
        return f"bringing the window to the front: {error}"
    time.sleep(0.5)
    return None


def saved_with(home, marker):
    """The saved conversation whose user message holds `marker`."""
    for root, _dirs, files in os.walk(home):
        if "sessions" not in root:
            continue
        for name in files:
            if not name.endswith(".json"):
                continue
            try:
                with open(os.path.join(root, name), encoding="utf-8") as handle:
                    data = json.load(handle)
            except (OSError, ValueError):
                continue
            messages = data.get("messages") if isinstance(data, dict) else None
            if isinstance(messages, list) and any(
                    isinstance(m, dict) and m.get("role") == "user" and marker in harness.content_of(m) for m in messages):
                return data
    return None


def last_stop(saved):
    runs_ = (saved or {}).get("runs") or []
    return runs_[-1].get("stop_reason") if runs_ else None


def scripts(provider):
    runs.scripts(provider)

    def hold(gate, rest):
        return lambda p, turn, index: [("text", "Holding"), ("gate", gate), ("text", rest), ("finish", "stop")]

    provider.scripts.update({
        "native-chat": lambda p, turn, index: [("text", "Native "), ("gate", "native-stream"), ("text", "answer "),
                                               ("text", "streamed."), ("finish", "stop")],
        "native-stop": hold("native-stop", " never."),
        "native-close": hold("native-close", " and kept working."),
        "native-reload": hold("native-reload", " after the reload."),
        "native-quit": hold("native-quit", " never."),
        "remote": lambda p, turn, index: [("text", "Answered remotely."), ("finish", "stop")],
    })


# Scenarios (full) ------------------------------------------------------------

def scenario_chat(view, provider, home, notes):
    check = Checks("chat")
    view.new_chat()
    view.send("[e2e:native-chat] Say something.")
    partial = view.js("""return !!(await until(() => text().includes("Native") && !text().includes("answer streamed.")
      && !!one("button", "Stop"), 30000));""", 40)
    view.shot("streaming")
    provider.open("native-stream")
    whole = view.wait_text(ANSWER)
    ended = view.idle()
    view.shot("answer")
    saved = saved_with(home, "[e2e:native-chat]")
    check.expect("a partial answer is shown while the run streams (Stop shown)", partial)
    check.expect("the whole answer is shown", whole)
    check.expect("the run ends in the UI", ended)
    check.expect("the answer is saved", (harness.answers(saved) or [""])[-1].endswith(ANSWER), harness.answers(saved))
    return check


def scenario_actions(view, host, notes):
    """R2-R5 on the answer scenario_chat left on screen."""
    check = Checks("response_actions")
    set_clipboard(SENTINEL)
    before = clipboard()
    view.js('press(await need("Copy response", () => one("button", "Copy response", lastBar()))); return true;')
    notice = view.js("return await until(() => noticeOf(lastBar()) || null, 5000);")
    after = harness.wait_until(lambda: clipboard() == ANSWER, timeout=10, every=0.5) and ANSWER or clipboard()
    check.expect("R2: the OS clipboard held the sentinel first", before == SENTINEL, before)
    check.expect("R2: Copy response puts the answer on the OS clipboard", after == ANSWER, {"clipboard": after, "notice": notice})
    time.sleep(2.6)
    speech = view.js("""
      const bar = lastBar();
      const button = one("button", "Read aloud", bar);
      const info = { api: "speechSynthesis" in window, unavailable: button.getAttribute("aria-disabled") === "true" };
      info.voices = info.api ? speechSynthesis.getVoices().length : 0;
      press(button);
      await sleep(700);
      info.notice = noticeOf(bar);
      info.pressed = button.getAttribute("aria-pressed");
      if (info.pressed === "true") { press(button); await sleep(300); info.stopped = button.getAttribute("aria-pressed") !== "true"; }
      return info;""")
    notes["read_aloud"] = speech
    consistent = (speech["api"] and "not supported" not in speech["notice"]) or (
        not speech["api"] and speech["unavailable"] and "not supported" in speech["notice"])
    check.expect("R3: Read aloud shows the state that matches this WebView's speech support", consistent, speech)
    time.sleep(2.6)
    set_clipboard(SENTINEL)
    share = view.js("""
      const bar = lastBar();
      const info = { api: typeof navigator.share === "function" };
      press(one("button", "Share", bar));
      await sleep(1500);
      info.notice = noticeOf(bar);
      return info;""")
    view.shot("share")
    if share["api"]:
        share["dismissed"] = os_keys(["Escape"]) or "Escape sent through the OS"
        check.expect("R4: with a share sheet, Share opens it without an error", "Could not share" not in share["notice"], share)
    else:
        share["clipboard"] = harness.wait_until(lambda: clipboard() == ANSWER, timeout=10, every=0.5) and ANSWER or clipboard()
        check.expect("R4: without a share sheet, Share copies the answer and says so",
                     "copied" in share["notice"] and share["clipboard"] == ANSWER, share)
    notes["share"] = share
    keys = view.js("""
      const bar = lastBar();
      const buttons = [...bar.querySelectorAll("[data-response-action]")];
      buttons[0].focus();
      const seen = [nameOf(document.activeElement)];
      for (const name of ["ArrowRight", "ArrowRight", "End", "Home", "ArrowLeft"]) { key(document.activeElement, name); seen.push(nameOf(document.activeElement)); }
      return seen;""")
    check.expect("R5: the row's arrow, Home and End keys move between its buttons",
                 keys == ["Copy response", "Rate response", "Read aloud", "More actions", "Copy response", "More actions"], keys)
    why = focus_window(host)
    view.js('one("button", "Copy response", lastBar()).focus(); return true;')
    why = why or os_keys(["Right"])
    time.sleep(0.4)
    moved = view.js("return nameOf(document.activeElement);")
    notes["toolbar_os_arrow"] = {"os_input": why or "sent", "focus_after": moved}
    if LINUX:
        check.expect("R5: an OS Right arrow moves from Copy to Rate", why is None and moved == "Rate response", notes["toolbar_os_arrow"])
    violations = view.axe()
    if violations is None:
        check.expect("R5: axe-core is available for the scan", False, os.environ.get("E2E_AXE"))
    else:
        check.expect("R5: no critical axe violations on the conversation with its action row",
                     not [v for v in violations if v["impact"] == "critical"], violations)
        notes["axe_conversation"] = violations
    return check


def scenario_approvals(view, provider, workspace):
    check = Checks("approvals")
    for approve in (True, False):
        name = "approve" if approve else "deny"
        path = os.path.join(workspace, "approved.txt" if approve else "denied.txt")
        view.new_chat()
        view.send(f"[e2e:{name}] Write the file.")
        card = view.js('return !!(await until(() => one("alertdialog", "Approve fs.write"), 30000));', 40)
        before = harness.read(path)
        view.shot(f"approval-{name}")
        view.js(T('press(await need($BUTTON, () => one("button", $BUTTON, one("alertdialog", "Approve fs.write")))); return true;',
                  BUTTON="Allow once" if approve else "Deny"))
        answered = view.wait_text("Wrote it." if approve else "Not written.")
        ended = view.idle()
        after = harness.read(path)
        check.expect(f"{name}: the approval card is shown and nothing ran before the decision", card and before is None, before)
        check.expect(f"{name}: the model's next turn is shown", answered and ended)
        if approve:
            check.expect("approve: the write ran exactly once", after == "granted\n", after)
        else:
            check.expect("deny: the write never ran", after is None, after)
    return check


def scenario_stop(view, provider, home):
    check = Checks("stop")
    view.new_chat()
    view.send("[e2e:native-stop] Work on something long.")
    held = provider.wait_for("gate", "native-stop", gate="native-stop")
    stop = view.js('return !!(await until(() => one("button", "Stop"), 15000));')
    view.js('press(await need("Stop", () => one("button", "Stop"))); return true;')
    ended = view.idle(30)
    saved = harness.wait_until(lambda: last_stop(saved_with(home, "[e2e:native-stop]")) == "Cancelled", timeout=20)
    provider.open("native-stop")
    check.expect("Stop is shown while the run works", held and stop)
    check.expect("Stop ends the run in the UI", ended)
    check.expect("the saved run says cancelled", saved, last_stop(saved_with(home, "[e2e:native-stop]")))
    return check


def scenario_continue(view, host, home):
    check = Checks("continue")
    status, _ = host.fetch("PUT", f"{API}/config/agent.wall_clock_secs", {"value": "1"})
    check.expect("a one-second time budget is set", status == 200, status)
    card = 'one("alertdialog", "Run paused on its time budget")'
    try:
        view.new_chat()
        view.send("[e2e:continue-extend] Look it up.")
        paused = view.js(f"return !!(await until(() => {card}, 30000));", 40)
        view.shot("paused")
        view.js(f'press(await need("Continue", () => one("button", "Continue", {card}))); return true;')
        found = view.wait_text("Found it.")
        check.expect("Continue: the paused card is shown and the continued run answers", paused and found and view.idle())
        prompt = "[e2e:continue-answer] Look it up."
        view.new_chat()
        view.send(prompt)
        paused = view.js(f"return !!(await until(() => {card}, 30000));", 40)
        view.reload()
        view.js(T("""
          // A title is the first message without its closing full stop.
          const title = $PROMPT.replace(/[.]+$/, "");
          const entry = await need("the conversation in the list", () => [...document.querySelectorAll(".agent-sessions__list *")]
            .find((el) => visible(el) && el.children.length === 0 && squash(el.textContent).replace(/[.…]+$/, "") === title));
          press(entry);
          return true;""", PROMPT=prompt))
        again = view.js(f"return !!(await until(() => {card}, 30000));", 40)
        view.shot("paused-after-reload")
        view.js(f'press(await need("Answer now", () => one("button", "Answer now", {card}))); return true;')
        ended = view.idle()
        check.expect("a paused run is found again after a reload", paused and again, (paused, again))
        check.expect("Answer now ends the recovered run", ended)
    finally:
        host.fetch("PUT", f"{API}/config/agent.wall_clock_secs", {"value": ""})
    return check


def scenario_screens(view, notes):
    check = Checks("screens")
    for item, route, cards in SCREENS:
        slug = re.sub(r"[^a-z0-9]+", "-", item.lower())
        try:
            view.go(item, route)
            missing = view.js(T("""
              await until(() => $CARDS.every((c) => text().includes(c)), 15000);
              return $CARDS.filter((c) => !text().includes(c));""", CARDS=cards), 25)
            alerts = view.js('return all("alert").map((el) => nameOf(el).slice(0, 200));')
            view.shot(f"screen-{slug}")
            check.expect(f"{item}: opens from the account menu and shows its sections", not missing, missing)
            notes.setdefault("screen_alerts", {})[item] = alerts
            violations = view.axe()
            if violations is None:
                check.expect(f"{item}: axe-core is available for the scan", False, os.environ.get("E2E_AXE"))
            else:
                notes.setdefault("axe", {})[item] = violations
                check.expect(f"{item}: no critical axe violations", not [v for v in violations if v["impact"] == "critical"], violations)
        except harness.HostError as error:
            check.expect(f"{item}: opens from the account menu", False, str(error))
            view.shot(f"failed-screen-{slug}")
    return check


def scenario_forms(view, home):
    """Memory and MCP forms, filled and submitted in the native page."""
    check = Checks("forms")
    fact = "Native probe: herons nest in colonies."
    view.go("Memory", "#/memory")
    view.js(T("""
      fill(await need("the memory text", () => document.querySelector("#memory-text")), $FACT);
      press(await need("Remember", () => one("button", "Remember")));
      return true;""", FACT=fact))
    check.expect("Memory: Remember saves the fact and lists it", view.wait_text(fact, 30))
    outcome = add_and_check_mcp(view, "native-offline", "http://lightagent-offline.invalid/mcp")
    check.expect("MCP: an HTTP server added from the typed form is listed", outcome["listed"], outcome)
    check.expect("MCP: checking an unreachable server shows why it failed", outcome["failed_shown"], outcome)
    return check


def add_and_check_mcp(view, name, url):
    view.go("Tools", "#/tools")
    view.js(T("""
      press(await need("HTTP endpoint", () => one("radio", /^HTTP endpoint/)));
      fill(await need("the server name", () => document.querySelector("#mcp-name")), $NAME);
      fill(await need("the server URL", () => document.querySelector("#mcp-url")), $URL);
      press(await need("Add server", () => one("button", "Add server")));
      return true;""", NAME=name, URL=url))
    listed = bool(view.js(T('return !!(await until(() => one("button", "Check " + $NAME), 15000));', NAME=name), 25))
    result = {"listed": listed, "failed_shown": False, "shown": None}
    if not listed:
        return result
    shown = view.js(T("""
      const row = () => all("listitem").find((el) => el.textContent.includes($NAME) && one("button", "Check " + $NAME, el));
      press(one("button", "Check " + $NAME, row()));
      const confirm = await until(() => all("button").find((el) => nameOf(el).startsWith("Confirm: ") && nameOf(el).includes($NAME)), 3000);
      if (confirm) press(confirm);
      return await until(() => {
        const r = row();
        const said = r && [...all("alert", undefined, r), ...all("status", undefined, r)].map((el) => nameOf(el)).filter(Boolean).join(" | ");
        return said && !/checking/i.test(said) ? said : null;
      }, 60000);""", NAME=name), 75)
    result["shown"] = shown
    result["failed_shown"] = bool(shown) and not shown.startswith("Connected")
    view.shot(f"mcp-{name}")
    return result


def scenario_keyboard(view, host, notes):
    check = Checks("keyboard")
    view.new_chat()
    why = focus_window(host)
    start = view.js("""
      const box = await need("the composer", () => one("textbox", "Ask Lightagent"));
      box.focus();
      return { focused: describe(document.activeElement), page_has_focus: document.hasFocus() };""")
    path = [start["focused"]]
    for _ in range(6):
        why = why or os_keys(["Tab"])
        if why:
            break
        time.sleep(0.4)
        path.append(view.js("return describe(document.activeElement);"))
    ring = view.js("""
      const el = document.activeElement; const s = getComputedStyle(el);
      return { element: describe(el), focus_visible: el.matches(":focus-visible"), outline: s.outlineStyle + " " + s.outlineWidth, shadow: s.boxShadow };""")
    moved = len(set(path)) > 1
    notes["keyboard"] = {"os_input": why or "sent", "page_had_focus": start["page_has_focus"], "tab_path": path, "focus": ring}
    if LINUX:
        check.expect("OS Tab moves focus through the window's controls", why is None and moved, path)
        check.expect("the focused control shows a visible focus indicator", ring["focus_visible"] and (
            ring["outline"].split()[0] != "none" or ring["shadow"] != "none"), ring)
    else:
        notes["keyboard"]["outcome"] = (
            "OS key presses reached the window and moved focus" if why is None and moved
            else "OS key presses were sent but focus did not move (the window did not receive them)" if why is None
            else f"OS key presses unavailable on this runner: {why}")
    return check


def type_path_into_picker(path):
    """Answer the open native file picker with `path` through OS input.
    None when the keys were sent; otherwise why not."""
    time.sleep(2.0)
    try:
        if LINUX:
            subprocess.run(["xdotool", "key", "--clearmodifiers", "ctrl+l"], check=True, timeout=10)
            time.sleep(0.5)
            subprocess.run(["xdotool", "type", "--delay", "20", path], check=True, timeout=60)
            time.sleep(0.5)
            subprocess.run(["xdotool", "key", "Return"], check=True, timeout=10)
        elif MACOS:
            script = ('tell application "System Events"\n keystroke "g" using {command down, shift down}\n delay 1\n'
                      f' keystroke "{path}"\n delay 0.5\n key code 36\n delay 1\n key code 36\nend tell')
            out = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=60)
            if out.returncode != 0:
                return f"System Events refused the key presses: {out.stderr.strip()[:240]}"
        else:
            escaped = "".join("{" + ch + "}" if ch in "+^%~(){}[]" else ch for ch in path)
            sends = f"[System.Windows.Forms.SendKeys]::SendWait('{escaped}'); Start-Sleep -Milliseconds 500; [System.Windows.Forms.SendKeys]::SendWait('{{ENTER}}')"
            out = subprocess.run(["powershell", "-NoProfile", "-Command", f"Add-Type -AssemblyName System.Windows.Forms; {sends}"],
                                 capture_output=True, text=True, timeout=60)
            if out.returncode != 0:
                return f"SendKeys failed: {out.stderr.strip()[:240]}"
        return None
    except Exception as error:
        return f"{type(error).__name__}: {error}"


def scenario_secret_input(view, host, home, notes):
    """The `--secret-stdin` rows in the rendered window: the Add form's
    host-read button stores a seeded secret the page never holds; a cancel
    and an unusable file say so; then the real native picker, answered
    through OS input where the runner allows it."""
    import secrets
    import tempfile

    check = Checks("secret_input")
    work = tempfile.mkdtemp(prefix="lightagent-native-secret-")
    seeds = ["LA-NATIVE-SEED-" + secrets.token_urlsafe(15), "LA-NATIVE-PICKER-" + secrets.token_urlsafe(15)]
    paths = []
    for index, seed in enumerate(seeds):
        paths.append(os.path.join(work, f"seed-{index}.secret"))
        with open(paths[-1], "w", encoding="utf-8") as handle:
            handle.write(seed + "\n")
    lines = os.path.join(work, "lines.secret")
    with open(lines, "w", encoding="utf-8") as handle:
        handle.write("LA-NATIVE-LINE-one\nLA-NATIVE-LINE-two\n")
    active = host.call("status")["profile"]
    view.go("Remote credentials", "#/admin/credentials")

    def submit(service, wanted):
        return view.js(T("""
          fill(await need("profile", () => document.querySelector("#rc-add-profile")), $PROFILE);
          fill(await need("service", () => document.querySelector("#rc-add-service")), $SERVICE);
          fill(await need("destinations", () => document.querySelector("#rc-add-destinations")), "https://docs.native.invalid");
          await sleep(200);
          press(await need("the host-read button", () => all("button").find((el) => nameOf(el) === "Add with a secret file chosen here…" && !el.disabled)));
          return await until(() => {
            const said = [...all("status"), ...all("alert")].map((el) => nameOf(el)).filter((s) => $WANTED.test(s));
            return said.length ? said.join(" | ") : null;
          }, 90000);""", PROFILE=active, SERVICE=service).replace('"$WANTED"', wanted).replace("$WANTED", wanted), 100)

    def page_dump():
        return view.js("""return document.documentElement.outerHTML + JSON.stringify(Object.entries(localStorage))
          + JSON.stringify(Object.entries(sessionStorage)) + document.body.innerText;""")

    host.call("pick_file", path=paths[0])
    stored = submit("mcp:native-docs", "/^Added /")
    view.shot("secret-stored")
    dump = page_dump()
    check.expect("the host-read button stores the secret and says where it came from", bool(stored) and "seed-0.secret" in stored, stored)
    check.expect("the page never holds the seeded secret (DOM, storage, text)", seeds[0] not in dump)
    host.call("pick_file", cancel=True)
    cancelled = submit("mcp:native-cancel", "/nothing was stored/")
    check.expect("a cancel says nothing was stored", bool(cancelled) and "No file was chosen" in cancelled, cancelled)
    host.call("pick_file", path=lines)
    invalid = submit("mcp:native-lines", "/cannot be used/")
    check.expect("an unusable file says why, near the form", bool(invalid) and "one line" in invalid and "LA-NATIVE-LINE" not in invalid, invalid)
    # The real picker: the page asks, the OS dialog opens, OS input answers it.
    host.call("pick_file", native=True)
    focus_window(host)
    view.js(T("""
      fill(document.querySelector("#rc-add-service"), "mcp:native-picker");
      await sleep(200);
      press(all("button").find((el) => nameOf(el) === "Add with a secret file chosen here…"));
      return true;""", ))
    time.sleep(1.0)
    view.shot("secret-native-picker")
    why = type_path_into_picker(paths[1])
    picked = view.js("""return await until(() => {
        const said = [...all("status"), ...all("alert")].map((el) => nameOf(el)).filter((s) => /seed-1\\.secret|No file was chosen|cannot be used/.test(s));
        return said.length ? said.join(" | ") : null;
      }, 45000);""", 60)
    view.shot("secret-native-done")
    if not picked:
        os_keys(["Escape"])
    leaked = seeds[1] in page_dump()
    notes["native_picker"] = {"os_input": why or "sent", "outcome": picked}
    if LINUX:
        check.expect("the real GTK picker, answered through OS input, stores the chosen file", why is None and bool(picked) and "seed-1.secret" in picked, notes["native_picker"])
    check.expect("the page never holds the secret read through the real picker", not leaked)
    check.expect("the chosen files are left as they were", all(harness.read(p) == s + "\n" for p, s in zip(paths, seeds)))
    check.seeds = seeds
    return check


def scenario_narrow(view, host):
    check = Checks("narrow")
    try:
        host.call("resize", width=900, height=600)
        time.sleep(1.2)
        layout = view.js("""return { width: innerWidth, height: innerHeight,
          overflow: document.documentElement.scrollWidth - innerWidth, composer: !!one("textbox", "Ask Lightagent") || !!one("button", "New chat") };""")
        view.shot("narrow")
        check.expect("at the minimum window size the page has no horizontal overflow", layout["overflow"] <= 1, layout)
        check.expect("at the minimum window size the chat controls are reachable", layout["composer"], layout)
    finally:
        host.call("resize", width=1280, height=820)
        time.sleep(0.8)
    return check


def scenario_conversation_profile(view, host, provider, cli):
    check = Checks("conversation_profile")
    active = json.loads(harness.run_cli(cli, "--json", "profile", "list")).get("active")
    runtime = host.call("status")["profile"]
    view.js('location.hash = "#/"; return true;')
    view.reload()
    chosen = view.js("""
      const select = await need("the conversation profile picker", () => document.querySelector("#conversation-profile"));
      fill(select, "native-b");
      await sleep(400);
      return { value: select.value, hint: text().includes("These chats run as") };""")
    view.new_chat()
    before = len(provider.requests("profile"))
    view.send("[e2e:profile] Where do herons nest?")
    answered = view.wait_text("Noted.") and view.idle()
    sent = provider.requests("profile")[before:]
    view.shot("conversation-profile")
    check.expect("the picker chooses native-b and says chats run as it", chosen["value"] == "native-b" and chosen["hint"], chosen)
    check.expect("the conversation runs as native-b (its persona reaches the model)",
                 answered and len(sent) == 1 and PERSONA in (sent[0].get("system") or ""), sent[-1:] and sent[-1].get("system", "")[:300])
    after = json.loads(harness.run_cli(cli, "--json", "profile", "list")).get("active")
    check.expect("the persisted active profile is unchanged", after == active, (active, after))
    check.expect("the runtime's profile is unchanged", host.call("status")["profile"] == runtime)
    view.js(T('fill(await need("the picker", () => document.querySelector("#conversation-profile")), $ID); return true;', ID=active))
    return check


REMOTE_KEY = "la-native-remote-" + __import__("secrets").token_urlsafe(18)


def scenario_remote(view, host, provider, cli):
    """Remote mode in the rendered window: the Settings card refuses a plain
    http address elsewhere and says why; connects to a keyed server on this
    machine (its own home), after which the scope badge names it, the
    bundled runtime is stopped and a conversation runs there; Disconnect
    returns to this computer's runtime and the badge goes."""
    import tempfile

    check = Checks("remote_mode")
    remote_home = tempfile.mkdtemp(prefix="lightagent-native-remote-")
    env = dict(os.environ, LIGHTAGENT_HOME=remote_home, LIGHTAGENT_API_KEY=REMOTE_KEY)
    for name in ("LIGHTAGENT_DESKTOP_E2E_DRIVER", "LIGHTAGENT_DESKTOP_E2E_REPORT"):
        env.pop(name, None)
    subprocess.run([cli, "config", "set", "inference.base_url", provider.url], env=env, check=True,
                   capture_output=True, timeout=60)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    server = subprocess.Popen([cli, "serve", "--host", "127.0.0.1", "--port", str(port), "--key-env", "LIGHTAGENT_API_KEY"],
                              env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    origin = f"http://127.0.0.1:{port}"

    def up():
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return True
        except OSError:
            return False

    try:
        harness.wait_until(up, timeout=60)
        view.go("Settings", "#/settings")
        refused = view.js("""
          fill(await need("the server address", () => document.querySelector("#remote-url")), "http://192.0.2.10:8787");
          press(await need("Connect", () => one("button", "Connect")));
          return await until(() => all("alert").map((el) => nameOf(el)).find((s) => /only for this computer/.test(s)) || null, 20000);""", 30)
        check.expect("a plain-http address elsewhere is refused before anything is sent, and the card says why", bool(refused), refused)
        view.js(T('fill(document.querySelector("#remote-url"), $ORIGIN); await sleep(200); press(one("button", "Connect")); return true;', ORIGIN=origin))
        connected = harness.wait_until(lambda: host.call("status").get("mode") == "remote", timeout=60)
        time.sleep(1.5)
        view.wait_ui()
        badge = view.js('return await until(() => all("status").map((el) => nameOf(el)).find((s) => s.startsWith("Remote ·")) || null, 15000);')
        view.shot("remote-connected")
        check.expect("Connect serves the window from the remote and the scope badge names it",
                     bool(connected) and f"127.0.0.1:{port}" in (badge or ""), (host.call("status").get("remote"), badge))
        check.expect("the bundled runtime is stopped while remote",
                     harness.wait_until(lambda: not harness.runtimes(host.process.pid), timeout=20))
        view.new_chat()
        view.send("[e2e:remote] Hello from the window.")
        answered = view.wait_text("Answered remotely.") and view.idle()
        check.expect("a conversation in the window runs on the remote server",
                     answered and saved_with(remote_home, "[e2e:remote]") is not None)
        check.expect("nothing of it is kept on this computer", saved_with(os.environ["LIGHTAGENT_HOME"], "[e2e:remote]") is None)
        view.go("Settings", "#/settings")
        view.js('press(await need("Disconnect", () => one("button", "Disconnect and use this computer"))); return true;')
        back = harness.wait_until(lambda: (lambda s: s.get("mode") == "local" and s["state"] == "ready")(host.call("status")), timeout=90)
        time.sleep(1.5)
        view.wait_ui()
        gone = view.js('return !all("status").some((el) => nameOf(el).startsWith("Remote ·"));')
        view.shot("remote-disconnected")
        check.expect("Disconnect returns to this computer's runtime and the badge goes", bool(back) and gone, host.call("status").get("mode"))
    finally:
        server.terminate()
        try:
            server.wait(timeout=15)
        except subprocess.TimeoutExpired:
            server.kill()
    check.seeds = [REMOTE_KEY]
    return check


def scenario_switch(view, host):
    check = Checks("profile_switch")
    original = host.call("status")["profile"]
    for target in ("native-b", original):
        view.go("Profiles", "#/profiles")
        view.js(T("""
          const row = await need("the row of " + $ID, () => [...document.querySelectorAll(".profile-row")]
            .find((el) => visible(el) && el.textContent.includes($ID + " ·")));
          press(await need("Use", () => one("button", "Use", row)));
          return true;""", ID=target))
        # A switch reloads the page, so the outcome is read from the host.
        switched = harness.wait_until(lambda: (lambda s: s["state"] == "ready" and s["profile"] == target)(host.call("status")), timeout=90)
        if not switched:
            _, blocked = view.try_js('return text().includes("still working in this profile");', 10)
            check.expect(f"{target}: the switch is not blocked by a run an earlier scenario left", blocked is not True, blocked)
        view.wait_ui()
        view.shot(f"switched-{target}")
        check.expect(f"Use switches the runtime to {target} and the page comes back", switched, host.call("status"))
    return check


def scenario_close(view, host, provider):
    check = Checks("close_with_runs")
    view.new_chat()
    view.send("[e2e:native-close] Keep at it.")
    held = provider.wait_for("gate", "native-close", gate="native-close")
    host.call("close")
    asked = view.js('return !!(await until(() => one("alertdialog", "Runs are still working"), 20000));', 30)
    view.shot("close-with-runs")
    view.js('press(await need("Keep working", () => one("button", "Keep working"))); return true;')
    gone = view.js('return !!(await until(() => !one("alertdialog", "Runs are still working"), 10000));')
    alive = host.process.poll() is None and host.call("status")["state"] == "ready"
    provider.open("native-close")
    # The text before the pause is shown apart from the rest of the answer.
    finished = view.wait_text("and kept working.") and view.idle()
    seen = (gone, alive, finished)
    if not finished:
        seen = {"gone": gone, "alive": alive, "page": view.js("return text().slice(-400);"),
                "saved": harness.answers(saved_with(os.environ["LIGHTAGENT_HOME"], "[e2e:native-close]"))}
    check.expect("closing with a working run asks first", held and asked)
    check.expect("Keep working keeps the window, the runtime and the run", gone and alive and finished, seen)
    return check


def scenario_reattach(view, provider, home, notes):
    """Recorded: a run still working when the page reloads is shown again
    when its conversation is opened, and its answer arrives."""
    check = Checks("reload_while_working")
    prompt = "[e2e:native-reload] Keep going through a reload."
    view.new_chat()
    view.send(prompt)
    held = provider.wait_for("gate", "native-reload", gate="native-reload")
    view.reload()
    view.js(T("""
      const title = $PROMPT.replace(/[.]+$/, "");
      const entry = await need("the conversation in the list", () => [...document.querySelectorAll(".agent-sessions__list *")]
        .find((el) => visible(el) && el.children.length === 0 && squash(el.textContent).replace(/[.…]+$/, "") === title));
      press(entry);
      return true;""", PROMPT=prompt))
    working = view.js('return !!(await until(() => one("button", "Stop"), 10000));')
    provider.open("native-reload")
    shown = view.wait_text("after the reload.", 30)
    saved = (harness.answers(saved_with(home, "[e2e:native-reload]")) or [""])[-1]
    notes["reload_while_working"] = {"held": held, "stop_shown_after_reload": working, "answer_shown": shown,
                                     "answer_saved": saved.endswith("after the reload.")}
    view.idle(30)
    return check


def scenario_crash(view, host, provider):
    check = Checks("crash_recovery")
    view.new_chat()
    view.send("[e2e:convo] Before the crash.")
    before = view.wait_text("Noted.") and view.idle()
    victims = harness.runtimes(host.process.pid)
    for victim in victims:
        victim.kill()
    try:
        restarted = bool(host.wait_ready(120))
    except harness.HostError as error:
        restarted = str(error)
    view.shot("after-crash")
    view.wait_ui()
    view.new_chat()
    view.send("[e2e:convo] After the crash.")
    after = view.wait_text("Noted.", 90) and view.idle()
    seen = None
    if not after:
        seen = {"page": view.js("return text().slice(-500);"), "status": host.call("status"), "runtime_output": host.call("logs", limit=40)}
    listed = view.js('return !!(await until(() => (document.querySelector(".agent-sessions__list") || {}).textContent?.includes("Before the crash"), 15000));')
    check.expect("the first conversation answered before the crash", before)
    check.expect("the host restarted its runtime", victims and restarted is True, restarted)
    check.expect("the UI runs a new conversation on the restarted runtime", after, seen)
    check.expect("the earlier conversation is still listed", listed)
    return check


def scenario_quit(view, host, provider, home):
    check = Checks("quit_with_runs")
    view.new_chat()
    view.send("[e2e:native-quit] Hold on.")
    held = provider.wait_for("gate", "native-quit", gate="native-quit")
    runtime = [p.pid for p in harness.runtimes(host.process.pid)]
    host.call("close")
    asked = view.js('return !!(await until(() => one("alertdialog", "Runs are still working"), 20000));', 30)
    host.send("eval", script=PRELUDE + '\npress(one("button", "Stop runs and quit")); return true;', timeout_ms=5000)
    try:
        host.process.wait(timeout=45)
        exited = True
    except subprocess.TimeoutExpired:
        exited = False
    import psutil

    left = [pid for pid in runtime if psutil.pid_exists(pid) and psutil.Process(pid).status() != psutil.STATUS_ZOMBIE]
    provider.open("native-quit")
    stopped = harness.wait_until(lambda: last_stop(saved_with(home, "[e2e:native-quit]")) == "Cancelled", timeout=10)
    check.expect("quitting with a working run asks first", held and asked)
    check.expect("Stop runs and quit ends the app and its runtime", exited and not left, (exited, left))
    check.expect("the stopped run is saved as cancelled", stopped, last_stop(saved_with(home, "[e2e:native-quit]")))
    return check


# Startup without internet ------------------------------------------------------

def scenario_offline(view, host, home):
    check = Checks("offline")
    facts = page_facts(view)
    check.expect("the UI is the bundled one, served by the app itself", facts["origin"] in BUNDLED_ORIGINS, facts)
    view.new_chat()
    prompt = "Hello while offline."
    view.send(prompt)
    ended = view.idle(150)
    shown = view.js("return text().slice(-700);")
    view.shot("offline-run")
    check.expect("a model call to an unreachable provider ends with an error shown",
                 ended and bool(re.search(r"fail|error|could not|unreachable|connect|resolve", shown, re.I)), shown[-300:])
    check.expect("the conversation is kept locally", saved_with(home, prompt) is not None)
    listed = view.js(T('return !!(await until(() => (document.querySelector(".agent-sessions__list") || {}).textContent?.includes($P), 15000));', P=prompt.rstrip(".")))
    check.expect("the conversation is listed", listed)
    view.go("Settings", "#/settings")
    check.expect("Settings load from the local runtime", view.wait_text("Agent runtime", 20))
    view.shot("offline-settings")
    outcome = add_and_check_mcp(view, "offline-mcp", "http://lightagent-offline.invalid/mcp")
    check.expect("an MCP server can be added offline", outcome["listed"], outcome)
    check.expect("checking an unreachable MCP server shows why it failed", outcome["failed_shown"], outcome)
    return check


def page_facts(view):
    return view.js("""return { origin: location.origin, protocol: location.protocol, user_agent: navigator.userAgent,
      device_pixel_ratio: devicePixelRatio, speech: "speechSynthesis" in window, share: typeof navigator.share === "function",
      clipboard: !!(navigator.clipboard && navigator.clipboard.writeText) };""")


def no_network():
    """True when nothing outside loopback is reachable (checked by connecting out)."""
    for address in (("github.com", 443), ("one.one.one.one", 443), ("dns.google", 443)):
        try:
            socket.create_connection(address, timeout=3).close()
            return False, f"reached {address}"
        except OSError:
            continue
    return True, "no address outside loopback answered"


def main():
    if sys.argv[1] == "--summary":
        return summary(sys.argv[2])
    out = os.path.abspath(sys.argv[1])
    cli, mode = sys.argv[2], sys.argv[3]
    command = sys.argv[sys.argv.index("--") + 1:]
    os.makedirs(out, exist_ok=True)
    home = os.environ["LIGHTAGENT_HOME"]
    results = {"mode": mode, "checks": {}, "recorded": {}, "failures": [],
               "runner": {"platform": platform.platform(), "machine": platform.machine(),
                          "image": f"{os.environ.get('ImageOS', '?')} {os.environ.get('ImageVersion', '?')}"}}
    failures, notes = results["failures"], results["recorded"]
    provider = harness.FakeProvider()
    scripts(provider)
    if mode == "offline":
        harness.run_cli(cli, "config", "set", "inference.base_url", OFFLINE_URL)
        if os.environ.get("E2E_NETNS") == "1":
            isolated, seen = no_network()
            results["checks"]["network"] = {"no network outside loopback": {"ok": isolated, "seen": seen}}
            if not isolated:
                failures.append(f"network: {seen}")
    else:
        harness.prepare_home(cli, home, provider)
        harness.run_cli(cli, "profile", "create", "native-b", "--persona", PERSONA)
        # The vault's key, outside the home, for the secret input scenario.
        import tempfile

        key_dir = tempfile.mkdtemp(prefix="lightagent-native-credential-key-")
        harness.run_cli(cli, "remote-credential", "keygen", os.path.join(key_dir, "credential.key"))
        os.environ["LIGHTAGENT_CREDENTIAL_KEY_FILE"] = os.path.join(key_dir, "credential.key")
        # Started with the remote server's key, as a person would start it
        # (remote mode reads it; the bundled runtime must not get it).
        os.environ["LIGHTAGENT_API_KEY"] = REMOTE_KEY
    env = dict(os.environ, LIGHTAGENT_DESKTOP_E2E_DRIVER="1",
               LIGHTAGENT_DESKTOP_E2E_REPORT=os.path.join(out, f"report-{mode}.txt"))
    host = harness.Host(command, env, os.path.join(out, f"driver-{mode}.log"))
    view = View(host, out, mode)
    try:
        status = host.ready()
        view.wait_ui()
        notes["page"] = page_facts(view)
        notes["runtime"] = {k: status.get(k) for k in ("runtime_version", "desktop_version", "profile")}
        try:
            notes["window"] = host.call("geometry")
        except harness.HostError as error:
            notes["window"] = str(error)
        view.shot("ready")
        if mode == "offline":
            scenarios = [("offline", lambda: scenario_offline(view, host, home))]
        else:
            workspace = host.workspace()
            scenarios = [
                ("chat", lambda: scenario_chat(view, provider, home, notes)),
                ("response_actions", lambda: scenario_actions(view, host, notes)),
                ("approvals", lambda: scenario_approvals(view, provider, workspace)),
                ("stop", lambda: scenario_stop(view, provider, home)),
                ("continue", lambda: scenario_continue(view, host, home)),
                ("screens", lambda: scenario_screens(view, notes)),
                ("forms", lambda: scenario_forms(view, home)),
                ("keyboard", lambda: scenario_keyboard(view, host, notes)),
                ("secret_input", lambda: scenario_secret_input(view, host, home, notes)),
                ("narrow", lambda: scenario_narrow(view, host)),
                ("conversation_profile", lambda: scenario_conversation_profile(view, host, provider, cli)),
                ("remote_mode", lambda: scenario_remote(view, host, provider, cli)),
                ("profile_switch", lambda: scenario_switch(view, host)),
                ("close_with_runs", lambda: scenario_close(view, host, provider)),
                ("reload_while_working", lambda: scenario_reattach(view, provider, home, notes)),
                # Near the end: the host restarts a crashed runtime at most once a minute.
                ("crash_recovery", lambda: scenario_crash(view, host, provider)),
                # Last: it quits the app.
                ("quit_with_runs", lambda: scenario_quit(view, host, provider, home)),
            ]
        seeded = []
        for name, scenario in scenarios:
            try:
                check = scenario()
                results["checks"][name] = check.results
                failures.extend(check.failures)
                seeded.extend(getattr(check, "seeds", []))
            except Exception as error:  # a broken scenario is a failure; the rest still run
                results["checks"][name] = {"error": repr(error)}
                failures.append(f"{name}: {error!r}")
                view.shot(f"failed-{name}")
                if host.process.poll() is not None:
                    failures.append("the host exited; later scenarios were not run")
                    break
    except Exception as error:
        failures.append(f"setup: {error!r}")
        view.shot("failed-setup")
    finally:
        if host.process.poll() is None:
            host.stop()
        else:
            host.stop()
        provider.close()
    results["screenshots"] = view.shots
    if mode == "full":
        logs = [os.path.join(out, f"driver-{mode}.log"), os.path.join(out, f"report-{mode}.txt")]
        held = [path for path in logs if any(seed in (harness.read(path) or "") for seed in seeded)]
        results["seeded_secret_in_logs"] = held
        if held:
            failures.append(f"secret_input: a seeded secret is in {held}")
    with open(os.path.join(out, f"native-{mode}.json"), "w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=1)
    print(json.dumps({"failures": failures}, indent=1))
    return 1 if failures else 0


def summary(out):
    lines = []
    for path in sorted(glob.glob(os.path.join(out, "**", "native-*.json"), recursive=True)):
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
        label = os.path.relpath(path, out)
        failed = data.get("failures") or []
        agent = ((data.get("recorded") or {}).get("page") or {}).get("user_agent", "?")
        lines.append(f"- `{label}`: {'passed' if not failed else f'{len(failed)} failure(s)'}; WebView: {agent[:140]}")
        for name, checks in (data.get("checks") or {}).items():
            if "error" in checks:
                lines.append(f"  - {name}: error {str(checks['error'])[:200]}")
            else:
                passed = sum(1 for c in checks.values() if c.get("ok"))
                lines.append(f"  - {name}: {passed}/{len(checks)}")
        for name, value in (data.get("recorded") or {}).items():
            if name.startswith("axe"):
                value = {k: [f"{v['id']}:{v['impact']}" for v in vs] for k, vs in value.items()} if isinstance(value, dict) else [f"{v['id']}:{v['impact']}" for v in value]
            lines.append(f"  - recorded {name}: {json.dumps(value)[:400]}")
        for failure in failed[:25]:
            lines.append(f"  - FAILED {failure[:300]}")
    print("\n".join(lines) or "- no native reports")
    return 0


if __name__ == "__main__":
    sys.exit(main())
