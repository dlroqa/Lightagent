// The run controls of the Agent screen, rendered against the real harness:
// the approval card (Allow once, Deny), Stop, the Continue card of a run
// paused on its time budget, and the steering queue.
//
// As in delivery.mjs and actions.mjs, the page is served through a small
// proxy in front of the live harness. Sessions, settings and the page come
// from the harness; the run endpoints are scripted and held open at chosen
// points, and every request the page makes to them is recorded, so what is
// checked is what each control sends, not only what it shows.

import { createServer } from "node:http";
import { mkdir } from "node:fs/promises";
import { seededSession } from "./delivery.mjs";

const API = "/api/lightagent/v1";
const frame = (id, type, data) => `id: ${id}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
const DROP_REQUEST = new Set(["host", "connection", "content-length", "origin", "referer", "accept-encoding"]);
const DROP_RESPONSE = new Set(["connection", "content-length", "content-encoding", "transfer-encoding", "keep-alive"]);
const TERMINAL = { completed: "EndTurn", cancelled: "Cancelled" };

/** One scripted run: its frames so far, its state, and the streams waiting on it. */
function scriptedRun(id) {
  return { id, frames: [], status: "running", stop: null, pending: null, ended: false, waiters: new Set() };
}

function push(run, type, data) {
  const position = run.frames.length;
  run.frames.push([type, data]);
  for (const write of [...run.waiters]) write(position);
}

function end(run, status) {
  run.status = status;
  run.stop = TERMINAL[status] ?? null;
  run.pending = null;
  run.ended = true;
  for (const write of [...run.waiters]) write(null);
}

function finish(run, text) {
  push(run, "model.delta", { content: text });
  push(run, "turn.completed", {});
  push(run, "run.completed", { reason: "EndTurn", complete: true });
  end(run, "completed");
}

/** What each run the page creates does, in order. */
const PLANS = [
  // Approval, allowed.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "approval.required", { approval_id: "ap-1", tool_call_id: "call_a", name: "fs.write" });
    run.pending = { approval_id: "ap-1", tool_call_id: "call_a", tool: "fs.write", risk: "" };
    run.status = "awaiting_approval";
  },
  // Approval, denied.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "approval.required", { approval_id: "ap-2", tool_call_id: "call_b", name: "fs.write" });
    run.pending = { approval_id: "ap-2", tool_call_id: "call_b", tool: "fs.write", risk: "" };
    run.status = "awaiting_approval";
  },
  // Stop.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "model.delta", { content: "Working on it" });
  },
  // Paused on its time budget.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "tool.requested", { id: "call_c", name: "datetime.now", arguments: "{}" });
    push(run, "tool.started", { id: "call_c", name: "datetime.now" });
    push(run, "tool.output", { id: "call_c", is_error: false, content: "12:00" });
    push(run, "run.paused", { reason: "time_budget", elapsed_secs: 1 });
    run.status = "awaiting_continue";
  },
  // A long run steers are queued behind.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "model.delta", { content: "Thinking" });
  },
  // The two queued steers.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    finish(run, "First steer done.");
  },
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    finish(run, "Second steer done.");
  },
  // Paused again; the page is reloaded and must find it from its session.
  (run) => {
    push(run, "run.started", { run: run.id, parent: null });
    push(run, "tool.requested", { id: "call_d", name: "datetime.now", arguments: "{}" });
    push(run, "tool.started", { id: "call_d", name: "datetime.now" });
    push(run, "tool.output", { id: "call_d", is_error: false, content: "12:01" });
    push(run, "run.paused", { reason: "time_budget", elapsed_secs: 1 });
    run.status = "awaiting_continue";
  },
];

async function scriptedProxy(base, record) {
  const runs = new Map();
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://proxy.invalid");
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const payload = Buffer.concat(chunks);
      const body = payload.length ? JSON.parse(payload.toString("utf8")) : {};
      const json = (status, value) => {
        response.writeHead(status, { "content-type": "application/json" });
        response.end(JSON.stringify(value));
      };
      if (request.method === "POST" && url.pathname === `${API}/runs`) {
        const index = record.created.length;
        record.created.push(body);
        const plan = PLANS[index];
        if (!plan) return json(409, { error: "session already has an active run" });
        const run = scriptedRun(`run-${String(index).padStart(32, "0")}`);
        runs.set(run.id, run);
        plan(run);
        return json(202, { id: run.id, status: run.status, session_id: body.session_id ?? null });
      }
      const parts = url.pathname.slice(`${API}/`.length).split("/");
      const run = parts[0] === "runs" ? runs.get(parts[1]) : parts[0] === "approvals" ? runs.get(parts[1]) : undefined;
      if (run && parts[0] === "runs" && parts[2] === "events") {
        const cursors = [url.searchParams.get("after"), request.headers["last-event-id"]]
          .filter((value) => value !== null && value !== undefined && value !== "")
          .map(Number)
          .filter(Number.isInteger);
        const start = cursors.length ? Math.max(...cursors) + 1 : 0;
        response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        response.write("retry: 300\n\n");
        for (let position = start; position < run.frames.length; position += 1) {
          response.write(frame(position, ...run.frames[position]));
        }
        if (run.ended) return response.end();
        const write = (position) => {
          if (position === null) {
            run.waiters.delete(write);
            response.end();
          } else {
            response.write(frame(position, ...run.frames[position]));
          }
        };
        run.waiters.add(write);
        request.on("close", () => run.waiters.delete(write));
        return;
      }
      if (run && parts[0] === "runs" && parts.length === 2 && request.method === "GET") {
        return json(200, {
          id: run.id, status: run.status, events: run.frames.length,
          pending_approval: run.pending, stop_reason: run.stop,
        });
      }
      if (run && parts[0] === "runs" && parts[2] === "cancel" && request.method === "POST") {
        record.cancels.push(run.id);
        push(run, "run.cancelled", { reason: "Cancelled", complete: false });
        end(run, "cancelled");
        return json(200, { id: run.id, cancelled: true });
      }
      if (run && parts[0] === "runs" && parts[2] === "continue" && request.method === "POST") {
        record.continues.push({ run: run.id, ...body });
        if (run.status !== "awaiting_continue") return json(409, { error: "run is not paused on its time budget" });
        run.status = "running";
        finish(run, "Found it.");
        return json(200, { id: run.id, continued: true });
      }
      if (run && parts[0] === "approvals" && request.method === "POST") {
        record.approvals.push({ run: run.id, ...body });
        const call = run.pending?.tool_call_id;
        run.pending = null;
        run.status = "running";
        if (body.approve) {
          push(run, "tool.started", { id: call, name: "fs.write" });
          push(run, "tool.output", { id: call, is_error: false, content: "wrote 8 bytes" });
          finish(run, "Written.");
        } else {
          push(run, "tool.failed", { id: call, is_error: true, content: "the approval for 'fs.write' was denied; the tool did not run" });
          finish(run, "Not written.");
        }
        return json(200, { run: run.id, delivered: true });
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
      // The session as the server shows it while one of its turns is paused:
      // with `active_run`, which a page that lost the run follows.
      const paused = [...runs.values()].find((candidate) => candidate.status === "awaiting_continue");
      if (paused && request.method === "GET" && parts[0] === "sessions" && parts.length === 2 && upstream.ok) {
        const view = await upstream.json();
        record.found.push(paused.id);
        return json(200, { ...view, active_run: { id: paused.id, status: paused.status } });
      }
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
  return {
    server,
    origin: `http://127.0.0.1:${server.address().port}`,
    run: (index) => runs.get(`run-${String(index).padStart(32, "0")}`),
  };
}

