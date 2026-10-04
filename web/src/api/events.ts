import { useEffect, useRef, useState } from "react";
import type { StackEvent } from "./tools";

// GET /api/v1/events: server-sent events named by level (info, warn,
// error), each carrying a StackEvent with its `seq` as the event id.

export type StreamState = "connecting" | "live" | "reconnecting";

/** Delay before reconnect attempt `n` (0-based): 1s doubling to 30s, plus up to 20% jitter. */
export function backoff(n: number, random: () => number = Math.random): number {
  const base = Math.min(30_000, 1000 * 2 ** Math.min(n, 5));
  return Math.round(base * (1 + 0.2 * random()));
}

export function eventsUrl(since: number): string {
  return since > 0 ? `/api/v1/events?since=${since}` : "/api/v1/events";
}

/**
 * Follow the event stream while mounted, reconnecting with backoff and
 * resuming after the last event seen. `onEvent` may change between renders.
 */
export function useEvents(onEvent: (e: StackEvent) => void, enabled = true): StreamState {
  const [state, setState] = useState<StreamState>("connecting");
  const handler = useRef(onEvent);
  useEffect(() => {
    handler.current = onEvent;
  });

  useEffect(() => {
    if (!enabled) return;
    let es: EventSource | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    let since = 0;
    let stopped = false;

    const deliver = (m: MessageEvent) => {
      try {
        const e = JSON.parse(m.data) as StackEvent;
        if (typeof e.seq === "number") since = Math.max(since, e.seq);
        handler.current(e);
      } catch {
        // not an event we understand
      }
    };

    const connect = () => {
      if (stopped) return;
      es = new EventSource(eventsUrl(since));
      es.onopen = () => {
        attempt = 0;
        setState("live");
      };
      es.addEventListener("info", deliver);
      es.addEventListener("warn", deliver);
      // "error" is both the server's event name for error-level events and
      // EventSource's own connection-failure event; only the first has data.
      es.addEventListener("error", (ev) => {
        if (ev instanceof MessageEvent && typeof ev.data === "string" && ev.data) {
          deliver(ev);
          return;
        }
        es?.close();
        es = null;
        if (stopped) return;
        setState("reconnecting");
        timer = setTimeout(connect, backoff(attempt++));
      });
    };

    connect();
    return () => {
      stopped = true;
      clearTimeout(timer);
      es?.close();
    };
  }, [enabled]);

  return state;
}
