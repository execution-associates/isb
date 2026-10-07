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

/** One org limit's budget (org_list `allocation`): bytes for memory and disk. */
export interface Budget {
  limit: number;
  allocated: number;
  free: number;
}

/**
 * An org on the Monitor's Orgs tab: what its instances use now (from its
 * server's host_monitor) beside what they are allocated against its limits.
 * A `used` of null means no live numbers (the server is unreachable or too
 * old); a missing budget means the org sets no limit there.
 */
export interface OrgUsage {
  name: string;
  /** The server it runs on, as host_monitor names it: null for this host. */
  server: string | null;
  live: boolean;
  running: number;
  total: number;
  /** Cores in use (the sum of each instance's percent of one core, over 100). */
  cpu: { used: number | null; budget?: Budget };
  mem: { used: number | null; budget?: Budget };
  disk: { budget?: Budget };
  instances: { budget?: Budget };
  net_rx: number | null;
  net_tx: number | null;
  /** The org's summed CPU samples (percent of one core), oldest first. */
  cpu_history: number[];
  /** The fullest budget, allocated over limit as a percentage; null with no limits. */
  pressure: number | null;
}

/** Sums equal-length tails: the series' last samples line up. */
function sumTails(series: number[][]): number[] {
  const n = Math.max(0, ...series.map((s) => s.length));
  const out = Array.from({ length: n }, () => 0);
  for (const s of series) s.forEach((v, i) => (out[n - s.length + i] += v));
  return out;
}

const sumOf = (xs: (number | null)[]): number | null => (xs.some((x) => x !== null) ? xs.reduce<number>((a, x) => a + (x ?? 0), 0) : null);

/**
 * Each org's usage: `orgs` from org_list, `live` each server's instances by
 * host_monitor name (null for this host), or null when that server gave none.
 */
export function orgUsage(
  orgs: { name: string; instances: number; allocation?: Record<string, Budget>; placement?: { server: string } }[],
  live: Map<string | null, MonitorInstance[] | null>,
): OrgUsage[] {
  return orgs.map((o) => {
    const server = !o.placement || o.placement.server === "local" ? null : o.placement.server;
    const all = live.get(server) ?? null;
    const mine = all?.filter((i) => i.org === o.name) ?? [];
    const run = mine.filter(isRunning);
    const a = o.allocation ?? {};
    const budgets = [a.cpu, a.memory, a.disk, a.instances].filter((b): b is Budget => !!b && b.limit > 0);
    return {
      name: o.name,
      server,
      live: all !== null,
      running: run.length,
      total: all ? mine.length : o.instances,
      cpu: { used: all ? (sumOf(run.map((i) => i.cpu_pct)) ?? 0) / 100 : null, budget: a.cpu },
      mem: { used: all ? (sumOf(run.map((i) => i.mem_bytes)) ?? 0) : null, budget: a.memory },
      disk: { budget: a.disk },
      instances: { budget: a.instances },
      net_rx: all ? sumOf(run.map((i) => i.net_rx_rate)) : null,
      net_tx: all ? sumOf(run.map((i) => i.net_tx_rate)) : null,
      cpu_history: sumTails(run.map((i) => i.cpu_history)),
      pressure: budgets.length ? Math.max(...budgets.map((b) => (b.allocated / b.limit) * 100)) : null,
    };
  });
}

export type OrgSort = "pressure" | "cpu" | "mem" | "name";

/** The orgs whose name has every word of `q`, ordered by `by` (largest first, name for ties). */
export function pickOrgs(list: OrgUsage[], q: string, by: OrgSort): OrgUsage[] {
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  const key = (o: OrgUsage): number => {
    switch (by) {
      case "pressure":
        return o.pressure ?? -1;
      case "cpu":
        return o.cpu.used ?? -1;
      case "mem":
        return o.mem.used ?? -1;
      default:
        return 0;
    }
  };
  return list
    .filter((o) => words.every((w) => [o.name, o.server].some((x) => x?.toLowerCase().includes(w))))
    .toSorted((a, b) => key(b) - key(a) || a.name.localeCompare(b.name));
}

/** A count, to one decimal under ten: CPU cores. A trace shows as "<0.1", not 0. */
export function cores(n: number): string {
  if (n > 0 && n < 0.05) return "<0.1";
  return n < 10 ? String(Number(n.toFixed(1))) : String(Math.round(n));
}
