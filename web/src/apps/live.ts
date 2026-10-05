// One shared connection to GET /api/v1/events for the whole app, with the
// deployment log lines (level `log`) included. The shell (useLiveSync) and
// pages subscribe; the connection opens with the first subscriber and closes
// shortly after the last one leaves.
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import { backoff, eventsUrl, type StreamState } from "@/api/events";
import { createInvalidator } from "@/lib/freshness";

export interface LiveEvent {
  seq: number;
  at: number;
  level: "info" | "warn" | "error" | "log";
  /** `org/stack`, or `stack` in the default org. */
  stack: string;
  service?: string;
  instance?: string;
  message: string;
}

type Listener = (e: LiveEvent) => void;

const LEVELS = ["info", "warn", "log"] as const;

export { splitStack } from "@/lib/freshness";

export class Hub {
  private listeners = new Set<Listener>();
  private stateListeners = new Set<(s: StreamState) => void>();
  private reopenListeners = new Set<() => void>();
  private opened = false;
  private es: EventSource | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private closeTimer: ReturnType<typeof setTimeout> | undefined;
  private attempt = 0;
  private since = 0;
  state: StreamState = "connecting";

  subscribe(l: Listener): () => void {
    this.listeners.add(l);
    clearTimeout(this.closeTimer);
    if (!this.es && this.timer === undefined) this.connect();
    return () => {
      this.listeners.delete(l);
      if (this.listeners.size === 0) this.closeTimer = setTimeout(() => this.close(), 5000);
    };
  }

  onState(f: (s: StreamState) => void): () => void {
    this.stateListeners.add(f);
    return () => this.stateListeners.delete(f);
  }

  /** Called when a connection opens after an earlier one: events may have been missed in between. */
  onReopen(f: () => void): () => void {
    this.reopenListeners.add(f);
    return () => this.reopenListeners.delete(f);
  }

  private setState(s: StreamState) {
    this.state = s;
    this.stateListeners.forEach((f) => f(s));
  }

  private deliver = (m: MessageEvent) => {
    try {
      const e = JSON.parse(m.data) as LiveEvent;
      // The server sends only events after the cursor, ascending. Not a
      // high-water mark: numbering restarts with the daemon, and the
      // server answers a cursor from before that from the start.
      if (typeof e.seq === "number") this.since = e.seq;
      this.listeners.forEach((l) => l(e));
    } catch {
      // not an event we understand
    }
  };

  private connect = () => {
    this.timer = undefined;
    const es = new EventSource(eventsUrl(this.since));
    this.es = es;
    es.onopen = () => {
      this.attempt = 0;
      this.setState("live");
      if (this.opened) this.reopenListeners.forEach((f) => f());
      this.opened = true;
    };
    for (const l of LEVELS) es.addEventListener(l, this.deliver);
    // "error" is both an event level and EventSource's failure event; only
    // the first carries data.
    es.addEventListener("error", (ev) => {
      if (ev instanceof MessageEvent && typeof ev.data === "string" && ev.data) {
        this.deliver(ev);
        return;
      }
      es.close();
      if (this.es !== es) return;
      this.es = null;
      if (this.listeners.size === 0) return;
      this.setState("reconnecting");
      this.timer = setTimeout(this.connect, backoff(this.attempt++));
    });
  };

  private close() {
    clearTimeout(this.timer);
    this.timer = undefined;
    this.es?.close();
    this.es = null;
    this.setState("connecting");
  }
}

const hub = new Hub();

/** Receive every event while mounted; `onEvent` may change between renders. */
export function useLiveEvents(onEvent: Listener): StreamState {
  const handler = useRef(onEvent);
  useEffect(() => {
    handler.current = onEvent;
  });
  const [state, setState] = useState<StreamState>(hub.state);
  useEffect(() => {
    const off = hub.onState(setState);
    const unsub = hub.subscribe((e) => handler.current(e));
    setState(hub.state);
    return () => {
      off();
      unsub();
    };
  }, []);
  return state;
}

/**
 * Keep every query in step with the server while signed in (mounted once,
 * by the app shell): an event invalidates what its org shows, bursts
 * coalesced, and a reconnect refetches everything, since events may have
 * been missed while the stream was down.
 */
export function useLiveSync(): StreamState {
  const qc = useQueryClient();
  const inv = useMemo(() => createInvalidator(qc), [qc]);
  useEffect(() => {
    const off = hub.onReopen(() => inv.resync());
    return () => {
      off();
      inv.dispose();
    };
  }, [inv]);
  return useLiveEvents((e) => inv.event(e));
}
