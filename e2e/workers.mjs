// The Specialists card against the real harness (D7-6): a role is
// assigned to the built-in worker, and revoking it reports that there was
// an assignment, then that there is none. With a second profile, "Workers
// of" selects it (--profile) and a role is assigned to the active profile
// (--worker) and revoked; the API confirms both for the profile named, and
// that the active profile's workers are untouched.
// crates/lightagent/tests/desktop_workers.rs proves the API's assignments
// are the CLI's.

const HELPER = "render-helper";

async function workersOf(base, profile) {
  const query = profile ? `?profile=${encodeURIComponent(profile)}` : "";
  const body = await (await fetch(`${base}/api/lightagent/v1/workers${query}`)).json();
  return body.workers ?? [];
}

function workerOf(workers, role) {
  return String(workers.find((worker) => worker.role === role)?.worker ?? "");
}

export async function checkWorkers(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1500 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  let created = false;
  try {
    await page.goto(`${base}/#/tools`);
    const card = page.getByTestId("specialists");
    await card.waitFor({ timeout: 15000 });
    const row = page.getByTestId("worker-coder");
    await row.waitFor({ timeout: 10000 });
    await row.getByRole("combobox").selectOption("builtin");
    await row.getByRole("button", { name: "Assign coder" }).click();
    await card.getByRole("status").filter({ hasText: "Assigned coder to the built-in worker." }).waitFor({ timeout: 10000 });
    await page.screenshot({ path: `${outDir}/workers-assigned.png`, fullPage: true });
    await row.getByRole("button", { name: "Revoke coder" }).click();
    await card.getByRole("status").filter({ hasText: "Revoked the coder assignment." }).waitFor({ timeout: 10000 });
    await row.getByRole("button", { name: "Revoke coder" }).click();
    await card.getByRole("status").filter({ hasText: "There was no coder assignment." }).waitFor({ timeout: 10000 });

    // Another profile's workers (--profile), assigned to a profile (--worker).
    const active = (await (await fetch(`${base}/api/lightagent/v1/profiles`)).json()).active_profile;
    const made = await fetch(`${base}/api/lightagent/v1/profiles`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: HELPER }),
    });
    if (!made.ok) throw new Error(`could not create the profile ${HELPER}: ${made.status}`);
    created = true;
    const before = workerOf(await workersOf(base), "coder");
    await page.reload();
    await card.waitFor({ timeout: 15000 });
    await page.locator("#workers-lead").selectOption(HELPER);
    // Under the active profile its own id is not offered as a target;
    // under the helper it is, so this waits for the helper's rows.
    await row.locator(`option[value="${active}"]`).waitFor({ state: "attached", timeout: 10000 });
    await row.getByRole("combobox").selectOption(active);
    await row.getByRole("button", { name: "Assign coder" }).click();
    await card.getByRole("status").filter({ hasText: `Assigned coder to profile ${active}.` }).waitFor({ timeout: 10000 });
    if (!workerOf(await workersOf(base, HELPER), "coder").startsWith("assigned:")) {
      throw new Error(`the ${HELPER} coder is not assigned: ${JSON.stringify(await workersOf(base, HELPER))}`);
    }
    if (workerOf(await workersOf(base), "coder") !== before) {
      throw new Error("assigning for another profile changed the active profile's coder");
    }
    await page.screenshot({ path: `${outDir}/workers-other-profile.png`, fullPage: true });
    await row.getByRole("button", { name: "Revoke coder" }).click();
    await card.getByRole("status").filter({ hasText: "Revoked the coder assignment." }).waitFor({ timeout: 10000 });
    if (workerOf(await workersOf(base, HELPER), "coder").startsWith("assigned:")) {
      throw new Error(`the ${HELPER} coder is still assigned after Revoke`);
    }
    if (errors.length) failures.push(`workers: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`workers: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/workers-failed.png` }).catch(() => {});
  } finally {
    await page.close();
    await fetch(`${base}/api/lightagent/v1/workers/coder`, { method: "DELETE" }).catch(() => {});
    if (created) {
      await fetch(`${base}/api/lightagent/v1/workers/coder?profile=${HELPER}`, { method: "DELETE" }).catch(() => {});
      await fetch(`${base}/api/lightagent/v1/profiles/${HELPER}`, { method: "DELETE" }).catch(() => {});
    }
  }
  return failures;
}
