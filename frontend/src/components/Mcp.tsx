import { useEffect, useState } from "react";

import { agentApi, type McpCheck, type McpServerInput, type McpServerView, type McpState } from "../api/agent";
import { Empty, Pill } from "./Bits";
import { Card } from "./Card";

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

/** `NAME=value` or `Name: value` lines into a map; blank lines are skipped. */
function pairs(text: string, separator: "=" | ":"): Record<string, string> | string {
  const out: Record<string, string> = {};
  for (const line of text.split("\n").map((value) => value.trim()).filter(Boolean)) {
    const at = line.indexOf(separator);
    if (at <= 0) return `"${line}" needs the form name${separator}value`;
    out[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  return out;
}

function list(text: string): string[] {
  return text.split(",").map((value) => value.trim()).filter(Boolean);
}

/** What a server would run or reach, with names only. */
function Target({ server }: { server: McpServerView }) {
  if (server.transport === "stdio") {
    return (
      <div className="muted" style={{ overflowWrap: "anywhere" }}>
        Runs <code>{[server.command, ...server.args].join(" ")}</code>
        {server.env.length > 0 && <> · environment: {server.env.join(", ")} (values hidden)</>}
      </div>
    );
  }
  return (
    <div className="muted" style={{ overflowWrap: "anywhere" }}>
      Connects to <code>{server.url}</code>
      {server.headers.length > 0 && <> · headers: {server.headers.join(", ")} (values hidden)</>}
      {server.auth && <> · auth: {server.auth.kind === "env" ? `environment variable ${server.auth.var}` : "a saved key file"}</>}
      {server.untrusted_profiles.length > 0 && <> · other profiles allowed: {server.untrusted_profiles.join(", ")}</>}
      {server.credential_profiles.length > 0 && <> · credentials for: {server.credential_profiles.join(", ")}</>}
    </div>
  );
}

function CheckResult({ check }: { check: McpCheck }) {
  const runs = check.mcp_enabled ? "" : " Agent runs do not use it while MCP is off.";
  if (check.ok) {
    const names = check.tools.length ? check.tools.join(", ") : "no tools";
    return (
      <div className="notice notice--info" role="status">
        Connected in {check.elapsed_ms} ms and disconnected. {check.tools.length + check.more_tools} tool
        {check.tools.length + check.more_tools === 1 ? "" : "s"}: {names}
        {check.more_tools > 0 ? ` and ${check.more_tools} more` : ""}.{runs}
      </div>
    );
  }
  return (
    <div className="notice notice--warn" role="alert">
      Could not connect: {check.error ?? "no answer"}.{runs}
    </div>
  );
}

/**
 * MCP servers through the shared operations: the switch (turning it on is
 * confirmed), the timeout, each configured server with values withheld,
 * an explicit Check (a stdio server's command is shown and confirmed
 * first), removal, and an Add form. Nothing here starts a server except
 * Check.
 */
export function McpCard() {
  const [state, setState] = useState<McpState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [confirmOn, setConfirmOn] = useState(false);
  const [timeout, setTimeoutValue] = useState("");
  const [confirmCheck, setConfirmCheck] = useState<string | null>(null);
  const [checking, setChecking] = useState<string | null>(null);
  const [checks, setChecks] = useState<Record<string, McpCheck>>({});
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [env, setEnv] = useState("");
  const [url, setUrl] = useState("");
  const [headers, setHeaders] = useState("");
  const [authVar, setAuthVar] = useState("");
  const [untrusted, setUntrusted] = useState("");
  const [credential, setCredential] = useState("");
  const [formError, setFormError] = useState<string | null>(null);

  async function load() {
    try {
      const next = await agentApi.mcp();
      setState(next);
      setTimeoutValue(String(next.mcp.timeout_secs));
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function act(action: (revision: string) => Promise<McpState>, done: string) {
    if (!state || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const next = await action(state.revision);
      setState(next);
      setTimeoutValue(String(next.mcp.timeout_secs));
      setMessage(`${done} ${next.effect}`);
    } catch (cause) {
      setError(errorText(cause));
      await load();
    } finally {
      setBusy(false);
    }
  }

  async function check(server: string) {
    setConfirmCheck(null);
    setChecking(server);
    setError(null);
    try {
      const result = await agentApi.checkMcpServer(server);
      setChecks((current) => ({ ...current, [server]: result }));
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setChecking(null);
    }
  }

  function addServer() {
    setFormError(null);
    let server: McpServerInput;
    if (transport === "stdio") {
      const values = pairs(env, "=");
      if (typeof values === "string") {
        setFormError(values);
        return;
      }
      server = {
        transport, name: name.trim(), command: command.trim(),
        args: args.split("\n").map((value) => value.trim()).filter(Boolean), env: values,
      };
    } else {
      const values = pairs(headers, ":");
      if (typeof values === "string") {
        setFormError(values);
        return;
      }
      server = {
        transport, name: name.trim(), url: url.trim(), headers: values,
        auth: authVar.trim() ? { kind: "env", var: authVar.trim() } : { kind: "none" },
        untrusted_profiles: list(untrusted), credential_profiles: list(credential),
      };
    }
    if (!state || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    agentApi.addMcpServer(server, state.revision)
      .then((next) => {
        setState(next);
        setMessage(`Added ${server.name}. Nothing was started. ${next.effect}`);
        setName(""); setCommand(""); setArgs(""); setEnv(""); setUrl(""); setHeaders(""); setAuthVar("");
        setUntrusted(""); setCredential("");
      })
      .catch(async (cause) => {
        setFormError(errorText(cause));
        await load();
      })
      .finally(() => setBusy(false));
  }

  const enabled = state?.mcp.enabled ?? false;
  const ready = Boolean(state) && !busy;
  return (
    <Card title="MCP servers">
      <div data-testid="mcp-card">
        <p className="card__note">
          Model Context Protocol servers the agent can use as tools, from the same configuration the terminal reads.
          Values of environment variables and headers are never shown. Nothing here starts a server except Check.
        </p>
        {loadError && (
          <div className="notice notice--warn" role="alert">
            MCP settings are unavailable: {loadError}{" "}
            <button type="button" className="btn" onClick={() => void load()}>Retry</button>
          </div>
        )}
        {error && <div className="notice notice--warn" role="alert">{error}</div>}
        {message && <div className="notice notice--info" role="status">{message}</div>}
        {!state && !loadError && <span className="muted">Loading MCP settings…</span>}
        {state && (
          <>
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <strong>MCP</strong>
              <Pill tone={enabled ? "warn" : "neutral"}>{enabled ? "On" : "Off"}</Pill>
              {enabled ? (
                <button type="button" className="btn" disabled={!ready}
                  onClick={() => void act((revision) => agentApi.setMcp({ enabled: false }, revision), "MCP is off.")}>
                  Turn MCP off
                </button>
              ) : !confirmOn ? (
                <button type="button" className="btn" disabled={!ready} onClick={() => setConfirmOn(true)}>
                  Turn MCP on
                </button>
              ) : (
                <>
                  <button type="button" className="btn btn--danger" disabled={!ready}
                    onClick={() => void act((revision) => agentApi.setMcp({ enabled: true }, revision), "MCP is on.")
                      .then(() => setConfirmOn(false))}>
                    Confirm: turn MCP on
                  </button>
                  <button type="button" className="btn" disabled={busy} onClick={() => setConfirmOn(false)}>
                    Keep MCP off
                  </button>
                </>
              )}
            </div>
            {confirmOn && !enabled && (
              <p className="muted" style={{ margin: "6px 0 0" }}>
                With MCP on, each agent run starts or connects every server below and every server an active
                extension declares, and offers their tools. Each tool call still follows the approval policy.
              </p>
            )}
            <div className="field" style={{ marginTop: 10 }}>
              <label className="field__label" htmlFor="mcp-timeout">Timeout for each request (seconds)</label>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                <input id="mcp-timeout" className="input" type="number" min={1} max={600} value={timeout}
                  disabled={!ready} style={{ maxWidth: 120 }} onChange={(event) => setTimeoutValue(event.target.value)} />
                <button type="button" className="btn" disabled={!ready || Number(timeout) === state.mcp.timeout_secs}
                  onClick={() => void act((revision) => agentApi.setMcp({ timeout_secs: Number(timeout) }, revision),
                    `Timeout set to ${timeout} seconds.`)}>
                  Save timeout
                </button>
              </div>
            </div>
            {state.mcp.servers.length === 0 ? (
              <Empty title="No MCP servers configured" hint="Add one below. Servers an extension declares are shown in its details." />
            ) : (
              <ul style={{ listStyle: "none", padding: 0, margin: "12px 0", display: "grid", gap: 12 }}>
                {state.mcp.servers.map((server) => (
                  <li key={server.name} data-testid={`mcp-${server.name}`} style={{ minWidth: 0 }}>
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                      <strong>{server.name}</strong>
                      <Pill tone="info">{server.transport === "stdio" ? "Local command" : "HTTP"}</Pill>
                    </div>
                    <Target server={server} />
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 8 }}>
                      {confirmCheck === server.name ? (
                        <>
                          <button type="button" className="btn btn--danger" disabled={checking !== null}
                            onClick={() => void check(server.name)}>
                            {`Confirm: start and check ${server.name}`}
                          </button>
                          <button type="button" className="btn" onClick={() => setConfirmCheck(null)}>Cancel</button>
                        </>
                      ) : (
                        <button type="button" className="btn" disabled={checking !== null || busy}
                          aria-busy={checking === server.name || undefined} aria-label={`Check ${server.name}`}
                          onClick={() => server.transport === "stdio" ? setConfirmCheck(server.name) : void check(server.name)}>
                          {checking === server.name ? "Checking…" : "Check"}
                        </button>
                      )}
                      {confirmRemove === server.name ? (
                        <>
                          <button type="button" className="btn btn--danger" disabled={busy}
                            onClick={() => void act((_revision) => agentApi.removeMcpServer(server.name), `Removed ${server.name}.`)
                              .then(() => setConfirmRemove(null))}>
                            {`Confirm: remove ${server.name}`}
                          </button>
                          <button type="button" className="btn" disabled={busy} onClick={() => setConfirmRemove(null)}>
                            {`Keep ${server.name}`}
                          </button>
                        </>
                      ) : (
                        <button type="button" className="btn" disabled={busy} aria-label={`Remove ${server.name}`}
                          onClick={() => setConfirmRemove(server.name)}>
                          Remove
                        </button>
                      )}
                    </div>
                    {confirmCheck === server.name && server.transport === "stdio" && (
                      <p className="muted" style={{ margin: "6px 0 0", overflowWrap: "anywhere" }}>
                        Checking starts <code>{[server.command, ...server.args].join(" ")}</code> on this computer, as you,
                        lists its tools and stops it. It changes no setting.
                      </p>
                    )}
                    {checks[server.name] && <CheckResult check={checks[server.name]} />}
                  </li>
                ))}
              </ul>
            )}
            <fieldset style={{ border: 0, padding: 0, margin: "16px 0 0", minWidth: 0 }}>
              <legend className="field__label">Add an MCP server</legend>
              <fieldset style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
                <legend className="field__label">Kind</legend>
                <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
                  <input type="radio" name="mcp-transport" checked={transport === "stdio"} disabled={!ready}
                    onChange={() => setTransport("stdio")} />
                  Local command (stdio)
                </label>
                <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
                  <input type="radio" name="mcp-transport" checked={transport === "http"} disabled={!ready}
                    onChange={() => setTransport("http")} />
                  HTTP endpoint
                </label>
              </fieldset>
              <div className="field">
                <label className="field__label" htmlFor="mcp-name">Name</label>
                <input id="mcp-name" className="input" value={name} spellCheck={false} disabled={!ready}
                  onChange={(event) => setName(event.target.value)} />
              </div>
              {transport === "stdio" ? (
                <>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-command">Command</label>
                    <input id="mcp-command" className="input" value={command} spellCheck={false} disabled={!ready}
                      onChange={(event) => setCommand(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-args">Arguments (one per line)</label>
                    <textarea id="mcp-args" className="input" rows={2} value={args} spellCheck={false} disabled={!ready}
                      onChange={(event) => setArgs(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-env">Environment (NAME=value per line)</label>
                    <textarea id="mcp-env" className="input" rows={2} value={env} spellCheck={false} disabled={!ready}
                      autoComplete="off" onChange={(event) => setEnv(event.target.value)} />
                  </div>
                </>
              ) : (
                <>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-url">URL</label>
                    <input id="mcp-url" className="input" type="url" placeholder="https://" value={url} spellCheck={false}
                      disabled={!ready} onChange={(event) => setUrl(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-headers">Headers (Name: value per line)</label>
                    <textarea id="mcp-headers" className="input" rows={2} value={headers} spellCheck={false}
                      disabled={!ready} autoComplete="off" onChange={(event) => setHeaders(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-auth">Bearer key from environment variable (optional)</label>
                    <input id="mcp-auth" className="input" value={authVar} spellCheck={false} disabled={!ready}
                      onChange={(event) => setAuthVar(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-untrusted">Other profiles that may use it (comma-separated)</label>
                    <input id="mcp-untrusted" className="input" value={untrusted} spellCheck={false} disabled={!ready}
                      onChange={(event) => setUntrusted(event.target.value)} />
                  </div>
                  <div className="field">
                    <label className="field__label" htmlFor="mcp-credential">Profiles that may send its credentials (comma-separated)</label>
                    <input id="mcp-credential" className="input" value={credential} spellCheck={false} disabled={!ready}
                      onChange={(event) => setCredential(event.target.value)} />
                  </div>
                </>
              )}
              {formError && (
                <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{formError}</div>
              )}
              <p className="muted" style={{ margin: "6px 0 0" }}>Adding a server starts nothing and does not turn MCP on.</p>
              <button type="button" className="btn" style={{ marginTop: 10 }}
                disabled={!ready || !name.trim() || (transport === "stdio" ? !command.trim() : !url.trim())}
                onClick={addServer}>
                Add server
              </button>
            </fieldset>
          </>
        )}
      </div>
    </Card>
  );
}
