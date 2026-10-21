// Run-delivery states of the Agent screen, rendered against the real harness.
//
// Sessions, settings and the page itself come from the live harness API. Only
// the run endpoints are scripted (POST /runs, GET /runs/{id}, and its SSE
// stream), so each scenario can drop the stream at an exact event, replay or
// resume it, or keep it dead after the run ends — the races a real network
// produces nondeterministically. Every frame carries the id the harness sends,
// and every reconnect's cursor is recorded and checked.

import { mkdir, writeFile } from "node:fs/promises";

const RUN = "run-00000000000000000000000000000d11";
const QUESTION = "When did Zephyr ship?";

const frame = (id, type, data) => `id: ${id}\nevent: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
// A short retry so a dropped stream reconnects within the test's patience.
const body = (frames, from = 0) =>
  "retry: 300\n\n" + frames.slice(from).map(([type, data], index) => frame(from + index, type, data)).join("");

const started = ["run.started", { run: RUN, parent: null }];
const toolRequested = ["tool.requested", { id: "call_1", name: "rag.realtime", arguments: JSON.stringify({ query: QUESTION }) }];
const toolStarted = ["tool.started", { id: "call_1", name: "rag.realtime" }];
const progress = (stage, extra = {}) => ["tool.progress", { id: "call_1", stage, completed: null, total: null, outcome: null, ...extra }];
const evidence = ["tool.output", { id: "call_1", is_error: false, content: "CURRENT WEB EVIDENCE for \"When did Zephyr ship?\"\n[1] Zephyr release\nURL: https://example.com/zephyr\nZephyr shipped on September 10, 2026." }];
const delta = (content) => ["model.delta", { content }];
const turn = ["turn.completed", {}];
const completed = (reason) => ["run.completed", { reason, complete: reason === "EndTurn" }];
const classification = (outcome, extra = {}) => ["classification.completed", {
  schema: 2, trigger: "preflight", mode: "shadow", outcome, source: outcome === "classified" ? "jev" : null,
  category: null, category_confidence: null, confidence_available: false, threshold: 0.9,
  planning_probability: null, retrieval_probability: null, tools_probability: null,
  complexity: null, complexity_confidence: null, model_version: null, input_tokens: null, output_tokens: null,
  jev_calls: 1, classification_latency_ms: null, preflight_latency_ms: 3, fallback_reason: null,
  influenced: false, accepted: null, workflow: null, workflow_eligibility: null, workflow_capability: null,
  workflow_applied: false, available_tools: null, approval_required: null, workflow_selection_us: null,
  routing_input: null, ...extra,
}];

/** The cursor a stream request resumes after: the later of `?after=` and `Last-Event-ID`. */
function cursorOf(request) {
  const values = [request.after, request.lastEventId]
    .filter((value) => value !== null && value !== undefined && value !== "")
    .map(Number)
    .filter(Number.isInteger);
  return values.length ? Math.max(...values) : -1;
}

const SCENARIOS = [
  {
    // The stream drops mid-retrieval, then a server that ignores the cursor
    // replays everything: the client must keep each event exactly once.
    name: "recovered-after-drop-before-tool-output",
    frames: [
      started,
      ["routing.selected", { source: "Jev", model: "default-model", kept_default: true }],
      classification("classified", { category: "research", category_confidence: 0.95, accepted: true, workflow: "research", workflow_eligibility: "eligible", classification_latency_ms: 42 }),
      turn, toolRequested, toolStarted,
      progress("searching"),
      progress("fetching", { completed: 1, total: 2 }),
      progress("completed", { completed: 1, total: 2, outcome: "partial" }),
      evidence,
      delta("Zephyr shipped on September 10, 2026, "),
      delta("according to the release notes."),
      turn,
      completed("EndTurn"),
    ],
    stream(attempt, cursor, frames) {
      if (attempt === 0) return { frames: body(frames.slice(0, 8)) };
      return { frames: body(frames) };
    },
    status: () => ({ status: "running" }),
    expect: {
      once: ["Zephyr shipped on September 10, 2026, according to the release notes."],
      present: [
        "Web research for “When did Zephyr ship?”: Partial evidence: 1 of 2 sources read",
        "Jev classified this as research (advisory only) · 42 ms",
        "Jev kept the default route: default-model",
      ],
      absent: ["step limit", "ended unexpectedly", "could not be recovered", "cut off"],
      reconnectedAfter: 7,
    },
  },
  {
    // The stream drops while the final answer streams; the server resumes
    // after the cursor. The run had used its last step for that answer.
    name: "resumed-during-final-synthesis",
    frames: [
      started, turn, toolRequested, toolStarted,
      progress("completed", { completed: 2, total: 2, outcome: "evidence" }),
      evidence,
      delta("The first half "),
      delta("of the answer."),
      turn,
      completed("MaxTurns"),
    ],
    stream(attempt, cursor, frames) {
      if (attempt === 0) return { frames: body(frames.slice(0, 7)) };
      return { frames: body(frames, cursor + 1) };
    },
    status: () => ({ status: "running" }),
    expect: {
      once: ["The first half of the answer."],
      present: [
        "Research reached its step limit, so this answer was written from the evidence gathered so far.",
        "2 of 2 sources read",
      ],
      absent: ["could not be recovered"],
      badge: "limit reached",
      reconnectedAfter: 6,
    },
  },
  {
    // The run ends on the server while the stream is dead and stays dead: the
    // UI tries to replay, then says plainly that output may be missing.
    name: "terminal-status-without-replay",
    frames: [started, delta("Partial evidence says")],
    stream(attempt, cursor, frames) {
      if (attempt === 0) return { frames: body(frames) };
      return { status: 503 };
    },
    status: () => ({ status: "completed", stop_reason: "Incomplete" }),
    expect: {
      once: ["Partial evidence says"],
      present: [
        "The model's response ended unexpectedly, so this answer may be incomplete.",
        "Some live output could not be recovered",
      ],
      absent: [],
      badge: "incomplete",
      reconnectedAfter: 1,
    },
  },
  {
    // A token-limited answer; Jev was not permitted for this profile.
    name: "token-limited-answer",
    frames: [
      started,
      classification("skipped", { fallback_reason: "profile_not_permitted" }),
      delta("The first half of"),
      turn,
      completed("MaxTokens"),
    ],
    stream: (attempt, cursor, frames) => ({ frames: body(frames) }),
    status: () => ({ status: "running" }),
    expect: {
      once: ["The first half of"],
      present: [
        "The answer was cut off at the model's output limit.",
        "Jev classification skipped (profile_not_permitted); the run continued normally",
      ],
      absent: ["could not be recovered"],
    },
  },
  {
    // Jev timed out, retrieval failed outright, and the step limit came
    // before any answer: nothing is dressed up as a result.
    name: "failed-retrieval-no-answer",
    frames: [
      started,
      classification("failed", { fallback_reason: "timeout", classification_latency_ms: 3000 }),
      turn, toolRequested, toolStarted,
      progress("searching"),
      progress("completed", { outcome: "failed" }),
      ["tool.failed", { id: "call_1", is_error: true, content: "search request failed" }],
      completed("MaxTurns"),
    ],
    stream: (attempt, cursor, frames) => ({ frames: body(frames) }),
    status: () => ({ status: "running" }),
    expect: {
      once: [],
      present: [
        "Web research for “When did Zephyr ship?”: Retrieval failed",
        "Research reached its step limit before an answer was written.",
        "Jev classification failed (timeout); the run continued normally · 3000 ms",
      ],
      absent: ["Partial evidence", "sources read"],
    },
  },
];

const occurrences = (haystack, needle) => haystack.split(needle).length - 1;

/**
 * A real session with one real, quickly failing run (the harness's model is
 * unreachable), so the Agent screen opens it as a conversation rather than as
 * a new chat. The scripted run under test is then sent into it.
 */
async function seededSession(base, title) {
  const api = async (path, init = {}) => {
    const response = await fetch(`${base}/api/lightagent/v1${path}`, {
      headers: { "content-type": "application/json" },
      ...init,
    });
    if (!response.ok) throw new Error(`${init.method ?? "GET"} ${path} answered ${response.status}`);
    return response.json();
  };
  const { id } = await api("/sessions", { method: "POST", body: "{}" });
  await api("/runs", { method: "POST", body: JSON.stringify({ message: title, session_id: id }) });
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const session = await api(`/sessions/${encodeURIComponent(id)}`);
    if (session.runs.length > 0) return id;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`the seed run for “${title}” did not finish`);
}

async function runScenario(browser, base, dir, scenario) {
  const failures = [];
  const context = await browser.newContext({ viewport: { width: 1280, height: 1100 }, colorScheme: "dark" });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (err) => errors.push(String(err)));
  const streams = [];

  await page.route("**/api/lightagent/v1/runs**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (request.method() === "POST" && url.pathname.endsWith("/runs")) {
      const sent = request.postDataJSON() ?? {};
      return route.fulfill({
        status: 202,
        contentType: "application/json",
        body: JSON.stringify({ id: RUN, status: "running", session_id: sent.session_id ?? null }),
      });
    }
    if (url.pathname.endsWith(`/runs/${RUN}/events`)) {
      const record = { after: url.searchParams.get("after"), lastEventId: request.headers()["last-event-id"] ?? null };
      const attempt = streams.length;
      streams.push(record);
      const reply = scenario.stream(attempt, cursorOf(record), scenario.frames);
      if (reply.status) {
        return route.fulfill({ status: reply.status, contentType: "text/plain", body: "unavailable" });
      }
      return route.fulfill({ status: 200, contentType: "text/event-stream", headers: { "cache-control": "no-cache" }, body: reply.frames });
    }
    if (request.method() === "GET" && url.pathname.endsWith(`/runs/${RUN}`)) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ id: RUN, events: scenario.frames.length, pending_approval: null, stop_reason: null, ...scenario.status() }),
      });
    }
    return route.continue();
  });

  try {
    // The seed is created before the route is used: its run is a real one.
    const title = `delivery ${scenario.name}`;
    await seededSession(base, title);
    await page.goto(`${base}/#/`, { waitUntil: "domcontentloaded" });
    await page.getByText(title, { exact: true }).first().click({ timeout: 15000 });
    const composer = page.locator(".chat-composer textarea");
    await composer.waitFor({ timeout: 15000 });
    await composer.fill(QUESTION);
    await page.getByRole("button", { name: "Send message" }).click();

    const wanted = [...scenario.expect.once, ...scenario.expect.present];
    const deadline = Date.now() + 25000;
    let text = "";
    while (Date.now() < deadline) {
      text = await page.evaluate(() => document.body.innerText).catch(() => "");
      if (wanted.every((needle) => text.includes(needle))) break;
      await page.waitForTimeout(250);
    }
    // Let any late duplicate land before counting.
    await page.waitForTimeout(800);
    text = await page.evaluate(() => document.body.innerText).catch(() => "");
    await page.screenshot({ path: `${dir}/${scenario.name}.png` });
    // The run badge and the tool rows live in the collapsed Status Control.
    await page.getByRole("button", { name: "Status Control" }).click();
    const controls = await page.evaluate(() => document.body.innerText).catch(() => "");
    await page.screenshot({ path: `${dir}/${scenario.name}-status-control.png` });
    if (scenario.expect.badge && !controls.includes(scenario.expect.badge)) {
      failures.push(`${scenario.name}: expected the run badge “${scenario.expect.badge}”`);
    }

    for (const needle of scenario.expect.present) {
      if (!text.includes(needle)) failures.push(`${scenario.name}: expected “${needle}”`);
    }
    for (const needle of scenario.expect.once) {
      const count = occurrences(text, needle);
      if (count !== 1) failures.push(`${scenario.name}: expected “${needle}” exactly once, saw ${count}`);
    }
    for (const needle of scenario.expect.absent) {
      if (text.includes(needle)) failures.push(`${scenario.name}: did not expect “${needle}”`);
    }
    if (scenario.expect.reconnectedAfter !== undefined) {
      const resumed = streams.slice(1).map(cursorOf);
      if (!resumed.includes(scenario.expect.reconnectedAfter)) {
        failures.push(`${scenario.name}: expected a reconnect after event ${scenario.expect.reconnectedAfter}, saw cursors ${JSON.stringify(resumed)}`);
      }
    }
    if (errors.length) failures.push(`${scenario.name}: uncaught page error — ${errors[0]}`);
    console.log(`  [${failures.length ? "FAIL" : "ok"}] delivery/${scenario.name} (${streams.length} stream request(s))`);
    return { failures, report: { scenario: scenario.name, streams, failures } };
  } finally {
    await context.close();
  }
}

/** Render every delivery scenario; returns the failures, and writes a JSON report beside the screenshots. */
export async function checkDeliveryStates(browser, base, outDir) {
  const dir = `${outDir}/delivery`;
  await mkdir(dir, { recursive: true });
  const failures = [];
  const reports = [];
  for (const scenario of SCENARIOS) {
    try {
      const result = await runScenario(browser, base, dir, scenario);
      failures.push(...result.failures);
      reports.push(result.report);
    } catch (err) {
      const message = `${scenario.name}: ${err instanceof Error ? err.message : String(err)}`;
      failures.push(message);
      reports.push({ scenario: scenario.name, failures: [message] });
      console.log(`  [FAIL] delivery/${scenario.name} — ${err}`);
    }
  }
  await writeFile(`${dir}/report.json`, JSON.stringify(reports, null, 2));
  return failures;
}
