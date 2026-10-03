// The audit log (docs/audit.md): the audit_list tool, and the live tail at
// GET /api/v1/audit/stream (server-sent `audit` events, only the entries
// the viewer may read).
import { useEffect, useRef, useState } from "react";
import { backoff, type StreamState } from "./events";
import { callTool } from "./tools";

export interface AuditEntry {
  id: number;
  /** Unix milliseconds. */
  time: number;
  /** null: platform level (sign-ins, users, org changes). */
  org: string | null;
  actor: string;
  actor_kind: "person" | "agent" | "local" | "webhook" | "anonymous" | "superadmin";
  user_id: number | null;
  user_email: string | null;
  token_id: number | null;
  token_name: string | null;
  surface: "mcp" | "rest" | "cli" | "web" | "webhook" | string;
  action: string;
  target: string | null;
  details: Record<string, unknown>;
  /** "ok", or an error code. */
  outcome: string;
  ip: string | null;
  user_agent: string | null;
  request_id: string | null;
  prev_hash: string;
  hash: string;
}

export interface AuditFilters {
  /** One org; with `platform`, only platform-level entries. */
  org?: string;
  platform?: boolean;
  actor?: string;
  action?: string;
  target?: string;
  outcome?: string;
  /** Unix milliseconds. */
  since?: number;
}

export interface AuditPage {
  entries: AuditEntry[];
  next_before: number | null;
  head: number;
}

/** Only the filters that are set, as audit_list takes them. */
export function auditArgs(f: AuditFilters, extra: { before?: number; after?: number; limit?: number } = {}) {
  const out: Record<string, unknown> = {};
  for (const [k, raw] of Object.entries({ ...f, ...extra })) {
    const v = typeof raw === "string" ? raw.trim() : raw;
    if (v === undefined || v === "" || v === false) continue;
    out[k] = v;
  }
  return out;
}

export const listAudit = (f: AuditFilters, extra: { before?: number; after?: number; limit?: number } = {}) =>
  callTool<AuditPage>("audit_list", auditArgs(f, extra));

/** A shell glob (`*`, `?`, `[a-z]`, `[!x]`) as a whole-string RegExp. */
export function globRegExp(glob: string): RegExp {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*") re += ".*";
    else if (c === "?") re += ".";
    else if (c === "[") {
      const j = glob.indexOf("]", i + 2);
      if (j < 0) {
        re += "\\[";
        continue;
      }
      let body = glob.slice(i + 1, j);
      if (body.startsWith("!")) body = "^" + body.slice(1);
      re += `[${body.replace(/\\/g, "\\\\")}]`;
      i = j;
    } else re += c.replace(/[.+^${}()|\\/\]]/g, "\\$&");
  }
  return new RegExp(`^${re}$`);
}

/** Does a live entry pass the filters (as the server would judge them)? */
export function matches(e: AuditEntry, f: AuditFilters): boolean {
  if (f.platform && e.org !== null) return false;
  if (!f.platform && f.org && e.org !== f.org) return false;
  if (f.actor) {
    const r = globRegExp(f.actor.trim());
    if (!r.test(e.actor) && !r.test(e.user_email ?? "")) return false;
  }
  if (f.action && !globRegExp(f.action.trim()).test(e.action)) return false;
  if (f.target && !globRegExp(f.target.trim()).test(e.target ?? "")) return false;
  if (f.outcome === "error" ? e.outcome === "ok" : f.outcome && e.outcome !== f.outcome) return false;
  if (f.since && e.time < f.since) return false;
  return true;
}

export function streamUrl(org: string | undefined, after: number): string {
  const q = new URLSearchParams();
  if (org) q.set("org", org);
  if (after > 0) q.set("after", String(after));
  const s = q.toString();
  return `/api/v1/audit/stream${s ? `?${s}` : ""}`;
}

/** Follow new entries while `enabled`, from `after` on, reconnecting with backoff. */
export function useAuditTail(
  org: string | undefined,
  after: number,
  enabled: boolean,
  onEntry: (e: AuditEntry) => void,
): StreamState | "off" {
  const [state, setState] = useState<StreamState>("connecting");
  const handler = useRef(onEntry);
  useEffect(() => {
    handler.current = onEntry;
  });
  useEffect(() => {
    if (!enabled) return;
    let es: EventSource | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    let last = after;
    let stopped = false;
    const connect = () => {
      if (stopped) return;
      setState(attempt ? "reconnecting" : "connecting");
      es = new EventSource(streamUrl(org, last));
      es.onopen = () => {
        attempt = 0;
        setState("live");
      };
      es.addEventListener("audit", (m) => {
        try {
          const e = JSON.parse((m as MessageEvent).data) as AuditEntry;
          last = Math.max(last, e.id);
          handler.current(e);
        } catch {
          // not an entry
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
  }, [org, after, enabled]);
  return enabled ? state : "off";
}

/** Every matching entry, oldest first, as JSON lines (at most `max`). */
export async function exportJsonl(f: AuditFilters, max = 50_000): Promise<{ text: string; count: number }> {
  const lines: string[] = [];
  let after = 0;
  for (;;) {
    const page = await listAudit(f, { after, limit: 1000 });
    for (const e of page.entries) lines.push(JSON.stringify(e));
    if (page.entries.length < 1000 || lines.length >= max) break;
    after = page.entries[page.entries.length - 1].id;
  }
  return { text: lines.length ? lines.join("\n") + "\n" : "", count: lines.length };
}
