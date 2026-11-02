// The Tools screen's catalog and skills against the real harness (D6-5):
// each tool shows its permission in words, as the API (and so `tools
// list`) reports it; nothing on the screen asks the server to connect MCP
// servers; an extension's skill is listed with its source.

import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const LABEL = { auto: "Runs without asking", ask: "Asks first", block: "Blocked" };
const EXTENSION = "render-probe-skills";

export async function checkToolCatalog(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-skills-"));
  const bundle = join(dir, EXTENSION);
  await mkdir(join(bundle, "skills", "render-probe-skill"), { recursive: true });
  await writeFile(join(bundle, "extension.json"), JSON.stringify({ name: EXTENSION, version: "1.0.0" }));
  await writeFile(
    join(bundle, "skills", "render-probe-skill", "SKILL.md"),
    "---\nname: render-probe-skill\ndescription: Probe skill\nversion: 2.1.0\n---\nDo it.\n",
  );
  const page = await browser.newPage({ viewport: { width: 1280, height: 1300 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const toolRequests = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (url.pathname.endsWith("/api/lightagent/v1/tools")) toolRequests.push(url.search);
  });
  try {
    const installed = await fetch(`${base}/api/lightagent/v1/extensions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source: bundle }),
    });
    if (installed.status !== 201) throw new Error(`the probe extension did not install (${installed.status})`);
    const catalog = await (await fetch(`${base}/api/lightagent/v1/tools?mcp=skip`)).json();
    for (const hash of ["#/", "#/settings", "#/tools"]) {
      await page.goto(`${base}/${hash}`);
      await page.waitForLoadState("networkidle").catch(() => {});
    }
    await page.getByText("datetime.now", { exact: true }).first().waitFor({ timeout: 15000 });
    const connecting = toolRequests.filter((search) => !search.includes("mcp=skip"));
    if (connecting.length) failures.push(`tools: ${connecting.length} catalog request(s) asked to connect MCP servers`);
    for (const tool of catalog.tools) {
      const label = LABEL[tool.permission];
      if (!label) {
        failures.push(`tools: ${tool.name} has permission ${tool.permission}`);
        continue;
      }
      const row = page.locator("div", { has: page.getByText(tool.name, { exact: true }) }).filter({ hasText: label });
      if ((await row.count()) === 0) failures.push(`tools: ${tool.name} does not show "${label}"`);
    }
    const skill = page.locator("div", { has: page.getByText("render-probe-skill", { exact: true }) });
    await skill.filter({ hasText: `Extension ${EXTENSION}` }).first().waitFor({ timeout: 10000 })
      .catch(() => failures.push("tools: the extension's skill is not listed with its source"));
    if (!(await skill.first().innerText().catch(() => "")).includes("2.1.0")) {
      failures.push("tools: the skill's version is not shown");
    }
    await page.screenshot({ path: `${outDir}/tools-permissions.png`, fullPage: true });
    if (errors.length) failures.push(`tools: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`tools: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    await fetch(`${base}/api/lightagent/v1/extensions/${EXTENSION}`, { method: "DELETE" }).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
