import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { BrainCircuit } from "lucide-react";

import {
  agentApi,
  type JevClassificationMode,
  type JevClassificationSettings,
  type JevDiagnostics,
  type JevSettings,
  type JevStatus,
  type ProfileSummary,
} from "../api/agent";
import { Pill, Switch } from "./Bits";

type Tone = "ok" | "warn" | "danger" | "info" | "accent" | "neutral";

const MODE_LABEL: Record<JevClassificationMode, string> = {
  off: "Off",
  shadow: "Shadow",
  active: "Active",
};

const OUTCOME_LABEL: Record<JevDiagnostics["outcome"], string> = {
  classified: "Classified",
  low_confidence: "Low confidence — not applied",
  skipped: "Skipped",
  failed: "Failed — fell back",
};

/** Configured, enabled and verified are separate facts and are shown separately. */
export function jevStates(status: JevStatus): { label: string; tone: Tone }[] {
  const configured: { label: string; tone: Tone } = !status.endpoint_configured
    ? { label: "Not configured", tone: "neutral" }
    : !status.api_key_configured
      ? { label: "API key missing", tone: "warn" }
      : !status.api_key_resolvable
        ? { label: "API key unavailable", tone: "warn" }
        : { label: "Configured", tone: "ok" };
  const enabled: { label: string; tone: Tone } = status.enabled
    ? { label: "Enabled", tone: "ok" }
    : { label: "Disabled", tone: "neutral" };
  const verified: { label: string; tone: Tone } = !status.verification
    ? { label: "Not verified", tone: "neutral" }
    : status.verification.ok
      ? { label: "Verified", tone: "ok" }
      : { label: `Check failed: ${readable(status.verification.error ?? "unknown")}`, tone: "danger" };
  return [configured, enabled, verified];
}

export function JevStatusPills({ status }: { status: JevStatus }) {
  return (
    <div className="jev-statuses" aria-label="Jev status">
      {jevStates(status).map((state) => (
        <Pill key={state.label} tone={state.tone} dot>{state.label}</Pill>
      ))}
    </div>
  );
}

function readable(code: string): string {
  return code.replaceAll("_", " ");
}

function percent(value: number | null): string {
  return value == null ? "Not supplied" : `${Math.round(value * 100)}%`;
}

/** A sanitized classification record. It never contains message text. */
export function JevDiagnosticsView({ diagnostics }: { diagnostics: JevDiagnostics | null }) {
  if (!diagnostics) {
    return (
      <p className="jev-hint">
        No classification has been recorded for this profile since the server started.
      </p>
    );
  }
  const facts: [string, string][] = [
    ["Outcome", OUTCOME_LABEL[diagnostics.outcome] ?? diagnostics.outcome],
    ["Trigger", diagnostics.trigger === "explicit" ? "Explicit tool call" : "Automatic"],
    ["Task category", diagnostics.category ? readable(diagnostics.category) : "None"],
    ["Category confidence", percent(diagnostics.category_confidence)],
    ["Threshold", diagnostics.threshold == null ? "—" : `${Math.round(diagnostics.threshold * 100)}%`],
    ["Planning probability", percent(diagnostics.planning_probability)],
    ["Retrieval probability", percent(diagnostics.retrieval_probability)],
    ["Tools probability", percent(diagnostics.tools_probability)],
    ["Complexity", diagnostics.complexity ?? "—"],
    ["Jev requests", String(diagnostics.jev_calls)],
    ["Request latency", diagnostics.classification_latency_ms == null ? "No request" : `${diagnostics.classification_latency_ms} ms`],
    ["Model version", diagnostics.model_version ?? "Not reported"],
    ["Fallback reason", diagnostics.fallback_reason ? readable(diagnostics.fallback_reason) : "None"],
    ["Influenced the run", diagnostics.influenced ? "Yes" : "No"],
  ];
  return (
    <dl className="jev-facts" aria-label="Last classification">
      {facts.map(([term, detail]) => (
        <div key={term}>
          <dt>{term}</dt>
          <dd className="tnum">{detail}</dd>
        </div>
      ))}
    </dl>
  );
}

