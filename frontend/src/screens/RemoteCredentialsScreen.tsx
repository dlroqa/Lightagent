import { useEffect, useState } from "react";

import { agentApi, type ProfileSummary, type RemoteCredentialGrant, type RemoteCredentialStatus } from "../api/agent";
import { Empty } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";
import { hostSecretInput, sealFromChosenFile } from "../desktop/secretInput";

const errorStyle = { fontSize: 11.5, color: "var(--danger)", marginTop: 4 } as const;
const rowStyle = { display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" } as const;
const listStyle = { margin: 0, paddingLeft: 0, listStyle: "none", display: "grid", gap: 10 } as const;

const KEY_SOURCE: Record<string, string> = {
  file: "from a key file (LIGHTAGENT_CREDENTIAL_KEY_FILE)",
  env: "from the environment (LIGHTAGENT_CREDENTIAL_KEY)",
};

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

function when(unix: number | null | undefined): string {
  return unix ? new Date(unix * 1000).toLocaleString() : "never";
}

function whole(raw: string): number | undefined {
  const value = Number(raw);
  return raw.trim() && Number.isInteger(value) && value > 0 ? value : undefined;
}

function items(raw: string): string[] {
  return raw.split(/[\s,]+/).map((item) => item.trim()).filter(Boolean);
}

/**
 * The profile-scoped remote credential vault, through the operations
 * `lightagent remote-credential` shares, as the local operator. A secret is
 * read by the runtime from a file the operator names; it never reaches
 * this screen.
 */
export function RemoteCredentialsScreen() {
  const [status, setStatus] = useState<RemoteCredentialStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [grants, setGrants] = useState<RemoteCredentialGrant[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [audit, setAudit] = useState<Record<string, unknown>[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<{ area: string; text: string } | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);

  const [keyPath, setKeyPath] = useState("");
  const [previousKey, setPreviousKey] = useState("");
  const [profile, setProfile] = useState("");
  const [service, setService] = useState("");
  const [destinations, setDestinations] = useState("");
  const [operations, setOperations] = useState("");
  const [expires, setExpires] = useState("");
  const [budget, setBudget] = useState("");
  const [secretFile, setSecretFile] = useState("");
  const [only, setOnly] = useState("");
  const [rotating, setRotating] = useState<string | null>(null);
  const [rotateFile, setRotateFile] = useState("");
  const [auditLimit, setAuditLimit] = useState("50");

  async function load(filter = only) {
    try {
      const [nextStatus, catalog] = await Promise.all([agentApi.remoteCredentialStatus(), agentApi.profiles()]);
      setStatus(nextStatus);
      setProfiles(catalog.profiles);
      setGrants(nextStatus.vault === "present" ? (await agentApi.remoteCredentials(filter || undefined)).credentials : []);
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function act(area: string, work: () => Promise<string>) {
    setBusy(true);
    setError(null);
    setMessage(null);
    setConfirm(null);
    try {
      setMessage(await work());
    } catch (cause) {
      setError({ area, text: errorText(cause) });
    } finally {
      setBusy(false);
      void load();
    }
  }

  const refusal = (area: string) =>
    error?.area === area ? <div role="alert" data-testid="credentials-error" style={errorStyle}>{error.text}</div> : null;

  const confirmable = (id: string, area: string, label: string, confirmLabel: string, run: () => Promise<string>, disabled = false) =>
    confirm === id ? (
      <>
        <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void act(area, run)}>{confirmLabel}</button>
        <button type="button" className="btn" disabled={busy} onClick={() => setConfirm(null)}>Keep it</button>
      </>
    ) : (
      <button type="button" className="btn" disabled={busy || disabled} onClick={() => setConfirm(id)}>{label}</button>
    );

  return (
    <>
      <TopBar title="Remote credentials" subtitle="Secrets a profile may send to the services you allow (operator only)" />
      <div className="page" data-testid="credentials-screen">
        {message && <div className="notice notice--info" role="status" data-testid="credentials-message">{message}</div>}
        {loadError && (
          <div className="notice notice--warn" role="alert">
            The credential vault is unavailable: {loadError}{" "}
            <button type="button" className="btn" onClick={() => void load()}>Retry</button>
          </div>
        )}

        <Card title="Vault">
          <p className="card__note">
            Shared with the terminal CLI (lightagent remote-credential). Secrets are sealed at rest with the key the
            runtime was started with; this screen never sees a secret.
          </p>
          {!status && !loadError && <span className="muted">Loading…</span>}
          {status && (
            <ul data-testid="credentials-status" style={{ margin: 0, paddingLeft: 18 }}>
              <li>
                Key:{" "}
                {status.key_configured
                  ? `${KEY_SOURCE[status.key_source ?? ""] ?? "configured"}, fingerprint ${status.key_fingerprint ?? "unknown"}`
                  : "none configured. Create a key file below, set LIGHTAGENT_CREDENTIAL_KEY_FILE to it, then restart."}
              </li>
              {status.vault === "absent" ? (
                <li>No vault yet. It is created by the first credential added.</li>
              ) : (
                <>
                  <li>
                    Grants: {status.grants?.active ?? 0} active, {status.grants?.revoked ?? 0} revoked,{" "}
                    {status.grants?.expired ?? 0} expired
                  </li>
                  {(status.unopenable_with_configured_key ?? 0) > 0 && (
                    <li data-testid="credentials-unopenable">
                      {status.unopenable_with_configured_key} active grant
                      {status.unopenable_with_configured_key === 1 ? " does" : "s do"} not open with the configured key:
                      re-seal from the previous key below.
                    </li>
                  )}
                </>
              )}
            </ul>
          )}
        </Card>

        <Card title="Master key">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 260px" }}>
              <label className="field__label" htmlFor="rc-keygen-path">New key file (absolute path, outside the Lightagent home)</label>
              <input id="rc-keygen-path" className="input" value={keyPath} spellCheck={false} disabled={busy}
                onChange={(event) => setKeyPath(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={busy || !keyPath.trim()}
              onClick={() => void act("key", async () => {
                const made = await agentApi.createCredentialKey(keyPath.trim());
                setKeyPath("");
                return `Created a key file (fingerprint ${made.key_fingerprint}). Set LIGHTAGENT_CREDENTIAL_KEY_FILE to it for the desktop app and the CLI, then restart.`;
              })}>Create key file</button>
          </div>
          {refusal("key")}
          <div style={{ ...rowStyle, alignItems: "flex-end", marginTop: 12 }}>
            <div className="field" style={{ flex: "1 1 260px" }}>
              <label className="field__label" htmlFor="rc-rekey-previous">Previous key file (absolute path)</label>
              <input id="rc-rekey-previous" className="input" value={previousKey} spellCheck={false} disabled={busy}
                onChange={(event) => setPreviousKey(event.target.value)} />
            </div>
            {confirmable("rekey", "rekey", "Re-seal…", "Confirm: re-seal every active grant", async () => {
              const resealed = await agentApi.rekeyRemoteCredentials(previousKey.trim());
              setPreviousKey("");
              return `Re-sealed ${resealed.resealed} grant${resealed.resealed === 1 ? "" : "s"} with the configured key (fingerprint ${resealed.key_fingerprint}).`;
            }, !previousKey.trim())}
          </div>
          <p className="card__note" style={{ marginTop: 6 }}>Nothing changes unless every active grant opens with the previous key.</p>
          {refusal("rekey")}
        </Card>

        <Card title="Add a credential">
          <div style={rowStyle}>
            <div className="field">
              <label className="field__label" htmlFor="rc-add-profile">Profile</label>
              <select id="rc-add-profile" className="input" value={profile} disabled={busy}
                onChange={(event) => setProfile(event.target.value)}>
                <option value="">Choose a profile</option>
                {profiles.map((item) => <option key={item.id} value={item.id}>{item.name || item.id}</option>)}
              </select>
            </div>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label className="field__label" htmlFor="rc-add-service">Service (web-search or mcp:&lt;server&gt;)</label>
              <input id="rc-add-service" className="input" value={service} spellCheck={false} disabled={busy}
                onChange={(event) => setService(event.target.value)} />
            </div>
          </div>
          <div className="field">
            <label className="field__label" htmlFor="rc-add-destinations">Destinations it may be sent to (https://host[:port], separated by spaces or commas)</label>
            <input id="rc-add-destinations" className="input" value={destinations} spellCheck={false} disabled={busy}
              onChange={(event) => setDestinations(event.target.value)} />
          </div>
          <div style={rowStyle}>
            <div className="field" style={{ flex: "1 1 200px" }}>
              <label className="field__label" htmlFor="rc-add-operations">Only these operations (optional)</label>
              <input id="rc-add-operations" className="input" value={operations} spellCheck={false} disabled={busy}
                onChange={(event) => setOperations(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="rc-add-expires">Expires in seconds (optional)</label>
              <input id="rc-add-expires" className="input" type="number" min={1} value={expires} disabled={busy}
                style={{ maxWidth: 160 }} onChange={(event) => setExpires(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="rc-add-budget">Requests per run (optional)</label>
              <input id="rc-add-budget" className="input" type="number" min={1} value={budget} disabled={busy}
                style={{ maxWidth: 160 }} onChange={(event) => setBudget(event.target.value)} />
            </div>
          </div>
          <div className="field">
            <label className="field__label" htmlFor="rc-add-secret-file">Read the secret from this file (absolute path; delete it afterwards)</label>
            <input id="rc-add-secret-file" className="input" value={secretFile} spellCheck={false} disabled={busy}
              placeholder="/path/to/secret" onChange={(event) => setSecretFile(event.target.value)} />
          </div>
          {refusal("add")}
          <button type="button" className="btn" style={{ marginTop: 10 }} aria-busy={busy || undefined}
            disabled={busy || !profile || !service.trim() || !destinations.trim() || !secretFile.trim()}
            onClick={() => void act("add", async () => {
              const added = await agentApi.addRemoteCredential({
                profile,
                service: service.trim(),
                destinations: items(destinations),
                operations: items(operations),
                expires_in: whole(expires),
                max_requests_per_run: whole(budget),
                secret_file: secretFile.trim(),
              });
              setSecretFile("");
              return `Added ${added.added}: ${added.service} for ${added.profile}. The secret was read from the file and sealed; delete the file now.`;
            })}>Add credential</button>
          {hostSecretInput && (
            <>
              <button type="button" className="btn" style={{ marginTop: 10, marginLeft: 8 }} aria-busy={busy || undefined}
                disabled={busy || !profile || !service.trim() || !destinations.trim()}
                aria-describedby="rc-add-host-note"
                onClick={() => void act("add", () => sealFromChosenFile(null, {
                  profile,
                  service: service.trim(),
                  destinations: items(destinations),
                  operations: items(operations),
                  expires_in: whole(expires),
                  max_requests_per_run: whole(budget),
                }, (added) => `Added ${String(added.added)}: ${String(added.service)} for ${String(added.profile)}.`))}>
                Add with a secret file chosen here…
              </button>
              <p id="rc-add-host-note" className="card__note" style={{ marginTop: 6 }}>
                Opens this computer&apos;s file picker. The app reads the file you choose itself, so the secret never enters
                this window, and leaves the file as it was.
              </p>
            </>
          )}
        </Card>

        <Card title="Credentials">
          <div className="field">
            <label className="field__label" htmlFor="rc-profile">Show</label>
            <select id="rc-profile" className="input" value={only} style={{ maxWidth: 260 }}
              onChange={(event) => { setOnly(event.target.value); void load(event.target.value); }}>
              <option value="">Every profile</option>
              {profiles.map((item) => <option key={item.id} value={item.id}>{item.name || item.id}</option>)}
            </select>
          </div>
          {refusal("grants")}
          {grants.length === 0 ? (
            <Empty title="No credentials" hint="Add one above." />
          ) : (
            <ul data-testid="credentials-list" style={{ ...listStyle, marginTop: 12 }}>
              {grants.map((item) => (
                <li key={item.id} data-testid={`rc-${item.id}`}>
                  <div style={rowStyle}>
                    <code>{item.id}</code>
                    <span className="muted">
                      {item.service} · {item.profile} · {item.state} · version {item.version} · to {item.destinations.join(", ")}
                      {item.operations && ` · only ${item.operations.join(", ")}`}
                      {item.max_requests_per_run !== null && ` · ${item.max_requests_per_run} requests per run`}
                      {` · expires ${when(item.expires_unix)}`}
                    </span>
                    {item.state === "active" && (
                      <>
                        <button type="button" className="btn" disabled={busy}
                          onClick={() => { setRotating(rotating === item.id ? null : item.id); setRotateFile(""); }}>Rotate…</button>
                        {confirmable(`revoke:${item.id}`, "grants", "Revoke…", `Confirm: revoke ${item.id}`, async () => {
                          await agentApi.revokeRemoteCredential(item.id);
                          return `Revoked ${item.id}; runs using it stop at their next request.`;
                        })}
                      </>
                    )}
                  </div>
                  {rotating === item.id && (
                    <div style={{ ...rowStyle, alignItems: "flex-end", marginTop: 8 }}>
                      <div className="field" style={{ flex: "1 1 260px" }}>
                        <label className="field__label" htmlFor="rc-rotate-file">Read the new secret from this file (absolute path)</label>
                        <input id="rc-rotate-file" className="input" value={rotateFile} spellCheck={false} disabled={busy}
                          onChange={(event) => setRotateFile(event.target.value)} />
                      </div>
                      <button type="button" className="btn" disabled={busy || !rotateFile.trim()}
                        onClick={() => void act("grants", async () => {
                          const rotated = await agentApi.rotateRemoteCredential(item.id, rotateFile.trim());
                          setRotating(null);
                          return `Rotated ${rotated.rotated} (version ${rotated.version}); delete the file now.`;
                        })}>{`Rotate ${item.id}`}</button>
                      {hostSecretInput && (
                        <button type="button" className="btn" disabled={busy} aria-busy={busy || undefined}
                          onClick={() => void act("grants", () => sealFromChosenFile(item.id, {}, (rotated) => {
                            setRotating(null);
                            return `Rotated ${String(rotated.rotated)} (version ${String(rotated.version)}).`;
                          }))}>{`Choose a file for ${item.id}…`}</button>
                      )}
                      <button type="button" className="btn" disabled={busy} onClick={() => setRotating(null)}>Cancel</button>
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card title="Audit">
          <div style={{ ...rowStyle, alignItems: "flex-end" }}>
            <div className="field">
              <label className="field__label" htmlFor="rc-audit-limit">Most recent records</label>
              <input id="rc-audit-limit" className="input" type="number" min={1} value={auditLimit} style={{ maxWidth: 120 }}
                onChange={(event) => setAuditLimit(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={busy}
              onClick={() => void agentApi.remoteCredentialAudit(whole(auditLimit) ?? 50)
                .then((result) => { setAudit(result.records); setError(null); })
                .catch((cause) => setError({ area: "audit", text: errorText(cause) }))}>Show audit</button>
          </div>
          {refusal("audit")}
          {audit && (audit.length === 0 ? <span className="muted">No audit records.</span> : (
            <ol data-testid="credentials-audit" style={{ margin: "10px 0 0", paddingLeft: 18, display: "grid", gap: 4 }}>
              {audit.map((record, index) => (
                <li key={index} style={{ fontSize: 12.5 }}>
                  {Object.entries(record)
                    .filter(([, value]) => value !== null && value !== undefined)
                    .map(([field, value]) => `${field}: ${typeof value === "string" ? value : JSON.stringify(value)}`)
                    .join(" · ")}
                </li>
              ))}
            </ol>
          ))}
        </Card>
      </div>
    </>
  );
}
