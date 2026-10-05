// How the UI keeps showing what the server has now, without a reload
// (docs/contributing/web-ui.md#keeping-data-live):
//
// - every query is stale at once and refetches when it mounts, when the
//   window regains focus and when the network comes back;
// - one app-level subscription to GET /api/v1/events invalidates what an
//   event's org shows, and everything after the stream reconnects;
// - what no event announces (a project or app made by the CLI, another
//   session or an agent) is polled while visible: LIVE_POLL.
import { type QueryClient, type QueryClientConfig, type QueryKey } from "@tanstack/react-query";
import { ApiError } from "@/api/client";

/** How often a live view (lists and statuses) polls while the tab is visible. */
export const LIVE_POLL = 15_000;

/** How long events are gathered before their invalidations go out. */
export const COALESCE_MS = 300;

export const queryDefaults: NonNullable<QueryClientConfig["defaultOptions"]> = {
  queries: {
    // A 4xx will not get better by asking again.
    retry: (n, e) => !(e instanceof ApiError && e.status >= 400 && e.status < 500) && n < 2,
    staleTime: 0,
    refetchOnMount: true,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    // A hidden tab stops polling; focus refetches when it comes back.
    refetchIntervalInBackground: false,
  },
};

/** An event's `stack`, `org/stack` -> {org, stack}; a bare name is the default org's. */
export function splitStack(s: string): { org: string; stack: string } {
  const i = s.indexOf("/");
  return i < 0 ? { org: "default", stack: s } : { org: s.slice(0, i), stack: s.slice(i + 1) };
}

/**
 * The query prefixes an event in `org` makes stale: everything the org's
 * pages hold (apps, databases, volumes, jobs, projects' compose lists, the
 * overview: all under ["apps", org]), its compose stacks, and the
 * cross-org stack list that project health is computed from.
 */
export function keysForOrg(org: string): QueryKey[] {
  return [["apps", org], ["stacks", org], ["tool", "stack_list"]];
}

/** What one event invalidates; a deployment's log line changes nothing. */
export function keysForEvent(e: { level: string; stack: string }): QueryKey[] {
  if (e.level === "log") return [];
  return keysForOrg(splitStack(e.stack).org);
}

/**
 * Gathers events and invalidates each touched prefix once per burst: a
 * rollout's dozen events cost one refetch of each active query.
 */
export function createInvalidator(qc: Pick<QueryClient, "invalidateQueries">, delay = COALESCE_MS) {
  const pending = new Map<string, QueryKey>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const flush = () => {
    timer = undefined;
    const ks = [...pending.values()];
    pending.clear();
    for (const queryKey of ks) void qc.invalidateQueries({ queryKey });
  };
  return {
    event(e: { level: string; stack: string }) {
      const ks = keysForEvent(e);
      if (!ks.length) return;
      for (const k of ks) pending.set(JSON.stringify(k), k);
      if (timer === undefined) timer = setTimeout(flush, delay);
    },
    /** Anything may have changed while the stream was down: refetch every active query. */
    resync() {
      clearTimeout(timer);
      timer = undefined;
      pending.clear();
      void qc.invalidateQueries();
    },
    dispose() {
      clearTimeout(timer);
      timer = undefined;
      pending.clear();
    },
  };
}
