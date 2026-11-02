import { useEffect, useState } from "react";

import {
  agentApi,
  type ExtensionDetail,
  type ExtensionListing,
  type ExtensionMcpServer,
  type ExtensionScope,
} from "../api/agent";
import { Empty, Pill } from "./Bits";
import { Card } from "./Card";

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

function statusOf(cause: unknown): number | null {
  const match = /^(\d{3}):/.exec(cause instanceof Error ? cause.message : String(cause));
  return match ? Number(match[1]) : null;
}

const SCOPE: Record<ExtensionScope, { label: string; tone: "info" | "accent" }> = {
  global: { label: "Every profile", tone: "info" },
  profile: { label: "This profile", tone: "accent" },
};

function servers(count: number): string {
  return `${count} MCP server${count === 1 ? "" : "s"}`;
}

/** What one MCP server would run or reach; only the names of its environment and headers. */
function McpServerLine({ server }: { server: ExtensionMcpServer }) {
  if (server.transport === "stdio") {
    return (
      <li>
        <strong>{server.name}</strong> runs{" "}
        <code style={{ overflowWrap: "anywhere" }}>{[server.command, ...server.args].join(" ")}</code>
        {server.env.length > 0 && (
          <span className="muted"> · environment: {server.env.join(", ")} (values hidden)</span>
        )}
      </li>
    );
  }
  return (
    <li>
      <strong>{server.name}</strong> connects to <code style={{ overflowWrap: "anywhere" }}>{server.url}</code>
      {server.headers.length > 0 && (
        <span className="muted"> · headers: {server.headers.join(", ")} (values hidden)</span>
      )}
      {server.auth && <span className="muted"> · sends a saved credential</span>}
    </li>
  );
}

function Details({ detail }: { detail: ExtensionDetail }) {
  const source = detail.source;
  const from = !source
    ? "Not recorded (placed by hand or by an earlier version)"
    : source.kind === "local"
      ? `The folder ${source.path}`
      : `${source.url}${source.subdir ? ` (folder ${source.subdir})` : ""} at commit ${source.revision.slice(0, 12)}`;
  const row = { margin: 0, overflowWrap: "anywhere" as const };
  return (
    <dl style={{ margin: "8px 0 0", display: "grid", gap: 8 }}>
      <div>
        <dt className="field__label">Folder</dt>
        <dd className="muted" style={row}>{detail.dir}</dd>
      </div>
      <div>
        <dt className="field__label">Installed from</dt>
        <dd className="muted" style={row}>{from}</dd>
      </div>
      <div>
        <dt className="field__label">Skills</dt>
        <dd className="muted" style={row}>{detail.skills.length ? detail.skills.join(", ") : "None"}</dd>
      </div>
      <div>
        <dt className="field__label">MCP servers</dt>
        <dd style={row}>
          {detail.mcp.length === 0 ? (
            <span className="muted">None</span>
          ) : (
            <>
              <ul style={{ margin: 0, paddingLeft: 18 }}>
                {detail.mcp.map((server) => <McpServerLine key={server.name} server={server} />)}
              </ul>
              <p className="muted" style={{ margin: "4px 0 0" }}>
                {detail.mcp_enabled
                  ? "MCP is on: these servers can run during agent runs, and each tool call follows the approval policy."
                  : "MCP is off, so these servers do not run."}
              </p>
            </>
          )}
        </dd>
      </div>
      {detail.instructions.trim() && (
        <div>
          <dt className="field__label">Instructions</dt>
          <dd className="muted" style={{ ...row, whiteSpace: "pre-wrap" }}>{detail.instructions.trim()}</dd>
        </div>
      )}
    </dl>
  );
}

/**
 * Extensions, through the operations `lightagent extensions` shares: list,
 * details, install, enable or disable, and uninstall. Installing copies a
 * folder and runs nothing; MCP is switched on only when asked.
 */
