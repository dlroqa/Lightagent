import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { NavLink } from "react-router-dom";
import { Archive, Ban, BookOpen, ChevronDown, ChevronRight, Cpu, FilePenLine, FileText, Folder, Globe, MoreHorizontal, Pencil, Pin, Plus, Search, Send, Share2, ShieldCheck, Sparkles, Terminal, Trash2, Wrench } from "lucide-react";

import {
  agentApi,
  type AgentSession,
  type LightagentSettings,
  type SessionPatch,
  type SessionMessage,
  type SessionSummary,
  type SystemTime,
  type ToolInfo,
  type UploadedAttachment,
} from "../api/agent";
import { promptTimestamp } from "../api/format";
import { Empty, Pill } from "../components/Bits";
import { Menu, MenuItem } from "../components/Menu";
import { TopBar } from "../components/Shell";
import { usePoll } from "../hooks/usePoll";
import { useRunEvents, type RunEvent } from "../hooks/useRunEvents";

const SESSION_KEY = "lightagent.agent.session";
const WELCOME_PROMPTS = [
  "Where should we begin?",
  "What would you like to make?",
  "What are we exploring today?",
  "Give me a problem worth solving.",
  "Let’s turn an idea into progress.",
  "What’s on your mind?",
  "Point me at the next challenge.",
  "Let’s untangle something.",
];
const text = (value: unknown) => (typeof value === "string" ? value : "");
const unix = (value: SystemTime) => value.secs_since_epoch;

type ToolStatus = "requested" | "running" | "ok" | "error";

interface ToolCall {
  id: string;
  name: string;
  arguments: string;
  status: ToolStatus;
  result: string;
  durationMs?: number;
}

function modelLabel(id: string, name?: string | null) {
  if (name?.trim()) return name.trim();
  return id
    .replace(/@(\d+)k\b/i, (_match, context) => " · " + context + "K context")
    .replace(/[._-]+/g, " ")
    .replace(/\b[a-z]/g, (letter) => letter.toUpperCase())
    .replace(/(\d)([a-z])/g, (_match, number, letter) => number + letter.toUpperCase());
}

function ModelOptions({ provider }: { provider?: import("../api/agent").ProviderCapabilities | null }) {
  if (!provider) return null;
  const byId = new Map(provider.runtime_models.map((model) => [model.id, model]));
  const runtimeOnly = provider.runtime_models.filter((model) => !provider.models.includes(model.id));
  const configuredOnly = Object.entries(provider.model_catalog)
    .filter(([id]) => !provider.models.includes(id) && !byId.has(id));
  return <>
    {provider.models.map((model) => <option key={model} value={model}>{modelLabel(model, byId.get(model)?.name ?? provider.model_aliases[model])}</option>)}
    {runtimeOnly.length > 0 && (
      <optgroup label="Backend runtime catalog">
        {runtimeOnly.map((model) => {
          const unavailable = model.state !== "available" && model.state !== "loaded";
          const unsupported = model.supported === false;
          const reason = unsupported ? "unsupported" : unavailable ? model.state : "load it in Runtime";
          return <option key={model.id} value={model.id} disabled>{modelLabel(model.id, model.name ?? provider.model_aliases[model.id]) + " — " + reason}</option>;
        })}
      </optgroup>
    )}
    {configuredOnly.length > 0 && (
      <optgroup label="Configured models">
        {configuredOnly.map(([id, name]) => (
          <option key={id} value={id} disabled>{modelLabel(id, name) + " — unavailable in backend"}</option>
        ))}
      </optgroup>
    )}
  </>;
}

/** The lifecycle word for a tool call's state, as the timeline shows it. */
const STATUS_LABEL: Record<ToolStatus, string> = {
  requested: "queued",
  running: "running",
  ok: "succeeded",
  error: "failed",
};

/** How each risk class is spoken and coloured, shared by both tool menus. */
const RISK_TONE: Record<string, "ok" | "warn" | "danger" | "info" | "neutral"> = {
  observe: "ok",
  external: "info",
  sensitive: "warn",
  mutating: "warn",
  executable: "danger",
  privileged: "danger",
};
const riskTone = (risk: string) => RISK_TONE[risk] ?? "neutral";

const POLICY_HINT: Record<string, string> = {
  permissive: "Auto-approves lower-risk tools; writes and commands still ask.",
  balanced: "Asks before a tool changes state or runs code.",
  strict: "Asks before any tool that does more than read.",
};

function foldTools(events: RunEvent[]): ToolCall[] {
  const calls = new Map<string, ToolCall>();
  for (const event of events) {
    const id = text(event.data.id);
    const call = calls.get(id);
    if (event.type === "tool.requested") {
      calls.set(id, {
        id,
        name: text(event.data.name),
        arguments: text(event.data.arguments),
        status: "requested",
        result: "",
      });
    } else if (event.type === "tool.started" && call) call.status = "running";
    else if (event.type === "tool.output" && call) {
      call.status = "ok";
      call.result = text(event.data.content);
      if (typeof event.data.duration_ms === "number") call.durationMs = event.data.duration_ms;
    } else if (event.type === "tool.failed" && call) {
      call.status = "error";
      call.result = text(event.data.content);
      if (typeof event.data.duration_ms === "number") call.durationMs = event.data.duration_ms;
    }
  }
  return [...calls.values()];
}

