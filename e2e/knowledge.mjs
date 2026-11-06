// The Knowledge screen against the real harness (D7-4): index a folder;
// both documents listed with chunks; search finds the passage; a missing
// path is shown as skipped with its reason; clear asks first, and is only
// confirmed when the harness index was empty before the check.
// crates/lightagent/tests/desktop_knowledge.rs proves the API's index is
// the CLI's.

import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

async function indexNow(base) {
  return (await fetch(`${base}/api/lightagent/v1/knowledge`)).json();
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  return Boolean(await probe());
}

export async function checkKnowledge(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-knowledge-"));
  await writeFile(join(dir, "render-probe-rivers.md"), "Render probe rivers carry water from glaciers to the sea.\n");
  await writeFile(join(dir, "render-probe-hills.md"), "Render probe hills are gentle slopes covered with heather.\n");
  const empty = ((await indexNow(base)).sources ?? []).length === 0;
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const screen = page.getByTestId("knowledge-screen");
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`knowledge: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/knowledge-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  try {
    await page.goto(`${base}/#/knowledge`);
    await screen.waitFor({ timeout: 15000 });

    await step("index a folder", async () => {
      await page.locator("#knowledge-path").fill(dir);
      await screen.getByRole("button", { name: "Index", exact: true }).click();
      await page.getByTestId("knowledge-added").getByText("render-probe-rivers.md", { exact: false }).waitFor({ timeout: 15000 });
      const names = ((await indexNow(base)).sources ?? []).map((source) => source.source);
      if (!names.includes("render-probe-rivers.md") || !names.includes("render-probe-hills.md")) {
        throw new Error(`indexed ${JSON.stringify(names)}`);
      }
      await page.getByTestId("knowledge-sources").getByText("render-probe-hills.md", { exact: false }).waitFor({ timeout: 10000 });
      await page.screenshot({ path: `${outDir}/knowledge-indexed.png`, fullPage: true });
    });

    await step("search finds the passage", async () => {
      await page.locator("#knowledge-query").fill("glaciers sea");
      await page.locator("#knowledge-top-k").fill("2");
      await screen.getByRole("button", { name: "Search documents" }).click();
      await page.getByTestId("knowledge-hits").getByText("render-probe-rivers.md", { exact: false }).first().waitFor({ timeout: 10000 });
    });

    await step("a source name names the document", async () => {
      const named = join(dir, "named");
      await mkdir(named);
      const file = join(named, "render-probe-lakes.md");
      await writeFile(file, "Render probe lakes hold still water between hills.\n");
      await page.locator("#knowledge-path").fill(file);
      await page.locator("#knowledge-source").fill("render-probe-named");
      await screen.getByRole("button", { name: "Index", exact: true }).click();
      await page.getByTestId("knowledge-added").getByText("Indexed render-probe-named", { exact: false }).waitFor({ timeout: 10000 });
      const names = ((await indexNow(base)).sources ?? []).map((source) => source.source);
      if (!names.includes("render-probe-named") || names.includes("render-probe-lakes.md")) {
        throw new Error(`indexed ${JSON.stringify(names)}`);
      }
      await page.getByTestId("knowledge-sources").getByText("render-probe-named", { exact: false }).waitFor({ timeout: 10000 });
      await page.locator("#knowledge-source").fill("");
    });

    await step("a missing path is shown as skipped", async () => {
      await page.locator("#knowledge-path").fill(join(dir, "missing.md"));
      await screen.getByRole("button", { name: "Index", exact: true }).click();
      await page.getByTestId("knowledge-added").getByText("Skipped", { exact: false }).waitFor({ timeout: 10000 });
    });

    await step("clear asks first", async () => {
      await screen.getByRole("button", { name: "Clear the index…" }).click();
      await screen.getByRole("button", { name: "Keep the index" }).click();
      if (((await indexNow(base)).sources ?? []).length === 0) throw new Error("Keep the index cleared it");
      if (empty) {
        await screen.getByRole("button", { name: "Clear the index…" }).click();
        await screen.getByRole("button", { name: /^Confirm: clear \d+ chunks$/ }).click();
        if (!(await until(async () => ((await indexNow(base)).sources ?? []).length === 0))) {
          throw new Error("Confirm did not clear the index");
        }
      }
    });

    if (errors.length) failures.push(`knowledge: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`knowledge: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
