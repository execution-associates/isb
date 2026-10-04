// One shared connection to GET /api/v1/events for every app page, with the
// deployment log lines (level `log`) included. Pages subscribe; the
// connection opens with the first subscriber and closes shortly after the
// last one leaves, so moving between pages keeps it.
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { backoff, eventsUrl, type StreamState } from "@/api/events";
import { keys } from "./api";

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

/** `org/stack` -> {org, stack}; a bare name is the default org's. */
export function splitStack(s: string): { org: string; stack: string } {
  const i = s.indexOf("/");
  return i < 0 ? { org: "default", stack: s } : { org: s.slice(0, i), stack: s.slice(i + 1) };
}

class Hub {
  private listeners = new Set<Listener>();
  private stateListeners = new Set<(s: StreamState) => void>();
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

  private setState(s: StreamState) {
    this.state = s;
    this.stateListeners.forEach((f) => f(s));
  }

  private deliver = (m: MessageEvent) => {
    try {
      const e = JSON.parse(m.data) as LiveEvent;
      if (typeof e.seq === "number") {
        if (e.seq <= this.since) return; // a replay after reconnecting
        this.since = e.seq;
      }
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
 * Keep an org's app queries fresh: any event in the org (but a log line)
 * refetches them, bursts coalesced.
 */
export function useOrgLive(org: string): StreamState {
  const qc = useQueryClient();
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return useLiveEvents((e) => {
    if (e.level === "log" || splitStack(e.stack).org !== org) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      qc.invalidateQueries({ queryKey: keys.org(org) });
      qc.invalidateQueries({ queryKey: ["tool", "stack_list"] });
    }, 300);
  });
}
