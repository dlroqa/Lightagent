// The Settings controls that mirror `lightagent setup` (inventory rows
// setup.tools.mode, setup.web.mode, setup.approvals.policy and
// setup.tui.reasoning) against the real harness. Each choice is made on the
// Settings screen and read back from the API and the config keys. Web access
// offers setup's four modes, and a custom search needs its URL first.
// Filesystem tools and Terminal tool give setup's three tool choices.
// Approval policy and Show reasoning in terminal save what setup saves.
// Everything is restored.
// crates/lightagent/tests/desktop_setup_sections.rs proves each choice
// writes what `lightagent setup` writes; desktop/e2e/runs.py
// `setup_sections` covers the host.

const ENDPOINT = "http://127.0.0.1:9/search?format=json";
const DUCKDUCKGO = "https://html.duckduckgo.com/html/";

export async function checkSetupSections(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const api = `${base}/api/lightagent/v1`;
  const settings = async () => (await fetch(`${api}/settings`)).json();
  const key = async (name) => {
    const response = await fetch(`${api}/config/${name}`);
    return response.status === 200 ? (await response.json()).value : null;
  };
  const until = async (what, probe, timeout = 10000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (await probe()) return;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    throw new Error(`timed out waiting for ${what}`);
  };
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`setup sections: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/setup-sections-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  const toggle = async (name, on) => {
    const control = page.getByRole("switch", { name, exact: true });
    if ((await control.getAttribute("aria-checked")) !== String(on)) {
      await control.click({ timeout: 10000 });
    }
    await until(`${name} ${on ? "on" : "off"}`, async () => (await control.getAttribute("aria-checked")) === String(on));
  };
  const original = await settings();
  try {
    await page.goto(`${base}/#/settings`);
    const web = page.locator("#web-access");
    await web.waitFor({ timeout: 15000 });

    await step("web access: setup's modes", async () => {
      for (const [mode, label, endpoint, realtime] of [
        ["fetch", "Fetch web pages only", "", "false"],
        ["duckduckgo", "Agentic search — DuckDuckGo (no account)", DUCKDUCKGO, "true"],
        ["off", "Off", "", "false"],
      ]) {
        await web.selectOption({ label });
        await until(`web mode ${mode}`, async () => (await settings()).web_mode === mode);
        const seen = [await key("web.enabled"), await key("web.search.endpoint"), await key("rag.realtime_enabled")];
        const expected = [String(mode !== "off"), endpoint, realtime];
        if (JSON.stringify(seen) !== JSON.stringify(expected)) {
          throw new Error(`${mode}: the config reads ${JSON.stringify(seen)}, not ${JSON.stringify(expected)}`);
        }
      }
    });

    await step("web access: a custom search needs its URL, then saves it", async () => {
      await web.selectOption({ label: "Agentic search — SearXNG or custom JSON endpoint" });
      const url = page.locator("#web-access-endpoint");
      await url.fill("");
      await page.getByRole("button", { name: "Save endpoint", exact: true }).click({ timeout: 10000 });
      await page.getByRole("alert").filter({ hasText: "Enter the search endpoint URL" }).waitFor({ timeout: 5000 });
      if ((await settings()).web_mode === "custom") throw new Error("a custom search was saved without its URL");
      await url.fill(ENDPOINT);
      await page.screenshot({ path: `${outDir}/setup-sections-web-custom.png` });
      await page.getByRole("button", { name: "Save endpoint", exact: true }).click({ timeout: 10000 });
      await until("the custom search", async () => (await settings()).web_mode === "custom");
      const seen = [await key("web.search.endpoint"), await key("rag.realtime_enabled")];
      if (seen[0] !== ENDPOINT || seen[1] !== "true") throw new Error(`the config reads ${JSON.stringify(seen)}`);
    });

    await step("local tools: setup's three choices", async () => {
      for (const [files, terminal] of [[true, true], [true, false], [false, false]]) {
        await toggle("Filesystem tools", files);
        if (files) await toggle("Terminal tool", terminal);
        await until(`tools ${files}/${terminal}`, async () => {
          const saved = await settings();
          return saved.filesystem_tools_enabled === files && saved.terminal_enabled === terminal;
        });
      }
    });

    await step("approval policy and reasoning: what setup saves", async () => {
      const policy = page.locator("#agent-approval");
      for (const value of ["strict", "permissive", "balanced"]) {
        await policy.selectOption(value);
        await until(`approval ${value}`, async () => (await settings()).approval_policy === value);
      }
      for (const on of [false, true]) {
        await toggle("Show reasoning in terminal", on);
        await until(`reasoning ${on}`, async () =>
          (await settings()).show_reasoning_in_tui === on && (await key("tui.show_reasoning")) === String(on));
      }
    });
    if (errors.length) failures.push(`setup sections: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`setup sections: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    const { revision: _revision, ...restore } = original;
    await fetch(`${api}/settings`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(restore),
    }).catch(() => {});
  }
  return failures;
}
