// The Settings screen's advanced configuration editor against the real
// harness: a write lands in the file `lightagent config` uses, a write made
// against an outdated revision is refused with nothing written, the editor
// then works from the current revision, and an invalid value changes
// nothing. crates/lightagent/tests/desktop_config.rs proves the API's
// writes are the CLI's and that the lock holds across processes.

export async function checkConfigEditor(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const valueOf = async (key) =>
    (await (await fetch(`${base}/api/lightagent/v1/config/${key}`)).json()).value;
  const set = page.getByRole("button", { name: "Set", exact: true });
  const valueField = page.getByLabel("Value", { exact: true });
  try {
    await page.goto(`${base}/#/settings`);
    await page.getByRole("heading", { name: "Advanced configuration" }).waitFor({ timeout: 15000 });

    await page.getByLabel("Key", { exact: true }).fill("memory.top_k");
    await page.getByRole("button", { name: "Read", exact: true }).click();
    await page.getByText("memory.top_k = ", { exact: false }).first().waitFor({ timeout: 5000 });

    await valueField.fill("7");
    await set.click();
    await page.getByText("Set memory.top_k = 7.", { exact: false }).waitFor({ timeout: 5000 });
    if (await valueOf("memory.top_k") !== "7") failures.push("config: the editor's write did not reach the config file");

    // Another writer changes the file; the editor still holds the older revision.
    const other = await fetch(`${base}/api/lightagent/v1/config/memory.top_k`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ value: "8" }),
    });
    if (!other.ok) failures.push(`config: the direct write answered ${other.status}`);
    await valueField.fill("9");
    await set.click();
    await page.getByText("changed since it was loaded", { exact: false }).waitFor({ timeout: 5000 });
    if (await valueOf("memory.top_k") !== "8") failures.push("config: a stale write changed the config file");
    await page.screenshot({ path: `${outDir}/settings-config-stale.png` });

    // Reloaded, the same write now lands.
    await set.click();
    await page.getByText("Set memory.top_k = 9.", { exact: false }).waitFor({ timeout: 5000 });
    if (await valueOf("memory.top_k") !== "9") failures.push("config: the write after reloading did not land");

    // An invalid value is refused next to the form and writes nothing.
    await valueField.fill("lots");
    await set.click();
    await page.locator(".config-editor__error").waitFor({ timeout: 5000 });
    if (await valueOf("memory.top_k") !== "9") failures.push("config: an invalid value changed the config file");

    if (errors.length) failures.push(`config: uncaught page error — ${errors[0]}`);
    console.log(`  [${failures.length ? "FAIL" : "ok"}] settings advanced configuration`);
  } finally {
    await page.close();
  }
  return failures;
}
