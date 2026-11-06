// The Extensions card on the Tools screen against the real harness (D6-2):
// an unusable source is refused under its field; a folder installs with MCP
// left off and its source recorded; Details shows what its MCP server would
// run and never an environment value; Disable and Enable work from the
// keyboard; Uninstall asks first. crates/lightagent/tests/desktop_extensions.rs
// proves the API's changes are the CLI's.

import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const NAME = "render-probe-extension";
const SECRET = "render-extension-env-4f1c9a-not-a-real-value";

async function api(base, path) {
  const response = await fetch(`${base}/api/lightagent/v1${path}`);
  return { status: response.status, body: await response.json() };
}

async function installed(base) {
  const { body } = await api(base, "/extensions");
  return (body.extensions ?? []).find((ext) => ext.name === NAME) ?? null;
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  return Boolean(await probe());
}

export async function checkExtensions(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-extension-"));
  const bundle = join(dir, NAME);
  await mkdir(join(bundle, "skills", "greet"), { recursive: true });
  await writeFile(join(bundle, "extension.json"), JSON.stringify({
    name: NAME,
    version: "1.0.0",
    description: "Render probe",
    mcp_servers: [{ transport: "stdio", name: "probe", command: "probe-server", args: ["--quiet"], env: { TOKEN: SECRET } }],
  }));
  await writeFile(join(bundle, "skills", "greet", "SKILL.md"), "---\nname: greet\ndescription: Say hello\n---\nSay hello.\n");
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const card = page.getByTestId("extensions-card");
  const source = page.locator("#extension-source");
  const install = card.getByRole("button", { name: "Install", exact: true });
  const row = page.getByTestId(`extension-${NAME}`);
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`extensions: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/extensions-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  try {
    await page.goto(`${base}/#/tools`);
    await card.waitFor({ timeout: 15000 });
    if (await installed(base)) throw new Error(`${NAME} is installed before the check`);

    await step("a relative source is refused under its field", async () => {
      await source.fill("relative/folder");
      await install.click();
      await card.getByRole("alert").filter({ hasText: "absolute folder path" }).first().waitFor({ timeout: 10000 });
      if (await installed(base)) throw new Error("something was installed");
    });

    await step("install leaves MCP off", async () => {
      await source.fill(bundle);
      if (await card.getByRole("checkbox", { name: /Turn MCP on/ }).isChecked()) {
        throw new Error("the MCP opt-in starts checked");
      }
      await install.click();
      await card.getByRole("status").filter({ hasText: `Installed ${NAME}` }).first().waitFor({ timeout: 20000 });
      await row.waitFor({ timeout: 10000 });
      const { body: detail } = await api(base, `/extensions/${NAME}`);
      if (detail.active !== true) throw new Error(`not active after the install: ${JSON.stringify(detail)}`);
      if (detail.mcp_enabled !== false) throw new Error("the install switched MCP on");
      if (detail.source?.kind !== "local") throw new Error(`the source was not recorded: ${JSON.stringify(detail.source)}`);
      await page.screenshot({ path: `${outDir}/extensions-installed.png` });
    });

    await step("details show the server, never its environment value", async () => {
      await row.getByRole("button", { name: `Details for ${NAME}` }).click();
      const region = page.getByRole("region", { name: `Details of ${NAME}` });
      await region.getByText("probe-server --quiet").waitFor({ timeout: 10000 });
      if (!(await region.innerText()).includes("TOKEN")) throw new Error("the environment variable's name is not shown");
      const everything = await page.evaluate(() =>
        document.documentElement.outerHTML + JSON.stringify(localStorage) + JSON.stringify(sessionStorage));
      if (everything.includes(SECRET)) throw new Error("the environment value reached the page");
      await page.screenshot({ path: `${outDir}/extensions-details.png` });
    });

    await step("disable and enable from the keyboard", async () => {
      await row.getByRole("button", { name: `Disable ${NAME}` }).focus();
      await page.keyboard.press("Enter");
      if (!(await until(async () => (await installed(base))?.active === false))) throw new Error("Disable did not reach the server");
      await row.getByText("Inactive", { exact: true }).waitFor({ timeout: 10000 });
      await row.getByRole("button", { name: `Enable ${NAME}` }).focus();
      await page.keyboard.press("Enter");
      if (!(await until(async () => (await installed(base))?.active === true))) throw new Error("Enable did not reach the server");
    });

    await step("the master switch turns every extension off and on", async () => {
      await card.getByRole("button", { name: "Turn extensions off" }).click();
      if (!(await until(async () => (await api(base, "/extensions")).body.enabled === false))) {
        throw new Error("Turn extensions off did not reach the server");
      }
      await card.getByText("Off for every profile", { exact: true }).waitFor({ timeout: 10000 });
      if ((await installed(base))?.active !== false) throw new Error("an extension stayed active with the switch off");
      await card.getByRole("button", { name: "Turn extensions on" }).click();
      if (!(await until(async () => (await api(base, "/extensions")).body.enabled === true))) {
        throw new Error("Turn extensions on did not reach the server");
      }
    });

    await step("uninstall asks first", async () => {
      await row.getByRole("button", { name: `Uninstall ${NAME}` }).click();
      await row.getByRole("button", { name: `Keep ${NAME}` }).click();
      if (!(await installed(base))) throw new Error("Keep removed it");
      await row.getByRole("button", { name: `Uninstall ${NAME}` }).click();
      await page.screenshot({ path: `${outDir}/extensions-confirm.png` });
      await row.getByRole("button", { name: `Confirm: uninstall ${NAME}` }).click();
      if (!(await until(async () => (await installed(base)) === null))) throw new Error("Confirm did not uninstall it");
      await row.waitFor({ state: "detached", timeout: 10000 });
    });

    if (errors.length) failures.push(`extensions: page errors: ${errors.join("; ")}`);
  } finally {
    await page.close();
    // Leave the harness as it was, even after a failed step.
    await fetch(`${base}/api/lightagent/v1/config/extensions.enabled`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ value: "true" }),
    }).catch(() => {});
    if (await installed(base).catch(() => null)) {
      await fetch(`${base}/api/lightagent/v1/extensions/${NAME}`, { method: "DELETE" }).catch(() => {});
    }
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
