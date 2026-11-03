// The Memory screen against the real harness (D7-2): remember with kind
// and tags; search; edit; the knowledge page; keep a seeded conversation's
// suggested fact with its source, kind and tag; forget asks first; "Forget
// everything" can be cancelled. crates/lightagent/tests/desktop_memory.rs
// proves the API's memory is the CLI's.

import { seededSession } from "./delivery.mjs";

const PROBE = "Render probe prefers tabs for indentation";
const EDITED = "Render probe prefers tabs for indentation everywhere";
const SEED = "Remember that I prefer short answers in render probes";

async function memoriesNow(base) {
  const response = await fetch(`${base}/api/lightagent/v1/memories`);
  return (await response.json()).memories ?? [];
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  return Boolean(await probe());
}

export async function checkMemory(browser, base, outDir) {
  const failures = [];
  const before = new Set((await memoriesNow(base)).map((memory) => memory.id));
  const page = await browser.newPage({ viewport: { width: 1280, height: 1600 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`memory: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/memory-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  const find = async (text) => (await memoriesNow(base)).find((memory) => memory.text === text);
  try {
    const sessionId = await seededSession(base, SEED);
    await page.goto(`${base}/#/memory`);
    await page.getByTestId("memory-screen").waitFor({ timeout: 15000 });

    await step("remember with kind and tags", async () => {
      await page.locator("#memory-text").fill(PROBE);
      await page.locator("#memory-kind").fill("preference");
      await page.locator("#memory-tags").fill("style, probe");
      await page.getByRole("button", { name: "Remember", exact: true }).click();
      if (!(await until(async () => Boolean(await find(PROBE))))) throw new Error("the memory was not saved");
      const saved = await find(PROBE);
      if (saved.kind !== "preference" || JSON.stringify(saved.tags) !== '["style","probe"]') {
        throw new Error(`saved as ${JSON.stringify(saved)}`);
      }
      await page.getByTestId(`memory-${saved.id}`).waitFor({ timeout: 10000 });
      await page.screenshot({ path: `${outDir}/memory-list.png`, fullPage: true });
    });

    await step("search finds it", async () => {
      await page.locator("#memory-query").fill("tabs indentation");
      await page.locator("#memory-top-k").fill("3");
      await page.getByRole("button", { name: "Search", exact: true }).click();
      await page.getByTestId("memory-hits").getByText(PROBE, { exact: false }).waitFor({ timeout: 10000 });
    });

    await step("edit saves", async () => {
      const saved = await find(PROBE);
      const row = page.getByTestId(`memory-${saved.id}`);
      await row.getByRole("button", { name: `Edit: ${PROBE}` }).click();
      await row.getByRole("textbox", { name: "Text" }).fill(EDITED);
      await row.getByRole("button", { name: "Save", exact: true }).click();
      if (!(await until(async () => Boolean(await find(EDITED))))) throw new Error("the edit was not saved");
    });

    await step("the knowledge page shows it", async () => {
      await page.getByRole("button", { name: "Show knowledge" }).click();
      await page.getByTestId("memory-knowledge").getByText("tabs", { exact: false }).waitFor({ timeout: 10000 });
    });

    await step("a conversation's fact is kept with its source", async () => {
      await page.locator("#memory-session").selectOption(sessionId);
      await page.getByRole("button", { name: "Suggest facts" }).click();
      await page.getByTestId("memory-candidates").getByText(SEED, { exact: false }).waitFor({ timeout: 10000 });
      await page.locator("#memory-promote-kind").fill("decision");
      await page.locator("#memory-promote-tags").fill("probe");
      await page.getByRole("button", { name: "Remember message 1" }).click();
      const kept = async () => (await memoriesNow(base)).find((memory) => memory.source?.session_id === sessionId);
      if (!(await until(async () => Boolean(await kept())))) throw new Error("the suggested fact was not kept");
      const memory = await kept();
      if (memory.kind !== "decision" || !memory.tags?.includes("probe") || memory.source.message_index !== 1) {
        throw new Error(`kept as ${JSON.stringify(memory)}`);
      }
    });

    await step("forget asks first", async () => {
      const saved = await find(EDITED);
      const row = page.getByTestId(`memory-${saved.id}`);
      await row.getByRole("button", { name: `Forget: ${EDITED}` }).click();
      await row.getByRole("button", { name: "Keep it" }).click();
      if (!(await find(EDITED))) throw new Error("Keep it forgot it");
      await row.getByRole("button", { name: `Forget: ${EDITED}` }).click();
      await row.getByRole("button", { name: "Confirm: forget it" }).click();
      if (!(await until(async () => !(await find(EDITED))))) throw new Error("Confirm did not forget it");
    });

    await step("forget everything can be cancelled", async () => {
      const count = (await memoriesNow(base)).length;
      await page.getByRole("button", { name: "Forget everything…" }).click();
      await page.getByRole("button", { name: /^Confirm: forget all / }).waitFor({ timeout: 5000 });
      await page.getByRole("button", { name: "Keep my memories" }).click();
      if ((await memoriesNow(base)).length !== count) throw new Error("cancelling forgot memories");
    });

    if (errors.length) failures.push(`memory: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`memory: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    // Leave the harness's memories as they were.
    for (const memory of await memoriesNow(base).catch(() => [])) {
      if (!before.has(memory.id)) {
        await fetch(`${base}/api/lightagent/v1/memories/items/${encodeURIComponent(memory.id)}`, { method: "DELETE" }).catch(() => {});
      }
    }
  }
  return failures;
}
