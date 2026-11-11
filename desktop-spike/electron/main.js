// D1 evidence spike: one sandboxed window over a static page. It appends
// timing lines to the file named by SPIKE_REPORT and quits after
// SPIKE_EXIT_AFTER_SECS, so a runner can measure it unattended.
const { app, BrowserWindow } = require("electron");
const fs = require("fs");
const path = require("path");

const report = process.env.SPIKE_REPORT;
const exitAfter = Number(process.env.SPIKE_EXIT_AFTER_SECS || 0);

function note(line) {
  if (report) fs.appendFileSync(report, `${line}\n`);
}

app.whenReady().then(() => {
  const win = new BrowserWindow({
    width: 1100,
    height: 720,
    title: "Lightagent Desktop Spike",
    webPreferences: { contextIsolation: true, sandbox: true, nodeIntegration: false },
  });
  win.webContents.once("did-finish-load", () => {
    note(`SPIKE_PAGE_LOADED_MS ${Math.round(process.uptime() * 1000)}`);
    setTimeout(() => {
      const kb = app.getAppMetrics().reduce((sum, metric) => sum + metric.memory.workingSetSize, 0);
      note(`SPIKE_APP_METRICS_WORKING_SET_KB ${kb}`);
    }, 10000);
  });
  win.loadFile(path.join(__dirname, "index.html"));
  if (exitAfter > 0) setTimeout(() => app.quit(), exitAfter * 1000);
});
