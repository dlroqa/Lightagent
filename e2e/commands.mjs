// The chat's /help and /reload against the real harness. The account menu's
// Commands dialog lists every command the API returns (the chat's /help
// list), with what it does and where it is in the app, and Escape closes
// it. On the Tools screen, a web setting changed elsewhere (here through
// the API, as the CLI would) shows after Reload tools, not before. Web
// access is restored.
// crates/lightagent/tests/desktop_commands_reload.rs proves the list is the
// chat's and that the next run uses a changed setting; desktop/e2e/runs.py
// `commands` and `reload` cover the host.

export async function checkCommands(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const api = `${base}/api/lightagent/v1`;
  const setWeb = (value) =>
    fetch(`${api}/config/web.enabled`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ value }),
    });
  const webTools = async () =>
    ((await (await fetch(`${api}/tools`)).json()).tools ?? []).map((tool) => tool.name).filter((name) => /web/i.test(name));
  const step = async (name, body) => {
    try {
      await body();
    } catch (err) {
      failures.push(`commands: ${name}: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
      await page.screenshot({ path: `${outDir}/commands-failed-${name.replace(/\W+/g, "-")}.png` }).catch(() => {});
    }
  };
  const original = (await (await fetch(`${api}/config/web.enabled`)).json()).value;
  try {
    await step("the Commands dialog lists the chat's /help", async () => {
      const commands = (await (await fetch(`${api}/commands`)).json()).commands ?? [];
      if (commands.length === 0) throw new Error("the API lists no commands");
      await page.goto(`${base}/#/`);
      await page.getByRole("button", { name: "Open account menu" }).click({ timeout: 15000 });
      await page.getByRole("menuitem", { name: /Commands/ }).click({ timeout: 10000 });
      const dialog = page.getByRole("dialog", { name: "Commands" });
      await dialog.waitFor({ timeout: 10000 });
      for (const command of commands) {
        const row = dialog.getByTestId(`command-${command.name}`);
        await row.getByText(`/${command.name}`, { exact: true }).waitFor({ timeout: 5000 });
        await row.getByText(command.summary, { exact: true }).waitFor({ timeout: 5000 });
        if ((await row.innerText()).includes("Not in the app")) throw new Error(`/${command.name} has no place in the app`);
      }
      await page.screenshot({ path: `${outDir}/commands-dialog.png` });
      await page.keyboard.press("Escape");
      await dialog.waitFor({ state: "detached", timeout: 5000 });
    });

    await step("Reload tools shows a setting changed elsewhere", async () => {
      await setWeb("false");
      await page.goto(`${base}/#/tools`);
      const reload = page.getByRole("button", { name: "Reload tools", exact: true });
      await reload.waitFor({ timeout: 15000 });
      await reload.click();
      if ((await webTools()).length !== 0) throw new Error("web tools are listed with web access off");
      await setWeb("true");
      const names = await webTools();
      if (names.length === 0) throw new Error("the API lists no web tool with web access on");
      const tool = page.getByText(names[0], { exact: true }).first();
      if (await tool.isVisible()) throw new Error("the screen changed before Reload tools");
      await reload.click();
      await tool.waitFor({ timeout: 10000 });
      await page.screenshot({ path: `${outDir}/commands-reloaded-tools.png` });
    });
    if (errors.length) failures.push(`commands: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`commands: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
    await setWeb(String(original ?? "false")).catch(() => {});
  }
  return failures;
}
