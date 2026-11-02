// The MCP card against the real harness (D6-4): adding a server starts
// nothing; viewing the Tools screen starts nothing; Check of a stdio server
// shows the command and asks first, then starts it once, and no
// environment value reaches the page; an HTTP server is checked and its
// tool listed; turning MCP on is confirmed first; removal asks first.
// crates/lightagent/tests/desktop_mcp.rs proves the API's writes are the
// configuration the CLI loads.

import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";

const STDIO = "render-probe-stdio";
const HTTP = "render-probe-http";
const ENV_SECRET = "render-mcp-env-6a1d-not-a-real-value";
const HEADER_SECRET = "render-mcp-header-2f9b-not-a-real-value";

async function mcpNow(base) {
  return (await fetch(`${base}/api/lightagent/v1/mcp`)).json();
}

/** A minimal streamable-HTTP MCP server with one tool, `ping`. */
async function httpMcp() {
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    let message = {};
    try {
      message = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
    } catch {
      message = {};
    }
    const reply = (result) => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ jsonrpc: "2.0", id: message.id ?? null, ...result }));
    };
    if (message.method === "initialize") return reply({ result: { protocolVersion: "2025-06-18", capabilities: { tools: {} } } });
    if (message.method === "tools/list") {
      return reply({ result: { tools: [{ name: "ping", description: "Ping", inputSchema: { type: "object" } }] } });
    }
    if (typeof message.method === "string" && message.method.startsWith("notifications/")) {
      response.writeHead(202);
      return response.end();
    }
    return reply({ error: { code: -32601, message: "no" } });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, url: `http://127.0.0.1:${server.address().port}/mcp` };
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  return Boolean(await probe());
}

