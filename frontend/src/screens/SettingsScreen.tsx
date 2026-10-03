import { useEffect, useState, type ReactNode } from "react";
import { Moon, Sun } from "lucide-react";

import {
  agentApi,
  type InfinitySettings,
  type JevSettings,
  type LightagentSettings,
  type OpenTerminalSettings,
  type PlatformEndpointSettings,
  type QdrantSettings,
} from "../api/agent";
import { Switch } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";
import { usePoll } from "../hooks/usePoll";
import { usePreferences } from "../state/preferences";

export function SettingsScreen() {
  const { preferences, update } = usePreferences();
  const settings = usePoll(agentApi.settings, 0);
  const profiles = usePoll(agentApi.profiles, 0);
  const tools = usePoll(() => agentApi.tools().then((body) => body.tools), 0);
  const [current, setCurrent] = useState<LightagentSettings | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (settings.data) setCurrent(settings.data);
  }, [settings.data]);

  async function persist(patch: Partial<LightagentSettings>) {
    if (!current || saving) return;
    setSaving(true);
    setMessage(null);
    try {
      const saved = await agentApi.saveSettings({ ...current, ...patch });
      setCurrent(saved);
      setMessage("Saved. New runs use these settings.");
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  }

  const value = current ?? settings.data;
  const delegationAvailable = tools.data?.some((tool) => tool.name === "agent.delegate");
  const nextTheme = preferences.theme === "dark" ? "light" : "dark";
  return (
    <>
      <TopBar title="Settings" subtitle="Harness policy, tools, memory, and appearance"
        actions={<button type="button" className="btn btn--icon" title={`Switch to ${nextTheme} theme`}
          aria-label={`Switch to ${nextTheme} theme`} onClick={() => update({ theme: nextTheme })}>
          {preferences.theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}
        </button>} />
      <div className="page">
        {settings.error && !value && (
          <div className="notice notice--warn" role="alert">
            Could not load Lightagent settings: {settings.error.message}
            <button type="button" className="btn" style={{ marginLeft: 10 }}
              onClick={settings.refresh}>Retry</button>
          </div>
        )}
        {message && <div className="notice notice--info">{message}</div>}

        <div className="grid" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(300px, 1fr))" }}>
          <Card title="Agent runtime">
            <p className="card__note">
              These controls are shared with the terminal CLI and apply to the next run.
            </p>
            <div className="field" style={{ marginBottom: 12 }}>
              <label className="field__label" htmlFor="agent-approval">Approval policy</label>
              <select id="agent-approval" className="select"
                value={value?.approval_policy ?? "balanced"} disabled={!value || saving}
                onChange={(event) => void persist({
                  approval_policy: event.target.value as LightagentSettings["approval_policy"],
                })}>
                <option value="permissive">Permissive</option>
                <option value="balanced">Balanced</option>
                <option value="strict">Strict</option>
              </select>
            </div>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10 }}>
              <NumberSetting label="Maximum turns" value={value?.max_turns}
                disabled={!value || saving} onSave={(max_turns) => persist({ max_turns })} />
              <NumberSetting label="Maximum tool calls" value={value?.max_tool_calls}
                disabled={!value || saving}
                onSave={(max_tool_calls) => persist({ max_tool_calls })} />
            </div>
            <OptionalNumberSetting label="Run time limit (seconds)"
              value={value?.wall_clock_secs} disabled={!value || saving}
              onSave={(wall_clock_secs) => persist({ wall_clock_secs })} />
          </Card>

          <Card title="Profiles">
            <p className="card__note">
              Saved CLI profiles keep model and routing choices together. The active profile is used by new sessions.
            </p>
            {profiles.data ? (
              <div className="profile-list">
                {profiles.data.profiles.map((profile) => (
                  <div className="profile-row" key={profile.id}>
                    <div>
                      <strong>{profile.name}</strong>
                      <span>{profile.model}</span>
                    </div>
                    {profile.active && <span className="status-chip">Active</span>}
                  </div>
                ))}
                {profiles.data.profiles.length === 0 && <span className="muted">No saved profiles were reported.</span>}
              </div>
            ) : (
              <span className="muted">{profiles.error ? "Profiles are unavailable for this server." : "Loading profiles…"}</span>
            )}
          </Card>

          <Card title="Subagents">
            {tools.data ? (
              delegationAvailable ? (
                <div className="capability-status capability-status--available">
                  <strong>Delegation available</strong>
                  <span>Eligible runs can request the approval-gated agent.delegate tool. It remains opt-in.</span>
                </div>
              ) : (
                <div className="capability-status">
                  <strong>Delegation is not enabled</strong>
                  <span>This harness is running in its standard single-agent mode.</span>
                </div>
              )
            ) : <span className="muted">Checking harness capabilities…</span>}
          </Card>


          <Card title="Capabilities">
            <ToggleRow label="Web tools" hint="Allow configured web search and fetch tools."
              checked={value?.web_enabled ?? false} disabled={!value || saving}
              onChange={(web_enabled) => void persist({ web_enabled })} />
            <ToggleRow label="Filesystem tools" hint="Allow confined file reads and writes."
              checked={value?.filesystem_tools_enabled ?? false} disabled={!value || saving}
              onChange={(filesystem_tools_enabled) => void persist({
                filesystem_tools_enabled,
                ...(!filesystem_tools_enabled ? { terminal_enabled: false } : {}),
              })} />
            <ToggleRow label="Terminal tool" hint="Allow approval-gated commands in the configured workspace."
              checked={value?.terminal_enabled ?? false}
              disabled={!value || saving || !value?.filesystem_tools_enabled}
              onChange={(terminal_enabled) => void persist({ terminal_enabled })} />
            <ToggleRow label="Durable memory" hint="Capture clear durable facts for future sessions."
              checked={value?.memory_enabled ?? false} disabled={!value || saving}
              onChange={(memory_enabled) => void persist({ memory_enabled })} />
            <ToggleRow label="Show reasoning in terminal" hint="Presentation setting for the terminal UI."
              checked={value?.show_reasoning_in_tui ?? true} disabled={!value || saving}
              onChange={(show_reasoning_in_tui) => void persist({ show_reasoning_in_tui })} />
          </Card>

          <Card title="Platform endpoints">
            <p className="card__note">
              Connect external services only when your deployment provides them. URLs must use HTTP or HTTPS; secrets stay in the CLI configuration and are never shown here.
            </p>
            <JevSettingsRow endpoint={value?.jev} disabled={!value || saving}
              onSave={(jev) => persist({ jev })} />
            <QdrantSettingsRow endpoint={value?.qdrant} disabled={!value || saving}
              onSave={(qdrant) => persist({ qdrant })} />
            <InfinitySettingsRow endpoint={value?.infinity} disabled={!value || saving}
              onSave={(infinity) => persist({ infinity })} />
            <OpenTerminalSettingsRow
              endpoint={value?.open_terminal} disabled={!value || saving}
              onSave={(open_terminal) => persist({ open_terminal })} />
          </Card>

          <Card title="Appearance">
            <div className="field" style={{ marginBottom: 16 }}>
              <label className="field__label" htmlFor="theme">Theme</label>
              <select id="theme" className="select" value={preferences.theme}
                onChange={(event) => update({
                  theme: event.target.value as typeof preferences.theme,
                })}>
                <option value="system">Match the system</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
            </div>
            <ToggleRow label="Translucent surfaces"
              hint="Use frosted surfaces over the page background."
              checked={preferences.translucent}
              onChange={(translucent) => update({ translucent })} />
            <ToggleRow label="Compact density" hint="Tighten spacing so more fits on screen."
              checked={preferences.compact} onChange={(compact) => update({ compact })} />
            <ToggleRow label="Collapse the sidebar" hint="Show navigation icons only."
              checked={preferences.railCollapsed}
              onChange={(railCollapsed) => update({ railCollapsed })} />
          </Card>

          <Card title="Architecture">
            <p className="card__note">
              Lightagent is an agent harness. It owns sessions, tools, approvals,
              memory, skills, extensions, MCP, ACP, and the agent loop. Inference
              remains behind the configured OpenAI-compatible provider endpoint.
            </p>
          </Card>
        </div>
      </div>
    </>
  );
}

