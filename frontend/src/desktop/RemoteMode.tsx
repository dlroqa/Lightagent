import { useEffect, useId, useState } from "react";

import { invoke, isDesktop } from "../api/transport";
import { Card } from "../components/Card";

type HostStatus = {
  mode?: "local" | "remote";
  remote?: string | null;
  runtime_version: string | null;
  profile: string | null;
};

type RemoteOutcome = { state: "connected" | "cancelled"; origin: string | null; version: string | null };

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

/**
 * Remote mode (desktop only): use a Lightagent server elsewhere instead of
 * this computer's runtime. The host reads the key itself (its own
 * LIGHTAGENT_API_KEY, or a key file picked in the native picker) and keeps
 * it; this window only names the server (desktop/src-tauri/src/remote.rs).
 */
export function RemoteModeCard() {
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [address, setAddress] = useState("");
  const [source, setSource] = useState<"environment" | "file">("environment");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const keyGroup = useId();

  useEffect(() => {
    if (!isDesktop) return;
    void invoke<HostStatus>("runtime_status").then(setStatus).catch(() => undefined);
  }, []);
  if (!isDesktop) return null;
  const remote = status?.mode === "remote";

  async function connect() {
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const outcome = await invoke<RemoteOutcome>("remote_connect", { url: address.trim(), source });
      if (outcome.state === "cancelled") {
        setMessage("No key file was chosen, so nothing was sent.");
        return;
      }
      // Everything on screen came from this computer's runtime: start fresh.
      window.location.reload();
    } catch (cause) {
      setError(`${errorText(cause)}. This computer's runtime is still in use.`);
    } finally {
      setBusy(false);
    }
  }

  async function disconnect() {
    setBusy(true);
    setError(null);
    try {
      await invoke("remote_disconnect");
      window.location.reload();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Remote server">
      <p className="card__note">
        Use a Lightagent server elsewhere, started with <code>lightagent serve --key-env LIGHTAGENT_API_KEY</code>,
        instead of this computer&apos;s runtime. While connected nothing runs here; if the server fails, its error is
        shown and no work moves to this computer. The key stays in the app&apos;s host process: this window never sees it.
      </p>
      {remote ? (
        <>
          <p>
            Connected to <strong>{status?.remote}</strong> · Lightagent {status?.runtime_version ?? "?"} · profile{" "}
            {status?.profile ?? "?"}
          </p>
          <button type="button" className="btn" disabled={busy} aria-busy={busy || undefined}
            onClick={() => void disconnect()}>Disconnect and use this computer</button>
        </>
      ) : (
        <>
          <div className="field">
            <label className="field__label" htmlFor="remote-url">Server address (https://…, or http:// for this computer only)</label>
            <input id="remote-url" className="input" value={address} spellCheck={false} autoComplete="url"
              placeholder="https://agent.example.com:8787" disabled={busy}
              onChange={(event) => setAddress(event.target.value)} />
          </div>
          <fieldset className="field" style={{ border: 0, padding: 0, margin: "10px 0 0" }}>
            <legend className="field__label">The server&apos;s key</legend>
            <label className="jev-check">
              <input type="radio" name={keyGroup} checked={source === "environment"} disabled={busy}
                onChange={() => setSource("environment")} />
              LIGHTAGENT_API_KEY in this app&apos;s environment
            </label>
            <label className="jev-check">
              <input type="radio" name={keyGroup} checked={source === "file"} disabled={busy}
                onChange={() => setSource("file")} />
              A key file chosen on this computer…
            </label>
          </fieldset>
          <button type="button" className="btn" style={{ marginTop: 10 }} disabled={busy || !address.trim()}
            aria-busy={busy || undefined} onClick={() => void connect()}>Connect</button>
        </>
      )}
      {message && <div className="notice notice--info" role="status">{message}</div>}
      {error && <div className="notice notice--warn" role="alert">{error}</div>}
    </Card>
  );
}
