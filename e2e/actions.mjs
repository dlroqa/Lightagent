// The response action row under the final answer, rendered against the real
// harness.
//
// As in delivery.mjs, the page is served through a small proxy in front of
// the live harness. The session under test is a real one, but its transcript,
// the regenerate request and the regenerated run's event stream are scripted,
// so a finished answer exists without a model and the stream can be held open
// to check that the row stays hidden while an answer is still arriving.

import { createServer } from "node:http";
import { mkdir } from "node:fs/promises";
import { seededSession } from "./delivery.mjs";

const FIRST_RUN = "run-00000000000000000000000000000a00";
const OLD_RUN = "run-00000000000000000000000000000a01";
const NEW_RUN = "run-00000000000000000000000000000a02";
const FIRST_PROMPT = "What is the capital of France?";
const FIRST_ANSWER = "Paris is the capital of France.";
const PROMPT = "What is Lyon known for?";
const OLD_ANSWER = "Lyon is known for its silk history.";
const NEW_ANSWER = "Lyon is famous for its food and its traditional bouchons.";
const LABELS = ["Copy response", "Rate response", "Read aloud", "Share", "Try again", "More actions"];

const now = () => ({ secs_since_epoch: Math.floor(Date.now() / 1000), nanos_since_epoch: 0 });
const message = (role, content) => ({ role, content, created_at: now() });
const run = (run_id) => ({ run_id, started_at: now(), ended_at: now(), stop_reason: "EndTurn", tools: [] });
const frame = (id, type, data) => `id: ${id}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`;

/** The scripted transcript for each stage of the regeneration. */
function transcript(stage) {
  const earlier = [message("user", FIRST_PROMPT), message("assistant", FIRST_ANSWER), message("user", PROMPT)];
  switch (stage) {
    case "answered": return { messages: [...earlier, message("assistant", OLD_ANSWER)], runs: [run(FIRST_RUN), run(OLD_RUN)] };
    case "regenerating": return { messages: earlier, runs: [run(FIRST_RUN)] };
    default: return { messages: [...earlier, message("assistant", NEW_ANSWER)], runs: [run(FIRST_RUN), run(NEW_RUN)] };
  }
}

const DROP_REQUEST = new Set(["host", "connection", "content-length", "origin", "referer", "accept-encoding"]);
const DROP_RESPONSE = new Set(["connection", "content-length", "content-encoding", "transfer-encoding", "keep-alive"]);

/** Script session `id`'s transcript and regeneration; forward the rest to `base`. */
async function scriptedProxy(base, id, record) {
  let stage = "answered";
  let release;
  const released = new Promise((resolve) => { release = resolve; });
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://proxy.invalid");
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const payload = Buffer.concat(chunks);
      const json = (status, value) => {
        response.writeHead(status, { "content-type": "application/json" });
        response.end(JSON.stringify(value));
      };
      const sessionPath = `/api/lightagent/v1/sessions/${id}`;
      if (request.method === "GET" && url.pathname === sessionPath) {
        const real = await (await fetch(`${base}${sessionPath}`)).json();
        return json(200, { ...real, ...transcript(stage) });
      }
      if (request.method === "POST" && url.pathname === `${sessionPath}/regenerate`) {
        record.regenerations.push(JSON.parse(payload.toString("utf8")));
        stage = "regenerating";
        return json(202, { id: NEW_RUN, status: "running", session_id: id });
      }
      if (url.pathname === `/api/lightagent/v1/runs/${NEW_RUN}/events`) {
        response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        response.write("retry: 300\n\n");
        response.write(frame(0, "run.started", { run: NEW_RUN, parent: null }));
        response.write(frame(1, "model.delta", { content: NEW_ANSWER }));
        // Held open until the test has looked at the streaming state.
        await released;
        stage = "regenerated";
        response.write(frame(2, "turn.completed", {}));
        response.write(frame(3, "run.completed", { reason: "EndTurn", complete: true }));
        return response.end();
      }
      if (request.method === "GET" && url.pathname === `/api/lightagent/v1/runs/${NEW_RUN}`) {
        const finished = stage === "regenerated";
        return json(200, {
          id: NEW_RUN, status: finished ? "completed" : "running", events: finished ? 4 : 2,
          pending_approval: null, stop_reason: finished ? "EndTurn" : null,
        });
      }
      const headers = Object.fromEntries(
        Object.entries(request.headers).filter(([name]) => !DROP_REQUEST.has(name)),
      );
      const upstream = await fetch(`${base}${request.url}`, {
        method: request.method,
        headers,
        body: ["GET", "HEAD"].includes(request.method ?? "GET") ? undefined : payload,
        redirect: "manual",
      });
      const kept = {};
      upstream.headers.forEach((value, name) => {
        if (!DROP_RESPONSE.has(name)) kept[name] = value;
      });
      response.writeHead(upstream.status, kept);
      response.end(Buffer.from(await upstream.arrayBuffer()));
    } catch (err) {
      if (!response.headersSent) response.writeHead(502, { "content-type": "text/plain" });
      response.end(String(err));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, origin: `http://127.0.0.1:${server.address().port}`, release };
}

