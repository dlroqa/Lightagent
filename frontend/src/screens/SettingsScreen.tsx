import { useEffect, useState } from "react";

import { agentApi, type LightagentSettings } from "../api/agent";
import { Switch } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";
import { usePoll } from "../hooks/usePoll";
import { usePreferences } from "../state/preferences";

export function SettingsScreen() {
  const { preferences, update } = usePreferences();
  const settings = usePoll(agentApi.settings, 0);
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
  return (
    <>
      <TopBar title="Settings" subtitle="Harness policy, tools, memory, and appearance" />
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
