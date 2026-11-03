// One key of every configuration section the CLI can set (inventory rows
// config.<section>, and setup → tui), with the Settings screen's Advanced
// configuration card against the real harness. Read shows the current value
// (or, for an optional key with no value, the same refusal as `config get`).
// Set writes the new value, Read shows it, and the API confirms it. The
// original is restored through the API, which changes the config revision,
// so the page reloads before each key: the card sends the revision it
// loaded, and a stale one is refused (409) by design.
// crates/lightagent/tests/desktop_config_sections.rs proves API = CLI per
// section; desktop/e2e/runs.py `config_sections` covers the host.

const SECTIONS = [
  ["inference.model", "sections-model-a"],
  ["agent.max_turns", "17"],
  ["web.timeout_secs", "21"],
  ["tools.profile_policy", "report_only"],
  ["rag.realtime_enabled", "true"],
  ["memory.top_k", "6"],
  ["runtime.threads", "3"],
  ["generation.max_output_tokens", "1234"],
  ["platform.infinity.timeout_secs", "11"],
  ["subagents.selection_profiles", "alpha"],
  ["tui.show_reasoning", "false"],
];

export async function checkConfigSections(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const url = (key) => `${base}/api/lightagent/v1/config/${key}`;
  // An optional key with no value is a 404, as `config get` refuses it.
  const current = async (key) => {
    const response = await fetch(url(key));
    const body = await response.json().catch(() => null);
    return { status: response.status, value: body?.value };
  };
  const restore = (key, value) =>
    fetch(url(key), { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ value }) });
  const heading = page.getByRole("heading", { name: "Advanced configuration" });
  const shows = (text) => page.getByText(text, { exact: true }).waitFor({ timeout: 5000 });
  try {
    await page.goto(`${base}/#/settings`);
    await heading.waitFor({ timeout: 15000 });
    for (const [key, value] of SECTIONS) {
      const before = await current(key);
      if (before.status !== 200 && before.status !== 404) {
        failures.push(`config sections: ${key} reads as ${before.status} through the API`);
        continue;
      }
      try {
        await page.reload();
        await heading.waitFor({ timeout: 15000 });
        await page.getByLabel("Key", { exact: true }).fill(key);
        await page.getByRole("button", { name: "Read", exact: true }).click();
        if (before.status === 404) {
          await page.getByRole("alert").filter({ hasText: `unknown or unreadable config key '${key}'` })
            .waitFor({ timeout: 5000 });
        } else {
          await shows(`${key} = ${before.value || "(empty)"}`);
        }
        await page.getByLabel("Value", { exact: true }).fill(value);
        await page.getByRole("button", { name: "Set", exact: true }).click();
        await shows(`Set ${key} = ${value}. New runs use it.`);
        await page.getByRole("button", { name: "Read", exact: true }).click();
        await shows(`${key} = ${value}`);
        const now = await current(key);
        if (now.status !== 200 || now.value !== value) {
          failures.push(`config sections: ${key} is ${JSON.stringify(now)} after the card set ${value}`);
        }
      } catch (err) {
        failures.push(`config sections: ${key}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
        await page.screenshot({ path: `${outDir}/config-sections-failed-${key.replace(/\W+/g, "-")}.png` }).catch(() => {});
      } finally {
        await restore(key, before.value ?? "").catch(() => {});
      }
      const after = await current(key);
      if (after.status !== before.status || after.value !== before.value) {
        failures.push(`config sections: ${key} not restored: ${JSON.stringify(before)} became ${JSON.stringify(after)}`);
      }
    }
    if (errors.length) failures.push(`config sections: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`config sections: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
  }
  return failures;
}
