import { useState } from "react";

import { agentApi, type ExtensionScope } from "../api/agent";

function errorText(cause: unknown): string {
  return (cause instanceof Error ? cause.message : String(cause)).replace(/^\d{3}:\s*/, "");
}

/**
 * Import skills from a repository or a folder. They are packaged as an
 * extension, installed inactive unless asked, and grant no tool
 * permission; nothing in the source is run.
 */
export function SkillImportForm({ onImported }: { onImported: () => void }) {
  const [source, setSource] = useState("");
  const [revision, setRevision] = useState("");
  const [name, setName] = useState("");
  const [scope, setScope] = useState<ExtensionScope>("global");
  const [activate, setActivate] = useState(false);
  const [replace, setReplace] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  async function submit() {
    if (!source.trim() || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const imported = await agentApi.importSkills({
        source: source.trim(),
        revision: revision.trim() || undefined,
        name: name.trim() || undefined,
        scope,
        activate,
        replace,
      });
      const from = imported.source.kind === "git" ? ` at commit ${imported.source.revision.slice(0, 12)}` : "";
      setMessage(
        `Imported ${imported.skills.length} skill${imported.skills.length === 1 ? "" : "s"} ` +
          `(${imported.skills.join(", ")}) as the extension ${imported.name}${from}` +
          `${imported.replaced ? ", replacing the earlier copy" : ""}. ` +
          (imported.active
            ? "They are available to new runs."
            : "They are inactive: enable the extension under Extensions to use them."),
      );
      setSource("");
      setRevision("");
      setName("");
      setActivate(false);
      setReplace(false);
      onImported();
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <fieldset data-testid="skill-import" style={{ border: 0, padding: 0, margin: "0 0 14px", minWidth: 0 }}>
      <legend className="field__label">Import skills</legend>
      {message && <div className="notice notice--info" role="status">{message}</div>}
      <div className="field">
        <label className="field__label" htmlFor="skill-import-source">Repository URL or folder</label>
        <input id="skill-import-source" className="input" value={source} spellCheck={false} disabled={busy}
          placeholder="https://github.com/owner/repo#folder or /path/to/skills" aria-describedby="skill-import-help"
          onChange={(event) => setSource(event.target.value)} />
        <p id="skill-import-help" className="muted" style={{ margin: "4px 0 0" }}>
          Supported: a SKILL.md, skills/&lt;name&gt;/SKILL.md, &lt;name&gt;/SKILL.md, or an extension. Nothing in it is
          run, and importing grants no tool permission.
        </p>
      </div>
      <div className="field">
        <label className="field__label" htmlFor="skill-import-revision">Branch, tag or commit (Git only, optional)</label>
        <input id="skill-import-revision" className="input" value={revision} spellCheck={false} disabled={busy}
          onChange={(event) => setRevision(event.target.value)} />
      </div>
      <div className="field">
        <label className="field__label" htmlFor="skill-import-name">Install as (optional)</label>
        <input id="skill-import-name" className="input" value={name} spellCheck={false} disabled={busy}
          onChange={(event) => setName(event.target.value)} />
      </div>
      <fieldset style={{ border: 0, padding: 0, margin: "10px 0 0", minWidth: 0 }}>
        <legend className="field__label">Import for</legend>
        <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input type="radio" name="skill-import-scope" checked={scope === "global"} disabled={busy}
            onChange={() => setScope("global")} />
          Every profile
        </label>
        <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <input type="radio" name="skill-import-scope" checked={scope === "profile"} disabled={busy}
            onChange={() => setScope("profile")} />
          This profile only
        </label>
      </fieldset>
      <label style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 10 }}>
        <input type="checkbox" checked={activate} disabled={busy} onChange={(event) => setActivate(event.target.checked)} />
        Make them available to runs now
      </label>
      <label style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 6 }}>
        <input type="checkbox" checked={replace} disabled={busy} onChange={(event) => setReplace(event.target.checked)} />
        Replace an installed copy with the same name (kept if the import fails)
      </label>
      {error && <div role="alert" style={{ fontSize: 11.5, color: "var(--danger)", marginTop: 6 }}>{error}</div>}
      <button type="button" className="btn" style={{ marginTop: 10 }} disabled={busy || !source.trim()}
        aria-busy={busy || undefined} onClick={() => void submit()}>
        Import
      </button>
    </fieldset>
  );
}