export function ExtensionsCard() {
  const [listing, setListing] = useState<ExtensionListing | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [detail, setDetail] = useState<ExtensionDetail | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [source, setSource] = useState("");
  const [scope, setScope] = useState<ExtensionScope>("global");
  const [activateMcp, setActivateMcp] = useState(false);
  const [sourceError, setSourceError] = useState<string | null>(null);

  async function load() {
    try {
      setListing(await agentApi.extensions());
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  /** One change; the list is reloaded afterwards whatever happened. */
  async function act(action: () => Promise<string>) {
    if (busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      setMessage(await action());
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      await load();
      setBusy(false);
    }
  }

  async function toggleDetails(name: string) {
    if (open === name) {
      setOpen(null);
      return;
    }
    setOpen(name);
    setDetail(null);
    try {
      setDetail(await agentApi.extension(name));
    } catch (cause) {
      setError(errorText(cause));
    }
  }

  async function install() {
    const value = source.trim();
    if (!value || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    setSourceError(null);
    try {
      const installed = await agentApi.installExtension(value, scope, activateMcp);
      const count = installed.mcp_servers.length;
      const mcp = count === 0
        ? ""
        : installed.mcp_enabled
          ? ` MCP is on: its ${servers(count)} can run during agent runs.`
          : ` Its ${servers(count)} do not run while MCP is off; review them in Details.`;
      setMessage(`Installed ${installed.name} for ${installed.scope === "profile" ? "this profile" : "every profile"}.${mcp}`);
      setSource("");
      setActivateMcp(false);
    } catch (cause) {
      const status = statusOf(cause);
      if (status === 400 || status === 409 || status === 502) setSourceError(errorText(cause));
      else setError(errorText(cause));
    } finally {
      await load();
      setBusy(false);
    }
  }

  return (
    <Card title="Extensions">
      <div data-testid="extensions-card">
        <p className="card__note">
          Shared with the terminal CLI (lightagent extensions). An extension adds skills, instructions and MCP
          servers. Installing one copies its folder and runs nothing.
        </p>
        {loadError && (
          <div className="notice notice--warn" role="alert">
            Extensions are unavailable: {loadError}{" "}
            <button type="button" className="btn" onClick={() => void load()}>Retry</button>
          </div>
        )}
        {listing && !listing.enabled && (
          <div className="notice notice--warn" role="status">
            Extensions are off for every profile (extensions.enabled), so none of them is used.
          </div>
        )}
        {error && <div className="notice notice--warn" role="alert">{error}</div>}
        {message && <div className="notice notice--info" role="status">{message}</div>}
        {!listing && !loadError && <span className="muted">Loading extensions…</span>}
        {listing && listing.extensions.length === 0 && (
          <Empty title="No extensions installed" hint="Install one below from a folder or an https:// Git repository." />
        )}
        {listing && listing.extensions.length > 0 && (
          <ul style={{ listStyle: "none", padding: 0, margin: "8px 0", display: "grid", gap: 12 }}>
            {listing.extensions.map((ext) => {
              const scopeBadge = SCOPE[ext.scope] ?? { label: ext.scope, tone: "info" as const };
              const detailId = `extension-detail-${ext.name}`;
              return (
                <li key={ext.name} data-testid={`extension-${ext.name}`} style={{ minWidth: 0 }}>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                    <strong>{ext.name}</strong>
                    {ext.version && <span className="muted">{ext.version}</span>}
                    <Pill tone={scopeBadge.tone}>{scopeBadge.label}</Pill>
                    <Pill tone={ext.active ? "ok" : "neutral"}>{ext.active ? "Active" : "Inactive"}</Pill>
                    {ext.mcp_servers.length > 0 && <Pill tone="warn">{servers(ext.mcp_servers.length)}</Pill>}
                  </div>
                  {ext.description && <div className="muted">{ext.description}</div>}
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 8 }}>
                    <button type="button" className="btn" aria-expanded={open === ext.name}
                      aria-controls={detailId} aria-label={`Details for ${ext.name}`}
                      onClick={() => void toggleDetails(ext.name)}>
                      {open === ext.name ? "Hide details" : "Details"}
                    </button>
                    {confirmRemove !== ext.name ? (
                      <>
                        <button type="button" className="btn" disabled={busy}
                          aria-label={`${ext.active ? "Disable" : "Enable"} ${ext.name}`}
                          onClick={() => void act(async () => {
                            const result = await agentApi.setExtensionEnabled(ext.name, !ext.active);
                            if (ext.active) return `Disabled ${ext.name}; it stays installed.`;
                            return result.extensions_enabled
                              ? `Enabled ${ext.name}.`
                              : `Enabled ${ext.name}, but extensions are off for every profile.`;
                          })}>
                          {ext.active ? "Disable" : "Enable"}
                        </button>
                        <button type="button" className="btn" disabled={busy} aria-label={`Uninstall ${ext.name}`}
                          onClick={() => setConfirmRemove(ext.name)}>
                          Uninstall
                        </button>
                      </>
                    ) : (
                      <>
                        <button type="button" className="btn btn--danger" disabled={busy} aria-busy={busy || undefined}
                          onClick={() => void act(async () => {
                            const removed = await agentApi.uninstallExtension(ext.name, ext.scope);
                            setConfirmRemove(null);
                            if (open === ext.name) setOpen(null);
                            return `Uninstalled ${ext.name} from ${removed.removed_from}.`;
                          })}>
                          {`Confirm: uninstall ${ext.name}`}
                        </button>
                        <button type="button" className="btn" disabled={busy} onClick={() => setConfirmRemove(null)}>
                          {`Keep ${ext.name}`}
                        </button>
                      </>
                    )}
                  </div>
                  {confirmRemove === ext.name && (
                    <p className="muted" style={{ margin: "6px 0 0" }}>
                      Deletes its installed folder. Its skills, instructions and MCP servers are gone from the next
                      run.
                    </p>
                  )}
                  {open === ext.name && (
                    <div id={detailId} role="region" aria-label={`Details of ${ext.name}`}>
                      {detail && detail.name === ext.name
                        ? <Details detail={detail} />
                        : <span className="muted">Loading details…</span>}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}

        <fieldset style={{ border: 0, padding: 0, margin: "16px 0 0", minWidth: 0 }}>
          <legend className="field__label">Install an extension</legend>
          <div className="field">
            <label className="field__label" htmlFor="extension-source">Folder or Git URL</label>
            <input id="extension-source" className="input" value={source} spellCheck={false} disabled={busy}
              placeholder="/path/to/extension or https://host/repo.git#folder"
              aria-describedby="extension-source-help" aria-invalid={sourceError ? true : undefined}
              onChange={(event) => {
                setSource(event.target.value);
                setSourceError(null);
              }} />
            <p id="extension-source-help" className="muted" style={{ margin: "4px 0 0" }}>
              An absolute folder on this computer, or an https:// Git repository (add #folder for a folder in it).
              The commit installed is recorded.
            </p>
            {sourceError && (
              <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{sourceError}</div>
            )}
          </div>
          <fieldset style={{ border: 0, padding: 0, margin: "10px 0 0", minWidth: 0 }}>
            <legend className="field__label">Install for</legend>
            <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <input type="radio" name="extension-scope" checked={scope === "global"} disabled={busy}
                onChange={() => setScope("global")} />
              Every profile
            </label>
            <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <input type="radio" name="extension-scope" checked={scope === "profile"} disabled={busy}
                onChange={() => setScope("profile")} />
              This profile only
            </label>
          </fieldset>
          <label style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 10 }}>
            <input type="checkbox" checked={activateMcp} disabled={busy}
              aria-describedby="extension-mcp-help" onChange={(event) => setActivateMcp(event.target.checked)} />
            Turn MCP on if it brings MCP servers
          </label>
          <p id="extension-mcp-help" className="muted" style={{ margin: "4px 0 0" }}>
            Off by default. With MCP on, every configured MCP server can run during agent runs; leave it off to
            review the servers in Details first.
          </p>
          <button type="button" className="btn" style={{ marginTop: 10 }} disabled={busy || !source.trim()}
            aria-busy={busy || undefined} onClick={() => void install()}>
            Install
          </button>
        </fieldset>
      </div>
    </Card>
  );
}
