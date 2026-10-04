// Uptime monitors (docs/guides/uptime.md). Shapes from
// crates/isb-apps/src/monitor/{mod,view,store,target}.rs.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { Tone } from "@/lib/status";

export type MonitorKind = "http" | "tcp" | "app";
export type MonitorStatus = "up" | "down" | "pending" | "paused";

export interface MonitorHeader {
  name: string;
  value?: string;
  secret?: string;
}

/** The fields a monitor is made of (monitor_create, monitor_update). */
export interface MonitorDef {
  name: string;
  type: MonitorKind;
  url?: string;
  host?: string;
  port?: number;
  app?: string;
  domain?: string;
  path?: string;
  method: "GET" | "HEAD";
  expected_status: string;
  keyword?: string;
  keyword_absent?: string;
  follow_redirects: boolean;
  headers?: MonitorHeader[];
  interval: number;
  timeout: number;
  failure_threshold: number;
  recovery_threshold: number;
  cert_expiry_days: number;
  paused: boolean;
  auto: boolean;
  created_at: number;
  updated_at: number;
}

export interface Outcome {
  /** Unix milliseconds. */
  at: number;
  ok: boolean;
  latency_ms?: number;
  status?: number;
  error?: string;
  url?: string;
  via?: string;
  cert_expires?: number;
  note?: string;
}

export interface Incident {
  id: number;
  monitor: string;
  /** Unix milliseconds. */
  started: number;
  ended?: number;
  duration_ms: number;
  error?: string;
}

export interface Check {
  at: number;
  ok: boolean;
  latency_ms?: number;
  status?: number;
  error?: string;
  /** A failure before the monitor's first success: not downtime. */
  pending?: boolean;
}

export interface Monitor extends MonitorDef {
  status: MonitorStatus;
  since: number;
  flapping: boolean;
  /** Pending for 30 minutes with only failures. */
  never_up?: boolean;
  target: string;
  last?: Outcome | null;
  uptime: { "24h": number | null; "7d": number | null; "30d": number | null };
  latency: { p50: number | null; p95: number | null };
  /** 24 hourly buckets: [unix ms, uptime % or null, pending checks]. */
  bars: Bar[];
  /** The last 30 checks: [unix ms, latency ms, or null when it failed]. */
  spark: [number, number | null][];
  incident?: Incident | null;
  /** Unix seconds. */
  cert_expires?: number | null;
  link?: string | null;
  incidents?: Incident[];
  checks?: Check[];
}

/** One uptime bar: [unix ms, uptime % or null, checks that were only pending]. */
export type Bar = [number, number | null, number?];

export interface Settings {
  auto_monitors: boolean;
  exclude_apps?: string[];
}

export interface MonitorList {
  monitors: Monitor[];
  down: number;
  incidents: Incident[];
  settings: Settings;
}

export interface Bucket {
  at: number;
  checks: number;
  ok: number;
  /** Failures before the first success: neither up nor down. */
  pending?: number;
  uptime: number | null;
  p50: number | null;
  p95: number | null;
}

export interface History {
  range: string;
  step_ms: number;
  uptime: number | null;
  buckets: Bucket[];
  checks: Check[];
}

export const RANGES = ["24h", "7d", "30d", "90d"] as const;
export type Range = (typeof RANGES)[number];

/** A monitor tool, with the arguments the forms build. */
export const callMonitor = <R>(tool: `monitor_${string}`, args: Record<string, unknown>, org: string) => callTool<R, typeof tool>(tool, args, org);

export const ukeys = {
  all: (org: string) => ["apps", org, "monitors"] as const,
  one: (org: string, name: string) => ["apps", org, "monitors", "one", name] as const,
  history: (org: string, name: string, range: string) => ["apps", org, "monitors", "history", name, range] as const,
};

export function useMonitors(org: string, enabled = true) {
  return useQuery({
    queryKey: ukeys.all(org),
    enabled,
    refetchInterval: 15_000,
    queryFn: () => callMonitor<MonitorList>("monitor_list", {}, org),
  });
}

