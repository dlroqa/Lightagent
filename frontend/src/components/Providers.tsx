import { useEffect, useState } from "react";

import { agentApi, type ProviderEntry, type ProviderList } from "../api/agent";
import { Pill } from "./Bits";
import { Card } from "./Card";

type Tone = "ok" | "warn" | "danger" | "info" | "accent" | "neutral";

const KEY: Record<ProviderEntry["key_state"], { label: string; tone: Tone }> = {
  absent: { label: "No key", tone: "neutral" },
  ready: { label: "Key saved", tone: "ok" },
  unreadable: { label: "Key cannot be read", tone: "danger" },
  unresolved: { label: "Key variable not set", tone: "danger" },
  empty: { label: "Key empty", tone: "danger" },
  invalid: { label: "Key invalid", tone: "danger" },
};

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function sameUrl(left: string, right: string): boolean {
  return left.trim().replace(/\/+$/, "") === right.trim().replace(/\/+$/, "");
}

/**
 * Providers and the model, through the operations `lightagent setup
 * provider` shares: add, rename or move, use, and remove saved providers,
 * and choose the model. Each provider keeps its own key, sent only to its
 * endpoint; this card shows only the key's state.
 */
export function ProvidersCard({ onChanged }: { onChanged?: () => void }) {
  const [list, setList] = useState<ProviderList | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [editing, setEditing] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [editUrl, setEditUrl] = useState("");
  const [keepKey, setKeepKey] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [model, setModel] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [listing, setListing] = useState(false);

  async function load(): Promise<ProviderList | null> {
    try {
      const next = await agentApi.providers();
      setList(next);
      return next;
    } catch (cause) {
      setError(errorText(cause));
      return null;
    }
  }

  useEffect(() => {
    void load().then((next) => {
      if (next) setModel(next.model ?? "");
    });
  }, []);

  /**
   * One change, made on the revision this card loaded. A refusal keeps
   * every entry on screen and reloads the list; nothing is retried.
   */
  async function change(
    action: (revision: string) => Promise<ProviderList>,
    done: string,
    after?: () => void,
  ) {
    if (!list || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const next = await action(list.revision);
      setList(next);
      setMessage(`${done} ${next.effect}`);
      after?.();
      onChanged?.();
    } catch (cause) {
      const text = errorText(cause);
      if (text.startsWith("409:")) {
        await load();
        setError(
          `Nothing was saved: ${text.slice(4).trim()} Your entries are kept and the list now shows the current providers. Review it, then try again.`,
        );
      } else {
        setError(text.replace(/^\d{3}:\s*/, ""));
      }
    } finally {
      setBusy(false);
    }
  }

  function startEdit(provider: ProviderEntry) {
    setEditing(provider.id);
    setEditName(provider.name);
    setEditUrl(provider.base_url);
    setKeepKey(false);
    setConfirmRemove(null);
  }

  function saveEdit(provider: ProviderEntry) {
    const edit: { name?: string; base_url?: string; keep_key?: boolean } = {};
    if (editName.trim() !== provider.name) edit.name = editName;
    if (!sameUrl(editUrl, provider.base_url)) {
      edit.base_url = editUrl;
      edit.keep_key = keepKey;
    }
    if (edit.name === undefined && edit.base_url === undefined) {
      setEditing(null);
      return;
    }
    void change(
      (revision) => agentApi.updateProvider(provider.id, edit, revision),
      `Saved the changes to ${editName.trim() || provider.name}.`,
      () => setEditing(null),
    );
  }

  // Only when asked: a check sends the active provider's own key to it.
  async function listModels(provider: ProviderEntry) {
    setListing(true);
    setError(null);
    try {
      const check = await agentApi.checkSavedProvider(provider.id);
      if (check.ok) {
        setModels(check.models);
        setMessage(
          check.models.length
            ? `${provider.name} offers: ${check.models.join(", ")}.`
            : `${provider.name} answered with no model loaded.`,
        );
      } else {
        setError(`Could not list the models of ${provider.name}: ${check.error ?? "no answer"}`);
      }
    } catch (cause) {
      setError(errorText(cause).replace(/^\d{3}:\s*/, ""));
    } finally {
      setListing(false);
    }
  }

  const active = list?.providers.find((provider) => provider.active) ?? null;
  const chosenModel = model.trim();
  return (
    <Card title="Providers and models">
      <div data-testid="providers-card">
        <p className="card__note">
          Shared with the terminal CLI (lightagent setup provider). Each provider keeps its own API key,
          which is sent only to that provider's endpoint.{list ? ` ${list.effect}` : ""}
        </p>
        {error && <div className="notice notice--warn" role="alert">{error}</div>}
        {message && <div className="notice notice--info" role="status">{message}</div>}
        {list && (
          <ul style={{ listStyle: "none", padding: 0, margin: "8px 0", display: "grid", gap: 12 }}>
            {list.providers.map((provider) => {
              const key = KEY[provider.key_state] ?? { label: provider.key_state, tone: "neutral" as Tone };
              const editable = provider.kind === "saved";
              const moving = editing === provider.id && !sameUrl(editUrl, provider.base_url);
              return (
                <li key={provider.id} data-testid={`provider-${provider.id}`} style={{ minWidth: 0 }}>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                    <strong>{provider.name}</strong>
                    {provider.active && <Pill tone="accent">Active</Pill>}
                    <Pill tone={key.tone}>{key.label}</Pill>
                  </div>
                  <div className="muted" style={{ overflowWrap: "anywhere" }}>{provider.base_url}</div>
                  {editing === provider.id ? (
                    <div style={{ marginTop: 8 }}>
                      <div className="field">
                        <label className="field__label" htmlFor={`provider-name-${provider.id}`}>Name</label>
                        <input id={`provider-name-${provider.id}`} className="input" value={editName}
                          disabled={busy} onChange={(event) => setEditName(event.target.value)} />
                      </div>
                      <div className="field">
                        <label className="field__label" htmlFor={`provider-url-${provider.id}`}>Endpoint URL</label>
                        <input id={`provider-url-${provider.id}`} className="input" type="url" value={editUrl}
                          spellCheck={false} disabled={busy} onChange={(event) => setEditUrl(event.target.value)} />
                      </div>
                      {moving && provider.key_state !== "absent" && (
                        <>
                          <label style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 8 }}>
                            <input type="checkbox" checked={keepKey} disabled={busy}
                              onChange={(event) => setKeepKey(event.target.checked)} />
                            Keep this provider's API key for the new endpoint
                          </label>
                          {!keepKey && (
                            <p className="muted" style={{ margin: "4px 0 0" }}>
                              The API key is removed when the endpoint changes, so it is never sent to the new
                              endpoint.
                            </p>
                          )}
                        </>
                      )}
                      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
                        <button type="button" className="btn" disabled={busy || !editName.trim() || !editUrl.trim()}
                          aria-busy={busy || undefined} onClick={() => saveEdit(provider)}>
                          Save changes
                        </button>
                        <button type="button" className="btn" disabled={busy} onClick={() => setEditing(null)}>
                          Cancel
                        </button>
                      </div>
                    </div>
                  ) : (
                    <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 8 }}>
                      {!provider.active && (
                        <button type="button" className="btn" disabled={busy} aria-label={`Use ${provider.name}`}
                          onClick={() => void change(
                            (revision) => agentApi.selectProvider(provider.id, null, revision),
                            `${provider.name} is the active provider, with automatic model selection.`,
                            () => {
                              setModel("");
                              setModels([]);
                            },
                          )}>
                          Use
                        </button>
                      )}
                      {editable && confirmRemove !== provider.id && (
                        <>
                          <button type="button" className="btn" disabled={busy} aria-label={`Edit ${provider.name}`}
                            onClick={() => startEdit(provider)}>
                            Edit
                          </button>
                          <button type="button" className="btn" disabled={busy} aria-label={`Remove ${provider.name}`}
                            onClick={() => setConfirmRemove(provider.id)}>
                            Remove
                          </button>
                        </>
                      )}
                      {editable && confirmRemove === provider.id && (
                        <>
                          <button type="button" className="btn" disabled={busy}
                            onClick={() => void change(
                              (revision) => agentApi.removeProvider(provider.id, revision),
                              `Removed ${provider.name}${provider.active ? "; the local endpoint is active" : ""}.`,
                              () => setConfirmRemove(null),
                            )}>
                            {`Confirm: remove ${provider.name}`}
                          </button>
                          <button type="button" className="btn" disabled={busy} onClick={() => setConfirmRemove(null)}>
                            {`Keep ${provider.name}`}
                          </button>
                        </>
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}

        <div className="field" style={{ marginTop: 12 }}>
          <label className="field__label" htmlFor="provider-model">
            {active ? `Model for ${active.name}` : "Model"}
          </label>
          <input id="provider-model" className="input" list="provider-models" placeholder="Automatic"
            spellCheck={false} value={model} disabled={!list || busy} onChange={(event) => setModel(event.target.value)} />
          <datalist id="provider-models">
            {models.map((id) => <option key={id} value={id} />)}
          </datalist>
          <p className="muted" style={{ margin: "4px 0 0" }}>
            Leave it empty for automatic selection: the model the endpoint has loaded.
          </p>
        </div>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
          <button type="button" className="btn" disabled={!list || busy} aria-busy={busy || undefined}
            onClick={() => void change(
              (revision) => agentApi.selectProviderModel(chosenModel || null, revision),
              chosenModel ? `Model set to ${chosenModel}.` : "Model set to automatic selection.",
            )}>
            Use this model
          </button>
          <button type="button" className="btn" disabled={!active || listing} aria-busy={listing || undefined}
            onClick={() => active && void listModels(active)}>
            {listing ? "Listing models…" : "List models"}
          </button>
        </div>

        <fieldset style={{ border: 0, padding: 0, margin: "16px 0 0", minWidth: 0 }}>
          <legend className="field__label">Add a provider</legend>
          <div className="field">
            <label className="field__label" htmlFor="provider-new-name">Name</label>
            <input id="provider-new-name" className="input" value={name} disabled={!list || busy}
              onChange={(event) => setName(event.target.value)} />
          </div>
          <div className="field">
            <label className="field__label" htmlFor="provider-new-url">OpenAI-compatible endpoint URL</label>
            <input id="provider-new-url" className="input" type="url" placeholder="https://" spellCheck={false}
              value={url} disabled={!list || busy} onChange={(event) => setUrl(event.target.value)} />
          </div>
          <button type="button" className="btn" style={{ marginTop: 10 }}
            disabled={!list || busy || !name.trim() || !url.trim()} aria-busy={busy || undefined}
            onClick={() => void change(
              (revision) => agentApi.createProvider(name, url, revision),
              `Saved ${name.trim()}. Use it, then save its API key in the API key card if it needs one.`,
              () => {
                setName("");
                setUrl("");
              },
            )}>
            Save provider
          </button>
        </fieldset>
      </div>
    </Card>
  );
}
