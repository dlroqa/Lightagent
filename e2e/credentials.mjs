// The Remote credentials screen against the real harness (D7-9). The
// harness (scripts/render-panel.sh) starts the server with a key file and
// seals one grant with a previous key. Here: the vault shows the key's
// source and the grant that does not open; a key file is created
// (owner-only; an existing file is refused); a grant is added from a
// secret file (the secret is in neither the page nor browser storage; a
// missing file is refused without naming it), rotated from a new file and
// revoked (asks first); the old grant is re-sealed from the previous key
// (asks first); the audit shows the records asked for.
// crates/lightagent/tests/desktop_remote_credentials.rs proves the API's
// vault is the CLI's; desktop/e2e/runs.py `remote_credentials` covers the
// host.

import { mkdtemp, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "/api/lightagent/v1/remote-credentials";

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

export async function checkRemoteCredentials(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-credentials-"));
  const page = await browser.newPage({ viewport: { width: 1280, height: 2000 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const screen = page.getByTestId("credentials-screen");
  const announced = (text) => page.getByTestId("credentials-message").filter({ hasText: text }).waitFor({ timeout: 10000 });
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`credentials: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/credentials-failed-${name.replace(/\W+/g, "-")}.png`, fullPage: true }).catch(() => {});
    }
  };
  const secrets = ["render-previous-secret"];
  const noSecretShown = async (where) => {
    const seen = await page.evaluate(() => {
      const stored = (storage) => Object.keys(storage).map((key) => `${key}=${storage.getItem(key)}`).join("\n");
      return `${document.documentElement.outerHTML}\n${stored(localStorage)}\n${stored(sessionStorage)}`;
    });
    for (const secret of secrets) {
      if (seen.includes(secret)) throw new Error(`a secret is in the page or browser storage (${where})`);
    }
  };
  let id = "";
  try {
    const active = (await get(base, "/api/lightagent/v1/profiles")).active_profile;
    await page.goto(`${base}/#/admin/credentials`);
    await screen.waitFor({ timeout: 15000 });
    const status = page.getByTestId("credentials-status");
    await status.waitFor({ timeout: 15000 });

    await step("the key's source and the grant that does not open", async () => {
      await status.getByText("from a key file", { exact: false }).waitFor({ timeout: 10000 });
      await page.getByTestId("credentials-unopenable").waitFor({ timeout: 10000 });
    });

    await step("create a key file", async () => {
      const file = join(dir, "new-credential.key");
      await page.locator("#rc-keygen-path").fill(file);
      await screen.getByRole("button", { name: "Create key file" }).click();
      await announced("Created a key file");
      const mode = (await stat(file)).mode & 0o777;
      if (process.platform !== "win32" && mode !== 0o600) throw new Error(`the key file mode is ${mode.toString(8)}`);
      await page.locator("#rc-keygen-path").fill(file);
      await screen.getByRole("button", { name: "Create key file" }).click();
      await page.getByTestId("credentials-error").filter({ hasText: "could not create the key file" }).waitFor({ timeout: 10000 });
    });

    await step("add a grant from a secret file", async () => {
      const secret = `render-probe-secret-${Date.now()}`;
      secrets.push(secret);
      const file = join(dir, "probe.secret");
      await writeFile(file, `${secret}\n`);
      await page.locator("#rc-add-profile").selectOption(active);
      await page.locator("#rc-add-service").fill("mcp:render-probe");
      await page.locator("#rc-add-destinations").fill("https://probe.render.invalid");
      await page.locator("#rc-add-budget").fill("3");
      await page.locator("#rc-add-secret-file").fill(join(dir, "missing.secret"));
      await screen.getByRole("button", { name: "Add credential" }).click();
      const refused = page.getByTestId("credentials-error").filter({ hasText: "could not read the secret file" });
      await refused.waitFor({ timeout: 10000 });
      if ((await refused.innerText()).includes(dir)) throw new Error("the refusal names the file");
      await page.locator("#rc-add-secret-file").fill(file);
      await screen.getByRole("button", { name: "Add credential" }).click();
      await announced("Added ");
      const text = await page.getByTestId("credentials-message").innerText();
      id = /Added ([^:]+):/.exec(text)?.[1] ?? "";
      const grant = (await get(base, API)).credentials.find((item) => item.id === id);
      if (!grant || grant.service !== "mcp:render-probe" || grant.max_requests_per_run !== 3) {
        throw new Error(`the grant is ${JSON.stringify(grant)}`);
      }
      await page.getByTestId(`rc-${id}`).waitFor({ timeout: 10000 });
      await noSecretShown("after add");
      await page.screenshot({ path: `${outDir}/credentials-added.png`, fullPage: true });
    });

    await step("rotate from a new file, then revoke", async () => {
      const row = page.getByTestId(`rc-${id}`);
      const secret = `render-probe-rotated-${Date.now()}`;
      secrets.push(secret);
      const file = join(dir, "probe-rotated.secret");
      await writeFile(file, `${secret}\n`);
      await row.getByRole("button", { name: "Rotate…" }).click();
      await page.locator("#rc-rotate-file").fill(file);
      await row.getByRole("button", { name: `Rotate ${id}` }).click();
      await announced(`Rotated ${id} (version 2)`);
      const grant = async () => (await get(base, API)).credentials.find((item) => item.id === id);
      if ((await grant())?.version !== 2) throw new Error("the grant was not rotated");
      await row.getByRole("button", { name: "Revoke…" }).click();
      await screen.getByRole("button", { name: "Keep it" }).click();
      if ((await grant())?.state !== "active") throw new Error("Keep it revoked the grant");
      await row.getByRole("button", { name: "Revoke…" }).click();
      await row.getByRole("button", { name: `Confirm: revoke ${id}` }).click();
      await announced(`Revoked ${id}`);
      if (!(await until(async () => (await grant())?.state === "revoked"))) throw new Error("Confirm did not revoke");
      await noSecretShown("after rotate and revoke");
    });

    await step("re-seal the old grant from the previous key", async () => {
      const previous = process.env.RENDER_PREVIOUS_CREDENTIAL_KEY;
      if (!previous) throw new Error("the harness named no previous key (RENDER_PREVIOUS_CREDENTIAL_KEY)");
      await page.locator("#rc-rekey-previous").fill(previous);
      await screen.getByRole("button", { name: "Re-seal…" }).click();
      await screen.getByRole("button", { name: "Confirm: re-seal every active grant" }).click();
      await announced("Re-sealed 1 grant");
      const vault = await get(base, `${API}/vault`);
      if (vault.unopenable_with_configured_key !== 0) throw new Error(`still unopenable: ${JSON.stringify(vault)}`);
    });

    await step("the audit shows the records asked for", async () => {
      await page.locator("#rc-audit-limit").fill("3");
      await screen.getByRole("button", { name: "Show audit" }).click();
      const rows = page.getByTestId("credentials-audit").locator("li");
      if (!(await until(async () => (await rows.count()) === 3))) throw new Error(`${await rows.count()} audit rows`);
      await noSecretShown("with the audit");
    });

    if (errors.length) failures.push(`credentials: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`credentials: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
