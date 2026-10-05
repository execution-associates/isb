// GET /api/v1/events: server-sent events named by level (info, warn,
// error, log), each carrying a StackEvent with its `seq` as the event id.
// apps/live.ts holds the one connection the app shares.

export type StreamState = "connecting" | "live" | "reconnecting";

/** Delay before reconnect attempt `n` (0-based): 1s doubling to 30s, plus up to 20% jitter. */
export function backoff(n: number, random: () => number = Math.random): number {
  const base = Math.min(30_000, 1000 * 2 ** Math.min(n, 5));
  return Math.round(base * (1 + 0.2 * random()));
}

export function eventsUrl(since: number): string {
  return since > 0 ? `/api/v1/events?since=${since}` : "/api/v1/events";
}