/** The Jev entry in the Tools catalog. */
export function JevToolCard() {
  const [status, setStatus] = useState<JevStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setError(null);
    agentApi.jevStatus()
      .then((next) => { if (live) setStatus(next); })
      .catch((cause) => { if (live) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => { live = false; };
  }, [attempt]);

  if (error) {
    return (
      <div role="alert" className="jev-panel">
        <p className="muted">Jev status is unavailable for this server: {error}</p>
        <div className="jev-actions">
          <button type="button" className="btn" onClick={() => setAttempt((value) => value + 1)}>Retry Jev status</button>
        </div>
      </div>
    );
  }
  if (!status) return <span className="muted">Loading Jev status…</span>;
  const explicit = status.explicit_tool.available
    ? `${status.explicit_tool.name} — available to this profile`
    : status.explicit_tool.enabled
      ? `${status.explicit_tool.name} — enabled, but not offered to this profile`
      : `${status.explicit_tool.name} — off`;
  return (
    <div className="jev-panel" data-testid="jev-tool">
      <div className="jev-panel__head">
        <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
          <BrainCircuit size={18} aria-hidden="true" />
          <div>
            <h3 className="jev-panel__title">{status.name}</h3>
            <p className="jev-panel__subtitle">{status.provider} · External decision and classification service</p>
          </div>
        </div>
        <JevStatusPills status={status} />
      </div>
      <p className="card__note" style={{ margin: 0 }}>{status.description}</p>
      <dl className="jev-facts">
        <div><dt>Automatic classification</dt><dd>{MODE_LABEL[status.classification.mode]}</dd></div>
        <div><dt>Explicit tool</dt><dd>{explicit}</dd></div>
        <div>
          <dt>Profile permission</dt>
          <dd>{status.classification.profile_permitted
            ? `Permitted for ${status.profile}`
            : `Not permitted for ${status.profile}`}</dd>
        </div>
        <div>
          <dt>Decision model</dt>
          <dd>{status.verification?.model_version
            ? `${status.model} (answered by ${status.verification.model_version})`
            : status.model}</dd>
        </div>
        <div><dt>Requests per run</dt><dd className="tnum">{status.classification.max_calls_per_run}</dd></div>
        <div><dt>Risk class</dt><dd>{status.permissions.risk}</dd></div>
      </dl>
      <div className="jev-section">
        <h4 className="jev-section__title">Requirements</h4>
        <ul className="jev-requirements">
          {status.permissions.requirements.map((requirement) => <li key={requirement}>{requirement}</li>)}
        </ul>
      </div>
      <div className="jev-section">
        <h4 className="jev-section__title">Last classification</h4>
        <JevDiagnosticsView diagnostics={status.last_classification} />
      </div>
      <div className="jev-actions">
        <Link className="btn" to="/settings">Configure Jev</Link>
      </div>
    </div>
  );
}

const DEFAULT_CLASSIFICATION: JevClassificationSettings = {
  mode: "off",
  allowed_profiles: [],
  max_input_chars: 400,
  default_threshold: 0.9,
  version_thresholds: {},
  explicit_tool: false,
  max_calls_per_run: 4,
};

function formatThresholds(thresholds: Record<string, number>): string {
  return Object.entries(thresholds).map(([version, value]) => `${version}=${value}`).join(", ");
}

function parseThresholds(text: string): Record<string, number> | string {
  const result: Record<string, number> = {};
  for (const entry of text.split(",").map((part) => part.trim()).filter(Boolean)) {
    const [version, value, ...rest] = entry.split("=").map((part) => part.trim());
    const threshold = Number(value);
    if (!version || rest.length || !Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
      return `“${entry}” must be written as version=threshold, with a threshold from 0 to 1.`;
    }
    result[version] = threshold;
  }
  return result;
}

