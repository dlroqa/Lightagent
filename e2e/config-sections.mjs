// One key of every configuration section the CLI can set (inventory rows
// config.<section>, and setup → tui), written with the Settings screen's
// Advanced configuration card against the real harness: Read, Value, Set,
// then the API confirms the value and the original is restored.
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
  const valueOf = async (key) => (await (await fetch(url(key))).json()).value;
  const restore = (key, value) =>
    fetch(url(key), { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ value }) });
  try {
    await page.goto(`${base}/#/settings`);
    await page.getByRole("heading", { name: "Advanced configuration" }).waitFor({ timeout: 15000 });
    for (const [key, value] of SECTIONS) {
      const original = await valueOf(key);
      try {
        await page.getByLabel("Key", { exact: true }).fill(key);
        await page.getByRole("button", { name: "Read", exact: true }).click();
        await page.getByText(`${key} = `, { exact: false }).first().waitFor({ timeout: 5000 });
        await page.getByLabel("Value", { exact: true }).fill(value);
        await page.getByRole("button", { name: "Set", exact: true }).click();
        await page.getByText(`Set ${key} = ${value}.`, { exact: false }).waitFor({ timeout: 5000 });
        const now = await valueOf(key);
        if (now !== value) failures.push(`config sections: ${key} is ${JSON.stringify(now)} after the card set ${value}`);
      } catch (err) {
        failures.push(`config sections: ${key}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
        await page.screenshot({ path: `${outDir}/config-sections-failed-${key.replace(/\W+/g, "-")}.png` }).catch(() => {});
      } finally {
        await restore(key, original ?? "").catch(() => {});
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
