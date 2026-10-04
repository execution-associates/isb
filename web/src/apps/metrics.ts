// metrics_query (docs/operations/metrics.md) shaped for the Monitoring tab's charts:
// per-instance series onto one time grid, with gaps kept as gaps, and their
// bucket-by-bucket sum.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";

export type Metric = "cpu" | "memory" | "net_rx" | "net_tx" | "disk_read" | "disk_write";

export interface MetricSeries {
  name: string;
  stack: string;
  service: string;
  replicas?: number[];
  /** [bucket start (unix s), value], oldest first; missing buckets are gaps. */
  points: [number, number][];
}

export interface MetricAnswer {
  metric: Metric;
  from: number;
  to: number;
  step: number;
  tier_step: number;
  series: MetricSeries[];
}

export const RANGES = [
  { id: "1h", label: "1 hour", seconds: 3600 },
  { id: "24h", label: "24 hours", seconds: 86_400 },
  { id: "7d", label: "7 days", seconds: 604_800 },
  { id: "30d", label: "30 days", seconds: 2_592_000 },
] as const;

export type RangeId = (typeof RANGES)[number]["id"];

/** Points per chart: enough for a smooth line, few enough for the DOM. */
export const TARGET_POINTS = 240;

export function stepFor(range: RangeId): number {
  const r = RANGES.find((x) => x.id === range) ?? RANGES[0];
  return Math.max(10, Math.round(r.seconds / TARGET_POINTS / 10) * 10);
}

export function useMetric(org: string, app: string, metric: Metric, range: RangeId, refetchInterval?: number) {
  return useQuery({
    queryKey: ["apps", org, "metrics", app, metric, range],
    queryFn: () => callTool<MetricAnswer>("metrics_query", { app, metric, range, step: stepFor(range) }, org),
    refetchInterval,
    placeholderData: (prev) => (prev && prev.metric === metric ? prev : undefined),
  });
}

/** One line on the grid: a value per bucket, null for a gap. */
export interface Line {
  name: string;
  values: (number | null)[];
}

export interface Grid {
  /** Bucket starts, unix seconds. */
  times: number[];
  lines: Line[];
  /** The sum of every line per bucket (null where all are gaps). */
  total: (number | null)[];
  /** How many lines had a value per bucket. */
  counts: number[];
}

/**
 * Put series on the grid `from`..`to` by `step`: bucket b is
 * `from - from % step + i*step`. A point lands in the bucket holding its
 * time; two in one bucket (a coarser step than the series') are averaged.
 */
export function toGrid(series: MetricSeries[], from: number, to: number, step: number): Grid {
  const s = Math.max(1, step);
  const start = from - (from % s);
  const n = Math.max(1, Math.ceil((to - start) / s));
  const times = Array.from({ length: n }, (_, i) => start + i * s);
  const lines: Line[] = series.map((x) => {
    const sum = Array.from({ length: n }, () => 0);
    const cnt = Array.from({ length: n }, () => 0);
    for (const [t, v] of x.points) {
      const i = Math.floor((t - start) / s);
      if (i < 0 || i >= n || !Number.isFinite(v)) continue;
      sum[i] += v;
      cnt[i]++;
    }
    return { name: x.name, values: sum.map((v, i) => (cnt[i] ? v / cnt[i] : null)) };
  });
  const total: (number | null)[] = [];
  const counts: number[] = [];
  for (let i = 0; i < n; i++) {
    let t = 0;
    let c = 0;
    for (const l of lines) {
      const v = l.values[i];
      if (v !== null) {
        t += v;
        c++;
      }
    }
    total.push(c ? t : null);
    counts.push(c);
  }
  return { times, lines, total, counts };
}

/** The largest value in some lines (gaps ignored), at least `floor`. */
export function maxOf(lines: (number | null)[][], floor = 0): number {
  let m = floor;
  for (const l of lines) for (const v of l) if (v !== null && v > m) m = v;
  return m;
}

/** The latest non-gap value. */
export function latest(values: (number | null)[]): number | null {
  for (let i = values.length - 1; i >= 0; i--) if (values[i] !== null) return values[i];
  return null;
}

/** A "nice" axis top above `v`: 1, 2 or 5 times a power of ten. */
export function niceMax(v: number): number {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 5, 10]) if (v <= m * p) return m * p;
  return 10 * p;
}

/** Short replica labels: the instance name's `-<slot>-<hex>` tail, else the name. */
export function replicaLabel(instance: string, service: string): string {
  const m = new RegExp(`${service.replace(/[-]/g, "\\-")}-(\\d+)-[0-9a-f]+$`).exec(instance);
  return m ? `replica ${m[1]}` : instance;
}

/**
 * Fill a lone missing bucket between two values with their mean: a sampler
 * that skips one bucket (a counter rate that needs two reads) is not an
 * outage. Two or more in a row stay a gap.
 */
export function bridge(values: (number | null)[]): (number | null)[] {
  return values.map((v, i) => {
    if (v !== null) return v;
    const a = values[i - 1];
    const b = values[i + 1];
    return a !== null && a !== undefined && b !== null && b !== undefined ? (a + b) / 2 : null;
  });
}

/** Bytes per second, short. */
export function rate(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "–";
  const units = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 && u > 0 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}
