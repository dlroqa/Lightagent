import { useCallback, useEffect, useId, useState, type FormEvent } from "react";

import { agentApi, type AgentProfileDetail, type ProfileCatalog } from "../api/agent";
import { invoke, isDesktop } from "../api/transport";
import { Card } from "../components/Card";
import { TopBar } from "../components/Shell";
import "./profiles.css";

/** What the desktop host answers when asked to switch profile. */
interface SwitchOutcome {
  status: "switched" | "blocked";
  runs: { id: string; status: string }[];
  persisted: boolean;
  profile: string;
}

const PROFILE_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;

function message(cause: unknown): string {
  const text = cause instanceof Error ? cause.message : String(cause);
  return text.replace(/^\d{3}: /, "");
}

/**
 * Profiles: the same operations as `lightagent profile …`, on the same store.
 * In the desktop app, switching restarts the bundled runtime on the chosen
 * profile (start the new one, then commit; docs/desktop/DESKTOP_ARCHITECTURE.md
 * §7). In the browser, it sets the active profile the CLI and later servers
 * use; the server already running keeps serving its own until it restarts.
 */
export function ProfilesScreen() {
  const [catalog, setCatalog] = useState<ProfileCatalog | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setCatalog(await agentApi.profiles());
      setLoadError(null);
    } catch (cause) {
      setLoadError(message(cause));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const active = catalog?.active_profile ?? null;
  return (
    <>
      <TopBar
        title="Profiles"
        subtitle={`Persona, model and routing, shared with the lightagent CLI.${active ? ` Serving: ${active}.` : ""}`}
      />
      <div className="page profiles-page">
        {notice && (
          <div className="notice notice--info" role="status">
            {notice}
          </div>
        )}
        <Card title="Saved profiles">
          {loadError && (
            <p className="form-error" role="alert">
              Profiles could not be loaded: {loadError}
            </p>
          )}
          {!catalog && !loadError && <p className="muted">Loading profiles…</p>}
          {catalog && (
            <ul className="profile-list" aria-label="Saved profiles">
              {catalog.profiles.map((profile) => (
                <ProfileRow
                  key={profile.id}
                  id={profile.id}
                  name={profile.name}
                  model={profile.model}
                  active={profile.active}
                  onSelect={() => setSelected(profile.id)}
                  onChanged={(text, deleted) => {
                    setNotice(text);
                    // A deleted profile has no details left to show.
                    if (deleted !== undefined) setSelected((current) => (current === deleted ? null : current));
                    void load();
                  }}
                />
              ))}
            </ul>
          )}
        </Card>
        {selected && <ProfileDetail id={selected} onChanged={() => void load()} />}
        <CreateProfile
          onCreated={(id) => {
            setNotice(`Created profile '${id}'.`);
            setSelected(id);
            void load();
          }}
        />
      </div>
    </>
  );
}

