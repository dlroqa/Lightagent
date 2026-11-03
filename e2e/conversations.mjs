// Conversations against the real harness (D7). Search finds a saved
// conversation by what was said (the page asks GET /sessions/search) and
// opens it. Rename edits the title in place: Enter saves it trimmed, Escape
// keeps the old one, and search then finds the new title. A file added to
// the composer is uploaded and saved byte for byte. Shift+Enter starts a new
// line, and Enter sends the draft with the attachment. New chat starts a
// fresh conversation in the same profile. The harness has no model, so a
// sent run fails; the check is what the page sent.
// crates/lightagent/tests/desktop_conversations.rs proves API = CLI;
// desktop/e2e/runs.py `conversations` covers the host and `lightagent chat`.

import { readFileSync } from "node:fs";

import { seededSession } from "./delivery.mjs";

const SAID = "The heron nests by the old quarry";
const TITLE = "Heronry field log";
const NOTE = Buffer.from("field notes\nheron, quarry\n");

export async function checkConversations(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const api = `${base}/api/lightagent/v1`;
  const searches = [];
  const sent = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (url.pathname === "/api/lightagent/v1/sessions/search") searches.push(url.searchParams.get("q"));
    if (url.pathname === "/api/lightagent/v1/runs" && request.method() === "POST") {
      try {
        sent.push(JSON.parse(request.postData() ?? "{}"));
      } catch {
        sent.push({});
      }
    }
  });
  const created = [];
  const sessionOf = async (id) => (await fetch(`${api}/sessions/${encodeURIComponent(id)}`)).json();
  const until = async (what, probe, timeout = 15000) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (await probe()) return;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    throw new Error(`timed out waiting for ${what}`);
  };
  const step = async (name, body) => {
    try {
      await body();
      return true;
    } catch (err) {
      const text = await page.evaluate(() => document.body.innerText).catch(() => "");
      const message = err instanceof Error ? err.message.split("\n")[0] : String(err);
      failures.push(`conversations: ${name}: ${message} [page ends: ${text.replace(/\s+/g, " ").slice(-300)}]`);
      await page.screenshot({ path: `${outDir}/conversations-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
      return false;
    }
  };
  const searchFor = async (term, expected) => {
    await page.getByRole("button", { name: "Search chats" }).first().click({ timeout: 10000 });
    const dialog = page.getByRole("dialog", { name: "Search chats" });
    await dialog.getByRole("textbox", { name: "Search agent conversations and messages" }).fill(term);
    const result = dialog.locator("button.chat-search__result").filter({ hasText: expected }).first();
    await result.waitFor({ timeout: 10000 });
    if (!searches.includes(term)) throw new Error(`the page searched ${JSON.stringify(searches)}, not “${term}”`);
    return result;
  };
  // The row's actions show while it is hovered or focused.
  const openRename = async (label) => {
    const row = page.locator("li.session-row").filter({ hasText: label }).first();
    await row.hover();
    await row.getByRole("button", { name: `More actions for ${label}` }).click({ timeout: 10000 });
    await page.getByRole("menu", { name: `Actions for ${label}` }).getByRole("menuitem", { name: /Rename/ })
      .click({ timeout: 10000 });
    const field = page.getByRole("textbox", { name: `Rename ${label}` });
    await field.waitFor({ timeout: 5000 });
    return field;
  };
  try {
    const id = await seededSession(base, SAID);
    created.push(id);
    const listed = (await (await fetch(`${api}/sessions`)).json()).sessions ?? [];
    const label = listed.find((session) => session.id === id)?.title || SAID;
    await page.goto(`${base}/#/`);

    const opened = await step("search finds what was said and opens the conversation", async () => {
      const result = await searchFor("old quarry", "old quarry");
      await page.screenshot({ path: `${outDir}/conversations-search.png` });
      await result.click({ timeout: 10000 });
      await page.getByRole("dialog", { name: "Search chats" }).waitFor({ state: "detached", timeout: 5000 });
      await page.locator('li.session-row .session-row__open[aria-current="true"]').filter({ hasText: label })
        .waitFor({ timeout: 10000 });
    });

    const renamed = opened && await step("Rename saves a trimmed title in place with Enter", async () => {
      const field = await openRename(label);
      const initial = await field.inputValue();
      if (initial !== label) throw new Error(`the field starts as “${initial}”`);
      await field.fill(`  ${TITLE}  `);
      await page.screenshot({ path: `${outDir}/conversations-rename.png` });
      await field.press("Enter");
      await page.locator("li.session-row").filter({ hasText: TITLE }).first().waitFor({ timeout: 10000 });
      await until("the server to hold the new title", async () => (await sessionOf(id)).title === TITLE);
    });
    if (renamed) {
      await step("Escape keeps the title", async () => {
        const field = await openRename(TITLE);
        await field.fill("Not this title");
        await field.press("Escape");
        await field.waitFor({ state: "detached", timeout: 5000 });
        await page.locator("li.session-row").filter({ hasText: TITLE }).first().waitFor({ timeout: 5000 });
        await new Promise((resolve) => setTimeout(resolve, 500));
        const title = (await sessionOf(id)).title;
        if (title !== TITLE) throw new Error(`Escape changed the title to “${title}”`);
      });
      await step("search finds the new title", async () => {
        const result = await searchFor("heronry", TITLE);
        await result.click({ timeout: 10000 });
        await page.getByRole("dialog", { name: "Search chats" }).waitFor({ state: "detached", timeout: 5000 });
      });
    }

    let attached = null;
    if (opened) {
      await step("a file added to the composer is uploaded and saved byte for byte", async () => {
        const response = page.waitForResponse((answer) => answer.request().method() === "POST"
          && answer.url().includes(`/sessions/${id}/attachments`), { timeout: 15000 });
        await page.locator("input.composer-attachment-input")
          .setInputFiles({ name: "field notes.txt", mimeType: "text/plain", buffer: NOTE });
        const answer = await response;
        const saved = await answer.json();
        if (answer.status() !== 200 || saved.name !== "field notes.txt") {
          throw new Error(`the upload answered ${answer.status()} ${JSON.stringify(saved)}`);
        }
        if (!readFileSync(saved.path).equals(NOTE)) throw new Error("the saved attachment differs from the file");
        await page.getByLabel("Attached files").getByText("field notes.txt").waitFor({ timeout: 5000 });
        attached = saved;
      });

      await step("Shift+Enter starts a new line and Enter sends the draft with the attachment", async () => {
        const composer = page.getByRole("textbox", { name: "Ask Lightagent" });
        await composer.click();
        await composer.pressSequentially("First line");
        await composer.press("Shift+Enter");
        await composer.pressSequentially("second line");
        await new Promise((resolve) => setTimeout(resolve, 300));
        if (sent.length !== 0) throw new Error("Shift+Enter sent the draft");
        const draft = await composer.inputValue();
        if (draft !== "First line\nsecond line") throw new Error(`the draft is ${JSON.stringify(draft)}`);
        await composer.press("Enter");
        await until("the page to send the draft", async () => sent.length === 1, 10000);
        const [message] = sent;
        if (message.session_id !== id || !message.message?.startsWith("First line\nsecond line")) {
          throw new Error(`the page sent ${JSON.stringify(message)}`);
        }
        if (attached && !message.message.includes(attached.path)) throw new Error("the sent draft does not name the attachment");
        await until("the run to settle", async () => ((await sessionOf(id)).runs ?? []).length >= 2);
      });
    }

    await step("New chat starts a fresh conversation in the same profile", async () => {
      await page.getByRole("button", { name: "New chat" }).first().click({ timeout: 10000 });
      await page.locator(".agent-welcome").waitFor({ timeout: 10000 });
      const composer = page.getByRole("textbox", { name: "Ask Lightagent" });
      const draft = await composer.inputValue();
      if (draft !== "") throw new Error(`the new conversation starts with ${JSON.stringify(draft)}`);
      await page.screenshot({ path: `${outDir}/conversations-new-chat.png` });
      const before = sent.length;
      await composer.pressSequentially("A fresh start");
      await composer.press("Enter");
      await until("the page to send the first turn", async () => sent.length === before + 1, 10000);
      const fresh = sent[before].session_id;
      if (!fresh || fresh === id) throw new Error(`the first turn went to ${JSON.stringify(fresh)}`);
      created.push(fresh);
      const [first, next] = [await sessionOf(id), await sessionOf(fresh)];
      if (!next.profile || next.profile !== first.profile) {
        throw new Error(`the profiles are ${first.profile} and ${next.profile}`);
      }
    });
    if (errors.length) failures.push(`conversations: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`conversations: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    for (const id of created) {
      // A run that is still settling keeps its session busy for a moment.
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        const answer = await fetch(`${api}/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => null);
        if (!answer || answer.status !== 409) break;
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
    }
  }
  return failures;
}