function NumberSetting({ label, value, disabled, onSave }: {
  label: string;
  value: number | undefined;
  disabled: boolean;
  onSave: (value: number) => Promise<void>;
}) {
  const [draft, setDraft] = useState("");
  useEffect(() => setDraft(value === undefined ? "" : String(value)), [value]);
  return (
    <div className="field">
      <label className="field__label">{label}</label>
      <input className="input tnum" type="number" min={1} value={draft} disabled={disabled}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          const next = Number(draft);
          if (Number.isInteger(next) && next > 0 && next !== value) void onSave(next);
          else setDraft(value === undefined ? "" : String(value));
        }} />
    </div>
  );
}

function OptionalNumberSetting({ label, value, disabled, onSave }: {
  label: string;
  value: number | null | undefined;
  disabled: boolean;
  onSave: (value: number | null) => Promise<void>;
}) {
  const [draft, setDraft] = useState("");
  useEffect(() => setDraft(value == null ? "" : String(value)), [value]);
  return (
    <div className="field" style={{ marginTop: 10 }}>
      <label className="field__label">{label}</label>
      <input className="input tnum" type="number" min={1} placeholder="No limit"
        value={draft} disabled={disabled} onChange={(event) => setDraft(event.target.value)}
        onBlur={() => {
          const next = draft.trim() ? Number(draft) : null;
          if ((next === null || Number.isInteger(next) && next > 0) && next !== value) {
            void onSave(next);
          } else setDraft(value == null ? "" : String(value));
        }} />
    </div>
  );
}