function ProfileRow(props: {
  id: string;
  name: string;
  model: string;
  active: boolean;
  onSelect: () => void;
  onChanged: (notice: string, deleted?: string) => void;
}) {
  const { id, name, model, active } = props;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [blocked, setBlocked] = useState<SwitchOutcome["runs"] | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [typed, setTyped] = useState("");
  const confirmId = useId();

  const use = async (cancelRuns: boolean) => {
    setBusy(true);
    setError(null);
    try {
      if (isDesktop) {
        const outcome = await invoke<SwitchOutcome>("profile_switch", { id, cancelRuns });
        if (outcome.status === "blocked") {
          setBlocked(outcome.runs);
          return;
        }
        setBlocked(null);
        // Everything on screen came from the previous runtime: start fresh.
        window.location.reload();
        return;
      }
      await agentApi.useProfile(id);
      props.onChanged(
        `'${id}' is now the active profile for the CLI and for servers started from now on. This server keeps serving its current profile until it restarts.`,
      );
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      await agentApi.deleteProfile(id);
      props.onChanged(`Deleted profile '${id}' and its local data.`, id);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="profile-row">
      <div>
        <strong>{name}</strong>
        <span>
          {id} · {model}
        </span>
      </div>
      <div className="profile-row__actions">
        {active ? (
          <span className="status-chip">Active</span>
        ) : (
          <button type="button" className="btn" disabled={busy} onClick={() => void use(false)}>
            Use
          </button>
        )}
        <button type="button" className="btn btn--ghost" onClick={props.onSelect}>
          Details
        </button>
        {id !== "default" && !active && (
          <button
            type="button"
            className="btn btn--danger"
            aria-expanded={confirming}
            onClick={() => setConfirming((value) => !value)}
          >
            Delete…
          </button>
        )}
      </div>
      {blocked && (
        <div className="profile-row__confirm" role="alertdialog" aria-label={`Switch to ${id}`}>
          <p>
            {blocked.length === 1 ? "A run is" : `${blocked.length} runs are`} still working in this profile. Switching stops
            them; a pending approval is recorded as rejected.
          </p>
          <div className="profile-row__actions">
            <button type="button" className="btn btn--danger" disabled={busy} onClick={() => void use(true)}>
              Stop the runs and switch
            </button>
            <button type="button" className="btn" onClick={() => setBlocked(null)}>
              Keep working
            </button>
          </div>
        </div>
      )}
      {confirming && (
        <div className="profile-row__confirm">
          <label htmlFor={confirmId}>
            Deleting removes the profile and all of its sessions, memory and settings. Type <code>{id}</code> to confirm.
          </label>
          <input
            id={confirmId}
            className="input"
            value={typed}
            autoComplete="off"
            onChange={(event) => setTyped(event.target.value)}
          />
          <div className="profile-row__actions">
            <button type="button" className="btn btn--danger" disabled={busy || typed !== id} onClick={() => void remove()}>
              Delete {id}
            </button>
            <button type="button" className="btn" onClick={() => setConfirming(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {error && (
        <p className="form-error" role="alert">
          {error}
        </p>
      )}
    </li>
  );
}

function ProfileDetail({ id, onChanged }: { id: string; onChanged: () => void }) {
  const [detail, setDetail] = useState<AgentProfileDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preset, setPreset] = useState("research@1");
  const [busy, setBusy] = useState(false);
  const [applied, setApplied] = useState<string | null>(null);
  const presetId = useId();

  useEffect(() => {
    setDetail(null);
    setError(null);
    setApplied(null);
    agentApi
      .profile(id)
      .then(setDetail)
      .catch((cause: unknown) => setError(message(cause)));
  }, [id]);

  const apply = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await agentApi.presetProfile(id, preset.trim());
      setApplied(`Applied ${preset.trim()} to '${id}'.`);
      setDetail(await agentApi.profile(id));
      onChanged();
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card title={`Profile: ${id}`}>
      {!detail && !error && <p className="muted">Loading…</p>}
      {detail && (
        <dl className="profile-detail">
          <dt>Name</dt>
          <dd>{detail.name}</dd>
          <dt>Model</dt>
          <dd>{detail.routing?.model ?? "default"}</dd>
          <dt>Persona</dt>
          <dd className="profile-detail__persona">{detail.persona}</dd>
        </dl>
      )}
      <form className="profile-form" onSubmit={(event) => void apply(event)}>
        <label htmlFor={presetId}>Capability preset (operator-owned)</label>
        <div className="profile-form__row">
          <input
            id={presetId}
            className="input"
            list={`${presetId}-known`}
            value={preset}
            onChange={(event) => setPreset(event.target.value)}
          />
          <datalist id={`${presetId}-known`}>
            <option value="research@1" />
          </datalist>
          <button type="submit" className="btn" disabled={busy || !preset.trim()}>
            Apply preset
          </button>
        </div>
      </form>
      {applied && <p role="status">{applied}</p>}
      {error && (
        <p className="form-error" role="alert">
          {error}
        </p>
      )}
    </Card>
  );
}

function CreateProfile({ onCreated }: { onCreated: (id: string) => void }) {
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [model, setModel] = useState("");
  const [persona, setPersona] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ids = { id: useId(), name: useId(), model: useId(), persona: useId(), hint: useId() };
  const idValid = PROFILE_ID.test(id);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!idValid) {
      setError("Use lowercase letters, digits, '-' and '_', starting with a letter or digit.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await agentApi.createProfile({
        id,
        name: name.trim() || undefined,
        model: model.trim() || undefined,
        persona: persona.trim() || undefined,
      });
      onCreated(id);
      setId("");
      setName("");
      setModel("");
      setPersona("");
    } catch (cause) {
      setError(message(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card title="Create a profile">
      <form className="profile-form" onSubmit={(event) => void submit(event)} noValidate>
        <label htmlFor={ids.id}>
          Profile id <span aria-hidden="true">*</span>
        </label>
        <input
          id={ids.id}
          className="input"
          required
          aria-required="true"
          aria-invalid={id !== "" && !idValid}
          aria-describedby={ids.hint}
          autoComplete="off"
          value={id}
          onChange={(event) => setId(event.target.value)}
        />
        <p id={ids.hint} className="card__note">
          Lowercase letters, digits, '-' and '_'. It names the profile's folder and cannot be changed later.
        </p>
        <label htmlFor={ids.name}>Display name</label>
        <input id={ids.name} className="input" value={name} onChange={(event) => setName(event.target.value)} />
        <label htmlFor={ids.model}>Model</label>
        <input
          id={ids.model}
          className="input"
          placeholder="default"
          value={model}
          onChange={(event) => setModel(event.target.value)}
        />
        <label htmlFor={ids.persona}>Persona</label>
        <textarea
          id={ids.persona}
          className="input"
          rows={3}
          placeholder="You are a helpful local agent."
          value={persona}
          onChange={(event) => setPersona(event.target.value)}
        />
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <div className="profile-form__row">
          <button type="submit" className="btn btn--primary" disabled={busy}>
            {busy ? "Creating…" : "Create profile"}
          </button>
        </div>
      </form>
    </Card>
  );
}
