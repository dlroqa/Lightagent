// The Settings screen against the real harness: a save made against an
// outdated revision keeps the change on screen and saves it on the current
// values only when asked, or discards it when asked; the Jev routing dry run
// shows the server's report; and a saved secret is nowhere in what the
// renderer holds. crates/lightagent/tests/desktop_config.rs proves the API's
// writes are the CLI's.

const SECRET = "render-state-secret-5d2c81-not-a-real-key";

async function settingsNow(base) {
  return (await fetch(`${base}/api/lightagent/v1/settings`)).json();
}

async function put(base, path, body) {
  return fetch(`${base}/api/lightagent/v1${path}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** Another writer: what the CLI or another window would do. */
async function writeElsewhere(base, key, value) {
  const response = await put(base, `/config/${key}`, { value: String(value) });
  if (!response.ok) throw new Error(`the direct write of ${key} answered ${response.status}`);
}

export async function checkSettingsStates(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const original = await settingsNow(base);
  const approval = page.locator("#agent-approval");
  const notice = page.getByTestId("settings-conflict");
  try {
    await page.goto(`${base}/#/settings`);
    await page.getByText("Agent runtime").first().waitFor({ timeout: 15000 });

    // A change refused as stale is kept, then applied to the current values.
    const turns = original.max_turns + 3;
    await writeElsewhere(base, "agent.max_turns", turns);
    const target = original.approval_policy === "strict" ? "permissive" : "strict";
    await approval.selectOption(target);
    await notice.waitFor({ timeout: 5000 });
    if (!(await notice.innerText()).includes("approval policy")) {
      failures.push("settings: the conflict notice does not name the unsaved change");
    }
    let now = await settingsNow(base);
    if (now.approval_policy !== original.approval_policy) failures.push("settings: a stale save changed the approval policy");
    await page.screenshot({ path: `${outDir}/settings-conflict.png` });
    await notice.getByRole("button", { name: "Apply my change to the current settings" }).click();
    await page.getByText("Saved. New runs use these settings.").waitFor({ timeout: 5000 });
    now = await settingsNow(base);
    if (now.approval_policy !== target) failures.push("settings: applying the kept change did not save it");
    if (now.max_turns !== turns) failures.push("settings: applying the kept change lost the other writer's change");

    // Refused again, then discarded: nothing is saved and the saved value shows.
    await writeElsewhere(base, "agent.max_turns", turns + 1);
    await approval.selectOption(original.approval_policy);
    await notice.waitFor({ timeout: 5000 });
    await notice.getByRole("button", { name: "Discard my change" }).click();
    await notice.waitFor({ state: "detached", timeout: 5000 });
    now = await settingsNow(base);
    if (now.approval_policy !== target) failures.push("settings: a discarded change was saved");
    if (await approval.inputValue() !== target) failures.push("settings: after discarding, the screen does not show the saved value");

    // The routing dry run is the server's report.
    await page.getByRole("button", { name: "Dry run: who Strict would deny" }).click();
    const report = page.getByTestId("jev-routing-report");
    await report.getByRole("table").waitFor({ timeout: 5000 });
    const served = await (await fetch(`${base}/api/lightagent/v1/jev/routing-report`)).json();
    const shownReport = await report.innerText();
    for (const row of served.profiles ?? []) {
      if (!shownReport.includes(row.profile)) failures.push(`settings: the routing dry run does not list ${row.profile}`);
    }
    if (served.changes_made !== false) failures.push("settings: the routing report claims to have changed something");

    // A saved secret is in none of the renderer's state.
    const current = await settingsNow(base);
    const saved = await put(base, "/settings", { ...current, jev: { ...current.jev, api_key: SECRET } });
    if (!saved.ok) failures.push(`settings: saving the test secret answered ${saved.status}`);
    await page.reload();
    await page.getByText("Agent runtime").first().waitFor({ timeout: 15000 });
    await page.waitForLoadState("networkidle").catch(() => {});
    const state = await page.evaluate(() => JSON.stringify({
      html: document.documentElement.outerHTML,
      local: Object.fromEntries(Object.entries(window.localStorage)),
      session: Object.fromEntries(Object.entries(window.sessionStorage)),
      fields: [...document.querySelectorAll("input, textarea, select")].map((field) => field.value),
    }));
    if (state.includes(SECRET)) failures.push("settings: the saved secret appears in the renderer's state");
    if (JSON.stringify(await settingsNow(base)).includes(SECRET)) failures.push("settings: the settings response carries the secret");

    if (errors.length) failures.push(`settings: uncaught page error — ${errors[0]}`);
    console.log(`  [${failures.length ? "FAIL" : "ok"}] settings conflict, routing dry run and renderer state`);
  } finally {
    await page.close();
    // Leave the harness's runtime settings as they were found.
    const after = await settingsNow(base).catch(() => null);
    if (after) {
      await put(base, "/settings", {
        ...after,
        approval_policy: original.approval_policy,
        max_turns: original.max_turns,
      }).catch(() => {});
    }
  }
  return failures;
}