export function Agent() {
  const sessions = usePoll(() => agentApi.sessions().then((body) => body.sessions), 2000);
  // Runtime facts exposed by this same Lightagent process.
  const toolCatalog = usePoll(() => agentApi.tools().then((body) => body.tools), 0);
  const provider = usePoll(agentApi.provider, 10_000);
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [welcomePromptIndex, setWelcomePromptIndex] = useState(0);
  const agentSettings = usePoll(agentApi.settings, 0);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [statusControlsOpen, setStatusControlsOpen] = useState(false);
  const composerToolsBtn = useRef<HTMLButtonElement | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [session, setSession] = useState<AgentSession | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [attachments, setAttachments] = useState<UploadedAttachment[]>([]);
  const [steering, setSteering] = useState<string[]>([]);
  const [queuePaused, setQueuePaused] = useState(false);
  const [search, setSearch] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [showArchived, setShowArchived] = useState(false);
  const [sidebarNotice, setSidebarNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [deciding, setDeciding] = useState(false);
  const [savingPolicy, setSavingPolicy] = useState(false);
  const [followTranscript, setFollowTranscript] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const end = useRef<HTMLDivElement | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const dispatching = useRef(false);
  const selected = useRef(activeId);
  const loadGeneration = useRef(0);
  selected.current = activeId;
  const { events, done } = useRunEvents(runId);

  const answer = useMemo(
    () => events.filter((event) => event.type === "model.delta")
      .map((event) => text(event.data.content)).join(""),
    [events],
  );
  const tools = useMemo(() => foldTools(events), [events]);
  const failure = useMemo(() => {
    const event = [...events].reverse().find((row) => row.type === "error");
    return event
      ? text(event.data.message) || "The run failed."
      : events.some((row) => row.type === "run.failed")
        ? "The run failed."
        : null;
  }, [events]);
  const cancelled = events.some((event) => event.type === "run.cancelled");
  const running = busy || (runId !== null && !done);
  const hasPendingWork = running || steering.length > 0;
  const isNewConversation = !session || (session.messages.length === 0 && session.runs.length === 0);
  const persisted = runId !== null && session?.runs.some((run) => run.run_id === runId);
  // The API records a run in its session only once the run is terminal, in the
  // same save as its assistant message. From then on the saved transcript holds
  // the answer, so the live copy is hidden even if the stream's terminal event
  // has not arrived yet — a session reloaded in that gap (a fast run finishing
  // before `send` reloads it) would otherwise show the answer twice.
  const savedAnswerMatchesLive = answer.length > 0 && session?.messages
    .filter((message) => message.role === "assistant")
    .some((message) => message.content.trim() === answer.trim());
  // Covers a fast terminal save whose run metadata has not reached the session payload yet.
  const showLiveAnswer = runId !== null && !persisted && !savedAnswerMatchesLive;
  const pending = useMemo(() => {
    if (done) return null;
    let open: { toolCallId: string; tool: string } | null = null;
    for (const event of events) {
      const id = text(event.data.id);
      if (event.type === "approval.required") {
        // Approval decisions have their own id. Later tool lifecycle events use
        // the model's tool-call id, so retain that id to clear this prompt once
        // the decision takes effect.
        open = { toolCallId: text(event.data.tool_call_id), tool: text(event.data.name) };
      }
      else if (
        open?.toolCallId === id &&
        ["tool.started", "tool.output", "tool.failed"].includes(event.type)
      ) open = null;
    }
    return open;
  }, [done, events]);

  useEffect(() => {
    if (!pending) setDeciding(false);
  }, [pending]);

  useEffect(() => {
    if (!isNewConversation || draft.trim()) return;
    const timer = window.setInterval(() => {
      setWelcomePromptIndex((current) => (current + 1) % WELCOME_PROMPTS.length);
    }, 12_000);
    return () => window.clearInterval(timer);
  }, [draft, isNewConversation]);

  const load = useCallback(async (id: string) => {
    const generation = ++loadGeneration.current;
    try {
      const loaded = await agentApi.session(id);
      if (selected.current === id && generation === loadGeneration.current) {
        setSession(loaded);
        setError(null);
      }
    } catch (cause) {
      if (selected.current === id && generation === loadGeneration.current) {
        setSession(null);
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    }
  }, []);

  useEffect(() => {
    if (activeId) void load(activeId);
    else setSession(null);
  }, [activeId, load]);

  useEffect(() => {
    if (!done || !activeId || persisted) return;
    let attempts = 0;
    const timer = window.setInterval(() => {
      void load(activeId);
      sessions.refresh();
      attempts += 1;
      if (attempts >= 20) window.clearInterval(timer);
    }, 250);
    return () => window.clearInterval(timer);
  }, [activeId, done, load, persisted, sessions.refresh]);

  useEffect(() => {
    if (!done || !activeId || steering.length === 0 || busy ||
        queuePaused || dispatching.current) return;
    const next = steering[0];
    if (!next) return;
    dispatching.current = true;
    setBusy(true);
    void (async () => {
      try {
        let created: Awaited<ReturnType<typeof agentApi.createRun>> | null = null;
        for (let attempt = 0; attempt < 20; attempt += 1) {
          try {
            created = await agentApi.createRun(next, undefined, activeId, selectedModel || undefined);
            break;
          } catch (cause) {
            const message = cause instanceof Error ? cause.message : String(cause);
            if (!message.startsWith("409: session already has an active run")) throw cause;
            await new Promise((resolve) => window.setTimeout(resolve, 250));
          }
        }
        if (!created) throw new Error("The previous run has not released this session. Retry the queued steer.");
        setSteering((current) => current.slice(1));
        setRunId(created.id);
        await load(activeId);
        sessions.refresh();
      } catch (cause) {
        setQueuePaused(true);
        setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        dispatching.current = false;
        setBusy(false);
      }
    })();
  }, [activeId, busy, done, load, queuePaused, sessions.refresh, steering]);

  useEffect(() => {
    if (followTranscript) end.current?.scrollIntoView({ behavior: "auto", block: "end" });
  }, [answer, followTranscript, session?.messages.length, tools.length]);

  const visible = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return (sessions.data ?? [])
      // Empty chats have no context to name. Keep them as an unsaved draft until
      // the first prompt gives the server a meaningful default title.
      .filter((row) => row.title !== "agent session" || row.message_count > 0 || row.run_count > 0)
      .filter((row) => row.archived === showArchived)
      .filter((row) =>
        !needle ||
        row.title.toLowerCase().includes(needle) ||
        row.profile.toLowerCase().includes(needle) ||
        row.project?.toLowerCase().includes(needle),
      );
  }, [search, sessions.data, showArchived]);
  const sessionSections = useMemo(() => {
    const pinned = visible.filter((row) => row.pinned);
    const projects = new Map<string, SessionSummary[]>();
    for (const row of visible) {
      if (!row.project) continue;
      const rows = projects.get(row.project) ?? [];
      rows.push(row);
      projects.set(row.project, rows);
    }
    return {
      pinned,
      projects: [...projects.entries()].sort(([left], [right]) => left.localeCompare(right)),
      recent: visible.filter((row) => !row.pinned && !row.project),
    };
  }, [visible]);

  function select(id: string) {
    if (hasPendingWork || id === activeId) return;
    window.localStorage.setItem(SESSION_KEY, id);
    selected.current = id;
    loadGeneration.current += 1;
    setActiveId(id);
    setSession(null);
    setRunId(null);
    setError(null);
  }

  function startNew() {
    if (hasPendingWork) return;
    setError(null);
    window.localStorage.removeItem(SESSION_KEY);
    selected.current = null;
    loadGeneration.current += 1;
    setActiveId(null);
    setSession(null);
    setRunId(null);
    setAttachments([]);
    setWelcomePromptIndex((current) => (current + 1) % WELCOME_PROMPTS.length);
  }

  async function remove(id: string) {
    if (hasPendingWork && id === activeId) return;
    try {
      await agentApi.deleteSession(id);
      if (id === activeId) {
        window.localStorage.removeItem(SESSION_KEY);
        selected.current = null;
        loadGeneration.current += 1;
        setActiveId(null);
        setSession(null);
        setRunId(null);
      }
      sessions.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  async function updateSession(id: string, patch: SessionPatch) {
    try {
      await agentApi.updateSession(id, patch);
      if (id === activeId && patch.archived) {
        window.localStorage.removeItem(SESSION_KEY);
        selected.current = null;
        loadGeneration.current += 1;
        setActiveId(null);
        setSession(null);
        setRunId(null);
      } else if (id === activeId) {
        await load(id);
      }
      sessions.refresh();
      return true;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return false;
    }
  }

  async function shareSession(row: SessionSummary) {
    try {
      const transcript = await agentApi.session(row.id);
      const text = transcript.messages.map((message) => `${message.role === "user" ? "You" : "Agent"}: ${message.content}`).join("\n\n");
      if (navigator.share) {
        await navigator.share({ title: row.title || "Lightagent conversation", text });
        setSidebarNotice("Conversation shared.");
      } else {
        await navigator.clipboard.writeText(text);
        setSidebarNotice("Conversation copied to your clipboard.");
      }
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === "AbortError") return;
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  async function send() {
    const prompt = draft.trim();
    const message = attachments.length > 0
      ? `${prompt}${prompt ? "\n\n" : ""}Attached files (available locally to inspect):\n${attachments.map((file) => `- ${file.path}`).join("\n")}`
      : prompt;
    if (!message) return;
    if ((runId !== null && !done) || steering.length > 0 || dispatching.current) {
      setSteering((current) => [...current, message]);
      setDraft("");
      setQueuePaused(false);
      return;
    }
    if (busy) return;
    setFollowTranscript(true);
    setBusy(true);
    setDraft("");
    setError(null);
    try {
      let id = activeId;
      if (!id) {
        id = (await agentApi.createSession()).id;
        window.localStorage.setItem(SESSION_KEY, id);
        selected.current = id;
        loadGeneration.current += 1;
        setActiveId(id);
      }
      const run = await agentApi.createRun(message, undefined, id, selectedModel || undefined);
      setRunId(run.id);
      setAttachments([]);
      await load(id);
      sessions.refresh();
    } catch (cause) {
      setDraft(message);
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  async function uploadFiles(files: FileList | null) {
    if (!files?.length || hasPendingWork) return;
    setBusy(true);
    setError(null);
    try {
      let id = activeId;
      if (!id) {
        id = (await agentApi.createSession()).id;
        window.localStorage.setItem(SESSION_KEY, id);
        selected.current = id;
        loadGeneration.current += 1;
        setActiveId(id);
      }
      const uploaded = await Promise.all([...files].map((file) => agentApi.uploadAttachment(id!, file)));
      setAttachments((current) => [...current, ...uploaded]);
      await load(id);
      sessions.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  async function cancel() {
    if (runId) await agentApi.cancelRun(runId).catch((cause) =>
      setError(cause instanceof Error ? cause.message : String(cause)));
  }

  async function decide(approve: boolean) {
    if (!runId || deciding) return;
    setDeciding(true);
    try {
      const result = await agentApi.respondApproval(runId, approve);
      if (!result.delivered) {
        setDeciding(false);
        setError("The approval was no longer waiting for a decision. Please wait for the run to update.");
      }
    } catch (cause) {
      setDeciding(false);
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }

  async function setApprovalPolicy(policy: LightagentSettings["approval_policy"]) {
    const settings = agentSettings.data;
    if (!settings || savingPolicy) return;
    setSavingPolicy(true);
    setError(null);
    try {
      await agentApi.saveSettings({ ...settings, approval_policy: policy });
      agentSettings.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSavingPolicy(false);
    }
  }

  const serviceUnavailable = sessions.error !== null && sessions.data === null;
  const shownError = error ?? sessions.error?.message ?? null;
  const activeTool = tools.find((tool) => tool.status === "running" || tool.status === "requested");
  const activity = pending
    ? { tone: "warn" as const, label: "approval needed", detail: `${pending.tool} is paused until you decide.` }
    : queuePaused
      ? { tone: "warn" as const, label: "queue paused", detail: "A queued steer is paused; retry or clear it." }
      : serviceUnavailable
        ? { tone: "danger" as const, label: "offline", detail: "The Lightagent service cannot be reached." }
        : failure
          ? { tone: "danger" as const, label: "error", detail: failure }
          : cancelled
            ? { tone: "warn" as const, label: "cancelled", detail: "The active run was cancelled." }
            : activeTool
              ? { tone: "accent" as const, label: "tool active", detail: `${activeTool.name} is ${STATUS_LABEL[activeTool.status]}.` }
              : running
                ? { tone: "accent" as const, label: "running", detail: answer ? "The model is responding." : "The agent is preparing its response." }
                : done
                  ? { tone: "ok" as const, label: "completed", detail: "The run completed." }
                  : { tone: "neutral" as const, label: "ready", detail: "No run is active." };
  const badge = { tone: activity.tone, label: activity.label };
  return (
    <>
      <input ref={fileInput} className="composer-attachment-input" type="file" multiple
        accept="image/*,.pdf,.txt,.md,.csv,.json,.doc,.docx,.xls,.xlsx,.ppt,.pptx"
        onChange={(event) => void uploadFiles(event.target.files)} />
      <TopBar
        title="Agent"
        subtitle={session?.title || "Tool-using conversations"}
        actions={
          running && (
            <button type="button" className="btn btn--danger" onClick={() => void cancel()}>
              <Ban size={15} /> Stop
            </button>
          )
        }
      />
      <div className="page agent-layout chat-workspace">
        <aside className="agent-sessions agent-sidebar chat-history" style={{ display: "flex", flexDirection: "column", gap: 12, minHeight: 0 }}>
          <div className="chat-sidebar__brand">
            <img src="/icon.png" alt="" width={30} height={30} />
            <span className="chat-sidebar__wordmark" aria-label="Lightagent">
              <strong className="chat-sidebar__brand-name chat-sidebar__brand-name--light"><span>Lightagent</span></strong>
              <strong className="chat-sidebar__brand-name chat-sidebar__brand-name--lite" aria-hidden="true"><span>Liteagent</span></strong>
            </span>
            <button type="button" className="chat-sidebar__search-button" aria-label="Search chats"
              aria-expanded={searchOpen} onClick={() => setSearchOpen((open) => {
                if (open) setSearch("");
                return !open;
              })}>
              <Search size={16} />
            </button>
          </div>
          <button type="button" className="chat-sidebar__new" disabled={hasPendingWork} onClick={() => void startNew()}>
            <Plus size={17} /> New chat
          </button>
          <nav className="chat-sidebar__nav" aria-label="Workspace">
            <NavLink to="/" end><Sparkles size={16} /> Chat</NavLink>
            <NavLink to="/tools"><Wrench size={16} /> Tools</NavLink>
            <NavLink to="/settings"><ShieldCheck size={16} /> Settings</NavLink>
          </nav>
          {searchOpen && (
            <div className="chat-sidebar__search">
              <Search size={15} aria-hidden="true" />
              <input autoFocus value={search} onChange={(event) => setSearch(event.target.value)}
                placeholder="Search chats…" aria-label="Search agent sessions" />
            </div>
          )}
          {sidebarNotice && <span className="chat-sidebar__notice" role="status">{sidebarNotice}</span>}
          <div className="agent-sessions__list" style={{ flex: 1, overflowY: "auto", margin: "0 -6px" }}>
            {visible.length === 0 ? (
              <div className="empty" style={{ padding: 20 }}>
                <span>{sessions.loading ? "Loading sessions…" : search ? "Nothing matches." : showArchived ? "No archived chats." : "No chats yet."}</span>
              </div>
            ) : (
              showArchived ? (
                <SessionGroup title="Archived chats" rows={visible} activeId={activeId} disabled={hasPendingWork}
                  onOpen={select} onUpdate={updateSession} onShare={shareSession} onDelete={remove}
                  trailing={<button type="button" className="chat-sidebar__archive-toggle" aria-pressed
                    onClick={() => setShowArchived(false)} title="Show recent chats"><Archive size={14} /></button>} />
              ) : (
                <>
                  {sessionSections.pinned.length > 0 && <SessionGroup title="Pinned" rows={sessionSections.pinned}
                    activeId={activeId} disabled={hasPendingWork} onOpen={select} onUpdate={updateSession}
                    onShare={shareSession} onDelete={remove} />}
                  {sessionSections.projects.length > 0 && (
                    <section className="session-group" aria-label="Projects">
                      <div className="chat-sidebar__section">Projects</div>
                      {sessionSections.projects.map(([project, rows]) => (
                        <div className="session-project" key={project}>
                          <span className="session-project__name">{project}</span>
                          <SessionGroup rows={rows} activeId={activeId} disabled={hasPendingWork} onOpen={select}
                            onUpdate={updateSession} onShare={shareSession} onDelete={remove} />
                        </div>
                      ))}
                    </section>
                  )}
                  <SessionGroup title="Recent chats" rows={sessionSections.recent} activeId={activeId}
                    disabled={hasPendingWork} onOpen={select} onUpdate={updateSession} onShare={shareSession}
                    onDelete={remove} trailing={<button type="button" className="chat-sidebar__archive-toggle" aria-pressed={false}
                      onClick={() => setShowArchived(true)} title="Show archived chats"><Archive size={14} /></button>} />
                </>
              )
            )}
          </div>
        </aside>
        <section className="agent-conversation chat-thread" style={{ display: "flex", flexDirection: "column", gap: 12, minHeight: 0 }}>
          {shownError && (
            <div className="notice notice--danger" role="alert">
              <div>{shownError}</div>
            </div>
          )}
          {isNewConversation ? (
            <div className="agent-welcome">
              <div className="agent-welcome__eyebrow"><Sparkles size={15} /> Lightagent</div>
              <h2 className="agent-welcome__prompt" key={welcomePromptIndex}>{WELCOME_PROMPTS[welcomePromptIndex]}</h2>
              <p>Start a conversation with your connected local agent.</p>
              <div className="welcome-composer">
                <button type="button" className="composer-attach" title="Add photos or files"
                  aria-label="Add photos or files" disabled={busy || serviceUnavailable}
                  onClick={() => fileInput.current?.click()}><Plus size={23} /></button>
                <textarea autoFocus rows={1} value={draft} onChange={(event) => setDraft(event.target.value)}
                  placeholder="Message Lightagent" disabled={busy || serviceUnavailable}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !event.shiftKey) {
                      event.preventDefault();
                      void send();
                    }
                  }} aria-label="Message Lightagent" />
                <label className="welcome-composer__model">
                  <Cpu size={14} />
                  <select value={selectedModel} onChange={(event) => setSelectedModel(event.target.value)} disabled={!provider.data || busy} aria-label="Model for this conversation">
                    <option value="">{provider.data?.configured_model ?? "Auto model"}</option>
                    <ModelOptions provider={provider.data} />
                  </select>
                </label>
                <button type="button" className="welcome-composer__send"
                  disabled={(!draft.trim() && attachments.length === 0) || busy || serviceUnavailable} onClick={() => void send()} aria-label="Send message">
                  <Send size={17} />
                </button>
              </div>
              {attachments.length > 0 && <div className="composer-attachments" aria-label="Attached files">
                {attachments.map((file) => <span key={file.path}>{file.name}</span>)}
              </div>}
              <div className="agent-suggestions">
                <button type="button" onClick={() => setDraft("Summarize the current project status, blockers, decisions, and next milestones.")}><FileText size={16} /> Summarize this project</button>
                <button type="button" onClick={() => setDraft("Help me plan the next steps for this task.")}><Sparkles size={16} /> Plan next steps</button>
              </div>
            </div>
          ) : (
            <>
              <div className="chat-transcript" onScroll={(event) => {
                const transcript = event.currentTarget;
                setFollowTranscript(
                  transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 64,
                );
              }}>
                <div className="chat-transcript__content">
                  {session.messages.length === 0 && !showLiveAnswer && (
                    <Empty title="Nothing said yet"
                      hint="Messages, runs, and tool calls are saved with this session." />
                  )}
                  {session.messages.map((message, index) => (
                    <AgentMessage key={message.role + index} message={message} />
                  ))}
                  {showLiveAnswer && answer && (
                    <AgentMessage message={{ role: "assistant", content: answer }} streaming={!done} />
                  )}
                  {running && !answer && tools.length === 0 && (
                    <div className="tool-activity is-active" style={{ marginTop: 10 }} aria-live="polite">
                      <div className="tool-activity__heading">
                        <span>Thinking</span><span className="tool-activity__dots" aria-hidden="true"><i /><i /><i /></span>
                      </div>
                    </div>
                  )}
                  {failure && <div className="notice notice--danger" style={{ marginTop: 10 }}>{failure}</div>}
                  {tools.length > 0 && <ToolList title="Current tool calls" tools={tools} />}
                  <SavedTools session={session} currentRunId={runId} />
                  <div ref={end} />
                </div>
              </div>
              {pending && (
                <div className="approval" role="alertdialog" aria-label={`Approve ${pending.tool}`}>
                  <ShieldCheck size={18} style={{ flex: "none", marginTop: 1 }} />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                      <strong style={{ fontSize: 13.5 }}>{pending.tool}</strong>
                      <span>needs your decision before it can run.</span>
                    </div>
                    <span className="muted" style={{ fontSize: 12 }}>
                      Allowing runs this call once. It is not remembered for later calls.
                    </span>
                  </div>
                  <div style={{ display: "flex", gap: 8, flex: "none" }}>
                    <button type="button" className="btn" disabled={deciding} onClick={() => void decide(false)}>Deny</button>
                    <button type="button" className="btn btn--primary" disabled={deciding} onClick={() => void decide(true)}>
                      {deciding ? "Deciding…" : "Allow once"}
                    </button>
                  </div>
                </div>
              )}
              {steering.length > 0 && (
                <div className="notice notice--info" role="status">
                  <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                    <strong>Queued steers ({steering.length})</strong>
                    <span style={{ flex: 1 }} />
                    {queuePaused && (
                      <button type="button" className="btn" onClick={() => setQueuePaused(false)}>
                        Retry
                      </button>
                    )}
                    <button type="button" className="btn" disabled={busy}
                      onClick={() => { setSteering([]); setQueuePaused(false); }}>
                      Clear queue
                    </button>
                  </div>
                  <ol style={{ margin: "8px 0 0", paddingLeft: 20 }}>
                    {steering.map((message, index) => (
                      <li key={index} style={{ whiteSpace: "pre-wrap" }}>{message}</li>
                    ))}
                  </ol>
                  <span className="muted">
                    These run in order after the active turn finishes. Keep this tab open
                    until the queue drains.
                  </span>
                </div>
              )}
              <div className="chat-composer">
                <button type="button" className="composer-attach" title="Add photos or files"
                  aria-label="Add photos or files" disabled={busy || serviceUnavailable}
                  onClick={() => fileInput.current?.click()}><Plus size={23} /></button>
                <textarea className="input" rows={1}
                  value={draft} placeholder={running ? "Type a steer to queue…" : "Ask the agent…"}
                  disabled={busy || serviceUnavailable}
                  onChange={(event) => setDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !event.shiftKey) {
                      event.preventDefault();
                      void send();
                    }
                  }} aria-label="Message" />
                <label className="chat-composer__model">
                  <Cpu size={15} />
                  <select value={selectedModel} onChange={(event) => setSelectedModel(event.target.value)}
                    disabled={!provider.data || running} aria-label="Model for the next run">
                    <option value="">{provider.data?.configured_model ?? "Auto model"}</option>
                    <ModelOptions provider={provider.data} />
                  </select>
                </label>
                <button type="button" className="btn btn--primary chat-composer__send"
                  disabled={(!draft.trim() && attachments.length === 0) || busy || serviceUnavailable} onClick={() => void send()}
                  aria-label={running || steering.length > 0 ? "Queue steer" : "Send message"}
                  title={running || steering.length > 0 ? "Queue steer" : "Send message"}>
                  <Send size={18} />
                </button>
              </div>
              {attachments.length > 0 && <div className="composer-attachments" aria-label="Attached files">
                {attachments.map((file) => <span key={file.path}>{file.name}</span>)}
              </div>}
              <div className="status-controls">
                <button type="button" className="status-controls__toggle"
                  aria-expanded={statusControlsOpen} onClick={() => {
                    if (statusControlsOpen) setToolsOpen(false);
                    setStatusControlsOpen((open) => !open);
                  }}>
                  <ChevronRight size={14} /> Status Control
                </button>
                {statusControlsOpen && (
                  <div className="composer-meta">
                    <button
                      ref={composerToolsBtn}
                      type="button"
                      className="composer-meta__tools"
                      aria-haspopup="menu"
                      aria-expanded={toolsOpen}
                      disabled={!toolCatalog.data}
                      onClick={() => setToolsOpen((current) => !current)}
                      title="Tools this agent can call"
                    >
                      <Wrench size={13} />
                      <span>{toolCatalog.data ? `${toolCatalog.data.length} tools` : "Tools"}</span>
                      <ChevronDown size={13} />
                    </button>
                    <ToolMenu open={toolsOpen} anchorRef={composerToolsBtn} onClose={() => setToolsOpen(false)} tools={toolCatalog.data} />
                    {agentSettings.data && (
                      <>
                        {provider.data && <span className="composer-fact"><Sparkles size={13} /> {provider.data.reasoning_content ? "Reasoning ready" : "Standard reasoning"}</span>}
                        <label className="composer-policy" title={POLICY_HINT[agentSettings.data.approval_policy]}>
                          <ShieldCheck size={13} />
                          <span className="sr-only">Approval policy for new runs</span>
                          <select value={agentSettings.data.approval_policy} disabled={savingPolicy}
                            aria-label="Approval policy for new runs"
                            onChange={(event) => void setApprovalPolicy(
                              event.target.value as LightagentSettings["approval_policy"],
                            )}>
                            <option value="balanced">Balanced</option>
                            <option value="strict">Strict</option>
                            <option value="permissive">Permissive</option>
                          </select>
                        </label>
                      </>
                    )}
                    <span style={{ flex: 1 }} />
                    <span className="composer-fact">{session.messages.length} msgs · {session.runs.length} runs</span>
                    <span title={activity.detail} aria-label={`Run activity: ${activity.detail}`}>
                      <Pill tone={badge.tone} dot>{badge.label}</Pill>
                    </span>
                  </div>
                )}
              </div>
            </>
          )}
        </section>
      </div>
    </>
  );
}

function SessionGroup({ title, rows, activeId, disabled, onOpen, onUpdate, onShare, onDelete, trailing }: {
  title?: string;
  rows: SessionSummary[];
  activeId: string | null;
  disabled: boolean;
  onOpen: (id: string) => void;
  onUpdate: (id: string, patch: SessionPatch) => Promise<boolean>;
  onShare: (row: SessionSummary) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  trailing?: ReactNode;
}) {
  return (
    <section className="session-group">
      {title && <div className="chat-sidebar__section chat-sidebar__section--controls"><span>{title}</span>{trailing}</div>}
      <ul className="session-group__list">
        {rows.map((row) => (
          <SessionRow key={row.id} row={row} active={row.id === activeId} disabled={disabled}
            onOpen={() => onOpen(row.id)} onUpdate={onUpdate} onShare={() => void onShare(row)}
            onDelete={() => void onDelete(row.id)} />
        ))}
      </ul>
    </section>
  );
}

function SessionRow({ row, active, disabled, onOpen, onUpdate, onShare, onDelete }: {
  row: SessionSummary; active: boolean; disabled: boolean;
  onOpen: () => void; onUpdate: (id: string, patch: SessionPatch) => Promise<boolean>;
  onShare: () => void; onDelete: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuButton = useRef<HTMLButtonElement | null>(null);
  const label = row.title || "Untitled";
  const rename = () => {
    const title = window.prompt("Rename conversation", label);
    if (title?.trim() && title.trim() !== label) void onUpdate(row.id, { title: title.trim() });
    setMenuOpen(false);
  };
  const moveToProject = () => {
    const project = window.prompt("Move conversation to project (leave blank to remove)", row.project ?? "");
    if (project !== null) void onUpdate(row.id, { project: project.trim() || null });
    setMenuOpen(false);
  };
  return (
    <li>
      <div style={{ display: "flex", gap: 8, padding: "10px 12px", borderRadius: "var(--radius)",
        background: active ? "var(--accent-soft)" : "transparent",
        opacity: disabled && !active ? 0.6 : 1 }}>
        <button type="button" disabled={disabled} onClick={onOpen}
          aria-current={active ? "true" : undefined}
          style={{ flex: 1, minWidth: 0, padding: 0, border: 0, background: "transparent",
            color: "inherit", textAlign: "left", cursor: disabled ? "default" : "pointer" }}>
          <div style={{ display: "flex", justifyContent: "space-between", gap: 8, fontSize: 13, fontWeight: active ? 600 : 500 }}>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{label}</span>
          </div>
        </button>
        <button type="button" className={`session-row__action${row.pinned ? " is-pinned" : ""}`}
          disabled={disabled && active} aria-pressed={row.pinned} aria-label={`${row.pinned ? "Unpin" : "Pin"} ${label}`}
          onClick={(event) => { event.stopPropagation(); void onUpdate(row.id, { pinned: !row.pinned }); }}>
          <Pin size={15} />
        </button>
        <button ref={menuButton} type="button" className="session-row__action" disabled={disabled && active}
          aria-label={`More actions for ${label}`} aria-haspopup="menu" aria-expanded={menuOpen}
          onClick={(event) => { event.stopPropagation(); setMenuOpen((open) => !open); }}>
          <MoreHorizontal size={17} />
        </button>
        <Menu open={menuOpen} anchorRef={menuButton} onClose={() => setMenuOpen(false)} align="end" minWidth={216} label={`Actions for ${label}`}>
          <MenuItem onClick={rename}><span className="session-menu__item"><Pencil size={17} /> Rename</span></MenuItem>
          <MenuItem onClick={() => { void onUpdate(row.id, { pinned: !row.pinned }); setMenuOpen(false); }}><span className="session-menu__item"><Pin size={17} /> {row.pinned ? "Unpin" : "Pin"}</span></MenuItem>
          <MenuItem onClick={moveToProject}><span className="session-menu__item"><Folder size={17} /> Move to project <ChevronRight size={16} /></span></MenuItem>
          <div className="menu__divider" role="separator" />
          <MenuItem onClick={() => { onShare(); setMenuOpen(false); }}><span className="session-menu__item"><Share2 size={17} /> Share</span></MenuItem>
          <div className="menu__divider" role="separator" />
          <MenuItem onClick={() => { void onUpdate(row.id, { archived: !row.archived }); setMenuOpen(false); }}><span className="session-menu__item"><Archive size={17} /> {row.archived ? "Unarchive" : "Archive"}</span></MenuItem>
          <MenuItem onClick={() => { setMenuOpen(false); onDelete(); }}><span className="session-menu__item session-menu__item--danger"><Trash2 size={17} /> Delete</span></MenuItem>
        </Menu>
      </div>
    </li>
  );
}

function AgentMessage({ message, streaming }: { message: SessionMessage; streaming?: boolean }) {
  const mine = message.role === "user";
  const timestamp = mine && message.created_at ? promptTimestamp(unix(message.created_at)) : null;
  return (
    <>
      {timestamp && <div className="chat-message__timestamp">{timestamp}</div>}
      <article className={`chat-message${mine ? " is-user" : ""}${streaming ? " is-streaming" : ""}`}
        aria-label={mine ? "Your message" : "Agent message"}>
        <div className="chat-message__content">{linkifyMessage(message.content)}</div>
      </article>
    </>
  );
}

/** Render Markdown-style and plain HTTP(S) URLs without interpreting arbitrary HTML. */
function linkifyMessage(content: string) {
  const links = /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)|(https?:\/\/[^\s<]+)/g;
  const parts: ReactNode[] = [];
  let cursor = 0;
  let match: RegExpExecArray | null;
  while ((match = links.exec(content)) !== null) {
    if (match.index > cursor) parts.push(content.slice(cursor, match.index));
    const href = match[2] ?? match[3];
    const label = match[1] ?? href;
    parts.push(
      <a key={`${match.index}-${href}`} href={href} target="_blank" rel="noreferrer noopener"
        aria-label={`Open ${label} in a new tab`}>
        {label}
      </a>,
    );
    cursor = match.index + match[0].length;
  }
  if (cursor < content.length) parts.push(content.slice(cursor));
  return parts;
}

function ToolList({ title, tools }: { title: string; tools: ToolCall[] }) {
  const active = tools.some((tool) => tool.status === "requested" || tool.status === "running");
  return (
    <div className={`tool-activity${active ? " is-active" : ""}`} style={{ marginTop: 12 }}>
      <div className="tool-activity__heading" aria-live="polite">
        {active ? <><span>Thinking</span><span className="tool-activity__dots" aria-hidden="true"><i /><i /><i /></span></> : title}
      </div>
      {tools.map((tool) => <ToolRow key={tool.id} tool={tool} />)}
    </div>
  );
}

function SavedTools({ session, currentRunId }: { session: AgentSession; currentRunId: string | null }) {
  const tools: ToolCall[] = session.runs.filter((run) => run.run_id !== currentRunId)
    .flatMap((run) => run.tools.map((tool) => ({
      id: run.run_id + tool.id, name: tool.tool, arguments: tool.arguments_preview,
      result: tool.result_excerpt, status: tool.outcome === "error" ? "error" as const : "ok" as const,
      durationMs: tool.duration_ms,
    })));
  return tools.length ? (
    <details style={{ marginTop: 12 }}>
      <summary className="muted" style={{ cursor: "pointer", fontSize: 12, fontWeight: 600 }}>
        Saved tool history ({tools.length})
      </summary>
      {tools.map((tool) => <ToolRow key={tool.id} tool={tool} />)}
    </details>
  ) : null;
}

function ToolRow({ tool }: { tool: ToolCall }) {
  const active = tool.status === "requested" || tool.status === "running";
  const [open, setOpen] = useState(active);
  const activity = describeTool(tool);

  useEffect(() => {
    setOpen(active);
  }, [active]);

  return (
    <details className={`tool-call${tool.status === "error" ? " is-error" : ""}`} open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary className="tool-call__summary">
        <ToolActivityIcon kind={activity.kind} />
        <span className="tool-call__label">
          {activity.verb} <span className="tool-call__subject">{activity.subject}</span>
        </span>
        {tool.status === "error" && <span className="tool-call__status">failed</span>}
        <span style={{ flex: 1 }} />
        {tool.durationMs !== undefined && (
          <span className="tnum muted" style={{ fontSize: 11.5 }}>{formatDuration(tool.durationMs)}</span>
        )}
      </summary>
      <div className="tool-call__details">
        {tool.arguments && tool.arguments !== "{}" && <code className="muted" style={{ fontSize: 12, overflowWrap: "anywhere" }}>{tool.arguments}</code>}
        {tool.result && <span style={{ fontSize: 13, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{tool.result}</span>}
      </div>
    </details>
  );
}

type ToolActivityKind = "read" | "search" | "edit" | "web" | "terminal" | "tool";

function describeTool(tool: ToolCall): { verb: string; subject: string; kind: ToolActivityKind } {
  let args: Record<string, unknown> = {};
  try {
    const parsed: unknown = JSON.parse(tool.arguments);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) args = parsed as Record<string, unknown>;
  } catch {
    // The raw arguments remain available in the expanded details.
  }
  const stringArg = (key: string) => typeof args[key] === "string" ? args[key] : "";

  switch (tool.name) {
    case "fs.read": return { verb: "Read", subject: stringArg("path") || "a file", kind: "read" };
    case "fs.list": return { verb: "Looked through", subject: stringArg("path") || "the workspace", kind: "read" };
    case "fs.write": return { verb: "Edited", subject: stringArg("path") || "a file", kind: "edit" };
    case "web.search": return { verb: "Searched for", subject: stringArg("query") || "the web", kind: "search" };
    case "web.fetch": return { verb: "Visited", subject: stringArg("url") || "a web page", kind: "web" };
    case "terminal.run":
    case "open_terminal.run": return { verb: "Ran", subject: stringArg("command") || "a command", kind: "terminal" };
    case "agent.delegate": return { verb: "Asked an agent to", subject: stringArg("task") || "help", kind: "tool" };
    default: return { verb: "Used", subject: tool.name, kind: "tool" };
  }
}

function ToolActivityIcon({ kind }: { kind: ToolActivityKind }) {
  const props = { size: 19, strokeWidth: 1.7, "aria-hidden": true as const };
  switch (kind) {
    case "read": return <BookOpen {...props} />;
    case "search": return <Search {...props} />;
    case "edit": return <FilePenLine {...props} />;
    case "web": return <Globe {...props} />;
    case "terminal": return <Terminal {...props} />;
    default: return <Wrench {...props} />;
  }
}

/** A tool call's wall time, as `840 ms` or `2.4 s`. */
function formatDuration(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}

/**
 * The tool menu, shared by the composer and the sidebar.
 *
 * Rendered on the opaque {@link Menu} primitive, sized to the longest built-in
 * tool name (`exec_shell_command` and its kind) so the name never collides with
 * its risk badge or its description, which wraps beneath rather than beside it.
 */
function ToolMenu({
  open,
  anchorRef,
  onClose,
  tools,
}: {
  open: boolean;
  anchorRef: React.RefObject<HTMLButtonElement | null>;
  onClose: () => void;
  tools: ToolInfo[] | null;
}) {
  return (
    <Menu open={open} anchorRef={anchorRef} onClose={onClose} minWidth={320} label="Runtime tools">
      <div className="menu__heading">Runtime tools{tools ? ` (${tools.length})` : ""}</div>
      {!tools ? (
        <div className="menu__empty">Loading tools…</div>
      ) : tools.length === 0 ? (
        <div className="menu__empty">No tools are enabled for this agent.</div>
      ) : (
        tools.map((tool) => (
          <div key={tool.name} className="menu__tool" role="menuitem">
            <div className="menu__tool-head">
              <code className="menu__tool-name">{tool.name}</code>
              <Pill tone={riskTone(tool.risk)}>{tool.risk}</Pill>
            </div>
            {tool.description && <span className="menu__tool-desc">{tool.description}</span>}
          </div>
        ))
      )}
    </Menu>
  );
}
