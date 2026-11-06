// The Settings screen's Security card against the real harness (D7-13):
// it shows the [security] section exactly as GET /config reports it (the
// section `lightagent config show` prints), read-only. The approval
// policy's control is covered by e2e/settings.mjs; desktop_security.rs and
// runs.py `security` prove the values are the CLI's.

export async function checkSecurity(browser, base, outDir) {
  const failures = [];
  const page = await browser.newPage({ viewport: { width: 1280, height: 2200 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    const security = (await (await fetch(`${base}/api/lightagent/v1/config`)).json()).config.security;
    if (typeof security?.approval_policy !== "string") throw new Error(`no [security] in GET /config: ${JSON.stringify(security)}`);
    await page.goto(`${base}/#/settings`);
    const card = page.getByTestId("settings-security");
    await card.waitFor({ timeout: 15000 });
    const field = async (name) => (await card.locator(`[data-field="${name}"]`).innerText()).trim();
    const yesNo = (value) => (value ? "yes" : "no");
    const expected = {
      approval_policy: security.approval_policy,
      remote_tools_default: yesNo(security.remote_tools_default),
      privacy_mode: yesNo(security.privacy_mode),
    };
    for (const [name, value] of Object.entries(expected)) {
      const shown = await field(name);
      if (shown !== value) failures.push(`security: ${name} shows ${JSON.stringify(shown)}, the API reports ${JSON.stringify(value)}`);
    }
    if ((await card.locator("input, select, textarea, button").count()) !== 0) {
      failures.push("security: the [security] card offers a control; it is read-only");
    }
    await page.screenshot({ path: `${outDir}/settings-security.png`, fullPage: true });
    if (errors.length) failures.push(`security: page errors: ${errors.join("; ")}`);
  } catch (err) {
    failures.push(`security: ${err instanceof Error ? err.message.split("\n")[0] : String(err)}`);
  } finally {
    await page.close();
  }
  return failures;
}