function ToggleRow({ label, hint, checked, onChange, disabled }: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between",
      gap: 16, padding: "12px 0", borderBottom: "1px solid var(--rule)" }}>
      <div style={{ minWidth: 0 }}>
        <div style={{ fontSize: 13.5, fontWeight: 500 }}>{label}</div>
        <div style={{ fontSize: 11.5, color: "var(--text-muted)", marginTop: 2 }}>{hint}</div>
      </div>
      <Switch checked={checked} onChange={onChange} label={label} disabled={disabled} />
    </div>
  );
}

function PlatformEndpointRow<E extends PlatformEndpointSettings>({ label, hint, endpoint, disabled, onSave, children }: {
  label: string;
  hint: string;
  endpoint: E | undefined;
  disabled: boolean;
  onSave: (endpoint: E) => Promise<void>;
  children?: ReactNode;
}) {
  const [baseUrl, setBaseUrl] = useState(endpoint?.base_url ?? "");
  useEffect(() => setBaseUrl(endpoint?.base_url ?? ""), [endpoint?.base_url]);

  if (!endpoint) return null;
  const saveUrl = () => {
    const next = baseUrl.trim() || null;
    if (next !== endpoint.base_url) void onSave({ ...endpoint, base_url: next });
  };
  return (
    <div style={{ padding: "14px 0", borderBottom: "1px solid var(--rule)" }}>
      <ToggleRow label={label} hint={hint} checked={endpoint.enabled} disabled={disabled}
        onChange={(enabled) => void onSave({ ...endpoint, enabled })} />
      <div className="field" style={{ marginTop: 10 }}>
        <label className="field__label" htmlFor={`platform-${label.toLowerCase().replaceAll(" ", "-")}`}>
          Base URL
        </label>
        <input className="input" type="url" inputMode="url" placeholder="https://service.example"
          id={`platform-${label.toLowerCase().replaceAll(" ", "-")}`} value={baseUrl}
          disabled={disabled} onChange={(event) => setBaseUrl(event.target.value)}
          onBlur={saveUrl} />
        <div style={{ fontSize: 11.5, color: "var(--text-muted)", marginTop: 4 }}>
          {endpoint.enabled
            ? endpoint.api_key_configured
              ? "A secret is configured in the CLI; its value is hidden."
              : "No secret configured."
            : "Set a valid URL, then enable this endpoint."}
        </div>
      </div>
      {children}
    </div>
  );
}

function JevSettingsRow({ endpoint, disabled, onSave }: {
  endpoint: JevSettings | undefined;
  disabled: boolean;
  onSave: (settings: JevSettings) => Promise<void>;
}) {
  return (
    <PlatformEndpointRow label="Jev" hint="Routing and confidence decisions before a run is dispatched."
      endpoint={endpoint} disabled={disabled} onSave={onSave}>
      {endpoint && <div style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr) 150px", gap: 10, marginTop: 10 }}>
        <TextPlatformSetting label="Decision model" value={endpoint.model} disabled={disabled || !endpoint.enabled}
          onSave={(model) => onSave({ ...endpoint, model })} />
        <NumberPlatformSetting label="Confidence threshold" value={endpoint.confidence_threshold}
          min={0} max={1} step={0.01} disabled={disabled || !endpoint.enabled}
          onSave={(confidence_threshold) => onSave({ ...endpoint, confidence_threshold })} />
      </div>}
      {endpoint && <>
        <TextPlatformSetting label="Permitted models (comma-separated)" value={endpoint.allowed_models.join(", ")}
          disabled={disabled || !endpoint.enabled} onSave={(value) => onSave({ ...endpoint, allowed_models: splitRoutes(value) })} />
        <TextPlatformSetting label="Permitted inference profiles (comma-separated)" value={endpoint.allowed_profiles.join(", ")}
          disabled={disabled || !endpoint.enabled} onSave={(value) => onSave({ ...endpoint, allowed_profiles: splitRoutes(value) })} />
        <NumberPlatformSetting label="Routing timeout (seconds)" value={endpoint.timeout_secs}
          min={1} max={300} step={1} disabled={disabled || !endpoint.enabled}
          onSave={(timeout_secs) => onSave({ ...endpoint, timeout_secs })} />
      </>}
    </PlatformEndpointRow>
  );
}

