// The Setup screen against the real harness (D7-11): Initialise on the
// initialised harness is refused with the server's words; Reconfigure asks
// first, and with the current profile and endpoint keeps both; a Hermes
// home is previewed (nothing written), imported, and a second import
// reports what it skipped. The imported profile is deleted afterwards.
// crates/lightagent/tests/desktop_setup.rs proves the API is the CLI;
// desktop/e2e/runs.py `setup` covers the host.

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "/api/lightagent/v1";

async function get(base, path) {
  return (await fetch(`${base}${API}${path}`)).json();
}

export async function checkSetup(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-hermes-"));
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const screen = page.getByTestId("setup-screen");
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`setup: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/setup-failed-${name.replace(/\W+/g, "-")}.png`, fullPage: true }).catch(() => {});
    }
  };
  let imported = false;
  try {
    const before = await get(base, "/profiles");
    const endpoint = (await get(base, "/config")).config.inference.base_url;
    await page.goto(`${base}/#/setup`);
    await screen.waitFor({ timeout: 15000 });
    await page.locator("#setup-profile").and(page.locator(`[value="${before.active_profile}"]`)).waitFor({ timeout: 10000 });
    await page.locator("#setup-base-url").and(page.locator(`[value="${endpoint}"]`)).waitFor({ timeout: 10000 });

    await step("initialise is refused on an initialised home", async () => {
      await screen.getByRole("button", { name: "Initialise", exact: true }).click();
      await page.getByTestId("setup-init-error").filter({ hasText: "already initialised" }).waitFor({ timeout: 10000 });
    });

    await step("reconfigure asks first and keeps the profile and endpoint", async () => {
      await screen.getByRole("button", { name: "Reconfigure…" }).click();
      await screen.getByRole("button", { name: "Keep it" }).click();
      await screen.getByRole("button", { name: "Reconfigure…" }).click();
      await screen.getByRole("button", { name: /^Confirm: reconfigure / }).click();
      await page.getByTestId("setup-message").filter({ hasText: `active profile ${before.active_profile}` }).waitFor({ timeout: 10000 });
      if ((await get(base, "/profiles")).active_profile !== before.active_profile) throw new Error("the active profile changed");
      if ((await get(base, "/config")).config.inference.base_url !== endpoint) throw new Error("the endpoint changed");
    });

    await step("preview and import from a Hermes home", async () => {
      const profile = join(dir, "profiles", "render-hermes");
      await mkdir(profile, { recursive: true });
      await writeFile(join(profile, "SOUL.md"), "You came from Hermes in the render.\n");
      await page.locator("#setup-hermes-from").fill(dir);
      await screen.getByRole("button", { name: "Preview" }).click();
      const result = page.getByTestId("setup-hermes-result");
      await result.getByText("Would import 1 profile(s)", { exact: false }).waitFor({ timeout: 10000 });
      if ((await get(base, "/profiles")).profiles.some((p) => p.id === "render-hermes")) throw new Error("the preview wrote");
      await screen.getByRole("button", { name: "Import", exact: true }).click();
      await result.getByText("Imported 1 profile(s)", { exact: false }).waitFor({ timeout: 10000 });
      imported = true;
      if (!(await get(base, "/profiles")).profiles.some((p) => p.id === "render-hermes")) throw new Error("not imported");
      await screen.getByRole("button", { name: "Import", exact: true }).click();
      await result.getByText("Skipped (already here): profile render-hermes", { exact: false }).waitFor({ timeout: 10000 });
      await page.screenshot({ path: `${outDir}/setup-imported.png`, fullPage: true });
    });

    if (errors.length) failures.push(`setup: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`setup: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    if (imported) await fetch(`${base}${API}/profiles/render-hermes`, { method: "DELETE" }).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
