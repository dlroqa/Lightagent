/** Same-origin client for the Lightagent HTTP API (`/api/lightagent/v1`). */

const BASE = "/api/lightagent/v1";

export interface ToolInfo {
  name: string;
  risk: string;
  description: string;
}
export interface SkillInfo {
  name: string;
  description: string;
}


export interface PendingApproval {
  approval_id: string;
  tool: string;
  risk: string;
}

export interface RunView {
  id: string;
  status: string;
  events: number;
  pending_approval: PendingApproval | null;
}

export interface SessionSummary {
  id: string;
  profile: string;
  title: string;
  pinned: boolean;
  archived: boolean;
  project?: string;
  updated_at: SystemTime;
  message_count: number;
  run_count: number;
}

export interface SessionPatch {
  title?: string;
  pinned?: boolean;
  archived?: boolean;
  project?: string | null;
}

export interface SystemTime {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

export interface SessionMessage {
  role: string;
  content: string;
  created_at?: SystemTime;
}

export interface UploadedAttachment {
  name: string;
  path: string;
}

export interface ToolHistoryEntry {
  id: string;
  tool: string;
  arguments_preview: string;
  result_excerpt: string;
  source?: string;
  truncated: boolean;
  outcome: string;
  duration_ms?: number;
}

export interface SessionRun {
  run_id: string;
  started_at: SystemTime;
  ended_at?: SystemTime;
  stop_reason?: string;
  tools: ToolHistoryEntry[];
}

/** Runtime model catalog and features reported by the connected provider. */
export interface ProviderCapabilities {
  provider: string;
  base_url: string;
  configured_model: string | null;
  models: string[];
  model_aliases: Record<string, string>;
  model_catalog: Record<string, string>;
  runtime_models: RuntimeModel[];
  streaming: boolean;
  tool_calls: boolean;
  reasoning_content: boolean;
}


/** A model registered with the optional backend runtime control plane. */
export interface RuntimeModel {
  id: string;
  name: string | null;
  state: string;
  supported: boolean | null;
}

/** A non-sensitive saved CLI profile advertised by the active server. */
export interface ProfileSummary {
  id: string;
  name: string;
  model: string;
  active: boolean;
}

export interface ProfileCatalog {
  active_profile: string;
  profiles: ProfileSummary[];
}
export interface AgentSession {
  id: string;
  profile: string;
  cwd?: string;
  title: string;
  created_at: SystemTime;
  messages: SessionMessage[];
  approvals_unrestricted: boolean;
  runs: SessionRun[];
}

export interface LightagentSettings {
  max_turns: number;
  max_tool_calls: number;
  wall_clock_secs: number | null;
  approval_policy: "permissive" | "balanced" | "strict";
  web_enabled: boolean;
  filesystem_tools_enabled: boolean;
  terminal_enabled: boolean;
  memory_enabled: boolean;
  show_reasoning_in_tui: boolean;
  jev: JevSettings;
  qdrant: QdrantSettings;
  infinity: InfinitySettings;
  open_terminal: OpenTerminalSettings;
}

/** Safe endpoint metadata for an optional platform service. Secrets never cross the API boundary. */
export interface PlatformEndpointSettings {
  enabled: boolean;
  base_url: string | null;
  api_key_configured: boolean;
}

export interface JevSettings extends PlatformEndpointSettings {
  model: string;
  confidence_threshold: number;
  allowed_models: string[];
  allowed_profiles: string[];
  timeout_secs: number;
}

export interface QdrantSettings extends PlatformEndpointSettings {
  collection: string;
  timeout_secs: number;
}

export interface InfinitySettings extends PlatformEndpointSettings {
  embedding_model: string;
  rerank_model: string;
  timeout_secs: number;
}

export interface OpenTerminalSettings extends PlatformEndpointSettings {
  request_timeout_secs: number;
  execution_timeout_secs: number;
  poll_interval_ms: number;
  max_output_bytes: number;
}


export interface ApprovalRow {
  run: string;
  pending: PendingApproval | null;
}

async function jsonRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.toLowerCase().includes("application/json")) {
    throw new Error(
      `Agent API returned ${response.status} ${contentType || "without a content type"} for ${BASE}${path}. ` +
      "The Lightagent server returned an unexpected response. Restart it with the current build, then try again.",
    );
  }
  const body = await response.json();
  if (!response.ok) {
    const message =
      typeof body?.error === "string"
        ? body.error
        : typeof body?.error?.message === "string"
          ? body.error.message
          : response.statusText;
    throw new Error(`${response.status}: ${message}`);
  }
  return body as T;
}

export const agentApi = {
  tools: () => jsonRequest<{ tools: ToolInfo[] }>("/tools"),
  skills: () => jsonRequest<{ skills: SkillInfo[] }>("/skills"),
  settings: () => jsonRequest<LightagentSettings>("/settings"),
  saveSettings: (settings: LightagentSettings) =>
    jsonRequest<LightagentSettings>("/settings", {
      method: "PUT",
      body: JSON.stringify(settings),
    }),
  provider: () => jsonRequest<ProviderCapabilities>("/provider"),
  profiles: () => jsonRequest<ProfileCatalog>("/profiles"),
  createRun: (message: string, profile?: string, sessionId?: string, model?: string) =>
    jsonRequest<{ id: string; status: string; session_id: string | null }>("/runs", {
      method: "POST",
      body: JSON.stringify({ message, profile, session_id: sessionId, model }),
    }),
  createSession: () => jsonRequest<{ id: string }>("/sessions", { method: "POST", body: "{}" }),
  uploadAttachment: async (sessionId: string, file: File): Promise<UploadedAttachment> => {
    const filename = file.name.replace(/[^A-Za-z0-9._ -]/g, "_").slice(0, 180) || "attachment";
    const response = await fetch(`${BASE}/sessions/${encodeURIComponent(sessionId)}/attachments`, {
      method: "POST",
      headers: { "X-Lightagent-Filename": filename },
      body: file,
    });
    const body = await response.json();
    if (!response.ok) throw new Error(`${response.status}: ${body?.error ?? response.statusText}`);
    return body as UploadedAttachment;
  },
  session: (id: string) =>
    jsonRequest<AgentSession>(`/sessions/${encodeURIComponent(id)}`),
  deleteSession: (id: string) =>
    jsonRequest<{ deleted: boolean }>(`/sessions/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),
  updateSession: (id: string, patch: SessionPatch) =>
    jsonRequest<SessionSummary>(`/sessions/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(patch),
    }),
  run: (id: string) => jsonRequest<RunView>(`/runs/${id}`),
  cancelRun: (id: string) =>
    jsonRequest<{ id: string; cancelled: boolean }>(`/runs/${id}/cancel`, {
      method: "POST",
      body: "{}",
    }),
  sessions: () => jsonRequest<{ sessions: SessionSummary[] }>("/sessions"),
  approvals: () => jsonRequest<{ approvals: ApprovalRow[] }>("/approvals"),
  respondApproval: (run: string, approve: boolean) =>
    jsonRequest<{ run: string; delivered: boolean }>(`/approvals/${run}`, {
      method: "POST",
      body: JSON.stringify({ approve }),
    }),
  eventsUrl: (id: string) => `${BASE}/runs/${id}/events`,
};