export async function checkMcp(browser, base, outDir) {
  const failures = [];
  const dir = await mkdtemp(join(tmpdir(), "lightagent-render-mcp-"));
  const marker = join(dir, "started");
  const script = `require("fs").writeFileSync(${JSON.stringify(marker)}, "started"); process.stderr.write("token " + process.env.TOKEN + "\\n"); process.exit(3);`;
  const http = await httpMcp();
  const original = await mcpNow(base);
  const page = await browser.newPage({ viewport: { width: 1280, height: 1300 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const card = page.getByTestId("mcp-card");
  const stdioRow = page.getByTestId(`mcp-${STDIO}`);
  const httpRow = page.getByTestId(`mcp-${HTTP}`);
  const leaks = async () => {
    const everything = await page.evaluate(() =>
      document.documentElement.outerHTML + JSON.stringify(localStorage) + JSON.stringify(sessionStorage));
    return [ENV_SECRET, HEADER_SECRET].filter((secret) => everything.includes(secret));
  };
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`mcp: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/mcp-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  try {
    if (original.mcp.enabled) throw new Error("the harness has MCP on before the check");
    await page.goto(`${base}/#/tools`);
    await card.waitFor({ timeout: 15000 });

    await step("adding a stdio server starts nothing", async () => {
      await card.getByRole("radio", { name: "Local command (stdio)" }).check();
      await page.locator("#mcp-name").fill(STDIO);
      await page.locator("#mcp-command").fill(process.execPath);
      await page.locator("#mcp-args").fill(`-e\n${script}`);
      await page.locator("#mcp-env").fill(`TOKEN=${ENV_SECRET}`);
      await card.getByRole("button", { name: "Add server" }).click();
      await card.getByRole("status").filter({ hasText: `Added ${STDIO}` }).first().waitFor({ timeout: 10000 });
      await page.reload();
      await stdioRow.waitFor({ timeout: 15000 });
      if (existsSync(marker)) throw new Error("adding or viewing the server started it");
      const now = await mcpNow(base);
      if (now.mcp.enabled) throw new Error("adding a server turned MCP on");
      if (JSON.stringify(now).includes(ENV_SECRET)) throw new Error("the API returned the environment value");
      const leaked = await leaks();
      if (leaked.length) throw new Error(`the page holds ${leaked.join(", ")}`);
    });

    await step("a stdio check asks first, then starts it once", async () => {
      await stdioRow.getByRole("button", { name: `Check ${STDIO}` }).click();
      await stdioRow.getByText("Checking starts").waitFor({ timeout: 5000 });
      if (!(await stdioRow.innerText()).includes(process.execPath)) throw new Error("the confirmation does not show the command");
      if (existsSync(marker)) throw new Error("the server started before it was confirmed");
      await page.screenshot({ path: `${outDir}/mcp-confirm-check.png` });
      await stdioRow.getByRole("button", { name: `Confirm: start and check ${STDIO}` }).click();
      await stdioRow.getByRole("alert").filter({ hasText: "Could not connect" }).waitFor({ timeout: 30000 });
      if (!existsSync(marker)) throw new Error("the confirmed check did not start the server");
      const leaked = await leaks();
      if (leaked.length) throw new Error(`the page holds ${leaked.join(", ")}`);
      if ((await mcpNow(base)).mcp.enabled) throw new Error("a check turned MCP on");
    });

    await step("an HTTP server is checked and its tool listed", async () => {
      await card.getByRole("radio", { name: "HTTP endpoint" }).check();
      await page.locator("#mcp-name").fill(HTTP);
      await page.locator("#mcp-url").fill(http.url);
      await page.locator("#mcp-headers").fill(`X-Api-Key: ${HEADER_SECRET}`);
      await card.getByRole("button", { name: "Add server" }).click();
      await httpRow.waitFor({ timeout: 10000 });
      await httpRow.getByRole("button", { name: `Check ${HTTP}` }).click();
      const connected = httpRow.getByRole("status").filter({ hasText: "Connected" });
      await connected.waitFor({ timeout: 30000 });
      if (!(await connected.innerText()).includes("ping")) throw new Error("the tool is not listed");
      await page.screenshot({ path: `${outDir}/mcp-checked.png` });
      const leaked = await leaks();
      if (leaked.length) throw new Error(`the page holds ${leaked.join(", ")}`);
    });

    await step("removal asks first", async () => {
      for (const [name, row] of [[STDIO, stdioRow], [HTTP, httpRow]]) {
        await row.getByRole("button", { name: `Remove ${name}` }).click();
        await row.getByRole("button", { name: `Keep ${name}` }).click();
        if (!(await mcpNow(base)).mcp.servers.some((server) => server.name === name)) throw new Error(`Keep removed ${name}`);
        await row.getByRole("button", { name: `Remove ${name}` }).click();
        await row.getByRole("button", { name: `Confirm: remove ${name}` }).click();
        if (!(await until(async () => !(await mcpNow(base)).mcp.servers.some((server) => server.name === name)))) {
          throw new Error(`Confirm did not remove ${name}`);
        }
      }
    });

    await step("turning MCP on is confirmed first", async () => {
      await card.getByRole("button", { name: "Turn MCP on" }).click();
      await card.getByText("each agent run starts or connects every server").waitFor({ timeout: 5000 });
      if ((await mcpNow(base)).mcp.enabled) throw new Error("MCP turned on before it was confirmed");
      await card.getByRole("button", { name: "Keep MCP off" }).click();
      await card.getByRole("button", { name: "Turn MCP on" }).click();
      await card.getByRole("button", { name: "Confirm: turn MCP on" }).click();
      if (!(await until(async () => (await mcpNow(base)).mcp.enabled === true))) throw new Error("Confirm did not turn MCP on");
      await card.getByRole("button", { name: "Turn MCP off" }).click();
      if (!(await until(async () => (await mcpNow(base)).mcp.enabled === false))) throw new Error("Turn MCP off did not");
    });

    if (errors.length) failures.push(`mcp: page errors: ${errors.join("; ")}`);
  } finally {
    await page.close();
    // Leave the harness as it was, even after a failed step.
    for (const name of [STDIO, HTTP]) {
      await fetch(`${base}/api/lightagent/v1/mcp/servers/${name}`, { method: "DELETE" }).catch(() => {});
    }
    await fetch(`${base}/api/lightagent/v1/mcp`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ enabled: original.mcp.enabled, timeout_secs: original.mcp.timeout_secs }),
    }).catch(() => {});
    http.server.close();
    await rm(dir, { recursive: true, force: true });
  }
  return failures;
}
