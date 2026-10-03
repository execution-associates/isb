// The history (docs/history.md): history_query merges the controller's
// events, incus lifecycle events, audit rows and markers into one timeline;
// GET /api/v1/history/stream tails it (server-sent `history` events, only
// what the viewer may read).
import { useEffect, useRef, useState } from "react";
import { globRegExp } from "./audit";
import { backoff, type StreamState } from "./events";
import { callTool } from "./tools";

export type HistorySource = "audit" | "controller" | "incus" | "marker";

export interface HistoryItem {
  source: HistorySource | string;
  id: number;
  /** Unix milliseconds. */
  time: number;
  /** null: host level (platform admins). */
  org: string | null;
  /** An event kind, an incus action, or an audit action. */
  kind: string;
  object_type: string | null;
  object: string | null;
  actor: string | null;
  /** info/warn/error for events; the outcome for audit rows. */
  level: string | null;
  message: string | null;
  details: Record<string, unknown>;
  /** For an incus instance event: the audit row that likely caused it. */
  inferred?: { audit_id: number; action: string; actor: string; seconds_before: number; why: string };
}

export interface HistoryFilters {
  org?: string;
  platform?: boolean;
  object?: string;
  exact?: boolean;
  kind?: string;
  /** Comma-separated sources; empty is all. */
  source?: string;
  actor?: string;
  since?: number;
}

export interface HistoryPage {
  items: HistoryItem[];
  next: string | null;
}

/** Only the filters that are set, as history_query takes them. */
export function historyArgs(f: HistoryFilters, extra: Record<string, unknown> = {}) {
  const out: Record<string, unknown> = {};
  for (const [k, raw] of Object.entries({ ...f, ...extra })) {
    const v = typeof raw === "string" ? raw.trim() : raw;
    if (v === undefined || v === "" || v === false || v === null) continue;
    out[k] = v;
  }
  return out;
}

export const queryHistory = (f: HistoryFilters, extra: { before?: string; limit?: number; ascending?: boolean; correlate?: boolean } = {}) =>
  callTool<HistoryPage>("history_query", historyArgs(f, extra));

/** Does a live item pass the filters (as the server would judge them)? */
export function itemMatches(i: HistoryItem, f: HistoryFilters): boolean {
  if (f.platform && i.org !== null) return false;
  if (!f.platform && f.org && i.org !== f.org) return false;
  if (f.source && !f.source.split(",").map((s) => s.trim()).includes(i.source)) return false;
  if (f.kind && !globRegExp(f.kind.trim()).test(i.kind)) return false;
  if (f.actor && !globRegExp(f.actor.trim()).test(i.actor ?? "")) return false;
  if (f.object && i.source !== "marker") {
    const o = f.object.trim();
    const names = [i.object, ...Object.values(i.details ?? {}).filter((v) => typeof v === "string")] as (string | null)[];
    if (!names.some((n) => n && (f.exact ? n === o : n.includes(o)))) return false;
  }
  if (f.since && i.time < f.since) return false;
  return true;
}

export function historyStreamUrl(org: string | undefined): string {
  return org ? `/api/v1/history/stream?org=${encodeURIComponent(org)}` : "/api/v1/history/stream";
}

/** Follow new items while `enabled`, reconnecting with backoff (resuming via Last-Event-ID). */
export function useHistoryTail(org: string | undefined, enabled: boolean, onItem: (i: HistoryItem) => void): StreamState | "off" {
  const [state, setState] = useState<StreamState>("connecting");
  const handler = useRef(onItem);
  useEffect(() => {
    handler.current = onItem;
  });
  useEffect(() => {
    if (!enabled) return;
    let es: EventSource | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    let stopped = false;
    const connect = () => {
      if (stopped) return;
      es = new EventSource(historyStreamUrl(org));
      es.onopen = () => {
        attempt = 0;
        setState("live");
      };
      es.addEventListener("history", (m) => {
        try {
          handler.current(JSON.parse((m as MessageEvent).data) as HistoryItem);
        } catch {
          // not an item
        }
      });
      es.onerror = () => {
        es?.close();
        es = null;
        if (stopped) return;
        setState("reconnecting");
        timer = setTimeout(connect, backoff(attempt++));
      };
    };
    connect();
    return () => {
      stopped = true;
      clearTimeout(timer);
      es?.close();
    };
  }, [org, enabled]);
  return enabled ? state : "off";
}

/** Every matching item, oldest first, as JSON lines (at most `max`). */
export async function exportHistory(f: HistoryFilters, max = 100_000): Promise<{ text: string; count: number }> {
  const lines: string[] = [];
  let before: string | undefined;
  for (;;) {
    const page = await queryHistory(f, { before, limit: 1000, ascending: true });
    for (const i of page.items) lines.push(JSON.stringify(i));
    if (!page.next || lines.length >= max) break;
    before = page.next;
  }
  return { text: lines.length ? lines.join("\n") + "\n" : "", count: lines.length };
}

/** A stable key: ids are per source. */
export const itemKey = (i: HistoryItem) => `${i.source}:${i.id}`;
