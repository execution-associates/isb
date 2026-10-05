import { QueryClient, type QueryKey } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "@/api/client";
import { createInvalidator, keysForEvent, LIVE_POLL, queryDefaults, splitStack } from "./freshness";

describe("query defaults", () => {
  const q = queryDefaults.queries!;

  it("never trusts a cached answer: stale at once, refetched on mount, focus and reconnect", () => {
    expect(q.staleTime).toBe(0);
    expect(q.refetchOnMount).toBe(true);
    expect(q.refetchOnWindowFocus).toBe(true);
    expect(q.refetchOnReconnect).toBe(true);
    expect(q.refetchIntervalInBackground).toBe(false);
  });

  it("retries a server or network failure, not a 4xx", () => {
    const retry = q.retry as (n: number, e: unknown) => boolean;
    expect(retry(0, new ApiError(404, "not_found", "gone"))).toBe(false);
    expect(retry(0, new ApiError(503, "unavailable", "busy"))).toBe(true);
    expect(retry(0, new ApiError(0, "network", "offline"))).toBe(true);
    expect(retry(2, new ApiError(503, "unavailable", "busy"))).toBe(false);
  });

  it("polls live views within the 5-15 s window", () => {
    expect(LIVE_POLL).toBeGreaterThanOrEqual(5_000);
    expect(LIVE_POLL).toBeLessThanOrEqual(15_000);
  });
});

describe("event -> invalidation", () => {
  it("names the org: org/stack, or the default org for a bare name", () => {
    expect(splitStack("acme/web")).toEqual({ org: "acme", stack: "web" });
    expect(splitStack("web")).toEqual({ org: "default", stack: "web" });
  });

  it("refreshes the event's org, its compose stacks and the cross-org stack list", () => {
    expect(keysForEvent({ level: "info", stack: "acme/web-production" })).toEqual([
      ["apps", "acme"],
      ["stacks", "acme"],
      ["tool", "stack_list"],
    ]);
    // A stack moved into the default org reports under its bare name.
    expect(keysForEvent({ level: "warn", stack: "gym" })[0]).toEqual(["apps", "default"]);
  });

  it("ignores deployment log lines", () => {
    expect(keysForEvent({ level: "log", stack: "acme/web" })).toEqual([]);
  });
});

describe("createInvalidator", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const recorder = () => {
    const calls: (QueryKey | undefined)[] = [];
    return { calls, qc: { invalidateQueries: (f?: { queryKey?: QueryKey }) => (calls.push(f?.queryKey), Promise.resolve()) } };
  };

  it("coalesces a burst into one invalidation per prefix", () => {
    const { calls, qc } = recorder();
    const inv = createInvalidator(qc, 300);
    for (let i = 0; i < 12; i++) inv.event({ level: "info", stack: "acme/web" });
    inv.event({ level: "info", stack: "default-thing" });
    inv.event({ level: "log", stack: "other/x" });
    expect(calls).toEqual([]);
    vi.advanceTimersByTime(300);
    expect(calls).toEqual([["apps", "acme"], ["stacks", "acme"], ["tool", "stack_list"], ["apps", "default"], ["stacks", "default"]]);
    vi.advanceTimersByTime(1000);
    expect(calls).toHaveLength(5);
  });

  it("refetches everything on a resync, dropping what was pending", () => {
    const { calls, qc } = recorder();
    const inv = createInvalidator(qc, 300);
    inv.event({ level: "info", stack: "acme/web" });
    inv.resync();
    vi.advanceTimersByTime(1000);
    expect(calls).toEqual([undefined]);
  });

  it("makes a project list fetched before a move stale, so it is fetched again", async () => {
    const qc = new QueryClient({ defaultOptions: queryDefaults });
    qc.setQueryData(["apps", "default", "projects"], [{ name: "gym", environments: [] }]);
    qc.setQueryData(["apps", "stephan", "projects"], [{ name: "gym", environments: [] }]);
    const inv = createInvalidator(qc, 10);
    inv.event({ level: "info", stack: "gym" });
    vi.advanceTimersByTime(10);
    expect(qc.getQueryState(["apps", "default", "projects"])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["apps", "stephan", "projects"])?.isInvalidated).toBe(false);
    qc.clear();
  });
});
