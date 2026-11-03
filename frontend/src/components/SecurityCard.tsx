import { useEffect, useState } from "react";

import { agentApi } from "../api/agent";
import { Card } from "./Card";

type Security = { approval_policy?: string; remote_tools_default?: boolean; privacy_mode?: boolean };

function yesNo(value: boolean | undefined): string {
  return value === undefined ? "—" : value ? "yes" : "no";
}

/**
 * The configuration's [security] section, read-only, as `lightagent config
 * show` reports it. The approval policy is set under Agent runtime (as
 * `lightagent setup` → approvals sets it); the other values have no setting.
 */
export function SecurityCard() {
  const [security, setSecurity] = useState<Security | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    agentApi
      .config()
      .then((view) => setSecurity((view.config.security ?? {}) as Security))
      .catch((cause) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, []);

  return (
    <Card title="Security">
      <p className="card__note">
        The configuration&apos;s [security] section, read-only, as lightagent config show reports it. The approval
        policy is set under Agent runtime; the other values have no setting.
      </p>
      {error && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)" }}>{error}</div>}
      {!security && !error && <span className="muted">Loading…</span>}
      {security && (
        <dl data-testid="settings-security"
          style={{ display: "grid", gridTemplateColumns: "minmax(120px, max-content) 1fr", gap: "4px 16px", margin: 0 }}>
          <dt className="muted">Approval policy</dt>
          <dd data-field="approval_policy" style={{ margin: 0 }}>{security.approval_policy ?? "—"}</dd>
          <dt className="muted">Network tools allowed by default</dt>
          <dd data-field="remote_tools_default" style={{ margin: 0 }}>{yesNo(security.remote_tools_default)}</dd>
          <dt className="muted">Privacy mode (user text kept out of logs)</dt>
          <dd data-field="privacy_mode" style={{ margin: 0 }}>{yesNo(security.privacy_mode)}</dd>
        </dl>
      )}
    </Card>
  );
}