/** Stand-ins for the clipboard and speech synthesis, recording what they were given. */
function browserStubs() {
  window.__copied = [];
  window.__spoken = [];
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: async (text) => { window.__copied.push(text); } },
  });
  // No share sheet in a headless browser: the row falls back to copying.
  Object.defineProperty(navigator, "share", { configurable: true, value: undefined });
  Object.defineProperty(window, "speechSynthesis", {
    configurable: true,
    value: { speak: (utterance) => window.__spoken.push(utterance.text), cancel: () => {} },
  });
}

const toolbarsIn = (page, selector) =>
  page.evaluate((sel) => [...document.querySelectorAll(sel)]
    .map((el) => el.querySelectorAll('[role="toolbar"][aria-label="Response actions"]').length), selector);

/** Exactly one row, inside the last assistant answer and no other message. */
async function assertOnlyUnderFinalAnswer(page, failures, when) {
  const agent = await toolbarsIn(page, 'article[aria-label="Agent message"]');
  const user = await toolbarsIn(page, 'article[aria-label="Your message"]');
  const total = await page.getByRole("toolbar", { name: "Response actions" }).count();
  if (total !== 1) failures.push(`actions ${when}: expected one action row, saw ${total}`);
  if (agent.at(-1) !== 1) failures.push(`actions ${when}: the row is not under the final answer (${JSON.stringify(agent)})`);
  if (user.some(Boolean)) failures.push(`actions ${when}: a user message has an action row`);
}

