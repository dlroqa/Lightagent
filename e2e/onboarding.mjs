// Profile guidance (the chat's `/onboard`) against the real harness: the
// Guidance card on the Profiles screen refuses a missing file and installs
// nothing, installs a Markdown file and shows it, replaces it, and removes
// it after asking (Keep it keeps it). The API confirms each step; whatever
// was installed before, and the extensions switch, are put back.
// crates/lightagent/tests/desktop_onboarding.rs proves the chat and the API
// act on one bundle; desktop/e2e/runs.py `onboarding` covers the host.

import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "/api/lightagent/v1/onboarding";

export async function checkOnboarding(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-guidance-"));
  const page = await browser.newPage({ viewport: { width: 1280, height: 1800 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const current = async () => (await fetch(`${base}${API}`)).json();
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`onboarding: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/onboarding-failed-${name.replace(/\W+/g, "-")}.png`, fullPage: true }).catch(() => {});
    }
  };
  const before = await current();
  const switchBefore = (await (await fetch(`${base}/api/lightagent/v1/config/extensions.enabled`)).json()).value;
  const field = page.getByLabel("Markdown file (absolute path on this machine)", { exact: true });
  const submit = (name) => page.getByRole("button", { name, exact: true }).click({ timeout: 10000 });
  try {
    if (before.installed) await fetch(`${base}${API}`, { method: "DELETE" });
    await page.goto(`${base}/#/profiles`);
    await page.getByTestId("guidance-none").waitFor({ timeout: 15000 });

    await step("a missing file is refused and installs nothing", async () => {
      await field.fill(join(dir, "missing.md"));
      await submit("Install guidance");
      await page.getByRole("alert").filter({ hasText: "could not read Markdown file" }).waitFor({ timeout: 10000 });
      if ((await current()).installed) throw new Error("a missing file installed guidance");
    });

    await step("install from a Markdown file", async () => {
      const first = join(dir, "first.md");
      await writeFile(first, "Answer as a heron would.");
      await field.fill(first);
      await submit("Install guidance");
      await page.getByRole("status").filter({ hasText: "Installed the guidance." }).waitFor({ timeout: 10000 });
      const shown = page.getByTestId("guidance-current");
      await shown.locator("summary").click();
      await shown.getByText("Answer as a heron would.", { exact: true }).waitFor({ timeout: 5000 });
      if ((await current()).text !== "Answer as a heron would.") throw new Error("the API does not show the guidance");
      await page.screenshot({ path: `${outDir}/onboarding-installed.png`, fullPage: true });
    });

    await step("replace it", async () => {
      const second = join(dir, "second.md");
      await writeFile(second, "Answer as an egret would.");
      await field.fill(second);
      await submit("Replace guidance");
      await page.getByTestId("guidance-current").getByText("Answer as an egret would.", { exact: true })
        .waitFor({ timeout: 10000 });
      if ((await current()).text !== "Answer as an egret would.") throw new Error("the guidance was not replaced");
    });

    await step("remove asks first", async () => {
      await submit("Remove guidance…");
      await submit("Keep it");
      if (!(await current()).installed) throw new Error("Keep it removed the guidance");
      await submit("Remove guidance…");
      await submit("Confirm: remove guidance");
      await page.getByTestId("guidance-none").waitFor({ timeout: 10000 });
      if ((await current()).installed) throw new Error("Confirm did not remove the guidance");
    });
    if (errors.length) failures.push(`onboarding: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`onboarding: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    if (before.installed) {
      const restore = join(dir, "restore.md");
      await writeFile(restore, before.text ?? "");
      await fetch(`${base}${API}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ path: restore }),
      }).catch(() => {});
    } else {
      await fetch(`${base}${API}`, { method: "DELETE" }).catch(() => {});
    }
    await fetch(`${base}/api/lightagent/v1/config/extensions.enabled`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ value: String(switchBefore) }),
    }).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
