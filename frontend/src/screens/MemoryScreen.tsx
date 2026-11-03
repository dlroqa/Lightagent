import { useEffect, useState } from "react";

import { agentApi, type MemoryItem, type SessionSummary } from "../api/agent";
import { Empty, Pill } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

function tagList(text: string): string[] {
  return text.split(",").map((tag) => tag.trim()).filter(Boolean);
}

const KINDS = ["fact", "preference", "decision", "project"];

function when(seconds?: number | null): string {
  return seconds ? new Date(seconds * 1000).toLocaleString() : "";
}

/** One memory: its text, kind, tags and source, with Edit and Forget. */
function MemoryRow({ memory, onChanged }: { memory: MemoryItem; onChanged: (message: string) => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(memory.text);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function act(action: () => Promise<string>) {
    setBusy(true);
    setError(null);
    try {
      onChanged(await action());
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <li data-testid={`memory-${memory.id}`} style={{ minWidth: 0, display: "grid", gap: 6 }}>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        <Pill tone="info">{memory.kind}</Pill>
        {(memory.tags ?? []).map((tag) => <Pill key={tag} tone="neutral">{tag}</Pill>)}
        <span className="muted" style={{ fontSize: 12.5 }}>
          {memory.source ? `From conversation ${memory.source.session_id}, message ${memory.source.message_index} · ` : ""}
          {when(memory.updated_at ?? memory.created_at)}
        </span>
      </div>
      {editing ? (
        <div className="field">
          <label className="field__label" htmlFor={`memory-edit-${memory.id}`}>Text</label>
          <textarea id={`memory-edit-${memory.id}`} className="input" rows={2} value={text} disabled={busy}
            onChange={(event) => setText(event.target.value)} />
        </div>
      ) : (
        <div style={{ overflowWrap: "anywhere" }}>{memory.text}</div>
      )}
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {editing ? (
          <>
            <button type="button" className="btn" disabled={busy || !text.trim()}
              onClick={() => void act(async () => {
                const result = await agentApi.updateMemory(memory.id, text);
                setEditing(false);
                return result.updated ? "Saved." : "That memory no longer exists.";
              })}>Save</button>
            <button type="button" className="btn" disabled={busy} onClick={() => { setEditing(false); setText(memory.text); }}>
              Cancel
            </button>
          </>
        ) : confirm ? (
          <>
            <button type="button" className="btn btn--danger" disabled={busy}
              onClick={() => void act(async () => {
                await agentApi.forgetMemory(memory.id);
                return "Forgotten.";
              })}>Confirm: forget it</button>
            <button type="button" className="btn" disabled={busy} onClick={() => setConfirm(false)}>Keep it</button>
          </>
        ) : (
          <>
            <button type="button" className="btn" aria-label={`Edit: ${memory.text}`} onClick={() => setEditing(true)}>Edit</button>
            <button type="button" className="btn" aria-label={`Forget: ${memory.text}`} onClick={() => setConfirm(true)}>Forget</button>
          </>
        )}
      </div>
      {error && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)" }}>{error}</div>}
    </li>
  );
}

/**
 * The active profile's memory, through the operations `lightagent memory`
 * shares: remember, search, edit, forget, the knowledge page, and facts
 * from a saved conversation.
 */
export function MemoryScreen() {
  const [memories, setMemories] = useState<MemoryItem[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [kind, setKind] = useState("fact");
  const [tags, setTags] = useState("");
  const [addError, setAddError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [topK, setTopK] = useState("");
  const [hits, setHits] = useState<MemoryItem[] | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [topic, setTopic] = useState("");
  const [knowledge, setKnowledge] = useState<string | null>(null);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [session, setSession] = useState("");
  const [candidates, setCandidates] = useState<{ message: number; text: string }[] | null>(null);
  const [messageNumber, setMessageNumber] = useState("");
  const [promoteText, setPromoteText] = useState("");
  const [promoteKind, setPromoteKind] = useState("fact");
  const [promoteTags, setPromoteTags] = useState("");
  const [promoteError, setPromoteError] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);

  async function load() {
    try {
      setMemories((await agentApi.memories()).memories);
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
    agentApi.sessions().then((result) => setSessions(result.sessions)).catch(() => setSessions([]));
  }, []);

  function changed(done: string) {
    setMessage(done);
    void load();
  }

  async function remember() {
    setBusy(true);
    setAddError(null);
    try {
      const result = await agentApi.addMemory(text, kind.trim() || undefined, tagList(tags));
      setText("");
      setTags("");
      changed(`Remembered (${result.id}).`);
    } catch (cause) {
      setAddError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  async function search() {
    setSearchError(null);
    try {
      setHits((await agentApi.searchMemories(query, Number(topK) > 0 ? Number(topK) : undefined)).memories);
    } catch (cause) {
      setSearchError(errorText(cause));
    }
  }

  async function promote(number: number) {
    setPromoteError(null);
    try {
      const result = await agentApi.promoteMemory({
        session,
        message: number,
        text: promoteText.trim() || undefined,
        kind: promoteKind.trim() || undefined,
        tags: tagList(promoteTags),
      });
      setPromoteText("");
      changed(`Remembered message ${number} (${result.id}).`);
    } catch (cause) {
      setPromoteError(errorText(cause));
    }
  }

  return (
    <>
      <TopBar title="Memory" subtitle="What the agent remembers for this profile" />
      <div className="page" data-testid="memory-screen">
        {message && <div className="notice notice--info" role="status">{message}</div>}

        <Card title="Remember something">
          <p className="card__note">Shared with the terminal CLI (lightagent memory). Runs recall these facts when relevant.</p>
          <div className="field">
            <label className="field__label" htmlFor="memory-text">Fact</label>
            <textarea id="memory-text" className="input" rows={2} value={text} disabled={busy}
              onChange={(event) => { setText(event.target.value); setAddError(null); }} />
          </div>
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
            <div className="field">
              <label className="field__label" htmlFor="memory-kind">Kind</label>
              <input id="memory-kind" className="input" list="memory-kinds" value={kind} disabled={busy}
                onChange={(event) => setKind(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="memory-tags">Tags (comma-separated)</label>
              <input id="memory-tags" className="input" value={tags} disabled={busy}
                onChange={(event) => setTags(event.target.value)} />
            </div>
          </div>
          <datalist id="memory-kinds">{KINDS.map((value) => <option key={value} value={value} />)}</datalist>
          {addError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{addError}</div>}
          <button type="button" className="btn" style={{ marginTop: 10 }} disabled={busy || !text.trim()}
            onClick={() => void remember()}>Remember</button>
        </Card>

        <Card title="Search">
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap", alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 240px" }}>
              <label className="field__label" htmlFor="memory-query">Search memories</label>
              <input id="memory-query" className="input" value={query} onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => { if (event.key === "Enter" && query.trim()) void search(); }} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="memory-top-k">Results (optional)</label>
              <input id="memory-top-k" className="input" type="number" min={1} value={topK} style={{ maxWidth: 110 }}
                onChange={(event) => setTopK(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={!query.trim()} onClick={() => void search()}>Search</button>
          </div>
          {searchError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{searchError}</div>}
          {hits && (
            <div data-testid="memory-hits" style={{ marginTop: 10 }}>
              {hits.length === 0 ? <span className="muted">No relevant memories.</span> : (
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  {hits.map((hit) => <li key={hit.id}>({hit.kind}) {hit.text}</li>)}
                </ul>
              )}
            </div>
          )}
        </Card>

        <Card title="Memories">
          {loadError && (
            <div className="notice notice--warn" role="alert">
              Memory is unavailable: {loadError}{" "}
              <button type="button" className="btn" onClick={() => void load()}>Retry</button>
            </div>
          )}
          {!memories && !loadError && <span className="muted">Loading memories…</span>}
          {memories && memories.length === 0 && (
            <Empty title="No memories" hint="Remember a fact above, or keep one from a conversation below." />
          )}
          {memories && memories.length > 0 && (
            <>
              <ul style={{ listStyle: "none", padding: 0, margin: 0, display: "grid", gap: 14 }}>
                {memories.map((memory) => <MemoryRow key={`${memory.id}-${memory.updated_at ?? 0}`} memory={memory} onChanged={changed} />)}
              </ul>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 14 }}>
                {confirmClear ? (
                  <>
                    <button type="button" className="btn btn--danger"
                      onClick={() => void agentApi.clearMemories().then(() => { setConfirmClear(false); changed("Every memory was forgotten."); })
                        .catch((cause) => setMessage(errorText(cause)))}>
                      {`Confirm: forget all ${memories.length}`}
                    </button>
                    <button type="button" className="btn" onClick={() => setConfirmClear(false)}>Keep my memories</button>
                  </>
                ) : (
                  <button type="button" className="btn" onClick={() => setConfirmClear(true)}>Forget everything…</button>
                )}
              </div>
            </>
          )}
        </Card>

        <Card title="Working knowledge">
          <p className="card__note">What the memories add up to, grouped by kind.</p>
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap", alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 240px" }}>
              <label className="field__label" htmlFor="memory-topic">Topic (optional)</label>
              <input id="memory-topic" className="input" value={topic} onChange={(event) => setTopic(event.target.value)} />
            </div>
            <button type="button" className="btn"
              onClick={() => void agentApi.reflectMemories(topic.trim() || undefined)
                .then((result) => setKnowledge(result.knowledge))
                .catch((cause) => setKnowledge(errorText(cause)))}>
              Show knowledge
            </button>
          </div>
          {knowledge !== null && (
            <pre data-testid="memory-knowledge" style={{ whiteSpace: "pre-wrap", marginTop: 10 }}>{knowledge || "Nothing yet."}</pre>
          )}
        </Card>

        <Card title="From a conversation">
          <p className="card__note">Keep a statement from a saved conversation, with where it came from.</p>
          <div className="field">
            <label className="field__label" htmlFor="memory-session">Conversation</label>
            <select id="memory-session" className="input" value={session}
              onChange={(event) => { setSession(event.target.value); setCandidates(null); }}>
              <option value="">Choose a conversation</option>
              {sessions.map((item) => <option key={item.id} value={item.id}>{item.title || item.id}</option>)}
            </select>
          </div>
          <button type="button" className="btn" style={{ marginTop: 8 }} disabled={!session}
            onClick={() => void agentApi.memoryCandidates(session)
              .then((result) => setCandidates(result.candidates))
              .catch((cause) => setPromoteError(errorText(cause)))}>
            Suggest facts
          </button>
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap", marginTop: 10 }}>
            <div className="field" style={{ flex: "1 1 240px" }}>
              <label className="field__label" htmlFor="memory-promote-text">Edited text (optional)</label>
              <input id="memory-promote-text" className="input" value={promoteText}
                onChange={(event) => setPromoteText(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="memory-promote-kind">Kind</label>
              <input id="memory-promote-kind" className="input" list="memory-kinds" value={promoteKind}
                onChange={(event) => setPromoteKind(event.target.value)} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="memory-promote-tags">Tags</label>
              <input id="memory-promote-tags" className="input" value={promoteTags}
                onChange={(event) => setPromoteTags(event.target.value)} />
            </div>
          </div>
          {candidates && (
            <div data-testid="memory-candidates" style={{ marginTop: 10 }}>
              {candidates.length === 0 ? (
                <span className="muted">No suggested facts. Any user message can still be kept by its number.</span>
              ) : (
                <ul style={{ listStyle: "none", padding: 0, margin: 0, display: "grid", gap: 8 }}>
                  {candidates.map((candidate) => (
                    <li key={candidate.message} style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                      <span style={{ flex: "1 1 240px", overflowWrap: "anywhere" }}>
                        <strong>{candidate.message}.</strong> {candidate.text}
                      </span>
                      <button type="button" className="btn" onClick={() => void promote(candidate.message)}>
                        {`Remember message ${candidate.message}`}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap", alignItems: "flex-end", marginTop: 10 }}>
            <div className="field">
              <label className="field__label" htmlFor="memory-message">Message number</label>
              <input id="memory-message" className="input" type="number" min={1} value={messageNumber} style={{ maxWidth: 120 }}
                onChange={(event) => setMessageNumber(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={!session || !(Number(messageNumber) > 0)}
              onClick={() => void promote(Number(messageNumber))}>Remember that message</button>
          </div>
          {promoteError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{promoteError}</div>}
        </Card>
      </div>
    </>
  );
}
