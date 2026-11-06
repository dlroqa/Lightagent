/**
 * How the UI reaches the Lightagent API.
 *
 * The browser build is served by `lightagent serve` itself, so it uses
 * same-origin `fetch` and `EventSource` unchanged. The desktop build
 * (`vite build --mode desktop`) has no network access at all: each request
 * goes to the desktop host over IPC, which checks it against the operation
 * contract (docs/desktop/contract/operations.json) and forwards it to the
 * runtime it owns, with a credential this code never sees.
 *
 * Both paths keep the same semantics, so `agent.ts` and `useRunEvents.ts`
 * (ordered event ids, gap detection, `?after=` replay) behave identically.
 */

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export const isDesktop = import.meta.env.MODE === "desktop";

/** Call a desktop host command. Rejects outside the desktop app. */
export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const tauri = (window as unknown as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__;
  const call = tauri?.core?.invoke;
  if (!call) return Promise.reject(new Error("The desktop host is not available."));
  return call<T>(command, args);
}

interface HostResponse {
  status: number;
  content_type: string;
  body: string;
}

async function blobToBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let start = 0; start < bytes.length; start += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(start, start + 0x8000));
  }
  return btoa(binary);
}

function requestTarget(input: RequestInfo | URL): string {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  if (/^[a-z]+:/i.test(url)) {
    const parsed = new URL(url);
    return parsed.pathname + parsed.search;
  }
  return url;
}

async function desktopFetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  let body: string | null = null;
  let bodyBase64: string | null = null;
  if (init.body instanceof Blob) {
    bodyBase64 = await blobToBase64(init.body);
  } else if (typeof init.body === "string") {
    body = init.body;
  }
  const result = await invoke<HostResponse>("api_fetch", {
    method: (init.method ?? "GET").toUpperCase(),
    target: requestTarget(input),
    body,
    bodyBase64,
    filename: headers.get("X-Lightagent-Filename"),
  });
  const empty = result.status === 204 || result.status === 304;
  return new Response(empty ? null : result.body, {
    status: result.status,
    headers: { "content-type": result.content_type },
  });
}

interface HostEvent {
  id: string | null;
  event: string;
  data: string;
}

interface HostPoll {
  events: HostEvent[];
  closed: boolean;
  error: string | null;
}

/**
 * The `EventSource` surface `useRunEvents` relies on, fed by the host's relay.
 * Like a browser source that has given up, it reports `CLOSED` and `onerror`
 * when the stream ends without `close()`, so the hook resumes with `?after=`.
 */
class DesktopEventSource extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readyState = DesktopEventSource.CONNECTING;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private subscription: number | null = null;
  private stopped = false;

  constructor(readonly url: string) {
    super();
    void this.relay();
  }

  close(): void {
    this.stopped = true;
    this.readyState = DesktopEventSource.CLOSED;
    if (this.subscription !== null) {
      void invoke("api_unsubscribe", { subscription: this.subscription }).catch(() => undefined);
    }
  }

  private async relay(): Promise<void> {
    try {
      this.subscription = await invoke<number>("api_subscribe", { target: this.url });
      if (this.stopped) {
        this.close();
        return;
      }
      this.readyState = DesktopEventSource.OPEN;
      this.onopen?.(new Event("open"));
      while (!this.stopped) {
        const batch = await invoke<HostPoll>("api_events_poll", { subscription: this.subscription, waitMs: 20000 });
        for (const event of batch.events) {
          if (this.stopped) return;
          this.dispatchEvent(new MessageEvent(event.event, { data: event.data, lastEventId: event.id ?? "" }));
        }
        if (batch.closed) break;
      }
    } catch {
      // Reported through onerror below, exactly as a failed browser stream is.
    }
    if (!this.stopped) {
      this.readyState = DesktopEventSource.CLOSED;
      this.onerror?.(new Event("error"));
    }
  }
}

/** `fetch` for the Lightagent API in this build. */
export const apiFetch: typeof fetch = isDesktop
  ? (desktopFetch as typeof fetch)
  : (input, init) => fetch(input, init);

/** `EventSource` for the Lightagent API in this build. */
export const ApiEventSource: typeof EventSource = isDesktop
  ? (DesktopEventSource as unknown as typeof EventSource)
  : EventSource;
