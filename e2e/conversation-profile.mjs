// A conversation's own profile in the browser (D7, `lightagent chat
// --profile`): the Profile choice by the conversation list starts chats that
// run as that profile against the panel's own server (its model a local
// stub for the length of this check), lists them under that profile only,
// and leaves the active profile as it was. The profile, its chat and the
// endpoint are put back afterwards. desktop/e2e/runs.py
// `conversation_profile` and crates/lightagent/tests/
// desktop_conversation_profile.rs cover the host, the CLI and the backend.

import { createServer } from "node:http";

const API = "/api/lightagent/v1";
const PROFILE = "render-research";
const PERSONA = "You answer as the render research desk.";
const PROMPT = "profile probe";

/** An OpenAI-compatible model on loopback that answers "Noted." and keeps what it was sent. */
async function recordingStub(model) {
  const seen = [];
  const chunk = (delta, finish) =>
    `data: ${JSON.stringify({ choices: [{ index: 0, delta, ...(finish ? { finish_reason: finish } : {}) }] })}\n\n`;
  const text = (message) => (typeof message?.content === "string"
    ? message.content
    : (message?.content ?? []).map((part) => part?.text ?? "").join(""));
  const server = createServer((request, response) => {
    let raw = "";
    request.on("data", (part) => { raw += part; });
    request.on("end", () => {
      if (request.method === "GET" && (request.url ?? "").startsWith("/v1/models")) {
        response.writeHead(200, { "content-type": "application/json" });
        return response.end(JSON.stringify({ object: "list", data: [{ id: model }] }));
      }
      let body = {};
      try { body = JSON.parse(raw); } catch { body = {}; }
      const messages = body.messages ?? [];
      seen.push({
        model: body.model,
        system: messages.filter((m) => m.role === "system" || m.role === "developer").map(text).join("\n"),
        user: text([...messages].reverse().find((m) => m.role === "user")),
      });
      response.writeHead(200, { "content-type": "text/event-stream", connection: "close" });
      response.write(chunk({ role: "assistant" }));
      response.write(chunk({ content: "Noted." }));
      response.write(chunk({}, "stop"));
      response.end("data: [DONE]\n\n");
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    seen,
    url: `http://127.0.0.1:${server.address().port}`,
    close: () => {
      server.closeAllConnections?.();
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

export async function checkConversationProfile(browser, base, outDir) {
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
  const until = async (what, predicate, timeout = 30000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (await predicate()) return;
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    throw new Error(`timed out waiting for ${what}`);
  };
  const ids = (listed) => (listed?.sessions ?? []).map((row) => row.id);
  const endpoint = await readKey("inference.base_url");
  const stub = await recordingStub((await readKey("inference.model")) || "render-model");
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  let created = false;
  let chat = null;
  let step = "point the server at the stub";
  try {
    const pointed = await writeKey("inference.base_url", stub.url);
    if (!pointed.ok) throw new Error(`PUT inference.base_url → ${pointed.status}`);
    const active = (await api("/profiles")).active_profile;
    step = "create the profile";
    await api("/profiles", { method: "POST", body: JSON.stringify({ id: PROFILE, persona: PERSONA }) });
    created = true;

    step = "choose the profile by the conversation list";
    await page.goto(`${base}/#/`, { waitUntil: "domcontentloaded" });
    const choice = page.locator("#conversation-profile");
    await choice.selectOption(PROFILE, { timeout: 15000 });
    await page.getByText(`These chats run as ${PROFILE}; the default profile stays ${active}.`, { exact: true })
      .waitFor({ timeout: 10000 });

    step = "a chat started there runs as the profile";
    const composer = page.getByRole("textbox", { name: "Ask Lightagent" });
    await composer.click();
    await composer.pressSequentially(PROMPT);
    await composer.press("Enter");
    await until("the model to be asked", async () => stub.seen.some((request) => request.user.includes(PROMPT)));
    const sent = stub.seen.find((request) => request.user.includes(PROMPT));
    if (!sent.system.includes(PERSONA)) failures.push("conversation profile: the run did not carry the profile's persona");
    await until("the chat to be listed under the profile", async () =>
      ((await api(`/sessions?profile=${PROFILE}`)).sessions ?? []).some((row) => row.title === PROMPT));
    chat = (await api(`/sessions?profile=${PROFILE}`)).sessions.find((row) => row.title === PROMPT).id;
    if (ids(await api("/sessions")).includes(chat)) failures.push("conversation profile: the chat is listed under the default profile");
    const saved = await api(`/sessions/${encodeURIComponent(chat)}?profile=${PROFILE}`);
    if (saved.profile !== PROFILE) failures.push(`conversation profile: the chat is bound to ${saved.profile}`);
    if ((await api("/profiles")).active_profile !== active) failures.push("conversation profile: the active profile changed");
    const listed = page.locator(".agent-sessions__list").getByText(PROMPT, { exact: true });
    await listed.waitFor({ timeout: 10000 });
    await page.screenshot({ path: `${outDir}/conversation-profile.png`, fullPage: true });
    console.log("  [ok] conversation profile: a chat runs as the chosen profile; the active profile stays");

    step = "the default profile's list does not hold it";
    await choice.selectOption(active);
    await page.getByText("Chats run as the default profile.", { exact: true }).waitFor({ timeout: 10000 });
    await until("the list to change", async () => (await listed.count()) === 0, 10000);
    console.log("  [ok] conversation profile: the default profile's list does not hold the chat");
  } catch (err) {
    failures.push(`conversation profile: ${step}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
    await page.screenshot({ path: `${outDir}/conversation-profile-failed.png`, fullPage: true }).catch(() => {});
  } finally {
    if (chat) await fetch(`${base}${API}/sessions/${encodeURIComponent(chat)}?profile=${PROFILE}`, { method: "DELETE" }).catch(() => {});
    if (created) await fetch(`${base}${API}/profiles/${PROFILE}`, { method: "DELETE" }).catch(() => {});
    if (endpoint !== null) await writeKey("inference.base_url", endpoint).catch(() => {});
    await page.close();
    await stub.close();
  }
  if (errors.length) failures.push(`conversation profile: page errors: ${errors.join("; ")}`);
  return failures;
}
