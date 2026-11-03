// Saved conversations against the real harness (D7-5): a seeded
// conversation is listed and opens with its message; Delete asks first;
// Keep it keeps it; Confirm deletes it from the server and the list.
// crates/lightagent/tests/desktop_sessions.rs proves the API's sessions are
// the CLI's.

import { seededSession } from "./delivery.mjs";

const TITLE = "Sessions probe conversation";

export async function checkSessions(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  let id = null;
  const exists = async () => (await fetch(`${base}/api/lightagent/v1/sessions/${id}`)).status === 200;
  try {
    id = await seededSession(base, TITLE);
    const listed = (await (await fetch(`${base}/api/lightagent/v1/sessions`)).json()).sessions ?? [];
    const label = listed.find((session) => session.id === id)?.title || TITLE;
    await page.goto(`${base}/#/`);
    const row = page.getByRole("button", { name: label, exact: true }).first();
    await row.waitFor({ timeout: 15000 });
    await row.click();
    await page.getByText(TITLE, { exact: false }).first().waitFor({ timeout: 15000 });

    await page.getByRole("button", { name: `More actions for ${label}` }).first().click();
    await page.getByRole("menuitem", { name: /Delete/ }).click();
    const confirm = page.getByRole("button", { name: `Confirm: delete ${label}` });
    await confirm.waitFor({ timeout: 5000 });
    await page.screenshot({ path: `${outDir}/sessions-confirm-delete.png` });
    if (!(await exists())) failures.push("sessions: Delete removed the conversation before it was confirmed");
    await page.getByRole("button", { name: "Keep it" }).click();
    if (!(await exists())) failures.push("sessions: Keep it removed the conversation");

    await page.getByRole("button", { name: `More actions for ${label}` }).first().click();
    await page.getByRole("menuitem", { name: /Delete/ }).click();
    await page.getByRole("button", { name: `Confirm: delete ${label}` }).click();
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline && (await exists())) await new Promise((resolve) => setTimeout(resolve, 150));
    if (await exists()) failures.push("sessions: Confirm did not delete the conversation");
    await page.getByRole("button", { name: label, exact: true }).waitFor({ state: "detached", timeout: 10000 })
      .catch(() => failures.push("sessions: the deleted conversation stayed in the list"));
    if (errors.length) failures.push(`sessions: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`sessions: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    if (id) await fetch(`${base}/api/lightagent/v1/sessions/${id}`, { method: "DELETE" }).catch(() => {});
  }
  return failures;
}