async function until(probe, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await probe()) return true;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return Boolean(await probe());
}

export async function checkRunControls(browser, base, outDir) {
  const dir = `${outDir}/runs`;
  await mkdir(dir, { recursive: true });
  const failures = [];
  const record = { created: [], approvals: [], cancels: [], continues: [], found: [] };
  const title = "run controls probe";
  await seededSession(base, title);
  const proxy = await scriptedProxy(base, record);
  const context = await browser.newContext({ viewport: { width: 1280, height: 1000 }, colorScheme: "dark" });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (err) => errors.push(String(err)));
  const composer = page.locator(".chat-composer textarea");
  const send = async (text) => {
    await composer.fill(text);
    await page.getByRole("button", { name: /^(Send message|Queue steer)$/ }).last().click();
  };
  const idle = () => until(async () => (await page.getByRole("button", { name: "Stop" }).count()) === 0);

  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      const text = await page.evaluate(() => document.body.innerText).catch(() => "");
      const message = err instanceof Error ? err.message.split("\n")[0] : String(err);
      failures.push(
        `run controls: ${name}: ${message} [runs ${record.created.length}, approvals ${record.approvals.length}, ` +
        `cancels ${record.cancels.length}, continues ${record.continues.length}; page ends: ` +
        `${text.replace(/\s+/g, " ").slice(-300)}]`,
      );
      await page.screenshot({ path: `${dir}/failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  const allow = page.getByRole("alertdialog", { name: "Approve fs.write" });
  const stop = page.getByRole("button", { name: "Stop", exact: true });
  const pausedCard = page.getByRole("alertdialog", { name: "Run paused on its time budget" });

  try {
    await page.goto(`${proxy.origin}/#/`, { waitUntil: "domcontentloaded" });
    await page.getByText(title, { exact: true }).first().click({ timeout: 15000 });
    await composer.waitFor({ timeout: 15000 });

    await step("allow once", async () => {
      await send("Write the approved file.");
      if (!(await until(async () => (await allow.count()) === 1))) throw new Error("no approval card for the first run");
      await page.screenshot({ path: `${dir}/approval.png` });
      await allow.getByRole("button", { name: "Allow once" }).click({ timeout: 10000 });
      if (!(await until(() => record.approvals.length === 1))) throw new Error("Allow once sent nothing");
      if (record.approvals[0].approve !== true) throw new Error(`Allow once sent ${JSON.stringify(record.approvals[0])}`);
      if (!(await until(async () => (await allow.count()) === 0))) throw new Error("the approval card stayed after Allow once");
      if (!(await idle())) throw new Error("the allowed run did not finish");
    });

    await step("deny from the keyboard", async () => {
      await send("Write the denied file.");
      if (!(await until(async () => (await allow.count()) === 1))) throw new Error("no approval card for the second run");
      await allow.getByRole("button", { name: "Deny" }).focus({ timeout: 10000 });
      await page.keyboard.press("Enter");
      if (!(await until(() => record.approvals.length === 2))) throw new Error("Deny (keyboard) sent nothing");
      if (record.approvals[1].approve !== false) throw new Error(`Deny sent ${JSON.stringify(record.approvals[1])}`);
      if (!(await idle())) throw new Error("the denied run did not finish");
    });

    await step("stop", async () => {
      await send("Work on something long.");
      if (!(await until(async () => (await stop.count()) === 1))) throw new Error("no Stop button while a run works");
      await stop.click({ timeout: 10000 });
      if (!(await until(() => record.cancels.length === 1))) throw new Error("Stop sent no cancel");
      if (record.cancels[0] !== proxy.run(2)?.id) throw new Error(`Stop cancelled ${record.cancels[0]}`);
      if (!(await idle())) throw new Error("the stopped run did not end");
    });

    await step("continue", async () => {
      await send("Look up the time.");
      if (!(await until(async () => (await pausedCard.count()) === 1))) throw new Error("no card for the paused run");
      await page.screenshot({ path: `${dir}/paused.png` });
      await pausedCard.getByRole("button", { name: "Continue" }).click({ timeout: 10000 });
      if (!(await until(() => record.continues.length === 1))) throw new Error("Continue sent nothing");
      const sent = record.continues[0];
      if (sent.mode !== "extend" || sent.run !== proxy.run(3)?.id) throw new Error(`Continue sent ${JSON.stringify(sent)}`);
      if (!(await until(async () => (await pausedCard.count()) === 0))) throw new Error("the paused card stayed after Continue");
      if (!(await idle())) throw new Error("the continued run did not finish");
    });

    await step("steering queue", async () => {
      await send("Start the long task.");
      if (!(await until(async () => (await stop.count()) === 1))) throw new Error("the long run did not start");
      await send("First steer.");
      await send("Second steer.");
      const queued = await until(async () => (await page.evaluate(() => document.body.innerText)).includes("Queued steers (2)"));
      await page.screenshot({ path: `${dir}/steering.png` });
      if (!queued) throw new Error("the two steers are not shown as queued");
      if (record.created.length !== 5) throw new Error(`steers were sent during the run (${record.created.length} runs created)`);
      finish(proxy.run(4), "Long task done.");
      if (!(await until(() => record.created.length === 7, 25000))) throw new Error("the queued steers were not both sent");
      const steers = record.created.slice(5).map((created) => created.message);
      if (JSON.stringify(steers) !== JSON.stringify(["First steer.", "Second steer."])) {
        throw new Error(`the steers were sent as ${JSON.stringify(steers)}`);
      }
    });

    await step("a paused run survives a reload", async () => {
      if (!(await idle())) throw new Error("the steers did not finish");
      await send("Look up the time again.");
      if (!(await until(async () => (await pausedCard.count()) === 1))) throw new Error("no card for the second paused run");
      await page.reload({ waitUntil: "domcontentloaded" });
      // The reloaded page opens on its welcome screen; the conversation is
      // picked from the list, as a person would.
      await page.getByText(title, { exact: true }).first().click({ timeout: 15000 });
      if (!(await until(async () => (await pausedCard.count()) === 1))) throw new Error("the reloaded page did not find the paused run");
      await page.screenshot({ path: `${dir}/paused-after-reload.png` });
      if (record.created.length !== 8) throw new Error(`the reload created runs (${record.created.length} created)`);
      await pausedCard.getByRole("button", { name: "Answer now" }).click({ timeout: 10000 });
      if (!(await until(() => record.continues.length === 2))) throw new Error("Answer now sent nothing");
      const sent = record.continues[1];
      if (sent.mode !== "answer" || sent.run !== proxy.run(7)?.id) throw new Error(`Answer now sent ${JSON.stringify(sent)}`);
      if (!(await idle())) throw new Error("the answered run did not finish");
    });

    const notPausing = record.created.filter((created) => created.on_time_budget !== "pause");
    if (notPausing.length) failures.push(`run controls: ${notPausing.length} run(s) were created without on_time_budget pause`);
    if (errors.length) failures.push(`run controls: page errors: ${errors.join("; ")}`);
  } finally {
    await page.screenshot({ path: `${dir}/final.png` }).catch(() => {});
    await context.close();
    proxy.server.close();
  }
  await import("node:fs/promises").then(({ writeFile }) =>
    writeFile(`${dir}/record.json`, JSON.stringify(record, null, 1)));
  return failures;
}
