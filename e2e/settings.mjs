// The Settings screen against the real harness: a save made against an
// outdated revision keeps the change on screen and saves it on the current
// values only when asked, or discards it when asked; the Jev routing dry run
// shows the server's report; and a saved secret is nowhere in what the
// renderer holds. crates/lightagent/tests/desktop_config.rs proves the API's
// writes are the CLI's.

const SECRET = "render-state-secret-5d2c81-not-a-real-key";
const PROVIDER_KEY = "render-provider-key-9e41a7-not-a-real-key";
const PROVIDER_KEY_2 = "render-provider-key-b07c33-not-a-real-key";

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

    // The provider key: saved write-only, replaced, then removed.
    const credentialNow = async () =>
      (await fetch(`${base}/api/lightagent/v1/provider/credential`)).json();
    const keyField = page.locator("#provider-api-key");
    if ((await credentialNow()).state !== "absent") {
      failures.push("settings: the harness started with a provider key; the credential flow needs none");
    } else {
      await page.getByTestId("provider-credential-state").waitFor({ timeout: 5000 });
      await keyField.fill(PROVIDER_KEY);
      const saveKey = page.getByRole("button", { name: "Save API key", exact: true });
      // The card loaded before the writes above, so its revision is stale:
      // the save is refused, and the entry stays in the field.
      await saveKey.click();
      await page.getByText("so the key was not saved", { exact: false }).waitFor({ timeout: 5000 });
      if (await keyField.inputValue() !== PROVIDER_KEY) failures.push("settings: a refused key save lost the entry");
      if ((await credentialNow()).state !== "absent") failures.push("settings: a refused key save stored the key");
      // On the current revision it is stored.
      await saveKey.click();
      await page.getByText("Provider API key saved.", { exact: true }).waitFor({ timeout: 5000 });
      let credential = await credentialNow();
      if (credential.state !== "ready") failures.push(`settings: after saving, the key is ${credential.state}`);
      if (JSON.stringify(credential).includes(PROVIDER_KEY)) failures.push("settings: the credential status carries the key");
      if (await keyField.inputValue()) failures.push("settings: the key stays in the field after saving");
      if ((await page.content()).includes(PROVIDER_KEY)) failures.push("settings: the saved key is in the page");
      await page.screenshot({ path: `${outDir}/settings-provider-key.png` });
      await keyField.fill(PROVIDER_KEY_2);
      await page.getByRole("button", { name: "Replace API key", exact: true }).click();
      await page.getByText("Provider API key replaced.", { exact: true }).waitFor({ timeout: 5000 });
      if ((await page.content()).includes(PROVIDER_KEY_2)) failures.push("settings: the replaced key is in the page");
      await page.getByRole("button", { name: "Remove API key", exact: true }).click();
      await page.getByRole("button", { name: "Confirm: remove the API key", exact: true }).click();
      await page.getByText("Provider API key removed.", { exact: false }).waitFor({ timeout: 5000 });
      credential = await credentialNow();
      if (credential.state !== "absent") failures.push(`settings: after removing, the key is ${credential.state}`);
    }

    // The endpoint check runs only when asked, and says what the endpoint answered.
    await page.getByRole("button", { name: "Check connection", exact: true }).click();
    const checked = page.getByTestId("provider-check").getByRole("status");
    await checked.waitFor({ timeout: 40000 });
    const checkText = await checked.innerText();
    if (!/^(Reachable in|Not reachable:)/.test(checkText)) failures.push(`settings: the endpoint check showed "${checkText}"`);
    if (checkText.includes(PROVIDER_KEY) || checkText.includes(PROVIDER_KEY_2)) failures.push("settings: the endpoint check shows a key");

    // Providers and models: the shared operations. The card loaded before
    // the writes above, so its first save is refused and the entry stays;
    // on the current revision it is saved, renamed, used and given a model,
    // and a stale removal is refused before it is made.
    const providersNow = async () => (await fetch(`${base}/api/lightagent/v1/providers`)).json();
    const startedWith = await providersNow();
    const originalUrl = startedWith.providers.find((provider) => provider.active)?.base_url;
    const originalModel = startedWith.model;
    const labUrl = "http://127.0.0.1:9/e2e-lab";
    const card = page.getByTestId("providers-card");
    const newName = page.locator("#provider-new-name");
    await newName.fill("E2E Lab");
    await page.locator("#provider-new-url").fill(labUrl);
    const saveProvider = page.getByRole("button", { name: "Save provider", exact: true });
    await saveProvider.click();
    await card.getByText("Nothing was saved", { exact: false }).waitFor({ timeout: 5000 });
    if (await newName.inputValue() !== "E2E Lab") failures.push("providers: a refused save lost the entry");
    if ((await providersNow()).providers.some((provider) => provider.id === "e2e-lab")) {
      failures.push("providers: a refused save stored the provider");
    }
    await saveProvider.click();
    await page.getByTestId("provider-e2e-lab").waitFor({ timeout: 5000 });
    if (await newName.inputValue()) failures.push("providers: the saved entry stays in the field");

    await page.getByRole("button", { name: "Edit E2E Lab", exact: true }).click();
    await page.locator("#provider-name-e2e-lab").fill("E2E Lab Two");
    await page.getByRole("button", { name: "Save changes", exact: true }).click();
    await page.getByTestId("provider-e2e-lab-two").waitFor({ timeout: 5000 });

    await page.getByRole("button", { name: "Use E2E Lab Two", exact: true }).click();
    await card.getByText("E2E Lab Two is the active provider", { exact: false }).waitFor({ timeout: 5000 });
    if ((await providersNow()).active !== "e2e-lab-two") failures.push("providers: Use did not select the provider");
    if (!(await card.innerText()).includes("Applies to the next run")) {
      failures.push("providers: the card does not say when a change applies");
    }
    if ((await credentialNow()).base_url !== labUrl) failures.push("providers: the key status is not the new provider's");
    await page.getByText(`(${labUrl})`, { exact: false }).first().waitFor({ timeout: 5000 });

    await page.locator("#provider-model").fill("e2e-model");
    await page.getByRole("button", { name: "Use this model", exact: true }).click();
    await card.getByText("Model set to e2e-model.", { exact: false }).waitFor({ timeout: 5000 });
    if ((await providersNow()).model !== "e2e-model") failures.push("providers: the model was not saved");
    await page.screenshot({ path: `${outDir}/settings-providers.png` });

    // Back to the harness's endpoint and model, as another writer.
    if (originalUrl) await writeElsewhere(base, "inference.base_url", originalUrl);
    if (originalModel) await writeElsewhere(base, "inference.model", originalModel);
    else await put(base, "/provider/model", { model: null });
    await page.getByRole("button", { name: "Remove E2E Lab Two", exact: true }).click();
    const confirmRemove = page.getByRole("button", { name: "Confirm: remove E2E Lab Two", exact: true });
    await confirmRemove.click();
    await card.getByText("Nothing was saved", { exact: false }).waitFor({ timeout: 5000 });
    if (!(await providersNow()).providers.some((provider) => provider.id === "e2e-lab-two")) {
      failures.push("providers: a refused removal removed the provider");
    }
    await confirmRemove.click();
    await card.getByText("Removed E2E Lab Two.", { exact: false }).waitFor({ timeout: 5000 });
    const finished = await providersNow();
    if (finished.providers.some((provider) => provider.id === "e2e-lab-two")) {
      failures.push("providers: the removal did not remove the provider");
    }
    if (originalUrl && finished.providers.find((provider) => provider.active)?.base_url !== originalUrl) {
      failures.push("providers: the harness endpoint was not restored");
    }

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