function QdrantSettingsRow({ endpoint, disabled, onSave }: {
  endpoint: QdrantSettings | undefined;
  disabled: boolean;
  onSave: (settings: QdrantSettings) => Promise<void>;
}) {
  return (
    <PlatformEndpointRow label="Qdrant" hint="Remote vector retrieval for configured knowledge sources."
      endpoint={endpoint} disabled={disabled} onSave={onSave}>
      {endpoint && <TextPlatformSetting label="Collection" value={endpoint.collection}
        disabled={disabled || !endpoint.enabled} onSave={(collection) => onSave({ ...endpoint, collection })} />}
      {endpoint && <NumberPlatformSetting label="Request timeout (seconds)" value={endpoint.timeout_secs}
        min={1} max={300} step={1} disabled={disabled || !endpoint.enabled}
        onSave={(timeout_secs) => onSave({ ...endpoint, timeout_secs })} />}
    </PlatformEndpointRow>
  );
}

function InfinitySettingsRow({ endpoint, disabled, onSave }: {
  endpoint: InfinitySettings | undefined;
  disabled: boolean;
  onSave: (settings: InfinitySettings) => Promise<void>;
}) {
  return (
    <PlatformEndpointRow label="Infinity" hint="Embeds and reranks retrieved candidates before they reach the agent."
      endpoint={endpoint} disabled={disabled} onSave={onSave}>
      {endpoint && <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(190px, 1fr))", gap: 10, marginTop: 10 }}>
        <TextPlatformSetting label="Embedding model" value={endpoint.embedding_model}
          disabled={disabled || !endpoint.enabled} onSave={(embedding_model) => onSave({ ...endpoint, embedding_model })} />
        <TextPlatformSetting label="Reranking model" value={endpoint.rerank_model}
          disabled={disabled || !endpoint.enabled} onSave={(rerank_model) => onSave({ ...endpoint, rerank_model })} />
        <NumberPlatformSetting label="Request timeout (seconds)" value={endpoint.timeout_secs}
          min={1} max={300} step={1} disabled={disabled || !endpoint.enabled}
          onSave={(timeout_secs) => onSave({ ...endpoint, timeout_secs })} />
      </div>}
    </PlatformEndpointRow>
  );
}

function splitRoutes(value: string): string[] {
  return [...new Set(value.split(",").map((route) => route.trim()).filter(Boolean))];
}

function OpenTerminalSettingsRow({ endpoint, disabled, onSave }: {
  endpoint: OpenTerminalSettings | undefined;
  disabled: boolean;
  onSave: (settings: OpenTerminalSettings) => Promise<void>;
}) {
  return <PlatformEndpointRow label="Open Terminal" hint="Approved commands run in the isolated service with bounded time and output."
    endpoint={endpoint} disabled={disabled} onSave={onSave}>
    {endpoint && <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(190px, 1fr))", gap: 10 }}>
      {([
        ["request_timeout_secs", "Request timeout (seconds)", 300],
        ["execution_timeout_secs", "Execution timeout (seconds)", 3600],
        ["poll_interval_ms", "Polling interval (milliseconds)", 60000],
        ["max_output_bytes", "Maximum output (bytes)", 1048576],
      ] as const).map(([key, label, max]) => <NumberPlatformSetting key={key} label={label}
        value={endpoint[key]} min={1} max={max} step={1} disabled={disabled || !endpoint.enabled}
        onSave={(value) => onSave({ ...endpoint, [key]: value })} />)}
    </div>}
  </PlatformEndpointRow>;
}

function TextPlatformSetting({ label, value, disabled, onSave }: {
  label: string;
  value: string;
  disabled: boolean;
  onSave: (value: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return <div className="field" style={{ marginTop: 10 }}>
    <label className="field__label">{label}</label>
    <input className="input" value={draft} disabled={disabled} onChange={(event) => setDraft(event.target.value)}
      onBlur={() => { const next = draft.trim(); if (next !== value) onSave(next); }} />
  </div>;
}

function NumberPlatformSetting({ label, value, min, max, step, disabled, onSave }: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  disabled: boolean;
  onSave: (value: number) => void;
}) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  return <div className="field" style={{ marginTop: 10 }}>
    <label className="field__label">{label}</label>
    <input className="input" type="number" min={min} max={max} step={step} value={draft} disabled={disabled}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={() => {
        const next = Number(draft);
        if (Number.isFinite(next) && (step !== 1 || Number.isInteger(next)) && next >= min && next <= max && next !== value) onSave(next);
        else setDraft(String(value));
      }} />
  </div>;
}