export function useMonitor(org: string, name: string) {
  return useQuery({
    queryKey: ukeys.one(org, name),
    refetchInterval: 15_000,
    queryFn: () => callMonitor<Monitor>("monitor_get", { name }, org),
  });
}

export function useHistory(org: string, name: string, range: Range) {
  return useQuery({
    queryKey: ukeys.history(org, name, range),
    refetchInterval: 60_000,
    queryFn: () => callMonitor<History>("monitor_checks", { name, range, limit: 50 }, org),
  });
}

export const STATUS_TONE: Record<MonitorStatus, Tone> = {
  up: "success",
  down: "danger",
  pending: "neutral",
  paused: "muted",
};

export const STATUS_LABEL: Record<MonitorStatus, string> = {
  up: "Up",
  down: "Down",
  pending: "Pending",
  paused: "Paused",
};

export const PENDING_HINT = "Waiting for the first successful check";
export const NEVER_UP_HINT = "Never came up: no successful check in the first 30 minutes";

/** The badge text of a monitor: its status, or why it is still pending. */
export function statusText(m: { status: MonitorStatus; never_up?: boolean }): string {
  return m.status === "pending" && m.never_up ? "Never came up" : STATUS_LABEL[m.status];
}

/** A pending monitor that never came up is a problem; one still waiting is not. */
export function statusTone(m: { status: MonitorStatus; never_up?: boolean }): Tone {
  return m.status === "pending" && m.never_up ? "danger" : STATUS_TONE[m.status];
}

/** "99.95%", "100%", "–". */
export function uptimeText(u: number | null | undefined): string {
  if (u === null || u === undefined) return "–";
  if (u >= 100) return "100%";
  // Rounded down, so 99.999% never reads as 100%.
  if (u >= 99) return `${(Math.floor(u * 100) / 100).toFixed(2)}%`;
  return `${(Math.floor(u * 10) / 10).toFixed(1)}%`;
}

/** The tone of an uptime percentage: all up, some down, mostly down. */
export function uptimeTone(u: number | null | undefined): Tone {
  if (u === null || u === undefined) return "muted";
  if (u >= 100) return "success";
  if (u >= 95) return "warning";
  return "danger";
}

export const msText = (ms: number | null | undefined) => (ms === null || ms === undefined ? "–" : ms >= 1000 ? `${(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)} s` : `${ms} ms`);

/** "4m 12s", "2h 5m", "3d 1h". */
export function downtimeText(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86_400)}d ${Math.floor((s % 86_400) / 3600)}h`;
}

/** What a monitor checks, for a listing. */
export function targetText(m: Pick<MonitorDef, "type" | "url" | "host" | "port" | "app" | "domain" | "path">): string {
  switch (m.type) {
    case "http":
      return (m.url ?? "").split(/[?#]/)[0];
    case "tcp":
      return `${m.host ?? ""}:${m.port ?? ""}`;
    case "app":
      return `app ${m.app ?? ""}${m.domain ? ` · ${m.domain}` : ""}${m.path ? ` ${m.path}` : ""}`;
  }
}

/** A monitor name problem, as the daemon would word it. */
export function monitorNameProblem(s: string): string | null {
  if (!s) return "Give the monitor a name.";
  if (s.length > 63 || !/^[a-z][a-z0-9-]*$/.test(s)) return "Up to 63 characters of a-z, 0-9 and -, starting with a letter.";
  return null;
}

/** Status ranges like 200-399 or 200,204; null when fine. */
export function statusProblem(s: string): string | null {
  const parts = s.split(",").map((p) => p.trim());
  const ok =
    parts.length > 0 &&
    parts.length <= 10 &&
    parts.every((p) => {
      const m = /^(\d{3})(?:\s*-\s*(\d{3}))?$/.exec(p);
      if (!m) return false;
      const a = Number(m[1]);
      const b = Number(m[2] ?? m[1]);
      return a >= 100 && b <= 599 && a <= b;
    });
  return ok ? null : "Codes and ranges like 200-399 or 200,204.";
}

/** The monitors watching an app. */
export const monitorsOfApp = (list: Monitor[] | undefined, app: string) => (list ?? []).filter((m) => m.type === "app" && m.app === app);
