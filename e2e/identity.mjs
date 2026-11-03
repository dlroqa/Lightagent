// The Identity screen against the real harness (D7-8): a user is created;
// a credential is issued to a new file (its secret is in neither the page
// nor browser storage; an existing file is refused and left unchanged),
// listed for its user, rotated and revoked (Revoke asks first); a profile
// is assigned, reassigned (asks first) and freed; a grant is revoked; a
// user is suspended (asks first); the audit shows the records asked for.
// crates/lightagent/tests/desktop_identity.rs proves the API's store is
// the CLI's, and desktop/e2e/runs.py `identity` covers the host.

import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "/api/lightagent/v1/identity";

async function get(base, path) {
  return (await fetch(`${base}${path}`)).json();
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  return Boolean(await probe());
}

export async function checkIdentity(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-identity-"));
  const page = await browser.newPage({ viewport: { width: 1280, height: 2200 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const screen = page.getByTestId("identity-screen");
  const announced = (text) => page.getByTestId("identity-message").filter({ hasText: text }).waitFor({ timeout: 10000 });
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`identity: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/identity-failed-${name.replace(/\W+/g, "-")}.png`, fullPage: true }).catch(() => {});
    }
  };
  const secrets = [];
  const noSecretShown = async (where) => {
    const seen = await page.evaluate(() => {
      const stored = (storage) => Object.keys(storage).map((key) => `${key}=${storage.getItem(key)}`).join("\n");
      return `${document.documentElement.outerHTML}\n${stored(localStorage)}\n${stored(sessionStorage)}`;
    });
    for (const secret of secrets) {
      if (seen.includes(secret)) throw new Error(`a credential's secret is in the page or browser storage (${where})`);
    }
  };
  let keyId = "";
  let active = "";
  try {
    active = (await get(base, "/api/lightagent/v1/profiles")).active_profile;
    await page.goto(`${base}/#/admin/identity`);
    await screen.waitFor({ timeout: 15000 });
    await page.getByTestId("identity-status").waitFor({ timeout: 15000 });

    await step("create users", async () => {
      for (const id of ["render-alice", "render-bob"]) {
        await page.locator("#identity-user-id").fill(id);
        await screen.getByRole("button", { name: "Create user" }).click();
        await announced(`Created user ${id}.`);
        await page.getByTestId(`user-${id}`).waitFor({ timeout: 10000 });
      }
    });

    await step("issue a credential to a new file", async () => {
      const file = join(dir, "alice.key");
      await page.locator("#identity-issue-principal").selectOption("render-alice");
      await screen.getByLabel("runs_read", { exact: true }).check();
      await page.locator("#identity-issue-expires").fill("600");
      await page.locator("#identity-issue-profiles").fill(active);
      await page.locator("#identity-issue-file").fill(file);
      await screen.getByRole("button", { name: "Issue credential" }).click();
      await announced("Issued ");
      const text = await page.getByTestId("identity-message").innerText();
      keyId = /Issued ([0-9a-f]{16})/.exec(text)?.[1] ?? "";
      if (!keyId) throw new Error(`no key id in "${text}"`);
      const token = (await readFile(file, "utf8")).trim();
      if (!token.startsWith(`la_${keyId}_`)) throw new Error("the file does not hold the issued credential");
      const issuedRow = (await get(base, `${API}/credentials`)).credentials.find((c) => c.key_id === keyId);
      if (JSON.stringify(issuedRow?.restriction?.profiles) !== JSON.stringify([active])) {
        throw new Error(`the credential's restriction is ${JSON.stringify(issuedRow?.restriction)}`);
      }
      secrets.push(token.split("_").pop());
      await noSecretShown("after issue");
      await page.screenshot({ path: `${outDir}/identity-issued.png`, fullPage: true });
    });

    await step("an existing file is refused and left unchanged", async () => {
      const file = join(dir, "alice.key");
      const before = await readFile(file, "utf8");
      const count = (await get(base, `${API}/credentials`)).credentials.length;
      await page.locator("#identity-issue-principal").selectOption("render-alice");
      await page.locator("#identity-issue-file").fill(file);
      await screen.getByRole("button", { name: "Issue credential" }).click();
      await page.getByTestId("identity-error").filter({ hasText: "could not create the secret file" }).waitFor({ timeout: 10000 });
      if ((await readFile(file, "utf8")) !== before) throw new Error("the existing file was changed");
      if ((await get(base, `${API}/credentials`)).credentials.length !== count) throw new Error("a credential was issued anyway");
      if ((await page.locator("#identity-issue-file").inputValue()) !== file) throw new Error("the input was not kept");
    });

    await step("list a user's credentials, rotate and revoke", async () => {
      await page.locator("#identity-credential-principal").fill("render-alice");
      await screen.getByRole("button", { name: "Filter" }).click();
      const row = page.getByTestId(`credential-${keyId}`);
      await row.waitFor({ timeout: 10000 });
      const rotated = join(dir, "alice-rotated.key");
      await row.getByRole("button", { name: "Rotate…" }).click();
      await page.locator("#identity-rotate-file").fill(rotated);
      await row.getByRole("button", { name: `Rotate ${keyId}` }).click();
      await announced(`to replace ${keyId}`);
      const token = (await readFile(rotated, "utf8")).trim();
      secrets.push(token.split("_").pop());
      await row.getByRole("button", { name: "Revoke…" }).click();
      await screen.getByRole("button", { name: "Keep it" }).click();
      const state = async () =>
        (await get(base, `${API}/credentials?principal=render-alice`)).credentials.find((c) => c.key_id === keyId)?.state;
      if ((await state()) !== "active") throw new Error("Keep it revoked the credential");
      await row.getByRole("button", { name: "Revoke…" }).click();
      await row.getByRole("button", { name: `Confirm: revoke ${keyId}` }).click();
      await announced(`Revoked ${keyId}.`);
      if (!(await until(async () => (await state()) === "revoked"))) throw new Error("Confirm did not revoke");
      await noSecretShown("after rotate and revoke");
    });

    await step("assign, reassign and free a profile", async () => {
      const owner = async () => (await get(base, `${API}/grants?profile=${active}`)).profiles[0]?.owner;
      await page.locator("#identity-owner-profile").selectOption(active);
      await page.locator("#identity-owner-principal").selectOption("render-alice");
      await screen.getByRole("button", { name: "Assign owner" }).click();
      await announced(`render-alice now owns ${active}.`);
      if ((await owner()) !== "render-alice") throw new Error("the owner was not assigned");
      const row = page.getByTestId(`owner-${active}`);
      await row.waitFor({ timeout: 10000 });
      // The grants list by profile (`identity grants list --profile`).
      await page.locator("#identity-grants-profile").selectOption(active);
      await row.waitFor({ timeout: 10000 });
      if ((await page.getByTestId("identity-owners").locator("li[data-testid^='owner-']").count()) !== 1) {
        throw new Error("the grants list by profile shows another profile");
      }
      await page.locator(`#identity-reassign-${active}`).selectOption("render-bob");
      await row.getByRole("button", { name: "Reassign…" }).click();
      await row.getByRole("button", { name: `Confirm: reassign ${active} to render-bob` }).click();
      await announced(`${active} moved to render-bob`);
      if ((await owner()) !== "render-bob") throw new Error("the profile was not reassigned");
      await page.screenshot({ path: `${outDir}/identity-owned.png`, fullPage: true });
      await row.getByRole("button", { name: "Remove owner…" }).click();
      await row.getByRole("button", { name: `Confirm: remove the owner of ${active}` }).click();
      await announced(`${active} has no owner now`);
      if ((await get(base, `${API}/grants?profile=${active}`)).profiles.length !== 0) throw new Error("the owner was not removed");
    });

    await step("revoke a grant", async () => {
      await page.locator("#identity-grant-profile").selectOption(active);
      await page.locator("#identity-grant-principal").fill("render-bob");
      await page.locator("#identity-grant-capability").selectOption("sessions_read");
      await screen.getByRole("button", { name: "Revoke grant…" }).click();
      await screen.getByRole("button", { name: "Confirm: revoke sessions_read" }).click();
      await announced(`Revoked sessions_read for render-bob on ${active}.`);
    });

    await step("suspend a user", async () => {
      const row = page.getByTestId("user-render-alice");
      await row.getByRole("button", { name: "Suspend…" }).click();
      await row.getByRole("button", { name: "Confirm: suspend render-alice" }).click();
      await announced("Suspended render-alice");
      const alice = (await get(base, `${API}/users`)).principals.find((p) => p.id === "render-alice");
      if (alice?.status !== "suspended" || alice?.unrevoked_credentials !== 0) {
        throw new Error(`render-alice is ${JSON.stringify(alice)}`);
      }
    });

    await step("the audit shows the records asked for", async () => {
      await page.locator("#identity-audit-limit").fill("3");
      await screen.getByRole("button", { name: "Show audit" }).click();
      const rows = page.getByTestId("identity-audit").locator("tbody tr");
      if (!(await until(async () => (await rows.count()) === 3))) throw new Error(`${await rows.count()} audit rows`);
      await noSecretShown("with the audit");
      await page.screenshot({ path: `${outDir}/identity-audit.png`, fullPage: true });
    });

    if (errors.length) failures.push(`identity: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`identity: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    if (active) await fetch(`${base}${API}/owners/${active}`, { method: "DELETE" }).catch(() => {});
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