/** Classification controls, connection check and diagnostics in Settings. */
export function JevClassificationSection({ endpoint, profiles, status, disabled, onSave, onChecked }: {
  endpoint: JevSettings;
  profiles: ProfileSummary[];
  status: JevStatus | null;
  disabled: boolean;
  onSave: (settings: JevSettings) => Promise<void>;
  onChecked: () => void;
}) {
  const classification = endpoint.classification ?? DEFAULT_CLASSIFICATION;
  const [thresholds, setThresholds] = useState(formatThresholds(classification.version_thresholds));
  const [thresholdError, setThresholdError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkMessage, setCheckMessage] = useState<string | null>(null);
  useEffect(() => setThresholds(formatThresholds(classification.version_thresholds)),
    [classification.version_thresholds]);
  const save = (patch: Partial<JevClassificationSettings>) =>
    void onSave({ ...endpoint, classification: { ...classification, ...patch } });
  const locked = disabled || !endpoint.classification;

  const check = async () => {
    setChecking(true);
    setCheckMessage(null);
    try {
      const result = await agentApi.verifyJev();
      setCheckMessage(result.ok
        ? `Connection verified in ${result.latency_ms} ms${result.model_version ? ` by ${result.model_version}` : ""}.`
        : `Connection check failed: ${readable(result.error ?? "unknown")}.`);
    } catch (cause) {
      setCheckMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setChecking(false);
      onChecked();
    }
  };

  return (
    <div data-testid="jev-classification">
      <div className="jev-section">
        <h4 className="jev-section__title">Connection</h4>
        {status && <JevStatusPills status={status} />}
        <div className="jev-actions" style={{ marginTop: 8 }}>
          <button type="button" className="btn" disabled={disabled || checking || !endpoint.base_url}
            onClick={() => void check()}>
            {checking ? "Checking…" : "Test connection"}
          </button>
          <span className="jev-hint" style={{ margin: 0 }}>
            Sends one synthetic request; no profile or message data.
          </span>
        </div>
        {checkMessage && <p className={checkMessage.startsWith("Connection verified") ? "jev-hint" : "jev-error"}
          role={checkMessage.startsWith("Connection verified") ? "status" : "alert"}>{checkMessage}</p>}
      </div>

      <div className="jev-section">
        <h4 className="jev-section__title">Task classification</h4>
        {!endpoint.classification && <p className="jev-hint">This server does not support task classification settings.</p>}
        <div className="field" style={{ marginTop: 6 }}>
          <label className="field__label" htmlFor="jev-classification-mode">Automatic classification</label>
          <select id="jev-classification-mode" className="select" value={classification.mode} disabled={locked}
            onChange={(event) => save({ mode: event.target.value as JevClassificationMode })}>
            <option value="off">Off</option>
            <option value="shadow">Shadow (record only)</option>
            <option value="active">Active (advisory guidance)</option>
          </select>
          <p className="jev-hint">
            Off makes no automatic requests. Shadow records results without changing a run. Active lets an
            accepted result add advisory guidance. Separate from model routing; Lightweight&apos;s Auto model
            is never replaced in any mode.
          </p>
        </div>
        <fieldset className="jev-profiles" disabled={locked} style={{ marginTop: 10 }}>
          <legend className="field__label">Profiles permitted to send a sanitized task description</legend>
          {profiles.length === 0 && <span className="jev-hint">No profiles reported.</span>}
          {profiles.map((profile) => {
            const checked = classification.allowed_profiles.includes(profile.id);
            return (
              <label key={profile.id} className="jev-check">
                <input type="checkbox" checked={checked}
                  onChange={() => save({
                    allowed_profiles: checked
                      ? classification.allowed_profiles.filter((id) => id !== profile.id)
                      : [...classification.allowed_profiles, profile.id],
                  })} />
                {profile.name} <span className="muted">({profile.id})</span>
              </label>
            );
          })}
        </fieldset>
        <p className="jev-hint">Profiles not listed never contact Jev for classification.</p>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(170px, 1fr))", gap: 10 }}>
          <NumberField id="jev-max-input" label="Maximum sanitized input (characters)" value={classification.max_input_chars}
            min={32} max={2000} step={1} disabled={locked} onSave={(max_input_chars) => save({ max_input_chars })} />
          <NumberField id="jev-default-threshold" label="Default confidence threshold" value={classification.default_threshold}
            min={0} max={1} step={0.01} disabled={locked} onSave={(default_threshold) => save({ default_threshold })} />
          <NumberField id="jev-max-calls" label="Jev requests per run" value={classification.max_calls_per_run}
            min={2} max={16} step={1} disabled={locked} onSave={(max_calls_per_run) => save({ max_calls_per_run })} />
        </div>
        <div className="field" style={{ marginTop: 10 }}>
          <label className="field__label" htmlFor="jev-version-thresholds">Calibrated thresholds by Jev version</label>
          <input id="jev-version-thresholds" className="input" placeholder="jev-1.13.0=0.8" value={thresholds}
            disabled={locked} aria-describedby="jev-version-thresholds-hint"
            aria-invalid={thresholdError ? true : undefined}
            onChange={(event) => setThresholds(event.target.value)}
            onBlur={() => {
              const parsed = parseThresholds(thresholds);
              if (typeof parsed === "string") { setThresholdError(parsed); return; }
              setThresholdError(null);
              if (formatThresholds(parsed) !== formatThresholds(classification.version_thresholds)) save({ version_thresholds: parsed });
            }} />
          <p className="jev-hint" id="jev-version-thresholds-hint">
            Used when that version answers; otherwise the default threshold applies.
          </p>
          {thresholdError && <p className="jev-error" role="alert">{thresholdError}</p>}
        </div>
        <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 16, paddingTop: 12 }}>
          <div>
            <div style={{ fontSize: 13.5, fontWeight: 500 }}>Explicit tool (jev.classify_task)</div>
            <div className="jev-hint">Lets the agent ask for a classification during a run. Permitted profiles only; the run&apos;s approval policy applies.</div>
          </div>
          <Switch label="Explicit tool (jev.classify_task)" checked={classification.explicit_tool} disabled={locked}
            onChange={(explicit_tool) => save({ explicit_tool })} />
        </div>
      </div>

      <div className="jev-section">
        <h4 className="jev-section__title">Last classification for {status?.profile ?? "this profile"}</h4>
        <JevDiagnosticsView diagnostics={status?.last_classification ?? null} />
      </div>
    </div>
  );
}

function NumberField({ id, label, value, min, max, step, disabled, onSave }: {
  id: string;
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled: boolean;
  onSave: (value: number) => void;
}) {
  const [draft, setDraft] = useState(String(value));
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setDraft(String(value)), [value]);
  return (
    <div className="field" style={{ marginTop: 10 }}>
      <label className="field__label" htmlFor={id}>{label}</label>
      <input id={id} className="input tnum" type="number" min={min} max={max} step={step} value={draft}
        disabled={disabled} aria-invalid={error ? true : undefined}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          const next = Number(draft);
          if (!Number.isFinite(next) || next < min || next > max || (step === 1 && !Number.isInteger(next))) {
            setError(`Enter a value from ${min} to ${max}.`);
            setDraft(String(value));
            return;
          }
          setError(null);
          if (next !== value) onSave(next);
        }} />
      {error && <p className="jev-error" role="alert">{error}</p>}
    </div>
  );
}
