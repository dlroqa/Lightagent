import { useEffect, useState } from "react";

import { agentApi } from "../api/agent";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";

type Imported = Awaited<ReturnType<typeof agentApi.importHermes>>;

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

const errorStyle = { fontSize: 11.5, color: "var(--danger)", marginTop: 4 } as const;
const rowStyle = { display: "flex", gap: 8, flexWrap: "wrap", alignItems: "flex-end" } as const;

function ImportLists({ result }: { result: Imported }) {
  const verb = result.dry_run ? "Would import" : "Imported";
  return (
    <div data-testid="setup-hermes-result" role="status" style={{ marginTop: 10, display: "grid", gap: 4 }}>
      <div>{verb} {result.profiles.length} profile(s) and {result.skills.length} skill(s) from {result.hermes_home}.</div>
      {result.profiles.length > 0 && <div>Profiles: {result.profiles.join(", ")}</div>}
      {result.skills.length > 0 && <div>Skills: {result.skills.join(", ")}</div>}
      {result.skipped.length > 0 && <div>Skipped (already here): {result.skipped.join(", ")}</div>}
      {result.errors.map((error) => <div key={error} style={{ color: "var(--danger)" }}>{error}</div>)}
    </div>
  );
}

/**
 * `lightagent init` and `lightagent import hermes`, through the operations
 * the CLI shares: initialise or reconfigure the home (asks first), and
 * import profiles and skills from a Hermes home after a preview.
 */
export function SetupScreen() {
  const [home, setHome] = useState<string | null>(null);
  const [profile, setProfile] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [initError, setInitError] = useState<string | null>(null);
  const [from, setFrom] = useState("");
  const [force, setForce] = useState(false);
  const [confirmImport, setConfirmImport] = useState(false);
  const [imported, setImported] = useState<Imported | null>(null);
  const [importError, setImportError] = useState<string | null>(null);

  useEffect(() => {
    void agentApi.doctor().then((doctor) => setHome(typeof doctor.home === "string" ? doctor.home : null)).catch(() => {});
    void agentApi.profiles().then((catalog) => setProfile(catalog.active_profile)).catch(() => {});
    void agentApi.config().then((view) => {
      const inference = (view.config.inference ?? {}) as { base_url?: string; model?: string | null };
      setBaseUrl(inference.base_url ?? "");
      setModel(inference.model ?? "");
    }).catch(() => {});
  }, []);

  async function init(forced: boolean) {
    setBusy(true);
    setInitError(null);
    setMessage(null);
    setConfirm(false);
    try {
      const report = await agentApi.initHome({
        force: forced,
        profile: profile.trim() || undefined,
        base_url: baseUrl.trim() || undefined,
        model: model.trim() || undefined,
      });
      setMessage(`${forced ? "Reconfigured" : "Initialised"} ${report.home}: active profile ${report.active_profile}, provider ${report.base_url}.`);
    } catch (cause) {
      setInitError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  async function runImport(dryRun: boolean) {
    setBusy(true);
    setImportError(null);
    setConfirmImport(false);
    try {
      setImported(await agentApi.importHermes({ from: from.trim() || undefined, dry_run: dryRun, force }));
    } catch (cause) {
      setImportError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <TopBar title="Setup" subtitle="Initialise this Lightagent home, or import from Hermes" />
      <div className="page" data-testid="setup-screen">
        {message && <div className="notice notice--info" role="status" data-testid="setup-message">{message}</div>}

        <Card title="Initialise or reconfigure">
          <p className="card__note">
            Shared with the terminal CLI (lightagent init). {home ? `This home: ${home}.` : ""} Reconfiguring sets the
            endpoint and the model and makes the profile active (it is created if it does not exist); the CLI uses it
            too.
          </p>
          <div style={rowStyle}>
            <div className="field">
              <label className="field__label" htmlFor="setup-profile">Profile</label>
              <input id="setup-profile" className="input" value={profile} spellCheck={false} disabled={busy}
                onChange={(event) => setProfile(event.target.value)} />
            </div>
            <div className="field" style={{ flex: "1 1 260px" }}>
              <label className="field__label" htmlFor="setup-base-url">Provider endpoint</label>
              <input id="setup-base-url" className="input" value={baseUrl} spellCheck={false} disabled={busy}
                onChange={(event) => setBaseUrl(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="setup-model">Model (optional)</label>
              <input id="setup-model" className="input" value={model} spellCheck={false} disabled={busy}
                onChange={(event) => setModel(event.target.value)} />
            </div>
          </div>
          {initError && <div role="alert" data-testid="setup-init-error" style={errorStyle}>{initError}</div>}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
            <button type="button" className="btn" disabled={busy} onClick={() => void init(false)}>Initialise</button>
            {confirm ? (
              <>
                <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void init(true)}>
                  {`Confirm: reconfigure ${home ?? "this home"}`}
                </button>
                <button type="button" className="btn" disabled={busy} onClick={() => setConfirm(false)}>Keep it</button>
              </>
            ) : (
              <button type="button" className="btn" disabled={busy} onClick={() => setConfirm(true)}>Reconfigure…</button>
            )}
          </div>
        </Card>

        <Card title="Import from Hermes">
          <p className="card__note">
            Shared with lightagent import hermes. Profiles (persona, description, model) and skills are copied from a
            Hermes home; preview first. Without a folder, HERMES_HOME (or ~/.hermes) is used.
          </p>
          <div className="field">
            <label className="field__label" htmlFor="setup-hermes-from">Hermes home (absolute path, optional)</label>
            <input id="setup-hermes-from" className="input" value={from} spellCheck={false} disabled={busy}
              onChange={(event) => { setFrom(event.target.value); setImported(null); }} />
          </div>
          <label style={{ display: "inline-flex", gap: 6, alignItems: "center", marginTop: 6 }}>
            <input id="setup-hermes-force" type="checkbox" checked={force} disabled={busy}
              onChange={(event) => setForce(event.target.checked)} />
            Replace profiles and skills that already exist
          </label>
          {importError && <div role="alert" data-testid="setup-hermes-error" style={errorStyle}>{importError}</div>}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
            <button type="button" className="btn" disabled={busy} onClick={() => void runImport(true)}>Preview</button>
            {force && confirmImport ? (
              <>
                <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void runImport(false)}>
                  Confirm: import and replace
                </button>
                <button type="button" className="btn" disabled={busy} onClick={() => setConfirmImport(false)}>Keep it</button>
              </>
            ) : (
              <button type="button" className="btn" disabled={busy}
                onClick={() => (force ? setConfirmImport(true) : void runImport(false))}>{force ? "Import…" : "Import"}</button>
            )}
          </div>
          {imported && <ImportLists result={imported} />}
        </Card>
      </div>
    </>
  );
}
