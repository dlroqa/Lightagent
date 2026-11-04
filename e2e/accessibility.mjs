// D8 accessibility gate in the browser (DESKTOP_ACCEPTANCE.md D8):
// keyboard-only navigation of every screen, and an axe scan of each with no
// critical violation. The rendered desktop WebViews get the same axe scan
// in desktop/e2e/native.py.
//
// For each screen, from the top of the page and with the keyboard only:
// Tab reaches the account menu, Enter opens it, Tab walks to the screen's
// item and Enter opens it. On the screen, Tab then walks its controls: the
// share reached and those showing no focus indicator are recorded.

import { readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";

const SCREENS = [
  ["Tools", "#/tools"],
  ["Memory", "#/memory"],
  ["Knowledge", "#/knowledge"],
  ["Identity", "#/admin/identity"],
  ["Remote credentials", "#/admin/credentials"],
  ["Diagnostics", "#/diagnostics"],
  ["Setup", "#/setup"],
  ["Settings", "#/settings"],
  // Reached from Settings: the chat screen's menu does not list it.
  ["Profiles", "#/profiles"],
];

const focusedName = () =>
  ((el) => (el ? (el.getAttribute("aria-label") || el.textContent || "").replace(/\s+/g, " ").trim() : ""))(document.activeElement);

async function tabUntil(page, wanted, limit) {
  for (let i = 0; i < limit; i += 1) {
    await page.keyboard.press("Tab");
    if ((await page.evaluate(focusedName)) === wanted) return true;
  }
  return false;
}

/** Open `item` from the account menu with the keyboard only. */
async function openByKeyboard(page, item) {
  await page.evaluate(() => {
    if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();
    window.scrollTo(0, 0);
  });
  if (!(await tabUntil(page, "Open account menu", 120))) throw new Error("Tab never reaches the account menu");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(250);
  const inMenu = (await page.evaluate(focusedName)) === item || (await tabUntil(page, item, 20));
  if (!inMenu) throw new Error(`Tab never reaches ${item} in the account menu`);
  await page.keyboard.press("Enter");
}

/** Tab through the screen: how many controls it reaches, and which show no focus indicator. */
async function walk(page) {
  const tabbable = await page.evaluate(() => [...document.querySelectorAll(
    'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
  )].filter((el) => el.getClientRects().length > 0 && getComputedStyle(el).visibility !== "hidden" && !el.closest("[inert], [aria-hidden='true']")).length);
  const reached = new Set();
  const unmarked = [];
  for (let step = 0; step < Math.min(tabbable + 10, 400); step += 1) {
    await page.keyboard.press("Tab");
    const seen = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const style = getComputedStyle(el);
      const rect = el.getBoundingClientRect();
      const ring = (style.outlineStyle !== "none" && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== "none";
      const name = (el.getAttribute("aria-label") || el.textContent || el.getAttribute("placeholder") || "").replace(/\s+/g, " ").trim().slice(0, 40);
      return { key: `${el.tagName}#${el.id}:${name}@${Math.round(rect.x)},${Math.round(rect.y + window.scrollY)}`, ring, name };
    });
    if (!seen) continue;
    if (reached.has(seen.key)) break;
    reached.add(seen.key);
    if (!seen.ring) unmarked.push(seen.name || seen.key);
  }
  return { tabbable, reached: reached.size, unmarked };
}

export async function checkAccessibility(browser, base, outDir) {
  const failures = [];
  const require = createRequire(import.meta.url);
  let axe = null;
  try {
    axe = await readFile(require.resolve("axe-core/axe.min.js"), "utf8");
  } catch {
    failures.push("accessibility: axe-core is not installed (scripts/render-panel.sh installs it on the runner)");
  }
  // bypassCSP: the scan's script is evaluated in the page; the app's CSP stays in force for the app.
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, bypassCSP: true });
  const page = await context.newPage();
  const report = {};
  try {
    for (const [item, route] of SCREENS) {
      const entry = (report[item] = {});
      try {
        await page.goto(`${base}/${item === "Profiles" ? "#/settings" : "#/"}`, { waitUntil: "domcontentloaded" });
        await page.getByRole("button", { name: "Open account menu" }).first().waitFor({ timeout: 15000 });
        await openByKeyboard(page, item);
        await page.waitForFunction((wanted) => location.hash.split("?")[0] === wanted, route, { timeout: 10000 });
        await page.waitForTimeout(600);
        entry.opened_by_keyboard = true;
        entry.walk = await walk(page);
        if (entry.walk.reached < Math.min(3, entry.walk.tabbable)) {
          failures.push(`accessibility: ${item}: Tab reaches only ${entry.walk.reached} of ${entry.walk.tabbable} controls`);
        }
        if (axe) {
          if (!(await page.evaluate(() => Boolean(window.axe)))) await page.evaluate(axe);
          entry.axe = await page.evaluate(async () => (await window.axe.run(document, { resultTypes: ["violations"] }))
            .violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help, nodes: v.nodes.slice(0, 4).map((n) => n.target.join(" ")) })));
          const critical = entry.axe.filter((v) => v.impact === "critical");
          if (critical.length) failures.push(`accessibility: ${item}: critical axe violations ${JSON.stringify(critical)}`);
        }
      } catch (err) {
        entry.error = err instanceof Error ? err.message.split("\n")[0] : String(err);
        failures.push(`accessibility: ${item}: ${entry.error}`);
        await page.screenshot({ path: `${outDir}/accessibility-failed-${item.replace(/\W+/g, "-")}.png` }).catch(() => {});
      }
    }
  } finally {
    await context.close();
    await writeFile(`${outDir}/accessibility.json`, JSON.stringify(report, null, 1));
  }
  if (!failures.length) console.log(`  [ok] accessibility: every screen opened with the keyboard only; no critical axe violation`);
  return failures;
}
