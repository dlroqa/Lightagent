// The Diagnostics screen against the real harness (D7-10): doctor (with
// the active home) and architecture show what the API reports; the bundle
// is prepared, shows what it holds and leaves out, and is saved as a file.
// The saved file itself is checked: exactly the allowlisted sections, and
// none of a memory, a session title, the harness's vault secret, the home
// path or a profile id seeded or present in the harness.

import { readFile, realpath } from "node:fs/promises";

const API = "/api/lightagent/v1";
const SECTIONS = ["schema", "about", "included", "excluded", "lightagent", "doctor", "architecture", "settings", "counts", "stores"];

async function call(base, method, path, body) {
  const response = await fetch(`${base}${API}${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return response.json();
}

export async function checkDiagnostics(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1600 }, acceptDownloads: true });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const marker = `render-diagnostics-private-${Date.now()}`;
  let memory = "";
  let session = "";
  try {
    const doctor = await call(base, "GET", "/diagnostics/doctor");
    const catalog = await call(base, "GET", "/profiles");
    memory = (await call(base, "POST", "/memories", { text: `${marker} memory` })).id ?? "";
    session = (await call(base, "POST", "/sessions", {})).id ?? "";
    if (session) await call(base, "PATCH", `/sessions/${session}`, { title: `${marker} title` });

    await page.goto(`${base}/#/diagnostics`);
    const screen = page.getByTestId("diagnostics-screen");
    await screen.waitFor({ timeout: 15000 });
    await page.getByTestId("diagnostics-doctor").getByText(String(doctor.home), { exact: true }).waitFor({ timeout: 15000 });
    // The home shown is the one LIGHTAGENT_HOME names (render-panel.sh exports it).
    const named = process.env.LIGHTAGENT_HOME;
    const resolve = (path) => realpath(path).catch(() => path);
    if (!named || (await resolve(String(doctor.home))) !== (await resolve(named))) {
      failures.push(`diagnostics: the home shown (${doctor.home}) is not LIGHTAGENT_HOME (${named})`);
    }
    await page.getByTestId("diagnostics-architecture").waitFor({ timeout: 10000 });

    await screen.getByRole("button", { name: "Prepare bundle" }).click();
    const shown = page.getByTestId("diagnostics-bundle");
    await shown.getByText("Left out", { exact: true }).waitFor({ timeout: 15000 });
    await page.screenshot({ path: `${outDir}/diagnostics-bundle.png`, fullPage: true });

    const [download] = await Promise.all([
      page.waitForEvent("download", { timeout: 15000 }),
      screen.getByRole("button", { name: "Save bundle…" }).click(),
    ]);
    const file = await download.path();
    const text = await readFile(file, "utf8");
    const saved = JSON.parse(text);
    const sections = Object.keys(saved).sort();
    if (JSON.stringify(sections) !== JSON.stringify([...SECTIONS].sort())) {
      failures.push(`diagnostics: the saved bundle's sections are ${JSON.stringify(sections)}`);
    }
    const forbidden = [marker, "render-previous-secret", String(doctor.home), ...catalog.profiles.map((p) => `"${p.id}"`)];
    for (const item of forbidden) {
      if (text.includes(item)) failures.push(`diagnostics: the saved bundle holds ${JSON.stringify(item)}`);
    }
    if ((saved.counts?.active_profile_memories ?? 0) < 1) {
      failures.push(`diagnostics: the bundle counts no memory: ${JSON.stringify(saved.counts)}`);
    }
    await page.getByRole("status").filter({ hasText: "It was not sent anywhere." }).waitFor({ timeout: 10000 });
    if (errors.length) failures.push(`diagnostics: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`diagnostics: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/diagnostics-failed.png`, fullPage: true }).catch(() => {});
  } finally {
    await page.close();
    if (memory) await call(base, "DELETE", `/memories/items/${memory}`).catch(() => {});
    if (session) await call(base, "DELETE", `/sessions/${session}`).catch(() => {});
  }
  return failures;
}