export async function checkResponseActions(browser, base, outDir) {
  const dir = `${outDir}/actions`;
  await mkdir(dir, { recursive: true });
  const failures = [];
  const record = { regenerations: [] };
  const title = "response actions probe";
  const id = await seededSession(base, title);
  const proxy = await scriptedProxy(base, id, record);
  const context = await browser.newContext({ viewport: { width: 1280, height: 1000 }, colorScheme: "dark" });
  await context.addInitScript(browserStubs);
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (err) => errors.push(String(err)));

  try {
    await page.goto(`${proxy.origin}/#/`, { waitUntil: "domcontentloaded" });
    await page.getByText(title, { exact: true }).first().click({ timeout: 15000 });
    const toolbar = page.getByRole("toolbar", { name: "Response actions" });
    await toolbar.waitFor({ timeout: 15000 });

    // Visibility, and the six actions in order.
    await assertOnlyUnderFinalAnswer(page, failures, "on load");
    const labels = await toolbar.locator("button").evaluateAll((buttons) => buttons.map((b) => b.getAttribute("aria-label")));
    if (JSON.stringify(labels) !== JSON.stringify(LABELS)) {
      failures.push(`actions: expected ${JSON.stringify(LABELS)}, saw ${JSON.stringify(labels)}`);
    }
    // Lined up with the answer's left edge, not the window's.
    const align = await page.evaluate(() => {
      const articles = document.querySelectorAll('article[aria-label="Agent message"]');
      const answer = articles[articles.length - 1];
      const content = answer?.querySelector(".chat-message__content")?.getBoundingClientRect();
      const glyph = answer?.querySelector('[role="toolbar"] button svg')?.getBoundingClientRect();
      return content && glyph ? { offset: Math.round(glyph.left - content.left), gap: Math.round(glyph.top - content.bottom) } : null;
    });
    if (!align || Math.abs(align.offset) > 3 || align.gap < 0 || align.gap > 24) {
      failures.push(`actions: the row is not aligned under the answer (${JSON.stringify(align)})`);
    }

    // Hover: charcoal square plus tooltip, with nothing moving.
    const copy = toolbar.getByRole("button", { name: "Copy response" });
    const before = await toolbar.boundingBox();
    await copy.hover();
    await page.waitForTimeout(250);
    const hovered = await copy.evaluate((button) => {
      const tip = button.querySelector(".response-actions__tooltip");
      return {
        background: getComputedStyle(button).backgroundColor,
        radius: getComputedStyle(button).borderTopLeftRadius,
        cursor: getComputedStyle(button).cursor,
        tip: tip?.textContent ?? "",
        tipOpacity: tip ? Number(getComputedStyle(tip).opacity) : 0,
      };
    });
    const after = await toolbar.boundingBox();
    if (hovered.background !== "rgb(29, 29, 29)") failures.push(`actions: hover background is ${hovered.background}`);
    if (hovered.cursor !== "pointer") failures.push(`actions: cursor is ${hovered.cursor}`);
    if (hovered.tip !== "Copy response" || hovered.tipOpacity < 1) {
      failures.push(`actions: hover tooltip “${hovered.tip}” at opacity ${hovered.tipOpacity}`);
    }
    if (JSON.stringify(before) !== JSON.stringify(after)) failures.push("actions: hovering moved the row");
    await page.screenshot({ path: `${dir}/hover-copy-dark.png` });

    // Keyboard focus shows the same tooltip; arrow keys move along the row.
    await copy.focus();
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    const focused = await page.evaluate(() => ({
      label: document.activeElement?.getAttribute("aria-label"),
      tip: document.activeElement?.querySelector(".response-actions__tooltip")?.textContent,
    }));
    if (focused.label !== "Share" || focused.tip !== "Share") {
      failures.push(`actions: arrow keys reached ${JSON.stringify(focused)}, not Share`);
    }
    const retryTip = await toolbar.getByRole("button", { name: "Try again" })
      .evaluate((b) => b.querySelector(".response-actions__tooltip")?.textContent);
    if (retryTip !== "Try again…") failures.push(`actions: retry tooltip is “${retryTip}”`);

    // Copy puts the answer itself on the clipboard and says so.
    await copy.click();
    await page.getByRole("status").filter({ hasText: "Copied to clipboard" }).waitFor({ timeout: 5000 });
    const copied = await page.evaluate(() => window.__copied);
    if (copied.at(-1) !== OLD_ANSWER) failures.push(`actions: copied ${JSON.stringify(copied)}`);

    // Read aloud toggles, and says it is on.
    const speak = toolbar.getByRole("button", { name: "Read aloud" });
    await speak.click();
    if (await speak.getAttribute("aria-pressed") !== "true") failures.push("actions: read aloud did not report it started");
    await speak.click();
    if (await speak.getAttribute("aria-pressed") !== "false") failures.push("actions: read aloud did not report it stopped");
    if ((await page.evaluate(() => window.__spoken)).at(-1) !== OLD_ANSWER) failures.push("actions: read aloud spoke something else");

    // Rating is a choice of two, kept on the button.
    await toolbar.getByRole("button", { name: "Rate response" }).click();
    await page.getByRole("menuitemradio", { name: "Good response" }).click();
    if (await toolbar.getByRole("button", { name: "Rate response" }).evaluate((b) => !b.classList.contains("is-active"))) {
      failures.push("actions: a rating is not shown on the button");
    }

    // More: a popover with the answer's time and the branch row; Escape and an
    // outside click both close it, and Escape hands focus back.
    const more = toolbar.getByRole("button", { name: "More actions" });
    await more.click();
    const menu = page.getByRole("menu", { name: "More actions" });
    await menu.waitFor({ timeout: 5000 });
    const menuText = await menu.innerText();
    if (!/Today, /.test(menuText)) failures.push(`actions: the more menu has no timestamp (“${menuText}”)`);
    if (!menuText.includes("Branch in new chat")) failures.push("actions: the more menu lacks “Branch in new chat”");
    await page.waitForTimeout(250);
    await page.screenshot({ path: `${dir}/more-menu-dark.png` });
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached", timeout: 5000 });
    if (await page.evaluate(() => document.activeElement?.getAttribute("aria-label")) !== "More actions") {
      failures.push("actions: Escape did not return focus to the more button");
    }
    await more.click();
    await menu.waitFor({ timeout: 5000 });
    await page.mouse.click(5, 5);
    await menu.waitFor({ state: "detached", timeout: 5000 });

    // Try again: one request naming the replaced run; no row while the new
    // answer streams; then the new answer alone, with its row.
    await toolbar.getByRole("button", { name: "Try again" }).click();
    await page.waitForFunction(() => !document.querySelector('[role="toolbar"][aria-label="Response actions"]'), null, { timeout: 5000 })
      .catch(() => failures.push("actions: the row stayed while the new answer streamed"));
    const streaming = await page.evaluate(() => document.body.innerText);
    if (streaming.includes(OLD_ANSWER)) failures.push("actions: the replaced answer stayed on screen");
    await page.screenshot({ path: `${dir}/regenerating-dark.png` });
    proxy.release();
    await toolbar.waitFor({ timeout: 15000 });
    await page.waitForTimeout(500);
    const text = await page.evaluate(() => document.body.innerText);
    if (record.regenerations.length !== 1 || record.regenerations[0]?.run_id !== OLD_RUN) {
      failures.push(`actions: regenerate requests ${JSON.stringify(record.regenerations)}`);
    }
    if (text.split(NEW_ANSWER).length - 1 !== 1) failures.push("actions: the new answer is not shown exactly once");
    if (text.split(PROMPT).length - 1 !== 1) failures.push("actions: the prompt is not shown exactly once");
    if (text.includes(OLD_ANSWER)) failures.push("actions: the replaced answer came back");
    await assertOnlyUnderFinalAnswer(page, failures, "after try again");

    // Narrow screens: the row fits, nothing scrolls sideways.
    await page.setViewportSize({ width: 360, height: 800 });
    await page.waitForTimeout(250);
    const narrow = await page.evaluate(() => {
      const row = document.querySelector('[role="toolbar"][aria-label="Response actions"]')?.getBoundingClientRect();
      return {
        overflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        right: row ? Math.round(row.right) : -1,
        width: window.innerWidth,
      };
    });
    if (narrow.overflow > 1 || narrow.right > narrow.width) failures.push(`actions: narrow layout overflows (${JSON.stringify(narrow)})`);
    await page.screenshot({ path: `${dir}/mobile-dark.png` });

    if (errors.length) failures.push(`actions: uncaught page error — ${errors[0]}`);
    console.log(`  [${failures.length ? "FAIL" : "ok"}] response actions`);
  } finally {
    proxy.release();
    await context.close();
    proxy.server.closeAllConnections();
    await new Promise((resolve) => proxy.server.close(resolve));
  }
  return failures;
}
