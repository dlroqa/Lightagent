import { useCallback, useEffect, useId, useState, type FormEvent } from "react";

import { agentApi, type ConfigView } from "../api/agent";
import { Card } from "./Card";
import "./config-editor.css";

function message(cause: unknown): string {
  const text = cause instanceof Error ? cause.message : String(cause);
  return text.replace(/^\d{3}: /, "");
}

/**
 * The CLI's `lightagent config path|show|get|set`, on the same file: the
 * server reads and writes it with the same key rules, under the same lock.
 * A write names the revision it was made against; if the file changed in
 * the meantime it is refused, nothing is written, and the editor reloads.
 */
export function ConfigEditorCard() {
  const keyId = useId();
  const valueId = useId();
  const [view, setView] = useState<ConfigView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setView(await agentApi.config());
      setLoadError(null);
    } catch (cause) {
      setLoadError(message(cause));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function read() {
    const name = key.trim();
    if (!name) {
      setFormError("Enter a key, such as agent.max_turns.");
      return;
    }
    setBusy(true);
    setFormError(null);
    setNotice(null);
    try {
      const current = await agentApi.configKey(name);
      setValue(current.value);
      setNotice(`${current.key} = ${current.value || "(empty)"}`);
    } catch (cause) {
      setFormError(message(cause));
    } finally {
      setBusy(false);
    }
  }

  async function write(event: FormEvent) {
    event.preventDefault();
    const name = key.trim();
    if (!name || !view) {
      setFormError("Enter a key, such as agent.max_turns.");
      return;
    }
    setBusy(true);
    setFormError(null);
    setNotice(null);
    try {
      const saved = await agentApi.setConfigKey(name, value, view.revision);
      setNotice(`Set ${saved.key} = ${saved.value || "(empty)"}. New runs use it.`);
      await load();
    } catch (cause) {
      const text = cause instanceof Error ? cause.message : String(cause);
      if (text.startsWith("409:")) {
        setFormError("The configuration changed since it was loaded (another window, the CLI or the desktop app). Nothing was written; the current configuration is loaded now. Check it, then set the key again.");
        await load();
      } else {
        setFormError(message(cause));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Advanced configuration">
      <p className="card__note">
        The keys of <code>lightagent config get</code> and <code>set</code>, on the same file
        {view ? <>: <code>{view.path}</code></> : null}. Secrets are never shown.
      </p>
      {loadError && (
        <p className="config-editor__error" role="alert">
          The configuration could not be loaded: {loadError}
        </p>
      )}
      <form className="config-editor" onSubmit={(event) => void write(event)}>
        <label htmlFor={keyId}>Key</label>
        <input id={keyId} value={key} onChange={(event) => setKey(event.target.value)}
          placeholder="agent.max_turns" autoComplete="off" spellCheck={false} />
        <label htmlFor={valueId}>Value</label>
        <input id={valueId} value={value} onChange={(event) => setValue(event.target.value)}
          autoComplete="off" spellCheck={false} />
        <div className="config-editor__actions">
          <button type="button" className="btn" disabled={busy} onClick={() => void read()}>Read</button>
          <button type="submit" className="btn btn--primary" disabled={busy || !view}>Set</button>
        </div>
      </form>
      {formError && <p className="config-editor__error" role="alert">{formError}</p>}
      {notice && <p className="notice notice--info" role="status">{notice}</p>}
      {view && (
        <details className="config-editor__show">
          <summary>Current configuration (secrets redacted)</summary>
          <pre>{JSON.stringify(view.config, null, 2)}</pre>
        </details>
      )}
    </Card>
  );
}
