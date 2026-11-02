// The skills import on the Tools screen against the real harness (D6-6,
// new scope): an import leaves "available to runs" unchecked, installs the
// skills inactive (they are not listed until enabled), says so, and the
// Extensions card shows the new extension as inactive.

import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PACK = "render-import-pack";
const SKILL = "render-import-skill";

export async function checkSkillImport(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-import-"));
  const pack = join(dir, PACK);
  await mkdir(join(pack, "skills", SKILL), { recursive: true });
  await writeFile(join(pack, "skills", SKILL, "SKILL.md"), `---\nname: ${SKILL}\ndescription: Imported\n---\nDo it.\n`);
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(`${base}/#/tools`);
    const form = page.getByTestId("skill-import");
    await form.waitFor({ timeout: 15000 });
    await page.locator("#skill-import-source").fill(pack);
    if (await form.getByRole("checkbox", { name: "Make them available to runs now" }).isChecked()) {
      failures.push("skills: activation starts checked");
    }
    await form.getByRole("button", { name: "Import", exact: true }).click();
    const done = form.getByRole("status").filter({ hasText: `as the extension ${PACK}` });
    await done.waitFor({ timeout: 20000 });
    if (!(await done.innerText()).includes("inactive")) failures.push("skills: the import does not say it is inactive");
    const skills = await (await fetch(`${base}/api/lightagent/v1/skills`)).json();
    if ((skills.skills ?? []).some((skill) => skill.name === SKILL)) failures.push("skills: an inactive import is listed for runs");
    const row = page.getByTestId(`extension-${PACK}`);
    await row.waitFor({ timeout: 10000 });
    if (!(await row.innerText()).includes("Inactive")) failures.push("skills: the Extensions card does not show it inactive");
    await page.screenshot({ path: `${outDir}/skills-imported.png`, fullPage: true });
    if (errors.length) failures.push(`skills: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`skills: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    await fetch(`${base}/api/lightagent/v1/extensions/${PACK}`, { method: "DELETE" }).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
