import { useEffect, useState } from "react";

import { agentApi, type UpdateCheck } from "../api/agent";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";

type Report = Record<string, unknown>;
type Bundle = Report & { about: string; included: string[]; excluded: string[] };

const DOCTOR_LABELS: [string, string][] = [
  ["version", "Version"],
  ["home", "Home (LIGHTAGENT_HOME)"],
  ["home_exists", "Home exists"],
  ["config_file", "Configuration file"],
  ["provider", "Provider"],
  ["base_url", "Provider endpoint"],
  ["active_profile", "Active profile"],
  ["profiles", "Profiles"],
  ["tools", "Built-in tools"],
];

const ARCHITECTURE_LABELS: [string, string][] = [
  ["jev", "Jev: routing and confidence decisions"],
  ["qdrant", "Qdrant: knowledge retrieval"],
  ["infinity", "Infinity: reranking"],
  ["searxng", "SearXNG: web search"],
  ["open_terminal", "Open Terminal: isolated code execution"],
  ["lightweight", "Lightweight: local inference and API gateway"],
];

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

function shown(value: unknown): string {
  if (value === null || value === undefined) return "not configured";
  if (Array.isArray(value)) return value.length ? value.join(", ") : "(none)";
  if (typeof value === "boolean") return value ? "yes" : "no";
  return typeof value === "string" ? value : JSON.stringify(value);
}

function Rows({ labels, report, testId }: { labels: [string, string][]; report: Report; testId: string }) {
  return (
    <dl data-testid={testId} style={{ display: "grid", gridTemplateColumns: "minmax(120px, max-content) 1fr", gap: "4px 16px", margin: 0 }}>
      {labels.map(([key, label]) => (
        <div key={key} style={{ display: "contents" }}>
          <dt className="muted">{label}</dt>
          <dd style={{ margin: 0, overflowWrap: "anywhere" }}>{shown(report[key])}</dd>
        </div>
      ))}
    </dl>
  );
}

/**
 * Doctor and architecture as `lightagent doctor|architecture` report them,
 * and the diagnostics bundle: prepared on request, shown in full before it
 * is saved, and saved only where the person chooses. Nothing is uploaded.
 */
export function DiagnosticsScreen() {
  const [doctor, setDoctor] = useState<Report | null>(null);
  const [architecture, setArchitecture] = useState<Report | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [bundle, setBundle] = useState<Bundle | null>(null);
  const [bundleError, setBundleError] = useState<string | null>(null);
  const [preparing, setPreparing] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  async function checkForUpdates() {
    setChecking(true);
    setUpdateError(null);
    try {
      setUpdate(await agentApi.updateCheck());
    } catch (cause) {
      setUpdate(null);
      setUpdateError(errorText(cause));
    } finally {
      setChecking(false);
    }
  }

  async function load() {
    try {
      const [nextDoctor, nextArchitecture] = await Promise.all([agentApi.doctor(), agentApi.architecture()]);
      setDoctor(nextDoctor);
      setArchitecture(nextArchitecture);
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function prepare() {
    setPreparing(true);
    setBundleError(null);
    setSaved(null);
    try {
      setBundle(await agentApi.diagnosticsBundle());
    } catch (cause) {
      setBundleError(errorText(cause));
    } finally {
      setPreparing(false);
    }
  }

  function save() {
    if (!bundle) return;
    const name = `lightagent-diagnostics-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.json`;
    const url = URL.createObjectURL(new Blob([`${JSON.stringify(bundle, null, 2)}\n`], { type: "application/json" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
    setSaved(`Saved as ${name}. It was not sent anywhere.`);
  }

  return (
    <>
      <TopBar title="Diagnostics" subtitle="What lightagent doctor and architecture report, and a bundle you can share" />
      <div className="page" data-testid="diagnostics-screen">
        {loadError && (
          <div className="notice notice--warn" role="alert">
            Diagnostics are unavailable: {loadError}{" "}
            <button type="button" className="btn" onClick={() => void load()}>Retry</button>
          </div>
        )}

        <Card title="Doctor">
          {doctor ? <Rows labels={DOCTOR_LABELS} report={doctor} testId="diagnostics-doctor" /> : !loadError && <span className="muted">Loading…</span>}
        </Card>

        <Card title="Architecture">
          {architecture
            ? <Rows labels={ARCHITECTURE_LABELS} report={architecture} testId="diagnostics-architecture" />
            : !loadError && <span className="muted">Loading…</span>}
        </Card>

        <Card title="Updates">
          <p className="card__note">
            Compares this Lightagent with the latest published release. Checking installs nothing; the desktop app is
            updated by installing a new version of it.
          </p>
          <button type="button" className="btn" disabled={checking} aria-busy={checking || undefined}
            onClick={() => void checkForUpdates()}>Check for updates</button>
          {updateError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{updateError}</div>}
          {update && (
            <div data-testid="diagnostics-update" role="status" style={{ marginTop: 10, display: "grid", gap: 4 }}>
              <div>
                {update.update_available
                  ? `Lightagent ${update.latest} is available; this is ${update.current ?? "an unknown version"}.`
                  : `Lightagent ${update.current ?? update.latest} is up to date (the latest release is ${update.latest}).`}
              </div>
              {update.blocked && <div className="muted">Not updated from here: {update.blocked}.</div>}
              <div className="muted" style={{ overflowWrap: "anywhere" }}>Release: {update.release}</div>
            </div>
          )}
        </Card>

        <Card title="Diagnostics bundle">
          <p className="card__note">
            A file to share with someone helping you. It is built from a fixed list of fields and never holds a
            credential, a conversation, memory or knowledge contents, identity records, profile names or file paths.
            Review it before you save it; nothing is uploaded.
          </p>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button type="button" className="btn" disabled={preparing} aria-busy={preparing || undefined}
              onClick={() => void prepare()}>Prepare bundle</button>
            <button type="button" className="btn" disabled={!bundle} onClick={save}>Save bundle…</button>
          </div>
          {bundleError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{bundleError}</div>}
          {saved && <div className="notice notice--info" role="status" style={{ marginTop: 10 }}>{saved}</div>}
          {bundle && (
            <div data-testid="diagnostics-bundle" style={{ marginTop: 12, display: "grid", gap: 10 }}>
              <div>{bundle.about}</div>
              <div>
                <strong>Included</strong>
                <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                  {bundle.included.map((item) => <li key={item}>{item}</li>)}
                </ul>
              </div>
              <div>
                <strong>Left out</strong>
                <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                  {bundle.excluded.map((item) => <li key={item}>{item}</li>)}
                </ul>
              </div>
              <details>
                <summary>The exact contents</summary>
                <pre data-testid="diagnostics-preview" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", fontSize: 12 }}>
                  {JSON.stringify(bundle, null, 2)}
                </pre>
              </details>
            </div>
          )}
        </Card>
      </div>
    </>
  );
}
