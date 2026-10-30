import { useEffect, useRef, useState, type ReactNode } from "react";

import { invoke, isDesktop } from "../api/transport";
import "./desktop-gate.css";

/** The desktop host's view of its bundled runtime (`runtime_status`). */
interface RuntimeStatus {
  state: "starting" | "ready" | "failed" | "mismatch";
  detail: string | null;
  desktop_version: string;
  runtime_version: string | null;
  profile: string | null;
}

/**
 * In the desktop app, hold the UI until the bundled runtime is ready, and
 * explain plainly when it cannot start. In the browser build this renders its
 * children directly.
 */
export function DesktopGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [hostError, setHostError] = useState<string | null>(null);
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
        if (next.state === "starting") {
          timer = window.setTimeout(() => void poll(), 250);
        } else {
          void invoke("e2e_note", { note: next.state === "ready" ? "ui_ready" : "ui_error" }).catch(() => undefined);
        }
      } catch (cause) {
        if (!stopped) setHostError(cause instanceof Error ? cause.message : String(cause));
      }
    };
    void poll();
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, []);

  const failed = hostError !== null || status?.state === "failed" || status?.state === "mismatch";
  useEffect(() => {
    if (failed) heading.current?.focus();
  }, [failed]);

  if (!isDesktop || status?.state === "ready") return <>{children}</>;

  if (!failed) {
    return (
      <main className="desktop-gate" aria-busy="true">
        <div className="desktop-gate__panel" role="status" aria-live="polite">
          <span className="desktop-gate__pulse" aria-hidden="true" />
          <h1 className="desktop-gate__title">Starting Lightagent…</h1>
          <p className="desktop-gate__text">Starting the bundled runtime on this computer.</p>
        </div>
      </main>
    );
  }

  const mismatch = status?.state === "mismatch";
  return (
    <main className="desktop-gate">
      <div className="desktop-gate__panel desktop-gate__panel--error" role="alert">
        <h1 className="desktop-gate__title" tabIndex={-1} ref={heading}>
          {mismatch ? "The bundled runtime does not match this app" : "Lightagent could not start its runtime"}
        </h1>
        <p className="desktop-gate__text">
          {hostError ?? status?.detail ?? "The runtime stopped before it was ready."}
        </p>
        <p className="desktop-gate__text desktop-gate__text--muted">
          {mismatch
            ? "Reinstall Lightagent Desktop so the app and its runtime come from the same release. Nothing was run."
            : "Quit and reopen Lightagent. If it happens again, the runtime log in Settings → Diagnostics shows why."}
        </p>
        <dl className="desktop-gate__facts">
          <dt>App</dt>
          <dd>{status?.desktop_version ?? "unknown"}</dd>
          <dt>Runtime</dt>
          <dd>{status?.runtime_version ?? "not reported"}</dd>
        </dl>
      </div>
    </main>
  );
}
