// Try again on the real backend (D7, row 56). Unlike actions.mjs, nothing
// is scripted but the model: the page and its requests go to the panel's
// own server, whose model is, for the length of this check, a
// deterministic OpenAI-compatible stub on loopback. A finished answer
// replaces the saved one; a provider failure, a refusal, an empty answer
// and Stop keep it, shown once with the kept notice; a Try again from a
// page that missed a newer answer is refused as stale and changes nothing.
// The endpoint is put back and the session deleted afterwards. Ownership
// and permissions are the backend's checks (api.rs, profile_scoped.rs);
// the profile-switch gate is the host's (switch.rs).

import { createServer } from "node:http";
import { seededSession } from "./delivery.mjs";

const API = "/api/lightagent/v1";
// Short enough that the sidebar shows it in full, so it can be clicked by its exact text.
const PROMPT = "regenerate probe";
const FIRST = "The Rhône runs through Lyon.";
const SECOND = "Two rivers: the Rhône and the Saône.";
const THIRD = "The Rhône and the Saône meet in Lyon.";
const PARTIAL = "Half an answer that";

/** A deterministic OpenAI-compatible model on loopback: `stub.mode` picks what it streams. */
async function providerStub(model) {
  const stub = { mode: "first", requests: 0, release: null };
  const answers = { first: FIRST, second: SECOND, third: THIRD };
  const chunk = (delta, finish) =>
    `data: ${JSON.stringify({ choices: [{ index: 0, delta, ...(finish ? { finish_reason: finish } : {}) }] })}\n\n`;
  const server = createServer((request, response) => {
    request.resume();
    request.on("end", () => {
      if (request.method === "GET" && (request.url ?? "").startsWith("/v1/models")) {
        response.writeHead(200, { "content-type": "application/json" });
        return response.end(JSON.stringify({ object: "list", data: [{ id: model }] }));
      }
      stub.requests += 1;
      const mode = stub.mode;
      if (mode === "fail") {
        response.writeHead(500, { "content-type": "application/json" });
        return response.end(JSON.stringify({ error: { message: "the stub failed on purpose" } }));
      }
      response.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
      response.write(chunk({ role: "assistant" }));
      const finish = () => {
        if (response.destroyed) return;
        response.write(chunk({}, "stop"));
        response.end("data: [DONE]\n\n");
      };
      if (mode === "refusal") {
        response.write(chunk({ refusal: "I can't help with that." }));
        return finish();
      }
      if (mode === "empty") return finish();
      if (mode === "hold") {
        response.write(chunk({ content: PARTIAL }));
        stub.release = () => {
          if (!response.destroyed) response.write(chunk({ content: " never finished." }));
          finish();
        };
        return;
      }
      response.write(chunk({ content: answers[mode] ?? "ok" }));
      finish();
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  stub.url = `http://127.0.0.1:${server.address().port}`;
  stub.close = () => {
    stub.release?.();
    server.closeAllConnections?.();
    return new Promise((resolve) => server.close(resolve));
  };
  return stub;
}

export async function checkRegenerateOnBackend(browser, base, outDir) {
  const failures = [];
  const api = async (path, init = {}) => {
    const response = await fetch(`${base}${API}${path}`, { headers: { "content-type": "application/json" }, ...init });
    const text = await response.text();
    if (!response.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${response.status}: ${text}`);
    return text ? JSON.parse(text) : null;
  };
  const readKey = async (name) => {
    const response = await fetch(`${base}${API}/config/${name}`);
    return response.ok ? (await response.json()).value : null;
  };
  const writeKey = (name, value) =>
    fetch(`${base}${API}/config/${name}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ value }),
    });
  const endpoint = await readKey("inference.base_url");
  const stub = await providerStub((await readKey("inference.model")) || "render-model");
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const toolbar = page.getByRole("toolbar", { name: "Response actions" });
  const tryAgain = () => toolbar.getByRole("button", { name: "Try again" }).click({ timeout: 15000 });
  const kept = () => page.getByText("the previous answer is kept", { exact: false }).waitFor({ timeout: 30000 });
  const shown = async (text) => (await page.evaluate(() => document.body.innerText)).split(text).length - 1;
  const until = async (what, predicate, timeout = 30000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (await predicate()) return;
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    throw new Error(`timed out waiting for ${what}`);
  };
  let id = null;
  let step = "point the server at the stub";
  const saved = async () => {
    const session = await api(`/sessions/${encodeURIComponent(id)}`);
    return JSON.stringify(session.messages.filter((m) => m.role === "assistant").map((m) => m.content));
  };
  const open = async () => {
    await page.goto("about:blank");
    await page.goto(`${base}/#/`, { waitUntil: "domcontentloaded" });
    await page.getByText(PROMPT, { exact: true }).first().click({ timeout: 15000 });
    await toolbar.waitFor({ timeout: 15000 });
  };
  try {
    const pointed = await writeKey("inference.base_url", stub.url);
    if (!pointed.ok) throw new Error(`PUT inference.base_url → ${pointed.status}`);
    step = "seed a conversation";
    id = await seededSession(base, PROMPT);
    await until("the first answer to be saved", async () => (await saved()) === JSON.stringify([FIRST]));

    step = "a finished answer replaces the saved one";
    await open();
    stub.mode = "second";
    await tryAgain();
    await until("the replacement to be saved", async () => (await saved()) === JSON.stringify([SECOND]));
    await page.getByText(SECOND, { exact: false }).first().waitFor({ timeout: 15000 });
    await toolbar.waitFor({ timeout: 15000 });
    if (await shown(FIRST)) failures.push("regenerate: the replaced answer is still shown");
    await page.screenshot({ path: `${outDir}/regenerate-backend-replaced.png`, fullPage: true });
    console.log("  [ok] regenerate (real backend): a finished answer replaces the saved one");

    for (const mode of ["fail", "refusal", "empty"]) {
      step = `${mode}: the saved answer is kept`;
      await open();
      const before = stub.requests;
      stub.mode = mode;
      await tryAgain();
      await kept();
      await toolbar.waitFor({ timeout: 15000 });
      if (stub.requests === before) failures.push(`regenerate: ${mode}: the new run never reached the model`);
      if ((await saved()) !== JSON.stringify([SECOND])) failures.push(`regenerate: ${mode}: the saved answer changed: ${await saved()}`);
      if ((await shown(SECOND)) !== 1) failures.push(`regenerate: ${mode}: the kept answer is not shown exactly once`);
      await page.screenshot({ path: `${outDir}/regenerate-backend-${mode}.png`, fullPage: true });
      console.log(`  [ok] regenerate (real backend): ${mode} keeps the saved answer`);
    }

    step = "Stop keeps the saved answer";
    await open();
    stub.mode = "hold";
    stub.release = null;
    await tryAgain();
    await until("the new answer to start streaming", async () => stub.release !== null, 15000);
    await page.getByRole("button", { name: "Stop", exact: true }).click({ timeout: 15000 });
    await kept();
    stub.release?.();
    await toolbar.waitFor({ timeout: 15000 });
    if ((await saved()) !== JSON.stringify([SECOND])) failures.push(`regenerate: stop: the saved answer changed: ${await saved()}`);
    if ((await shown(SECOND)) !== 1) failures.push("regenerate: stop: the kept answer is not shown exactly once");
    if (await shown(PARTIAL)) failures.push("regenerate: stop: the stopped run's partial answer is shown");
    await page.screenshot({ path: `${outDir}/regenerate-backend-stop.png`, fullPage: true });
    console.log("  [ok] regenerate (real backend): Stop keeps the saved answer");

    step = "a Try again from a page that missed a newer answer is refused as stale";
    await open();
    stub.mode = "third";
    const latest = (await api(`/sessions/${encodeURIComponent(id)}`)).runs.at(-1).run_id;
    await api(`/sessions/${encodeURIComponent(id)}/regenerate`, { method: "POST", body: JSON.stringify({ run_id: latest }) });
    await until("the newer answer to be saved", async () => (await saved()) === JSON.stringify([THIRD]));
    const runsBefore = JSON.stringify((await api(`/sessions/${encodeURIComponent(id)}`)).runs);
    const refusal = page.waitForResponse((response) =>
      response.url().endsWith(`/sessions/${encodeURIComponent(id)}/regenerate`) && response.request().method() === "POST",
      { timeout: 15000 });
    await tryAgain();
    const refused = await refusal;
    await page.getByText("only the latest answer can be regenerated", { exact: false }).waitFor({ timeout: 15000 });
    if (refused.status() !== 409) failures.push(`regenerate: stale: the page's Try again was answered ${refused.status()}, not 409`);
    if (JSON.stringify((await api(`/sessions/${encodeURIComponent(id)}`)).runs) !== runsBefore) {
      failures.push("regenerate: stale: the refused Try again changed the session's runs");
    }
    if ((await saved()) !== JSON.stringify([THIRD])) failures.push(`regenerate: stale: the saved answer changed: ${await saved()}`);
    await page.screenshot({ path: `${outDir}/regenerate-backend-stale.png`, fullPage: true });
    await open();
    if ((await shown(THIRD)) !== 1) failures.push("regenerate: stale: the newer answer is not shown once on reload");
    console.log("  [ok] regenerate (real backend): a stale Try again is refused and changes nothing");
  } catch (err) {
    failures.push(`regenerate (real backend): ${step}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/regenerate-backend-failed.png`, fullPage: true }).catch(() => {});
  } finally {
    stub.release?.();
    if (endpoint !== null) await writeKey("inference.base_url", endpoint).catch(() => {});
    if (id) await fetch(`${base}${API}/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
    await page.close();
    await stub.close();
  }
  if (errors.length) failures.push(`regenerate (real backend): page errors: ${errors.join("; ")}`);
  return failures;
}
