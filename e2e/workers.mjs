// The Specialists card against the real harness (D7-6): a role is
// assigned to the built-in worker, and revoking it reports that there was
// an assignment, then that there is none. crates/lightagent/tests/
// desktop_workers.rs proves the API's assignments are the CLI's.

export async function checkWorkers(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1500 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
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
    if (errors.length) failures.push(`workers: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`workers: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/workers-failed.png` }).catch(() => {});
  } finally {
    await page.close();
    await fetch(`${base}/api/lightagent/v1/workers/coder`, { method: "DELETE" }).catch(() => {});
  }
  return failures;
}
