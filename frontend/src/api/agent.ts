/** Same-origin client for the Lightagent HTTP API (`/api/lightagent/v1`). */

import { apiFetch } from "./transport";

const BASE = "/api/lightagent/v1";

export interface ToolInfo {
  name: string;
  risk: string;
  description: string;
  /** What a run's call does under the profile's approval policy. */
  permission?: "auto" | "ask" | "block";
}
export interface SkillInfo {
  name: string;
  description: string;
  version?: string | null;
  /** Where a run loads it from; a later source overrides an earlier one. */
  source?: { scope: "global" | "extension" | "profile"; extension?: string };
}

/** Where an extension is installed: every profile's root, or the active profile's. */
export type ExtensionScope = "global" | "profile";

export interface ExtensionSummary {
  name: string;
  version: string;
  description: string;
  active: boolean;
  scope: ExtensionScope;
  skills_dir: string;
  mcp_servers: string[];
}

export interface ExtensionListing {
  /** The master switch, `extensions.enabled`. */
  enabled: boolean;
  extensions: ExtensionSummary[];
}

/** Where an install came from, as it was recorded. */
export type ExtensionSource =
  | { kind: "local"; path: string }
  | { kind: "git"; url: string; subdir: string; revision: string };

/** What an MCP server would run or reach. Environment and header values are never sent. */
export type ExtensionMcpServer =
  | { transport: "stdio"; name: string; command: string; args: string[]; env: string[] }
  | { transport: "http"; name: string; url: string; headers: string[]; auth: boolean };

export interface ExtensionDetail {
  name: string;
  version: string;
  description: string;
  active: boolean;
  scope: ExtensionScope;
  dir: string;
  instructions: string;
  mcp_servers: string[];
  mcp: ExtensionMcpServer[];
  skills: string[];
  source: ExtensionSource | null;
  mcp_enabled: boolean;
}

/** The `mcp` section as the server shows it: names only, never a value. */
export type McpServerView =
  | { transport: "stdio"; name: string; command: string; args: string[]; env: string[] }
  | {
      transport: "http";
      name: string;
      url: string;
      headers: string[];
      auth: { kind: "env"; var: string } | { kind: "file" } | null;
      untrusted_profiles: string[];
      credential_profiles: string[];
    };

export interface McpState {
  mcp: { enabled: boolean; timeout_secs: number; servers: McpServerView[] };
  revision: string;
  effect: string;
}

/** A server as a write states it; a `null` value keeps the saved one. */
export type McpServerInput =
  | { transport: "stdio"; name: string; command: string; args: string[]; env: Record<string, string | null> }
  | {
      transport: "http";
      name: string;
      url: string;
      headers: Record<string, string | null>;
      auth: { kind: "none" } | { kind: "keep" } | { kind: "env"; var: string };
      untrusted_profiles: string[];
      credential_profiles: string[];
    };

/** What an explicit check found. */
export interface McpCheck {
  name: string;
  transport: "stdio" | "http";
  declared_by: string;
  ok: boolean;
  tools: string[];
  more_tools: number;
  error: string | null;
  elapsed_ms: number;
  mcp_enabled: boolean;
}

/** What a skills import did. */
export interface SkillImported {
  name: string;
  dir: string;
  scope: ExtensionScope;
  layout: "extension" | "single_skill" | "skills_folder" | "skill_folders";
  skills: string[];
  active: boolean;
  replaced: boolean;
  source: ExtensionSource;
}

export interface ExtensionInstalled {
  name: string;
  dir: string;
  scope: ExtensionScope;
  mcp_servers: string[];
  active: boolean;
  mcp_enabled: boolean;
  source: ExtensionSource;
}

/** One specialist role's readiness, from `GET /workers`. */
export interface WorkerReadiness {
  role: string;
  state: "ready" | "degraded" | "disabled" | "permission_denied" | "unavailable";
  worker: string;
  model: string | null;
  model_source: string;
  skill: string | null;
  tools: string[];
  reasons: string[];
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
  /** Why a terminal run stopped (`EndTurn`, `MaxTokens`, `Incomplete`, …). Absent from older servers. */
  stop_reason?: string | null;
}

/** One remembered fact, as `lightagent memory list --json` shows it. */
/** What `lightagent rag add --json` reports. */
export interface KnowledgeAdded {
  indexed: { source: string; chunks: number }[];
  total_chunks: number;
  skipped: { path: string; reason: string }[];
  notes: string[];
}

export interface KnowledgeHit {
  source: string;
  score: number;
  text: string;
}

