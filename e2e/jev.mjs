// Jev Task Classification UI states, rendered against the real harness API.
//
// A deterministic local stub stands in for TypeSafe's SystemOne endpoint, so
// no credential or paid request is involved. Each state is reached through the
// same settings API the Web UI uses; the harness then talks to the stub for
// connection checks and run classification exactly as it would to TypeSafe.
// The stub's path prefix selects its behaviour: /ok, /low, /auth, /slow.

import { createServer } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";

function jevStub() {
  const server = createServer((request, response) => {
    let raw = "";
    request.on("data", (chunk) => { raw += chunk; });
    request.on("end", () => {
      const path = request.url ?? "/";
      const reply = (status, body) => {
        response.writeHead(status, { "content-type": "application/json" });
        response.end(JSON.stringify(body));
      };
      if (path.startsWith("/auth/")) return reply(401, { error: "invalid API key" });
      if (path.startsWith("/slow/")) return void setTimeout(() => reply(200, {}), 4000);
      let body;
      try { body = JSON.parse(raw); } catch { return reply(422, { error: "invalid body" }); }
      const confidence = path.startsWith("/low/") ? 0.4 : 0.95;
      const answers = {};
      for (const [id, question] of Object.entries(body.questions ?? {})) {
        if (question.type === "noul") {
          answers[id] = { type: "noul", noul: 0.7 };
        } else if (question.type === "score") {
          const levels = question.criteria.length;
          const probabilities = {};
          for (let level = 0; level < levels; level += 1) {
            probabilities[String(level)] = level === 1 ? 0.8 : 0.2 / (levels - 1);
          }
          answers[id] = { type: "score", score: 1, legend: {}, probabilities, confidence: 0.7 };
        } else {
          const options = Object.keys(question.criteria);
          const choice = options.includes("research") ? "research" : options[0];
          const rest = (1 - confidence) / Math.max(options.length - 1, 1);
          const probabilities = Object.fromEntries(options.map((option) => [option, option === choice ? confidence : rest]));
          answers[id] = { type: "choice", choice, probabilities, confidence };
        }
      }
      reply(200, { model: "jev-1.13.0", answers, usage: { input_tokens: 120, output_tokens: 20 } });
    });
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

export async function checkJevStates(browser, base, outDir) {
  const dir = `${outDir}/jev`;
  await mkdir(dir, { recursive: true });
  const stub = await jevStub();
  const stubUrl = `http://127.0.0.1:${stub.address().port}`;
  const api = async (path, init = {}) => {
    const response = await fetch(`${base}/api/lightagent/v1${path}`, {
      headers: { "content-type": "application/json" },
      ...init,
    });
    const text = await response.text();
    if (!response.ok) throw new Error(`${init.method ?? "GET"} ${path} → ${response.status}: ${text}`);
    return text ? JSON.parse(text) : null;
  };
  const { active_profile: profile } = await api("/profiles");
  const configure = async ({ jev = {}, classification = {} }) => {
    const settings = await api("/settings");
    const next = {
      ...settings,
      jev: {
        ...settings.jev,
        ...jev,
        classification: { ...settings.jev.classification, ...classification },
      },
    };
    await api("/settings", { method: "PUT", body: JSON.stringify(next) });
  };
  const waitForClassification = async (outcome) => {
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const status = await api("/jev");
      if (status.last_classification?.outcome === outcome) return status;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    throw new Error(`no ${outcome} classification was recorded`);
  };

  // The panel keeps its own theme preference (dark by default), so each
  // context states the theme it captures rather than relying on the media
  // query alone. Tall viewports keep each captured element fully on screen:
  // the panel scrolls an inner container, not the window.
  const themed = async (scheme, width, height) => {
    const view = await browser.newContext({ viewport: { width, height }, colorScheme: scheme });
    await view.addInitScript((theme) => {
      // Also runs on the blank page between states, which has no storage.
      if (window.location.protocol === "about:") return;
      window.localStorage.setItem("lightagent.web.preferences", JSON.stringify({ theme }));
    }, scheme);
    return view;
  };

  const failures = [];
  const context = await themed("dark", 1440, 3200);
  await context.tracing.start({ screenshots: true, snapshots: true });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));

  const toolsCard = () => page.getByTestId("jev-tool");
  const endpointsCard = () => page.getByTestId("platform-jev");
  // A hash-only navigation to the URL already shown does not remount the SPA,
  // so every state starts from a fresh document load.
  const open = async (route) => {
    await page.goto("about:blank");
    await page.goto(`${base}/#/${route}`, { waitUntil: "domcontentloaded" });
  };
  const capture = async (name, expectations, { settings = true } = {}) => {
    try {
      await open("tools");
      await toolsCard().waitFor({ timeout: 15000 });
      const text = await toolsCard().innerText();
      for (const expected of expectations) {
        if (!text.includes(expected)) failures.push(`${name}: Tools card lacks “${expected}”`);
      }
      await toolsCard().screenshot({ path: `${dir}/${name}-tools.png` });
      if (settings) {
        await open("settings");
        await page.getByTestId("jev-classification").waitFor({ timeout: 15000 });
        await endpointsCard().screenshot({ path: `${dir}/${name}-settings.png` });
      }
      const mark = failures.some((failure) => failure.startsWith(`${name}:`)) ? "FAIL" : "ok";
      console.log(`  [${mark}] jev ${name}`);
    } catch (error) {
      failures.push(`${name}: ${error instanceof Error ? error.message : String(error)}`);
    }
  };
  const toolNames = async () => (await api("/tools")).tools.map((tool) => tool.name);
  const eventually = async (predicate) => {
    for (let attempt = 0; attempt < 40; attempt += 1) {
      if (await predicate()) return true;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return false;
  };

  try {
    // 1. Nothing configured.
    await capture("01-not-configured", ["Not configured", "Disabled", "Not verified", "Off", "Sanitized task"]);

    // 2. An endpoint without a credential.
    await configure({ jev: { enabled: true, base_url: `${stubUrl}/ok` } });
    await capture("02-missing-credential", ["API key missing", "Enabled"]);

    // 3. Configured with a credential, but switched off.
    // A per-run placeholder: the stub accepts any key, and none is real.
    const placeholder = ["render", "fixture", process.pid].join("-");
    await configure({ jev: { enabled: false, api_key: placeholder } });
    await capture("03-configured-disabled", ["Configured", "Disabled", "Not verified"]);

    // 4. Shadow mode, permitted, verified through the real connection check.
    await configure({
      jev: { enabled: true, base_url: `${stubUrl}/ok` },
      classification: { mode: "shadow", allowed_profiles: [profile] },
    });
    const verified = await api("/jev/verify", { method: "POST", body: "{}" });
    if (!verified.ok) failures.push("shadow: the connection check against the stub did not verify");
    await capture("04-shadow-verified", ["Configured", "Enabled", "Verified", "Shadow", `Permitted for ${profile}`]);

    // 5. Active mode.
    await configure({ classification: { mode: "active" } });
    await capture("05-active", ["Active", "Verified"]);

    // 6. The profile is not permitted to send anything.
    await configure({ classification: { allowed_profiles: [] } });
    await capture("06-profile-not-permitted", [`Not permitted for ${profile}`]);

    // 7. Explicit tool off: not in the catalog.
    await configure({ classification: { allowed_profiles: [profile], explicit_tool: false } });
    if ((await toolNames()).includes("jev.classify_task")) {
      failures.push("explicit-disabled: jev.classify_task is listed although it is off");
    }
    await capture("07-explicit-tool-disabled", ["jev.classify_task — off"], { settings: false });

    // 8. Explicit tool on for a permitted profile: in the catalog.
    await configure({ classification: { explicit_tool: true } });
    if (!(await toolNames()).includes("jev.classify_task")) {
      failures.push("explicit-enabled: jev.classify_task is missing from the tool catalog");
    }
    await capture("08-explicit-tool-enabled", ["jev.classify_task — available to this profile"], { settings: false });
    await open("tools");
    await page.getByText("jev.classify_task", { exact: true }).waitFor({ timeout: 15000 });
    await page.screenshot({ path: `${dir}/08-explicit-tool-catalog.png`, fullPage: true });

    // 9. An invalid credential, reported by the real connection check.
    await configure({ jev: { base_url: `${stubUrl}/auth` } });
    const rejected = await api("/jev/verify", { method: "POST", body: "{}" });
    if (rejected.ok || rejected.error !== "unauthorized") failures.push(`invalid-credential: unexpected check ${JSON.stringify(rejected)}`);
    await capture("09-invalid-credential", ["Check failed: unauthorized"]);

    // 10. A timeout, bounded by the configured request timeout.
    await configure({ jev: { base_url: `${stubUrl}/slow`, timeout_secs: 1 } });
    const slow = await api("/jev/verify", { method: "POST", body: "{}" });
    if (slow.ok || slow.error !== "timeout") failures.push(`timeout: unexpected check ${JSON.stringify(slow)}`);
    await capture("10-timeout", ["Check failed: timeout"]);

    // 11. A run classified in shadow mode (its model call fails; the
    // classification happens before it and is recorded).
    await configure({ jev: { base_url: `${stubUrl}/ok`, timeout_secs: 3 }, classification: { mode: "shadow" } });
    await api("/runs", { method: "POST", body: JSON.stringify({ message: "Find recent papers on solid-state batteries" }) });
    const classified = await waitForClassification("classified");
    if (JSON.stringify(classified).includes("batteries")) failures.push("classified: diagnostics contain message text");
    // Web tools are off in this harness, so the research workflow has no
    // retrieval capability to point at: evaluated, not applied.
    await capture("11-classified", ["Classified", "research", "Automatic", "not configured", "Advisory only"]);

    // 12. A low-confidence classification falls back.
    await configure({ jev: { base_url: `${stubUrl}/low` } });
    await api("/runs", { method: "POST", body: JSON.stringify({ message: "Compare two approaches" }) });
    await waitForClassification("low_confidence");
    await capture("12-low-confidence", ["Low confidence — not applied", "low confidence"]);

    // Model routing permission, separate from classification permission.
    const routingSection = () => page.getByTestId("jev-routing-permission");
    const settingsNow = async () => (await api("/settings")).jev;
    // 15. A fresh configuration: strict, no profile may send content for routing.
    if ((await settingsNow()).routing_permission !== "strict") {
      failures.push("routing: a fresh configuration is not strict");
    }
    await capture("15-routing-denied", [`Denied for ${profile}`]);
    await open("settings");
    await routingSection().waitFor({ timeout: 15000 });
    if (!(await routingSection().innerText()).includes("Denied")) failures.push("routing-denied: settings lack Denied");

    // 16. Granted through the checkbox, keyboard-operable and labelled.
    // The box is controlled: it shows checked only once the save returns,
    // so click and wait for the saved state rather than asserting at once.
    const routingBox = routingSection().getByRole("checkbox", { name: new RegExp(`\\(${profile}\\)`) });
    await routingBox.click();
    if (!(await eventually(async () => (await settingsNow()).routing_allowed_profiles?.includes(profile)))) {
      failures.push("routing-allowed: checking the profile did not save");
    }
    if (!(await eventually(async () => await routingBox.isChecked()))) {
      failures.push("routing-allowed: the saved grant is not shown as checked");
    }
    const granted = await settingsNow();
    if (granted.classification.allowed_profiles.join() !== [profile].join()
      || granted.allowed_profiles.length !== 0) {
      failures.push("routing-allowed: the routing grant changed another profile list");
    }
    await capture("16-routing-allowed", [`Allowed for ${profile}`]);

    // 17. The compatibility policy, as an existing configuration loads it:
    // visible, explained, and left only by choosing Strict.
    await configure({ jev: { routing_permission: "legacy_all_profiles" } });
    await capture("17-routing-compatibility", ["Compatibility policy: every profile"]);
    await open("settings");
    await page.getByTestId("jev-routing-compatibility").waitFor({ timeout: 15000 });
    await routingSection().screenshot({ path: `${dir}/17-routing-compatibility-section.png` });
    await page.getByLabel("Routing permission policy").selectOption("strict");
    if (!(await eventually(async () => (await settingsNow()).routing_permission === "strict"))) {
      failures.push("routing-compatibility: choosing Strict did not save");
    }
    if (!(await eventually(async () =>
      (await page.locator('#jev-routing-permission option[value="legacy_all_profiles"]').count()) === 0))) {
      failures.push("routing-compatibility: the screen still offers widening back to every profile");
    }
    await routingSection().screenshot({ path: `${dir}/17-routing-strict-after-compatibility.png` });
    console.log("  [ok] jev routing compatibility → strict");

    // 18. Configuration errors. Invalid values are refused by the API and
    // never widen the permission.
    for (const [field, value] of [["routing_permission", "allow_all"], ["routing_allowed_profiles", ["Not A Profile!"]]]) {
      const current = await api("/settings");
      const response = await fetch(`${base}/api/lightagent/v1/settings`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ ...current, jev: { ...current.jev, [field]: value } }),
      });
      if (response.status < 400 || response.status >= 500) failures.push(`routing-invalid ${field}: answered ${response.status}`);
    }
    const afterInvalid = await settingsNow();
    if (afterInvalid.routing_permission !== "strict" || afterInvalid.routing_allowed_profiles.join() !== profile) {
      failures.push(`routing-invalid: the saved permission changed ${JSON.stringify(afterInvalid)}`);
    }
    // A hand-edited file holding an invalid identifier: shown, matches no
    // profile, and every save is refused with the reason until it is fixed.
    const home = process.env.LIGHTAGENT_HOME;
    if (!home) {
      failures.push("routing-config-error: LIGHTAGENT_HOME is not set for the render harness");
    } else {
      const file = `${home}/config.json`;
      const saved = JSON.parse(await readFile(file, "utf8"));
      saved.platform.jev.routing_allowed_profiles = [profile, "Not A Profile!"];
      await writeFile(file, JSON.stringify(saved, null, 2));
      await open("settings");
      await routingSection().getByText("not found as a profile: Not A Profile!").waitFor({ timeout: 15000 });
      // The save is refused, so the box stays checked: click, not uncheck.
      await routingSection().getByRole("checkbox", { name: new RegExp(`\\(${profile}\\)`) }).click();
      await page.getByText("must contain valid profile identifiers").first().waitFor({ timeout: 10000 });
      await page.screenshot({ path: `${dir}/18-routing-config-error.png`, fullPage: true });
      const status = await api("/jev");
      if (status.routing?.profile_permission !== "allowed") {
        failures.push(`routing-config-error: a refused save changed the permission (${status.routing?.profile_permission})`);
      }
      console.log("  [ok] jev routing configuration errors: refused and explained");
    }
    // Revoke through the API; the next status read reports it at once.
    await configure({ jev: { routing_allowed_profiles: [] } });
    if ((await api("/jev")).routing?.profile_permission !== "denied") failures.push("routing-revoked: status still permits");
    await capture("19-routing-revoked", [`Denied for ${profile}`]);

    // The controls themselves: keyboard, labels and inline errors.
    await open("settings");
    await page.getByTestId("jev-classification").waitFor({ timeout: 15000 });
    await page.getByLabel("Automatic classification").selectOption("active");
    if (!(await eventually(async () => (await api("/settings")).jev.classification.mode === "active"))) {
      failures.push("controls: choosing Active in the select did not save");
    }
    const thresholds = page.getByLabel("Calibrated thresholds by Jev version");
    await thresholds.fill("jev-1.13.0");
    await thresholds.blur();
    await page.getByRole("alert").filter({ hasText: "version=threshold" }).waitFor({ timeout: 5000 });
    await thresholds.fill("jev-1.13.0=0.8");
    await thresholds.blur();
    if (!(await eventually(async () =>
      (await api("/settings")).jev.classification.version_thresholds["jev-1.13.0"] === 0.8))) {
      failures.push("controls: a valid version threshold did not save");
    }
    await page.getByLabel("What model routing sends to Jev").selectOption("legacy_raw");
    if (!(await eventually(async () => (await api("/settings")).jev.routing_input === "legacy_raw"))) {
      failures.push("controls: choosing legacy raw routing input did not save");
    }
    await page.getByLabel("What model routing sends to Jev").selectOption("sanitized");
    await page.getByRole("switch", { name: "Workflow guidance" }).click();
    if (!(await eventually(async () => (await api("/settings")).jev.classification.workflow_guidance === true))) {
      failures.push("controls: the workflow guidance switch did not save");
    }
    const check = page.getByRole("button", { name: "Test connection" });
    // The button is disabled while the settings save above is in flight; the
    // API can report the new value before the panel re-enables it. Pressing
    // Enter on a disabled button does nothing, so wait until it can act.
    if (!(await eventually(async () => await check.isEnabled()))) {
      failures.push("controls: Test connection stayed disabled after the settings saved");
    }
    await check.focus();
    await page.keyboard.press("Enter");
    await page.getByRole("status").filter({ hasText: "Connection verified" }).waitFor({ timeout: 10000 });
    await endpointsCard().screenshot({ path: `${dir}/13-controls-dark.png` });
    console.log("  [ok] jev controls: selects, switches, inline validation, keyboard connection check");
  } catch (error) {
    failures.push(`jev states: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    await context.tracing.stop({ path: `${dir}/jev-trace.zip` });
    await context.close();
  }

  // Light appearance and a phone-width layout of the same, final state.
  for (const [scheme, width, height] of [["light", 1440, 3200], ["dark", 390, 4200], ["light", 390, 4200]]) {
    const view = await themed(scheme, width, height);
    const viewPage = await view.newPage();
    viewPage.on("pageerror", (error) => errors.push(String(error)));
    try {
      for (const route of ["tools", "settings"]) {
        await viewPage.goto(`${base}/#/${route}`, { waitUntil: "domcontentloaded" });
        const target = route === "tools"
          ? viewPage.getByTestId("jev-tool")
          : viewPage.getByTestId("platform-jev");
        await target.waitFor({ timeout: 15000 });
        const applied = await viewPage.evaluate(() => document.documentElement.getAttribute("data-theme"));
        if (applied !== scheme) failures.push(`${route} ${scheme}: the panel rendered the ${applied} theme`);
        const overflow = await viewPage.evaluate(() =>
          document.documentElement.scrollWidth - document.documentElement.clientWidth);
        if (overflow > 1) failures.push(`${route} ${scheme} ${width}px: page overflows horizontally by ${overflow}px`);
        await target.screenshot({ path: `${dir}/14-${route}-${scheme}-${width}.png` });
      }
    } catch (error) {
      failures.push(`jev ${scheme} ${width}px: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      await view.close();
    }
  }
  if (errors.length) failures.push(`jev screens: uncaught page error — ${errors[0]}`);
  stub.close();
  return failures;
}
