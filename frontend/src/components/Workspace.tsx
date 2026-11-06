import { useEffect, useState } from "react";

import { agentApi, type WorkspaceState } from "../api/agent";
import { Pill } from "./Bits";
import { Card } from "./Card";

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

/**
 * The folder file and terminal tools are confined to, set through the
 * operation `lightagent setup tools` shares: an absolute folder, or the
 * active profile's own folder.
 */
export function WorkspaceCard() {
  const [state, setState] = useState<WorkspaceState | null>(null);
  const [folder, setFolder] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  async function load(): Promise<WorkspaceState | null> {
    try {
      const next = await agentApi.workspace();
      setState(next);
      return next;
    } catch (cause) {
      setError(errorText(cause));
      return null;
    }
  }

  useEffect(() => {
    void load().then((next) => {
      if (next) setFolder(next.workspace.configured ?? "");
    });
  }, []);

  async function save(path: string | null) {
    if (!state || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const next = await agentApi.setWorkspace(path, state.revision);
      setState(next);
      setFolder(next.workspace.configured ?? "");
      setMessage(
        `${path === null ? "Runs use the profile's own workspace folder." : "Workspace folder saved."} ${next.effect}`,
      );
    } catch (cause) {
      const text = errorText(cause);
      if (text.startsWith("409:")) {
        // Nothing was saved; the entry stays in the field for another try.
        await load();
        setError(
          "The configuration changed elsewhere after this card loaded, so the folder was not saved. Your entry is kept and the current folder is shown; review it, then save again.",
        );
      } else {
        setError(text.replace(/^\d{3}:\s*/, ""));
      }
    } finally {
      setBusy(false);
    }
  }

  const view = state?.workspace ?? null;
  return (
    <Card title="Workspace">
      <div data-testid="workspace-card">
        <p className="card__note">
          The folder file and terminal tools are confined to. Shared with the terminal CLI (lightagent setup
          tools).{state ? ` ${state.effect}` : ""}
        </p>
        {error && <div className="notice notice--warn" role="alert">{error}</div>}
        {message && <div className="notice notice--info" role="status">{message}</div>}
        {view && (
          <p data-testid="workspace-effective" style={{ margin: "8px 0", overflowWrap: "anywhere" }}>
            <Pill tone={view.source === "config" ? "accent" : "neutral"}>
              {view.source === "config" ? "Chosen folder" : view.profile ? `Profile ${view.profile}` : "Profile folder"}
            </Pill>{" "}
            {view.effective ?? "the active profile's own workspace folder"}
            {view.effective && !view.exists ? <span className="muted"> (the next run creates it)</span> : null}
          </p>
        )}
        {view && !view.tools_enabled && (
          <p className="muted">File and terminal tools are off, so no run uses this folder until they are turned on.</p>
        )}
        <div className="field">
          <label className="field__label" htmlFor="workspace-folder">Workspace folder (absolute path)</label>
          <input id="workspace-folder" className="input" spellCheck={false} autoComplete="off" value={folder}
            disabled={!state || busy} onChange={(event) => setFolder(event.target.value)} />
          <p className="muted" style={{ margin: "4px 0 0" }}>
            An existing folder, or a new one inside an existing folder.
          </p>
        </div>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
          <button type="button" className="btn" disabled={!state || busy || !folder.trim()}
            aria-busy={busy || undefined} onClick={() => void save(folder)}>
            Save folder
          </button>
          {view?.configured && (
            <button type="button" className="btn" disabled={busy} onClick={() => void save(null)}>
              Use the profile's own folder
            </button>
          )}
        </div>
      </div>
    </Card>
  );
}
