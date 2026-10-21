import { useEffect, useState } from "react";

import { agentApi } from "../api/agent";

/**
 * One decoded event from a run's SSE stream.
 *
 * `type` is the canonical name the API sent (`run.started`, `model.delta`,
 * `tool.output`, `run.completed`, …); `data` is its parsed JSON payload; `id`
 * is the event's position in the run, when the server numbers its events.
 */
export interface RunEvent {
  type: string;
  data: Record<string, unknown>;
  id?: number;
}

/**
 * How completely the browser received the run.
 *
 * - `live`: the run is still streaming.
 * - `complete`: every event arrived on the first connection.
 * - `recovered`: the stream dropped, and the missed events were replayed
 *   through the terminal event.
 * - `incomplete`: the run ended, but its remaining events could not be
 *   recovered; the saved session holds what the server recorded.
 */
export type Delivery = "live" | "complete" | "recovered" | "incomplete";

/** The named events the API emits, listened for individually as SSE requires. */
const EVENT_NAMES = [
  "run.started",
  "routing.selected",
  "classification.completed",
  "model.delta",
  "tool.requested",
  "tool.started",
  "tool.progress",
  "tool.output",
  "tool.failed",
  "approval.required",
  "turn.completed",
  "run.completed",
  "run.cancelled",
  "run.failed",
  "error",
];

const TERMINAL = new Set(["run.completed", "run.cancelled", "run.failed"]);
/** Reconnects made after the browser gives up on a stream, before the gap is reported. */
const MAX_RECOVERIES = 4;

/**
 * Stream a run's events over SSE. Returns the events so far, whether the run
 * has reached a terminal state, and how completely it was delivered. Passing a
 * new `runId` starts fresh; `null` is idle.
 *
 * The server numbers every event. Frames are accepted strictly in order, so a
 * reconnect (the browser's own, which sends `Last-Event-ID`, or ours, which
 * asks for `?after=`) neither repeats nor skips content. A terminal run status
 * does not prove the browser has every event: when the run endpoint reports
 * a terminal run the stream has not finished, the stream is reopened after the
 * last event received so the remainder, terminal event included, is replayed.
 * Only when that replay keeps failing is the run marked done with
 * `delivery: "incomplete"`.
 */
export function useRunEvents(runId: string | null): { events: RunEvent[]; done: boolean; delivery: Delivery } {
  const [events, setEvents] = useState<RunEvent[]>([]);
  const [done, setDone] = useState(false);
  const [delivery, setDelivery] = useState<Delivery>("live");
  const [observedId, setObservedId] = useState<string | null>(null);

  useEffect(() => {
    setObservedId(runId);
    setEvents([]);
    setDone(false);
    setDelivery("live");
    if (!runId) return;

    let source: EventSource | null = null;
    let closed = false;
    let faulted = false;
    let lastId = -1;
    let recoveries = 0;
    let statusTimer: number | undefined;
    let reconnectTimer: number | undefined;

    const stopTimers = () => {
      if (statusTimer !== undefined) window.clearInterval(statusTimer);
      if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
      statusTimer = undefined;
      reconnectTimer = undefined;
    };
    const finish = (how: Delivery) => {
      if (closed) return;
      closed = true;
      setDelivery(how);
      setDone(true);
      source?.close();
      stopTimers();
    };

    const accept = (name: string, message: Event) => {
      // A connection fault is dispatched as a plain `error` Event, the same
      // name as the run's own `error` frames: it is not run output.
      if (!(message instanceof MessageEvent)) return;
      const id = message.lastEventId === "" ? NaN : Number(message.lastEventId);
      if (Number.isInteger(id)) {
        // Already have it (a replay after a reconnect): skip it.
        if (id <= lastId) return;
        // A gap would mean a missed event: resume from the last one held.
        if (lastId >= 0 && id !== lastId + 1) {
          reconnect();
          return;
        }
        lastId = id;
      }
      let data: Record<string, unknown> = {};
      try {
        data = JSON.parse(message.data) as Record<string, unknown>;
      } catch {
        // A frame that will not parse costs only itself.
      }
      setEvents((current) => [...current, Number.isInteger(id) ? { type: name, data, id } : { type: name, data }]);
      if (TERMINAL.has(name)) finish(faulted ? "recovered" : "complete");
    };

    const connect = () => {
      source?.close();
      const next = new EventSource(agentApi.eventsUrl(runId, lastId));
      for (const name of EVENT_NAMES) {
        next.addEventListener(name, (message: Event) => {
          if (source === next) accept(name, message);
        });
      }
      // The healthy path is SSE-driven, so a terminal event closes the stream
      // and no polling runs. When the stream faults, probe the run once now and
      // keep a fallback poll until the stream recovers or the run is seen
      // terminal; ordered ids keep the poll from double-rendering anything.
      next.onerror = () => {
        if (closed || source !== next) return;
        faulted = true;
        void checkStatus();
        if (statusTimer === undefined) {
          statusTimer = window.setInterval(() => void checkStatus(), 2000);
        }
        // A CONNECTING source is retrying with Last-Event-ID on its own; a
        // CLOSED one has given up and needs a fresh stream.
        if (next.readyState === EventSource.CLOSED) scheduleReconnect();
      };
      source = next;
    };

    const reconnect = () => {
      if (closed) return;
      faulted = true;
      connect();
    };

    const scheduleReconnect = () => {
      if (closed || reconnectTimer !== undefined) return;
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = undefined;
        reconnect();
      }, Math.min(1000 * 2 ** recoveries, 8000));
    };

    const checkStatus = async () => {
      if (closed) return;
      try {
        const run = await agentApi.run(runId);
        if (closed || !TERMINAL.has(`run.${run.status}`)) return;
        // The run is over but its terminal event has not arrived: replay the
        // rest of the stream before calling delivery done.
        if (recoveries < MAX_RECOVERIES) {
          recoveries += 1;
          reconnect();
          return;
        }
        setEvents((current) => current.some((event) => TERMINAL.has(event.type))
          ? current
          : [...current, { type: `run.${run.status}`, data: { reason: run.stop_reason ?? null, recovered: false } }]);
        finish("incomplete");
      } catch {
        // Let the stream reconnect; a failed status probe is not completion.
      }
    };

    connect();
    return () => {
      closed = true;
      stopTimers();
      source?.close();
      source = null;
    };
  }, [runId]);

  // A new id can render before this effect has connected its EventSource.
  // Never expose the previous run's terminal state or events in that gap.
  return observedId === runId ? { events, done, delivery } : { events: [], done: false, delivery: "live" };
}