export interface MemoryItem {
  id: string;
  kind: string;
  tags?: string[];
  created_at?: number;
  updated_at?: number | null;
  source?: { session_id: string; message_index: number } | null;
  text: string;
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

export interface SessionSearchResult extends SessionSummary {
  matched_role?: string;
  snippet?: string;
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
/** A saved profile as `lightagent profile show --json` prints it (the fields the UI reads). */
export interface AgentProfileDetail {
  id: string;
  name: string;
  persona: string;
  routing?: { model?: string };
}

/** A new profile, as `lightagent profile create` takes it. */
export interface NewProfile {
  id: string;
  name?: string;
  model?: string;
  persona?: string;
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
  /** A turn the server is running (or holding paused) that the caller may see. */
  active_run?: { id: string; status: string };
}

export interface LightagentSettings {
  /** The config revision these values were read at; responses only. */
  revision?: string;
  /** Sent on a save: refused with 409 unless the config is still at it. */
  expected_revision?: string;
  max_turns: number;
  max_tool_calls: number;
  wall_clock_secs: number | null;
  approval_policy: "permissive" | "balanced" | "strict";
  web_enabled: boolean;
  /** `lightagent setup` → web; servers before it omit it. */
  web_mode?: "off" | "fetch" | "duckduckgo" | "custom";
  /** The custom search URL; the configured search endpoint in a response. */
  web_search_endpoint?: string | null;
  filesystem_tools_enabled: boolean;
  terminal_enabled: boolean;
  memory_enabled: boolean;
  show_reasoning_in_tui: boolean;
  subagents_enabled: boolean;
  delegate_timeout_secs: number;
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
  /** Task classification, separate from model routing. Absent from older servers. */
  classification?: JevClassificationSettings;
  /** What model routing sends to Jev. Absent from older servers. */
  routing_input?: JevRoutingInput;
  /** Which profiles may send task content to Jev for model routing. Absent from older servers. */
  routing_permission?: JevRoutingPermission;
  /** Profiles permitted to send task content for model routing under `strict`. */
  routing_allowed_profiles?: string[];
  /** Write-only: submitted to the server but never returned in settings responses. */
  api_key?: string;
}

export type JevClassificationMode = "off" | "shadow" | "active";
export type JevRoutingInput = "legacy_raw" | "sanitized";
/** `legacy_all_profiles` is the compatibility policy for configurations that predate the permission. */
export type JevRoutingPermission = "strict" | "legacy_all_profiles";
export type JevRoutingGrant = "allowed" | "compatibility" | "denied";

export interface JevClassificationSettings {
  mode: JevClassificationMode;
  allowed_profiles: string[];
  max_input_chars: number;
  default_threshold: number;
  version_thresholds: Record<string, number>;
  explicit_tool: boolean;
  max_calls_per_run: number;
  workflow_guidance?: boolean;
}

/** A sanitized classification record: fixed vocabulary, numbers and the Jev version only. */
export interface JevDiagnostics {
  schema: number;
  trigger: "preflight" | "explicit";
  mode: JevClassificationMode;
  outcome: "classified" | "low_confidence" | "skipped" | "failed";
  source: string | null;
  category: string | null;
  category_confidence: number | null;
  confidence_available: boolean;
  threshold: number | null;
  planning_probability: number | null;
  retrieval_probability: number | null;
  tools_probability: number | null;
  complexity: string | null;
  complexity_confidence: number | null;
  model_version: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  jev_calls: number;
  classification_latency_ms: number | null;
  preflight_latency_ms: number;
  fallback_reason: string | null;
  influenced: boolean;
  /** Schema 2. */
  accepted?: boolean | null;
  workflow?: string | null;
  workflow_eligibility?: string | null;
  workflow_capability?: string | null;
  workflow_applied?: boolean;
  available_tools?: number | null;
  approval_required?: boolean | null;
  workflow_selection_us?: number | null;
  routing_input?: string | null;
  recorded_at_unix?: number;
}

export interface JevVerification {
  ok: boolean;
  checked_at_unix: number;
  model_version: string | null;
  latency_ms: number;
  error: string | null;
}

/** The server's privacy-safe Jev status for the active profile. */
export interface JevStatus {
  name: string;
  provider: string;
  kind: string;
  description: string;
  profile: string;
  endpoint_configured: boolean;
  api_key_configured: boolean;
  api_key_resolvable: boolean;
  configured: boolean;
  enabled: boolean;
  model: string;
  routing_input?: JevRoutingInput;
  /** Model-routing permission for this profile. Absent from older servers. */
  routing?: {
    permission_policy: JevRoutingPermission;
    compatibility: boolean;
    profile_permission: JevRoutingGrant;
    allowed_profiles: string[];
  };
  verification: JevVerification | null;
  classification: {
    mode: JevClassificationMode;
    profile_permitted: boolean;
    max_input_chars: number;
    default_threshold: number;
    version_thresholds: Record<string, number>;
    max_calls_per_run: number;
    workflow_guidance?: boolean;
  };
  explicit_tool: { name: string; enabled: boolean; available: boolean };
  permissions: { risk: string; requirements: string[] };
  last_classification: JevDiagnostics | null;
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


/** `lightagent identity status`: the store's state and counts. Never a secret. */
export interface IdentityStatus {
  store: "absent" | "present";
  schema_supported: number;
  generation?: number;
  principals?: { users: number; operators: number; suspended: number };
  credentials?: { active: number; revoked: number; expired: number };
  owned_profiles?: { recorded: number; effective: number };
  multi_user: string;
}

export interface IdentityPrincipal {
  id: string;
  role: "user" | "operator";
  status: "active" | "suspended";
  created_unix: number;
  updated_unix: number;
  unrevoked_credentials: number;
}

/** A credential by key id. The secret and its verifier are never listed. */
export interface IdentityCredential {
  key_id: string;
  principal: string;
  role: string;
  state: "active" | "revoked" | "expired";
  created_unix: number;
  expires_unix: number | null;
  revoked_unix: number | null;
  replaces: string | null;
  restriction: { capabilities?: string[] | null; profiles?: string[] | null } | null;
}

export interface IdentityOwnership {
  profile: string;
  owner: string;
  effective: boolean;
  grants: { principal: string; capabilities: string[] }[];
}

export interface IdentityAuditRecord {
  at_unix: number;
  generation: number;
  action: unknown;
  principal: unknown;
  key_id: unknown;
  subject: unknown;
  profile: unknown;
  outcome: unknown;
  denied: boolean;
}

/** An issued or rotated credential: its key id only; the secret went to the named file. */
export interface IdentityIssued {
  issued: string;
  principal?: string;
  replaces?: string;
  expires_unix: number | null;
  secret_delivered_to: string;
}

/** `lightagent remote-credential status`: the vault and its key, never a secret. */
export interface RemoteCredentialStatus {
  vault: "absent" | "present";
  generation?: number;
  grants?: { active: number; revoked: number; expired: number };
  key_configured: boolean;
  key_fingerprint?: string | null;
  key_source: "file" | "env" | null;
  unopenable_with_configured_key?: number;
}

/** One grant: metadata only, never a secret or ciphertext. */
export interface RemoteCredentialGrant {
  id: string;
  profile: string;
  service: string;
  operations: string[] | null;
  destinations: string[];
  state: string;
  version: number;
  created_unix: number;
  expires_unix: number | null;
  revoked_unix: number | null;
  rotated_unix: number | null;
  max_requests_per_run: number | null;
  key_fingerprint: string;
}

/** `lightagent update --check --json`: read-only; nothing is installed. */
export interface UpdateCheck {
  current: string | null;
  latest: string;
  update_available: boolean;
  release: string;
  method: string;
  blocked: string | null;
}

export interface ApprovalRow {
  run: string;
  pending: PendingApproval | null;
}

async function jsonRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await apiFetch(`${BASE}${path}`, {
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

/** `lightagent config path|show`: where the config lives, its revision, and its redacted content. */
export interface ConfigView {
  path: string;
  revision: string;
  config: Record<string, unknown>;
}

/** The active endpoint's provider API key as status only; the key itself is never returned. */
export interface ProviderCredentialStatus {
  provider_key_id: string;
  base_url: string;
  state: "absent" | "ready" | "unreadable" | "unresolved" | "empty" | "invalid";
  source: "file" | "env" | null;
  revision: string;
}

/** The tools workspace (`lightagent setup tools`) for the next run of the active profile. */
export interface WorkspaceState {
  workspace: {
    configured: string | null;
    effective: string | null;
    source: "config" | "profile";
    profile: string | null;
    exists: boolean;
    tools_enabled: boolean;
    terminal_enabled: boolean;
  };
  revision: string;
  effect: string;
}

/** A provider as the picker shows it: its key's state only, never the key. */
export interface ProviderEntry {
  id: string;
  name: string;
  base_url: string;
  kind: "local" | "saved" | "custom";
  active: boolean;
  key_state: ProviderCredentialStatus["state"];
  key_source: "file" | "env" | null;
}

/** The providers `lightagent setup provider` offers, the model, and when a change applies. */
export interface ProviderList {
  providers: ProviderEntry[];
  active: string | null;
  model: string | null;
  revision: string;
  effect: string;
}

/** A saved provider's edit; an absent field stays as it is. */
export interface ProviderEdit {
  name?: string;
  base_url?: string;
  keep_key?: boolean;
}

/** What an explicit endpoint check found (`lightagent models`). It never holds a key. */
export interface EndpointCheck {
  base_url: string;
  ok: boolean;
  models: string[];
  error: string | null;
  key_sent: boolean;
  elapsed_ms: number;
}

/** `lightagent config jev-routing-report`: the routing-permission dry run. It changes nothing. */
export interface JevRoutingReport {
  report: string;
  changes_made: boolean;
  routing_enabled: boolean;
  routing_input: string;
  permission_policy: string;
  compatibility: boolean;
  profiles: { profile: string; now: string; under_strict: string }[];
  denied_under_strict: string[];
  allowed_profiles_not_found: string[];
}

/** One config key as `lightagent config get` reads it (secrets redacted). */
export interface ConfigValue {
  key: string;
  value: string;
  revision: string;
}

/** The profile's onboarding guidance (the chat's `/onboard`). */
export interface Onboarding {
  installed: boolean;
  text: string | null;
}

export const agentApi = {
  /**
   * The tool catalog. MCP servers are connected (a local MCP command is
   * started) only with `includeMcp`; every screen asks without it.
   */
  tools: (options: { includeMcp?: boolean } = {}) =>
    jsonRequest<{ tools: ToolInfo[]; mcp?: "off" | "skipped" | "connected" }>(
      options.includeMcp ? "/tools" : "/tools?mcp=skip",
    ),
  skills: () => jsonRequest<{ skills: SkillInfo[] }>("/skills"),
  /** `lightagent extensions list|show|install|enable|disable|uninstall`, the same operations. */
  extensions: () => jsonRequest<ExtensionListing>("/extensions"),
  extension: (name: string) =>
    jsonRequest<ExtensionDetail>(`/extensions/${encodeURIComponent(name)}`),
  installExtension: (source: string, scope: ExtensionScope, activateMcp: boolean) =>
    jsonRequest<ExtensionInstalled>("/extensions", {
      method: "POST",
      body: JSON.stringify({ source, scope, activate_mcp: activateMcp }),
    }),
  setExtensionEnabled: (name: string, enabled: boolean) =>
    jsonRequest<{ name: string; active: boolean; extensions_enabled: boolean }>(
      `/extensions/${encodeURIComponent(name)}/enabled`,
      { method: "PUT", body: JSON.stringify({ enabled }) },
    ),
  uninstallExtension: (name: string, scope: ExtensionScope) =>
    jsonRequest<{ name: string; removed_from: string }>(
      `/extensions/${encodeURIComponent(name)}?scope=${scope}`,
      { method: "DELETE" },
    ),
  /** MCP servers through the shared operations; values are never returned. */
  mcp: () => jsonRequest<McpState>("/mcp"),
  setMcp: (change: { enabled?: boolean; timeout_secs?: number }, revision: string) =>
    jsonRequest<McpState>("/mcp", {
      method: "PUT",
      body: JSON.stringify({ ...change, expected_revision: revision }),
    }),
  addMcpServer: (server: McpServerInput, revision: string) =>
    jsonRequest<McpState>("/mcp/servers", {
      method: "POST",
      body: JSON.stringify({ server, expected_revision: revision }),
    }),
  removeMcpServer: (name: string) =>
    jsonRequest<McpState>(`/mcp/servers/${encodeURIComponent(name)}`, { method: "DELETE" }),
  /** `lightagent rag …` on the active profile's index, the same operations. */
  knowledge: () =>
    jsonRequest<{ sources: { source: string; chunks: number }[]; total_chunks: number }>("/knowledge"),
  addKnowledge: (path: string, source?: string) =>
    jsonRequest<KnowledgeAdded>("/knowledge", { method: "POST", body: JSON.stringify({ path, source }) }),
  clearKnowledge: () => jsonRequest<{ cleared: boolean }>("/knowledge", { method: "DELETE" }),
  searchKnowledge: (query: string, topK?: number) =>
    jsonRequest<{ query: string; hits: KnowledgeHit[] }>(
      topK
        ? `/knowledge/search?q=${encodeURIComponent(query)}&top_k=${topK}`
        : `/knowledge/search?q=${encodeURIComponent(query)}`,
    ),
  /** `lightagent memory …` on the active profile, the same operations. */
  memories: () => jsonRequest<{ memories: MemoryItem[] }>("/memories"),
  addMemory: (text: string, kind: string | undefined, tags: string[]) =>
    jsonRequest<{ id: string }>("/memories", {
      method: "POST",
      body: JSON.stringify({ text, kind, tags }),
    }),
  updateMemory: (id: string, text: string) =>
    jsonRequest<{ updated: boolean }>(`/memories/items/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify({ text }),
    }),
  forgetMemory: (id: string) =>
    jsonRequest<{ removed: boolean }>(`/memories/items/${encodeURIComponent(id)}`, { method: "DELETE" }),
  clearMemories: () => jsonRequest<{ cleared: boolean }>("/memories", { method: "DELETE" }),
  searchMemories: (query: string, topK?: number) =>
    jsonRequest<{ query: string; memories: MemoryItem[] }>(
      topK
        ? `/memories/search?q=${encodeURIComponent(query)}&top_k=${topK}`
        : `/memories/search?q=${encodeURIComponent(query)}`,
    ),
  reflectMemories: (topic?: string) =>
    jsonRequest<{ topic: string | null; knowledge: string }>(
      topic ? `/memories/reflect?topic=${encodeURIComponent(topic)}` : "/memories/reflect",
    ),
  memoryCandidates: (session: string) =>
    jsonRequest<{ session_id: string; candidates: { message: number; text: string }[] }>(
      `/memories/candidates?session=${encodeURIComponent(session)}`,
    ),
  promoteMemory: (request: { session: string; message: number; text?: string; kind?: string; tags: string[] }) =>
    jsonRequest<{ id: string }>("/memories/promote", { method: "POST", body: JSON.stringify(request) }),
  /** Import skills from a repository or folder (new scope); inactive unless `activate`. */
  importSkills: (request: {
    source: string;
    revision?: string;
    name?: string;
    scope: ExtensionScope;
    activate: boolean;
    replace: boolean;
  }) => jsonRequest<SkillImported>("/skills/imports", { method: "POST", body: JSON.stringify(request) }),
  /** Connects the server now; a stdio server's command is started. */
  checkMcpServer: (name: string) =>
    jsonRequest<McpCheck>(`/mcp/servers/${encodeURIComponent(name)}/check`, { method: "POST" }),
  workers: () => jsonRequest<{ workers: WorkerReadiness[] }>("/workers"),
  /** `lightagent workers status|assign|revoke`, for the active profile or the one named. */
  workersFor: (profile?: string) =>
    jsonRequest<{ workers: WorkerReadiness[] }>(
      profile ? `/workers?profile=${encodeURIComponent(profile)}` : "/workers",
    ),
  assignWorker: (role: string, worker: string | null, profile?: string) =>
    jsonRequest<Record<string, unknown>>(`/workers/${encodeURIComponent(role)}`, {
      method: "PUT",
      body: JSON.stringify({ worker, profile }),
    }),
  revokeWorker: (role: string, profile?: string) =>
    jsonRequest<{ role: string; revoked: boolean }>(
      profile
        ? `/workers/${encodeURIComponent(role)}?profile=${encodeURIComponent(profile)}`
        : `/workers/${encodeURIComponent(role)}`,
      { method: "DELETE" },
    ),
  /** `lightagent identity …` as the local operator. A credential's secret goes to a new file, never to the UI. */
  identityStatus: () => jsonRequest<IdentityStatus>("/identity"),
  identityUsers: () => jsonRequest<{ principals: IdentityPrincipal[] }>("/identity/users"),
  createIdentityUser: (id: string) =>
    jsonRequest<{ created: string }>("/identity/users", { method: "POST", body: JSON.stringify({ id }) }),
  suspendIdentityUser: (id: string) =>
    jsonRequest<{ suspended: string }>(`/identity/users/${encodeURIComponent(id)}/suspend`, { method: "POST" }),
  identityCredentials: (principal?: string) =>
    jsonRequest<{ credentials: IdentityCredential[] }>(
      principal ? `/identity/credentials?principal=${encodeURIComponent(principal)}` : "/identity/credentials",
    ),
  issueIdentityCredential: (request: {
    principal: string;
    expires_in?: number;
    capabilities: string[];
    profiles: string[];
    secret_file: string;
  }) => jsonRequest<IdentityIssued>("/identity/credentials", { method: "POST", body: JSON.stringify(request) }),
  rotateIdentityCredential: (keyId: string, request: { expires_in?: number; secret_file: string }) =>
    jsonRequest<IdentityIssued>(`/identity/credentials/${encodeURIComponent(keyId)}/rotate`, {
      method: "POST",
      body: JSON.stringify(request),
    }),
  revokeIdentityCredential: (keyId: string) =>
    jsonRequest<{ revoked: string }>(`/identity/credentials/${encodeURIComponent(keyId)}/revoke`, { method: "POST" }),
  assignIdentityOwner: (profile: string, principal: string) =>
    jsonRequest<{ owner: string; assignment: string }>(`/identity/owners/${encodeURIComponent(profile)}`, {
      method: "PUT",
      body: JSON.stringify({ principal }),
    }),
  reassignIdentityOwner: (profile: string, principal: string) =>
    jsonRequest<{ owner: string; assignment: string }>(`/identity/owners/${encodeURIComponent(profile)}/reassign`, {
      method: "POST",
      body: JSON.stringify({ principal }),
    }),
  removeIdentityOwner: (profile: string) =>
    jsonRequest<{ owner: null }>(`/identity/owners/${encodeURIComponent(profile)}`, { method: "DELETE" }),
  identityGrants: (profile?: string) =>
    jsonRequest<{ profiles: IdentityOwnership[] }>(
      profile ? `/identity/grants?profile=${encodeURIComponent(profile)}` : "/identity/grants",
    ),
  revokeIdentityGrant: (request: { profile: string; principal: string; capability: string }) =>
    jsonRequest<{ revoked: string }>("/identity/grants/revoke", { method: "POST", body: JSON.stringify(request) }),
  identityAudit: (limit: number) =>
    jsonRequest<{ records: IdentityAuditRecord[] }>(`/identity/audit?limit=${limit}`),
  /** `lightagent remote-credential …` as the local operator. Secrets are read by the runtime from files; never sent here. */
  remoteCredentialStatus: () => jsonRequest<RemoteCredentialStatus>("/remote-credentials/vault"),
  createCredentialKey: (path: string) =>
    jsonRequest<{ key_file: string; key_fingerprint: string; next: string }>("/remote-credentials/keys", {
      method: "POST",
      body: JSON.stringify({ path }),
    }),
  remoteCredentials: (profile?: string) =>
    jsonRequest<{ credentials: RemoteCredentialGrant[] }>(
      profile ? `/remote-credentials?profile=${encodeURIComponent(profile)}` : "/remote-credentials",
    ),
  addRemoteCredential: (request: {
    profile: string;
    service: string;
    destinations: string[];
    operations: string[];
    expires_in?: number;
    max_requests_per_run?: number;
    secret_file: string;
  }) => jsonRequest<{ added: string; profile: string; service: string }>("/remote-credentials", {
    method: "POST",
    body: JSON.stringify(request),
  }),
  rotateRemoteCredential: (id: string, secretFile: string) =>
    jsonRequest<{ rotated: string; version: number }>(`/remote-credentials/${encodeURIComponent(id)}/rotate`, {
      method: "POST",
      body: JSON.stringify({ secret_file: secretFile }),
    }),
  revokeRemoteCredential: (id: string) =>
    jsonRequest<{ revoked: string }>(`/remote-credentials/${encodeURIComponent(id)}/revoke`, { method: "POST" }),
  remoteCredentialAudit: (limit: number) =>
    jsonRequest<{ records: Record<string, unknown>[] }>(`/remote-credentials/audit?limit=${limit}`),
  rekeyRemoteCredentials: (previousKeyFile: string) =>
    jsonRequest<{ resealed: number; key_fingerprint: string }>("/remote-credentials/rekey", {
      method: "POST",
      body: JSON.stringify({ previous_key_file: previousKeyFile }),
    }),
  /** `lightagent doctor|architecture --json`, and the diagnostics bundle (allowlisted fields only; never uploaded). */
  doctor: () => jsonRequest<Record<string, unknown>>("/diagnostics/doctor"),
  architecture: () => jsonRequest<Record<string, unknown>>("/diagnostics/architecture"),
  updateCheck: () => jsonRequest<UpdateCheck>("/update/check"),
  diagnosticsBundle: () =>
    jsonRequest<Record<string, unknown> & { about: string; included: string[]; excluded: string[] }>(
      "/diagnostics/bundle",
    ),
  /** `lightagent init`: an initialised home is refused (409) unless `force`. */
  initHome: (request: { force: boolean; profile?: string; base_url?: string; model?: string }) =>
    jsonRequest<{ home: string; config_file: string; active_profile: string; base_url: string }>("/setup/init", {
      method: "POST",
      body: JSON.stringify(request),
    }),
  /** `lightagent import hermes`: a dry run reports what would be imported or skipped. */
  importHermes: (request: { from?: string; dry_run: boolean; force: boolean }) =>
    jsonRequest<{ dry_run: boolean; hermes_home: string; profiles: string[]; skills: string[]; skipped: string[]; errors: string[] }>(
      "/import/hermes",
      { method: "POST", body: JSON.stringify(request) },
    ),
  settings: () => jsonRequest<LightagentSettings>("/settings"),
  saveSettings: (settings: LightagentSettings) =>
    jsonRequest<LightagentSettings>("/settings", {
      method: "PUT",
      body: JSON.stringify(settings),
    }),
  config: () => jsonRequest<ConfigView>("/config"),
  configKey: (key: string) => jsonRequest<ConfigValue>(`/config/${encodeURIComponent(key)}`),
  /** `/onboard`: the guidance installed for the profile the runtime serves. */
  onboarding: () => jsonRequest<Onboarding>("/onboarding"),
  /** `/onboard <file.md>`: install or replace it from a Markdown file on this machine. */
  setOnboarding: (path: string) =>
    jsonRequest<Onboarding>("/onboarding", { method: "PUT", body: JSON.stringify({ path }) }),
  /** `/onboard remove`. */
  removeOnboarding: () => jsonRequest<Onboarding>("/onboarding", { method: "DELETE" }),
  /** `lightagent config set`; refused with 409 when `expectedRevision` is stale. */
  setConfigKey: (key: string, value: string, expectedRevision?: string) =>
    jsonRequest<ConfigValue>(`/config/${encodeURIComponent(key)}`, {
      method: "PUT",
      body: JSON.stringify({ value, expected_revision: expectedRevision }),
    }),
  provider: () => jsonRequest<ProviderCapabilities>("/provider"),
  jevStatus: () => jsonRequest<JevStatus>("/jev"),
  jevRoutingReport: () => jsonRequest<JevRoutingReport>("/jev/routing-report"),
  providerCredential: () => jsonRequest<ProviderCredentialStatus>("/provider/credential"),
  /** Write-only: the response is the status, never the key. 409 when `expectedRevision` is stale. */
  saveProviderCredential: (key: string, expectedRevision?: string) =>
    jsonRequest<ProviderCredentialStatus>("/provider/credential", {
      method: "PUT",
      body: JSON.stringify({ key, expected_revision: expectedRevision }),
    }),
  removeProviderCredential: () =>
    jsonRequest<ProviderCredentialStatus>("/provider/credential", { method: "DELETE" }),
  /** An explicit, bounded check of the active endpoint; it follows no redirect. */
  checkProvider: (timeoutSecs?: number) =>
    jsonRequest<EndpointCheck>("/provider/check", {
      method: "POST",
      body: JSON.stringify({ timeout_secs: timeoutSecs }),
    }),
  workspace: () => jsonRequest<WorkspaceState>("/workspace"),
  /** An absolute folder, or `null` for the active profile's own folder; 409 when the revision is stale. */
  setWorkspace: (path: string | null, expectedRevision: string) =>
    jsonRequest<WorkspaceState>("/workspace", {
      method: "PUT",
      body: JSON.stringify({ path, expected_revision: expectedRevision }),
    }),
  providers: () => jsonRequest<ProviderList>("/providers"),
  /** Every provider change names the revision it was made on: 409 when it is stale. */
  createProvider: (name: string, baseUrl: string, expectedRevision: string) =>
    jsonRequest<ProviderList>("/providers", {
      method: "POST",
      body: JSON.stringify({ name, base_url: baseUrl, expected_revision: expectedRevision }),
    }),
  updateProvider: (id: string, edit: ProviderEdit, expectedRevision: string) =>
    jsonRequest<ProviderList>(`/providers/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify({ ...edit, expected_revision: expectedRevision }),
    }),
  removeProvider: (id: string, expectedRevision: string) =>
    jsonRequest<ProviderList>(
      `/providers/${encodeURIComponent(id)}?expected_revision=${encodeURIComponent(expectedRevision)}`,
      { method: "DELETE" },
    ),
  /** An explicit, bounded check of one provider's endpoint, with its own key only. */
  checkSavedProvider: (id: string, timeoutSecs?: number) =>
    jsonRequest<EndpointCheck>(`/providers/${encodeURIComponent(id)}/check`, {
      method: "POST",
      body: JSON.stringify({ timeout_secs: timeoutSecs }),
    }),
  selectProvider: (id: string, model: string | null, expectedRevision: string) =>
    jsonRequest<ProviderList>("/provider/active", {
      method: "PUT",
      body: JSON.stringify({ id, model, expected_revision: expectedRevision }),
    }),
  selectProviderModel: (model: string | null, expectedRevision: string) =>
    jsonRequest<ProviderList>("/provider/model", {
      method: "PUT",
      body: JSON.stringify({ model, expected_revision: expectedRevision }),
    }),
  verifyJev: () => jsonRequest<JevVerification>("/jev/verify", { method: "POST", body: "{}" }),
  profiles: () => jsonRequest<ProfileCatalog>("/profiles"),
  profile: (id: string) => jsonRequest<AgentProfileDetail>(`/profiles/${encodeURIComponent(id)}`),
  createProfile: (profile: NewProfile) =>
    jsonRequest<{ created: string }>("/profiles", { method: "POST", body: JSON.stringify(profile) }),
  useProfile: (id: string) =>
    jsonRequest<{ active: string }>(`/profiles/${encodeURIComponent(id)}/active`, { method: "PUT", body: "{}" }),
  presetProfile: (id: string, preset: string) =>
    jsonRequest<{ profile: string; preset: string }>(`/profiles/${encodeURIComponent(id)}/preset`, {
      method: "POST",
      body: JSON.stringify({ preset }),
    }),
  deleteProfile: (id: string) =>
    jsonRequest<{ deleted: string }>(`/profiles/${encodeURIComponent(id)}`, { method: "DELETE" }),
  /** Runs that have not finished (operator view). */
  activeRuns: () => jsonRequest<{ runs: { id: string; status: string }[] }>("/runs"),
  createRun: (message: string, profile?: string, sessionId?: string, model?: string) =>
    jsonRequest<{ id: string; status: string; session_id: string | null }>("/runs", {
      method: "POST",
      // Interactive, like the terminal: a run that reaches its time budget
      // with unread tool results pauses until the person continues it.
      body: JSON.stringify({ message, profile, session_id: sessionId, model, on_time_budget: "pause" }),
    }),
  /** Continue a run paused on its time budget: renew it (`extend`) or answer now (`answer`). */
  continueRun: (runId: string, mode: "extend" | "answer") =>
    jsonRequest<{ id: string; continued: boolean }>(`/runs/${encodeURIComponent(runId)}/continue`, {
      method: "POST",
      body: JSON.stringify({ mode }),
    }),
  /** Replace the session's latest answer, written by `runId`, with a new run of its prompt. */
  regenerate: (sessionId: string, runId: string, model?: string) =>
    jsonRequest<{ id: string; status: string; session_id: string | null }>(
      `/sessions/${encodeURIComponent(sessionId)}/regenerate`,
      { method: "POST", body: JSON.stringify({ run_id: runId, model }) },
    ),
  createSession: () => jsonRequest<{ id: string }>("/sessions", { method: "POST", body: "{}" }),
  uploadAttachment: async (sessionId: string, file: File): Promise<UploadedAttachment> => {
    const filename = file.name.replace(/[^A-Za-z0-9._ -]/g, "_").slice(0, 180) || "attachment";
    const response = await apiFetch(`${BASE}/sessions/${encodeURIComponent(sessionId)}/attachments`, {
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
  searchSessions: (query: string) =>
    jsonRequest<{ sessions: SessionSearchResult[] }>(`/sessions/search?q=${encodeURIComponent(query)}`),
  approvals: () => jsonRequest<{ approvals: ApprovalRow[] }>("/approvals"),
  respondApproval: (run: string, approve: boolean) =>
    jsonRequest<{ run: string; delivered: boolean }>(`/approvals/${run}`, {
      method: "POST",
      body: JSON.stringify({ approve }),
    }),
  /** The run's event stream; `after` resumes after the event with that id. */
  eventsUrl: (id: string, after?: number) =>
    after === undefined || after < 0
      ? `${BASE}/runs/${id}/events`
      : `${BASE}/runs/${id}/events?after=${after}`,
};
