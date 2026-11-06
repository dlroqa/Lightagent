import { useEffect, useState } from "react";

import { agentApi, type KnowledgeAdded, type KnowledgeHit } from "../api/agent";
import { Empty } from "../components/Bits";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

/**
 * The active profile's document index, through the operations
 * `lightagent rag` shares: index, search, list and clear.
 */
export function KnowledgeScreen() {
  const [listing, setListing] = useState<{ sources: { source: string; chunks: number }[]; total_chunks: number } | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [path, setPath] = useState("");
  const [source, setSource] = useState("");
  const [busy, setBusy] = useState(false);
  const [added, setAdded] = useState<KnowledgeAdded | null>(null);
  const [addError, setAddError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [topK, setTopK] = useState("");
  const [hits, setHits] = useState<KnowledgeHit[] | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  async function load() {
    try {
      setListing(await agentApi.knowledge());
      setLoadError(null);
    } catch (cause) {
      setLoadError(errorText(cause));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function index() {
    setBusy(true);
    setAddError(null);
    setAdded(null);
    try {
      setAdded(await agentApi.addKnowledge(path.trim(), source.trim() || undefined));
      void load();
    } catch (cause) {
      setAddError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  async function search() {
    setSearchError(null);
    try {
      setHits((await agentApi.searchKnowledge(query, Number(topK) > 0 ? Number(topK) : undefined)).hits);
    } catch (cause) {
      setSearchError(errorText(cause));
    }
  }

  return (
    <>
      <TopBar title="Knowledge" subtitle="Documents the agent can search for this profile" />
      <div className="page" data-testid="knowledge-screen">
        {message && <div className="notice notice--info" role="status">{message}</div>}

        <Card title="Add documents">
          <p className="card__note">
            Shared with the terminal CLI (lightagent rag). A file, or every file directly in a folder, is read and
            indexed; nothing in it is run.
          </p>
          <div className="field">
            <label className="field__label" htmlFor="knowledge-path">File or folder (absolute path)</label>
            <input id="knowledge-path" className="input" value={path} spellCheck={false} disabled={busy}
              placeholder="/path/to/documents" onChange={(event) => { setPath(event.target.value); setAddError(null); }} />
          </div>
          <div className="field">
            <label className="field__label" htmlFor="knowledge-source">Source name (optional)</label>
            <input id="knowledge-source" className="input" value={source} disabled={busy}
              onChange={(event) => setSource(event.target.value)} />
          </div>
          {addError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{addError}</div>}
          <button type="button" className="btn" style={{ marginTop: 10 }} disabled={busy || !path.trim()}
            aria-busy={busy || undefined} onClick={() => void index()}>Index</button>
          {added && (
            <div data-testid="knowledge-added" role="status" style={{ marginTop: 10 }}>
              <div>{added.total_chunks} chunk{added.total_chunks === 1 ? "" : "s"} added.</div>
              {added.indexed.length > 0 && (
                <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                  {added.indexed.map((item) => <li key={item.source}>Indexed {item.source} ({item.chunks} chunks)</li>)}
                </ul>
              )}
              {added.skipped.length > 0 && (
                <ul style={{ margin: "4px 0 0", paddingLeft: 18 }}>
                  {added.skipped.map((item) => <li key={item.path}>Skipped {item.path}: {item.reason}</li>)}
                </ul>
              )}
              {added.notes.map((note) => <div key={note} className="muted">{note}</div>)}
            </div>
          )}
        </Card>

        <Card title="Search">
          <div style={{ display: "flex", gap: 12, flexWrap: "wrap", alignItems: "flex-end" }}>
            <div className="field" style={{ flex: "1 1 240px" }}>
              <label className="field__label" htmlFor="knowledge-query">Search documents</label>
              <input id="knowledge-query" className="input" value={query} onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => { if (event.key === "Enter" && query.trim()) void search(); }} />
            </div>
            <div className="field">
              <label className="field__label" htmlFor="knowledge-top-k">Results (optional)</label>
              <input id="knowledge-top-k" className="input" type="number" min={1} value={topK} style={{ maxWidth: 110 }}
                onChange={(event) => setTopK(event.target.value)} />
            </div>
            <button type="button" className="btn" disabled={!query.trim()} onClick={() => void search()}>
              Search documents
            </button>
          </div>
          {searchError && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 4 }}>{searchError}</div>}
          {hits && (
            <div data-testid="knowledge-hits" style={{ marginTop: 10 }}>
              {hits.length === 0 ? <span className="muted">No relevant passages found.</span> : (
                <ol style={{ margin: 0, paddingLeft: 18, display: "grid", gap: 8 }}>
                  {hits.map((hit, rank) => (
                    <li key={`${hit.source}-${rank}`}>
                      <strong>{hit.source}</strong> <span className="muted">(score {hit.score.toFixed(2)})</span>
                      <div style={{ whiteSpace: "pre-wrap" }}>{hit.text}</div>
                    </li>
                  ))}
                </ol>
              )}
            </div>
          )}
        </Card>

        <Card title="Indexed sources">
          {loadError && (
            <div className="notice notice--warn" role="alert">
              The index is unavailable: {loadError}{" "}
              <button type="button" className="btn" onClick={() => void load()}>Retry</button>
            </div>
          )}
          {!listing && !loadError && <span className="muted">Loading…</span>}
          {listing && listing.sources.length === 0 && (
            <Empty title="Nothing indexed" hint="Add a file or a folder above." />
          )}
          {listing && listing.sources.length > 0 && (
            <>
              <ul data-testid="knowledge-sources" style={{ margin: 0, paddingLeft: 18 }}>
                {listing.sources.map((item) => <li key={item.source}>{item.source} ({item.chunks} chunks)</li>)}
              </ul>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 12 }}>
                {confirmClear ? (
                  <>
                    <button type="button" className="btn btn--danger"
                      onClick={() => void agentApi.clearKnowledge()
                        .then(() => { setConfirmClear(false); setMessage("The index was cleared."); void load(); })
                        .catch((cause) => setMessage(errorText(cause)))}>
                      {`Confirm: clear ${listing.total_chunks} chunks`}
                    </button>
                    <button type="button" className="btn" onClick={() => setConfirmClear(false)}>Keep the index</button>
                  </>
                ) : (
                  <button type="button" className="btn" onClick={() => setConfirmClear(true)}>Clear the index…</button>
                )}
              </div>
            </>
          )}
        </Card>
      </div>
    </>
  );
}
