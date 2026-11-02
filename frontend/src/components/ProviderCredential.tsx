import { useEffect, useState } from "react";

import { agentApi, type ProviderCredentialStatus } from "../api/agent";
import { Pill } from "./Bits";
import { Card } from "./Card";

type Tone = "ok" | "warn" | "danger" | "info" | "accent" | "neutral";

const STATE: Record<ProviderCredentialStatus["state"], { label: string; tone: Tone; hint: string }> = {
  absent: {
    label: "No key",
    tone: "neutral",
    hint: "Requests are sent without a key, as local providers such as Lightweight expect.",
  },
  ready: {
    label: "Key saved",
    tone: "ok",
    hint: "Runs send this key to the provider. It is never shown again.",
  },
  unreadable: {
    label: "Cannot be read",
    tone: "danger",
    hint: "The configured key file cannot be read, so runs are refused until the key is replaced or removed.",
  },
  unresolved: {
    label: "Variable not set",
    tone: "danger",
    hint: "The configured environment variable is not set, so runs are refused until it is set, or the key is replaced or removed.",
  },
  empty: {
    label: "Empty",
    tone: "danger",
    hint: "The configured key is empty, so runs are refused until it is replaced or removed.",
  },
  invalid: {
    label: "Invalid",
    tone: "danger",
    hint: "The configured key contains a line break or control character, so runs are refused until it is replaced or removed.",
  },
};

/**
 * The active endpoint's provider API key: its status, a write-only field to
 * save or replace it, and removal after a confirmation. The key goes to the
 * runtime's shared secrets store and is never read back.
 */
export function ProviderCredentialCard() {
  const [status, setStatus] = useState<ProviderCredentialStatus | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  async function load() {
    try {
      setStatus(await agentApi.providerCredential());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function save() {
    if (!status || busy || !key.trim()) return;
    const replacing = status.state !== "absent";
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      setStatus(await agentApi.saveProviderCredential(key, status.revision));
      setKey("");
      setMessage(replacing ? "Key replaced." : "Key saved.");
    } catch (cause) {
      const text = cause instanceof Error ? cause.message : String(cause);
      if (text.startsWith("409:")) {
        // Nothing was saved; the entry stays in the field for another try.
        setError("The configuration changed elsewhere after this card loaded, so the key was not saved. Your entry is kept and the current status is shown; save again to store it.");
        void load();
      } else {
        setError(text);
      }
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      setStatus(await agentApi.removeProviderCredential());
      setConfirmRemove(false);
      setMessage("Key removed. Requests are sent without a key.");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  const view = status ? STATE[status.state] ?? { label: status.state, tone: "neutral" as Tone, hint: "" } : null;
  const configured = status !== null && status.state !== "absent";
  return (
    <Card title="Provider API key">
      <p className="card__note">
        For the active endpoint{status ? ` (${status.base_url})` : ""}. Stored owner-only on this machine and
        shared with the terminal CLI; the key is write-only and never shown.
      </p>
      {error && <div className="notice notice--warn" role="alert">{error}</div>}
      {message && <div className="notice notice--info" role="status">{message}</div>}
      {status && view && (
        <p data-testid="provider-credential-state" style={{ margin: "8px 0" }}>
          <Pill tone={view.tone}>{view.label}</Pill>
          {status.source === "env" ? " (from an environment variable)" : ""}{" "}
          <span className="muted">{view.hint}</span>
        </p>
      )}
      <div className="field">
        <label className="field__label" htmlFor="provider-api-key">{configured ? "Replace the key" : "API key"}</label>
        <input id="provider-api-key" className="input" type="password" autoComplete="off" spellCheck={false}
          value={key} disabled={!status || busy} onChange={(event) => setKey(event.target.value)} />
      </div>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
        <button type="button" className="btn" disabled={!status || busy || !key.trim()} aria-busy={busy || undefined}
          onClick={() => void save()}>
          {configured ? "Replace key" : "Save key"}
        </button>
        {configured && !confirmRemove && (
          <button type="button" className="btn" disabled={busy} onClick={() => setConfirmRemove(true)}>
            Remove key
          </button>
        )}
        {configured && confirmRemove && (
          <>
            <button type="button" className="btn" disabled={busy} onClick={() => void remove()}>
              Confirm: remove the key
            </button>
            <button type="button" className="btn" disabled={busy} onClick={() => setConfirmRemove(false)}>
              Keep the key
            </button>
          </>
        )}
      </div>
    </Card>
  );
}
