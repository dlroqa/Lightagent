// The environment rows in the browser (D7, row 56): a second server that
// scripts/render-panel.sh starts as a user would, with the vault key itself
// in LIGHTAGENT_CREDENTIAL_KEY (no key file) and HERMES_HOME naming a Hermes
// home, in its own home. The Remote credentials screen names the key's
// source, and a grant sealed with that key opens with it; the Setup
// screen's preview with no folder reads HERMES_HOME. No JSON answer the
// page receives holds the key. desktop/e2e/runs.py `environment` covers the
// host and the CLI.

import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "/api/lightagent/v1";

async function api(base, path, init = {}) {
  const response = await fetch(`${base}${API}${path}`, { headers: { "content-type": "application/json" }, ...init });
  const text = await response.text();
  if (!response.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${response.status}`);
  return text ? JSON.parse(text) : null;
}

export async function checkEnvironment(browser, base, outDir) {
  const key = process.env.RENDER_ENV_CREDENTIAL_KEY ?? "";
  const hermes = process.env.RENDER_ENV_HERMES_HOME ?? "";
  if (!base || !/^[0-9a-f]{64}$/.test(key) || !hermes) {
    return ["environment: the harness did not start the second server (PANEL_ENV_BASE, its key and its Hermes home)"];
  }
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-env-"));
  const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  // Every JSON answer the page receives, to look for the key in afterwards.
  const received = [];
  page.on("response", (response) => {
    if (response.url().includes(API) && (response.headers()["content-type"] ?? "").includes("application/json")) {
      received.push(response.text().catch(() => ""));
    }
  });
  const step = async (name, body) => {
    try {
      await body();
      console.log(`  [ok] environment: ${name}`);
    } catch (err) {
      failures.push(`environment: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/environment-failed-${name.replace(/\W+/g, "-")}.png`, fullPage: true })
        .catch(() => {});
    }
  };
  const credentials = async () => {
    await page.goto("about:blank");
    await page.goto(`${base}/#/admin/credentials`, { waitUntil: "domcontentloaded" });
  };
  try {
    const status = page.getByTestId("credentials-status");
    await step("Remote credentials names LIGHTAGENT_CREDENTIAL_KEY as the key's source", async () => {
      await credentials();
      await status.getByText("from the environment (LIGHTAGENT_CREDENTIAL_KEY)", { exact: false }).waitFor({ timeout: 15000 });
      await page.screenshot({ path: `${outDir}/environment-credential-key.png`, fullPage: true });
    });
    await step("a grant sealed with that key opens with it", async () => {
      const secretFile = join(dir, "env.secret");
      await writeFile(secretFile, "render-env-secret-2b3c4d5e\n", { mode: 0o600 });
      const { active_profile: profile } = await api(base, "/profiles");
      const added = await api(base, "/remote-credentials", {
        method: "POST",
        body: JSON.stringify({
          profile,
          service: "mcp:render-env",
          destinations: ["https://env.render.invalid"],
          secret_file: secretFile,
        }),
      });
      if (typeof added?.added !== "string") throw new Error(`not added: ${JSON.stringify(added)}`);
      await credentials();
      await status.getByText("Grants: 1 active", { exact: false }).waitFor({ timeout: 15000 });
      if (await page.getByTestId("credentials-unopenable").count()) {
        throw new Error("the grant does not open with the key that sealed it");
      }
      await page.screenshot({ path: `${outDir}/environment-credential-grant.png`, fullPage: true });
    });
    await step("the Setup preview with no folder reads HERMES_HOME", async () => {
      await page.goto("about:blank");
      await page.goto(`${base}/#/setup`, { waitUntil: "domcontentloaded" });
      const screen = page.getByTestId("setup-screen");
      await page.locator("#setup-hermes-from").fill("");
      await screen.getByRole("button", { name: "Preview" }).click();
      const result = page.getByTestId("setup-hermes-result");
      await result.getByText(`Would import 1 profile(s) and 0 skill(s) from ${hermes}.`, { exact: false })
        .waitFor({ timeout: 10000 });
      await result.getByText("Profiles: render-env-hermes", { exact: false }).waitFor({ timeout: 10000 });
      if ((await api(base, "/profiles")).profiles.some((p) => p.id === "render-env-hermes")) {
        throw new Error("the preview wrote");
      }
      await page.screenshot({ path: `${outDir}/environment-hermes-home.png`, fullPage: true });
    });
    const answers = (await Promise.all(received)).join("\n");
    if (answers.includes(key)) failures.push("environment: a JSON answer the page received holds the credential key");
    if (errors.length) failures.push(`environment: page errors: ${errors.join("; ")}`);
  } finally {
    await page.close();
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
