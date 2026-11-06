// The Diagnostics screen's Updates card against the real harness (D7-12):
// render-panel.sh serves a controlled release API on loopback naming
// v99.0.0, so Check for updates reports that release as available. It is
// read-only: the API reports what `update --check --json` prints.
// crates/lightagent/tests/desktop_update_check.rs is the fixture evidence
// for every answer; .github/workflows/update-check-live.yml is the live one.

export async function checkUpdate(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(`${base}/#/diagnostics`);
    const screen = page.getByTestId("diagnostics-screen");
    await screen.waitFor({ timeout: 15000 });
    await screen.getByText("Checking installs nothing", { exact: false }).waitFor({ timeout: 10000 });
    await screen.getByRole("button", { name: "Check for updates" }).click();
    await page.getByTestId("diagnostics-update").getByText("Lightagent 99.0.0 is available", { exact: false })
      .waitFor({ timeout: 15000 });
    const report = await (await fetch(`${base}/api/lightagent/v1/update/check`)).json();
    if (report.latest !== "99.0.0" || report.update_available !== true) {
      failures.push(`update: the API reports ${JSON.stringify(report)}`);
    }
    await page.screenshot({ path: `${outDir}/diagnostics-update.png`, fullPage: true });
    if (errors.length) failures.push(`update: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`update: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/update-failed.png`, fullPage: true }).catch(() => {});
  } finally {
    await page.close();
  }
  return failures;
}
