import { useCallback, useEffect, useRef, useState, type ReactNode, type RefObject } from "react";

import { invoke, isDesktop } from "../api/transport";
import "./desktop-gate.css";

/** The desktop host's view of its bundled runtime (`runtime_status`). */
interface RuntimeStatus {
  state: "starting" | "ready" | "failed" | "mismatch" | "duplicate";
  detail: string | null;
  desktop_version: string;
  runtime_version: string | null;
  profile: string | null;
  /** The window was asked to close while runs were working. */
  close_requested?: boolean;
}

/** How often the gate re-checks the runtime once the app is running. */
const WATCH_MS = 1000;

/**
 * In the desktop app: hold the UI until the bundled runtime is ready, keep
 * watching it afterwards, and explain plainly — with a way out — when it
 * cannot start, stops, does not match, or another window already owns it.
 * In the browser build this renders its children directly.
 */
export function DesktopGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [hostError, setHostError] = useState<string | null>(null);
  const [everReady, setEverReady] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [logs, setLogs] = useState<string[] | null>(null);
  const [generation, setGeneration] = useState(0);
  const heading = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    if (!isDesktop) return;
    let stopped = false;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const next = await invoke<RuntimeStatus>("runtime_status");
        if (stopped) return;
        setStatus(next);
        setHostError(null);
        if (next.state === "ready") {
          setEverReady(true);
          setRestarting(false);
          void invoke("e2e_note", { note: "ui_ready" }).catch(() => undefined);
        } else if (next.state !== "starting") {
          setRestarting(false);
          void invoke("e2e_note", { note: "ui_error" }).catch(() => undefined);
        }
        timer = window.setTimeout(() => void poll(), next.state === "starting" ? 250 : WATCH_MS);
      } catch (cause) {
        if (!stopped) setHostError(cause instanceof Error ? cause.message : String(cause));
      }
    };
    void poll();
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [generation]);

  const failed =
    hostError !== null ||
    status?.state === "failed" ||
    status?.state === "mismatch" ||
    status?.state === "duplicate";

  useEffect(() => {
    if (failed) heading.current?.focus();
  }, [failed]);

  const restart = useCallback(async () => {
    setRestarting(true);
    setLogs(null);
    try {
      await invoke("runtime_restart");
    } catch (cause) {
      setHostError(cause instanceof Error ? cause.message : String(cause));
      setRestarting(false);
    }
    setGeneration((value) => value + 1);
  }, []);

  const showLogs = useCallback(async () => {
    try {
      setLogs(await invoke<string[]>("runtime_logs", { limit: 200 }));
    } catch {
      setLogs([]);
    }
  }, []);

  if (!isDesktop) return <>{children}</>;
  const ready = status?.state === "ready" && hostError === null;
  if (ready && status?.close_requested) {
    return (
      <>
        <div aria-hidden="true" inert>
          {children}
        </div>
        <div className="desktop-gate desktop-gate--overlay" role="presentation">
          <CloseWithRuns onKeep={() => setGeneration((value) => value + 1)} />
        </div>
      </>
    );
  }
  if (ready) return <>{children}</>;

  const panel = failed && !restarting ? (
    <FailurePanel
      status={status}
      hostError={hostError}
      heading={heading}
      logs={logs}
      onRestart={() => void restart()}
      onShowLogs={() => void showLogs()}
    />
  ) : (
    <div className="desktop-gate__panel" role="status" aria-live="polite">
      <span className="desktop-gate__pulse" aria-hidden="true" />
      <h1 className="desktop-gate__title">
        {everReady || restarting ? "Restarting Lightagent's runtime…" : "Starting Lightagent…"}
      </h1>
      <p className="desktop-gate__text">Starting the bundled runtime on this computer.</p>
    </div>
  );

  // After the app has been running, keep it mounted under the notice so
  // nothing on screen is lost while the runtime recovers.
  if (everReady) {
    return (
      <>
        <div aria-hidden="true" inert>
          {children}
        </div>
        <div className="desktop-gate desktop-gate--overlay" role="presentation">
          {panel}
        </div>
      </>
    );
  }
  return <main className="desktop-gate">{panel}</main>;
}

function FailurePanel(props: {
  status: RuntimeStatus | null;
  hostError: string | null;
  heading: RefObject<HTMLHeadingElement | null>;
  logs: string[] | null;
  onRestart: () => void;
  onShowLogs: () => void;
}) {
  const { status, hostError, heading, logs } = props;
  const state = status?.state;
  const title =
    state === "mismatch"
      ? "The bundled runtime does not match this app"
      : state === "duplicate"
        ? "Lightagent is already open"
        : "Lightagent's runtime is not running";
  const remedy =
    state === "mismatch"
      ? "Reinstall Lightagent Desktop so the app and its runtime come from the same release. Nothing was run."
      : state === "duplicate"
        ? "Switch to the window that is already open. Close this one; it has not started anything."
        : "Restart the runtime. If it stops again, the runtime log shows why.";
  return (
    <div className="desktop-gate__panel desktop-gate__panel--error" role="alert">
      <h1 className="desktop-gate__title" tabIndex={-1} ref={heading}>
        {title}
      </h1>
      <p className="desktop-gate__text">{hostError ?? status?.detail ?? "The runtime stopped before it was ready."}</p>
      <p className="desktop-gate__text desktop-gate__text--muted">{remedy}</p>
      <dl className="desktop-gate__facts">
        <dt>App</dt>
        <dd>{status?.desktop_version ?? "unknown"}</dd>
        <dt>Runtime</dt>
        <dd>{status?.runtime_version ?? "not reported"}</dd>
      </dl>
      {state !== "duplicate" && (
        <div className="desktop-gate__actions">
          <button type="button" className="btn btn--primary" onClick={props.onRestart}>
            Restart runtime
          </button>
          <button type="button" className="btn" onClick={props.onShowLogs} aria-expanded={logs !== null}>
            Runtime log
          </button>
        </div>
      )}
      {logs !== null && (
        <pre className="desktop-gate__log" tabIndex={0} aria-label="Runtime log, most recent lines last">
          {logs.length ? logs.join("\n") : "The runtime wrote nothing before it stopped."}
        </pre>
      )}
    </div>
  );
}

/** Asked to close while runs are working: stop them and quit, or keep working. */
function CloseWithRuns({ onKeep }: { onKeep: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const keep = useRef<HTMLButtonElement>(null);
  useEffect(() => keep.current?.focus(), []);
  return (
    <div className="desktop-gate__panel" role="alertdialog" aria-modal="true" aria-labelledby="close-with-runs-title">
      <h1 className="desktop-gate__title" id="close-with-runs-title">
        Runs are still working
      </h1>
      <p className="desktop-gate__text">
        Quitting stops them. A run waiting for your approval is recorded as rejected.
      </p>
      {error && (
        <p className="desktop-gate__text" role="alert">
          {error}
        </p>
      )}
      <div className="desktop-gate__actions">
        <button
          type="button"
          ref={keep}
          className="btn btn--primary"
          onClick={() => {
            void invoke("app_close_cancel").finally(onKeep);
          }}
        >
          Keep working
        </button>
        <button
          type="button"
          className="btn btn--danger"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setError(null);
            invoke("app_quit", { cancelRuns: true }).catch((cause: unknown) => {
              setError(cause instanceof Error ? cause.message : String(cause));
              setBusy(false);
            });
          }}
        >
          {busy ? "Stopping runs…" : "Stop runs and quit"}
        </button>
      </div>
    </div>
  );
}
