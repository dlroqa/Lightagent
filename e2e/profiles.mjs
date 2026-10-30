// The Profiles screen against the real harness API: create, inspect, preset,
// activate and delete a profile through the UI, and confirm the server's
// profile store (the store `lightagent profile` uses) changed each time.
// Activation is checked only as the UI reports it: the running server keeps
// serving its own profile, and crates/lightagent/tests/desktop_profiles.rs
// proves the API's activation is the CLI's.

export async function checkProfileStates(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const catalog = async () => (await fetch(`${base}/api/lightagent/v1/profiles`)).json();
  const has = (data, id) => data.profiles.some((profile) => profile.id === id);
  try {
    await page.goto(`${base}/#/profiles`);
    await page.getByRole("heading", { name: "Saved profiles" }).waitFor({ timeout: 15000 });

    // An invalid id is refused next to the form, before any request.
    await page.getByLabel("Profile id").fill("Bad Id");
    await page.getByRole("button", { name: "Create profile" }).click();
    await page.getByText("Use lowercase letters, digits").first().waitFor({ timeout: 5000 });
    if (has(await catalog(), "Bad Id")) failures.push("profiles: an invalid id reached the store");

    await page.getByLabel("Profile id").fill("e2e-work");
    await page.getByLabel("Display name").fill("E2E Work");
    await page.getByLabel("Model", { exact: true }).fill("e2e-model");
    await page.getByRole("button", { name: "Create profile" }).click();
    await page.getByText("Created profile 'e2e-work'.").waitFor({ timeout: 10000 });
    if (!has(await catalog(), "e2e-work")) failures.push("profiles: the created profile is not in the store");

    await page.getByRole("heading", { name: "Profile: e2e-work" }).waitFor({ timeout: 10000 });
    await page.getByRole("button", { name: "Apply preset" }).click();
    await page.getByText("Applied research@1 to 'e2e-work'.").waitFor({ timeout: 10000 });

    const row = page.getByRole("listitem").filter({ hasText: "e2e-work" });
    await row.getByRole("button", { name: "Use", exact: true }).click();
    await page.getByText("'e2e-work' is now the active profile").waitFor({ timeout: 10000 });

    await row.getByRole("button", { name: "Delete…" }).click();
    const confirm = row.getByRole("button", { name: "Delete e2e-work" });
    if (await confirm.isEnabled()) failures.push("profiles: delete was enabled before the id was typed");
    await row.getByLabel(/Type e2e-work to confirm/).fill("e2e-work");
    await confirm.click();
    await page.getByText("Deleted profile 'e2e-work'").waitFor({ timeout: 10000 });
    if (has(await catalog(), "e2e-work")) failures.push("profiles: the deleted profile is still in the store");
    await page.waitForTimeout(500);
    if (await page.getByRole("heading", { name: "Profile: e2e-work" }).count()) {
      failures.push("profiles: the deleted profile's details are still shown");
    }

    // `default` is protected: the UI offers no delete for it.
    const defaultRow = page.getByRole("listitem").filter({ hasText: "default ·" });
    if (await defaultRow.getByRole("button", { name: "Delete…" }).count()) {
      failures.push("profiles: the protected default profile offers delete");
    }
    await page.screenshot({ path: `${outDir}/profiles.png`, fullPage: true });
  } catch (error) {
    failures.push(`profiles: ${error instanceof Error ? error.message : String(error)}`);
    await page.screenshot({ path: `${outDir}/profiles-failure.png`, fullPage: true }).catch(() => {});
  }
  if (errors.length) failures.push(`profiles: uncaught page error — ${errors[0]}`);
  await page.close();
  return failures;
}
