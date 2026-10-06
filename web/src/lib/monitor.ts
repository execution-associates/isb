// Monitor (host_monitor): the words and numbers the live view shows, and
// the instance list's filter, sort and links. Pure, so it is tested.
import type { HistoryPoint, MonitorInstance, MonitorServer } from "@/api/tools";

export const MONITOR_POLL = 2000;

/** The last success is this old (ms) before the view says it is stale. */
export const STALE_AFTER = 6000;

export const MONITOR_RANGES = [
  { value: 60, label: "1m" },
  { value: 300, label: "5m" },
  { value: 900, label: "15m" },
  { value: 3600, label: "1h" },
] as const;

export const DEFAULT_RANGE = 300;

/** A `range` search param, or the default when it is not one of the choices. */
export function parseRange(s: string | null): number {
  const n = Number(s);
  return MONITOR_RANGES.some((r) => r.value === n) ? n : DEFAULT_RANGE;
}

const none = (n: number | null | undefined): n is null | undefined => n === null || n === undefined || !Number.isFinite(n);

/** Bytes per second in decimal units: "12.4 MB/s", "40 KB/s". */
export function bps(n: number | null | undefined): string {
  if (none(n)) return "—";
  const units = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"];
  let v = Math.max(0, n);
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u++;
  }
  // One decimal under 100 (dropped when it is .0); whole numbers above.
  const text = u > 0 && v < 100 ? String(Number(v.toFixed(1))) : String(Math.round(v));
  return `${text} ${units[u]}`;
}

/** Seconds up as its two largest units: "12d 4h", "3h 5m", "7m", "40s". */
export function uptime(secs: number | null | undefined): string {
  if (none(secs)) return "—";
  const s = Math.max(0, Math.floor(secs));
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m`;
  return `${s}s`;
}

/** A whole percentage, or "—". */
export function pctText(n: number | null | undefined): string {
  return none(n) ? "—" : `${Math.round(n)}%`;
}

/** A load average to two places, or "—". */
export function load(n: number | null | undefined): string {
  return none(n) ? "—" : n.toFixed(2);
}

/** History as one time axis and a line per metric, gaps kept as null. */
export function historySeries(points: HistoryPoint[]) {
  return {
    times: points.map((p) => p.t),
    cpu: points.map((p) => p.cpu),
    mem: points.map((p) => p.mem_used),
    rx: points.map((p) => p.net_rx),
    tx: points.map((p) => p.net_tx),
  };
}

/** Samples as sparkline points: [index, value]. */
export const sparkPoints = (values: number[]): [number, number][] => values.map((v, i) => [i, v]);

/** How a server is reached, in a word: local, ssh, or vm·<org>. */
export function serverKind(s: Pick<MonitorServer, "kind" | "vm_org">): string {
  if (s.kind === "vm") return s.vm_org ? `vm·${s.vm_org}` : "vm";
  return s.kind;
}

export const isRunning = (i: Pick<MonitorInstance, "status">) => i.status.toLowerCase() === "running";

export type InstanceSort = "cpu" | "mem" | "net" | "name";

export interface InstanceFilter {
  q: string;
  /** An org's name, or null for every org. */
  org: string | null;
  /** Stopped ones too. */
  all: boolean;
}

/** The instances matching `f`, ordered by `by` (largest first, name for ties). */
export function pickInstances(list: MonitorInstance[], f: InstanceFilter, by: InstanceSort): MonitorInstance[] {
  const words = f.q.toLowerCase().split(/\s+/).filter(Boolean);
  const key = (i: MonitorInstance): number => {
    switch (by) {
      case "cpu":
        return i.cpu_pct ?? -1;
      case "mem":
        return i.mem_bytes ?? -1;
      case "net":
        return i.net_rx_rate === null && i.net_tx_rate === null ? -1 : (i.net_rx_rate ?? 0) + (i.net_tx_rate ?? 0);
      default:
        return 0;
    }
  };
  return list
    .filter((i) => f.all || isRunning(i))
    .filter((i) => f.org === null || i.org === f.org)
    .filter((i) => words.every((w) => [i.name, i.org, i.stack, i.project].some((x) => x?.toLowerCase().includes(w))))
    .toSorted((a, b) => key(b) - key(a) || a.name.localeCompare(b.name));
}

/** The orgs present, sorted. */
export function instanceOrgs(list: MonitorInstance[]): string[] {
  return [...new Set(list.flatMap((i) => (i.org ? [i.org] : [])))].toSorted();
}

/**
 * The page an instance belongs to: its stack's (an app's or a compose
 * stack's; /stacks/ redirects to the right one), or its org's workspace.
 * Null for anything else (a sandbox, a non-isb instance).
 */
export function instanceLink(i: Pick<MonitorInstance, "name" | "org" | "stack">): string | null {
  if (!i.org) return null;
  const o = encodeURIComponent(i.org);
  if (i.stack) return `/orgs/${o}/stacks/${encodeURIComponent(i.stack)}`;
  if (i.name === "workspace") return `/orgs/${o}/workspace`;
  return null;
}

/** The monitor's cards: a faint wash of the brand colour from the top left, over the card's own fill. */
export const plate = "bg-linear-160 from-brand/[0.07] via-transparent via-50% to-foreground/[0.025]";
