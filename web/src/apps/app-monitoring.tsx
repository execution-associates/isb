// The Monitoring tab: the app's metrics history (metrics_query, kept a
// month: docs/metrics.md) for CPU, memory, network and disk, per replica or
// summed, over 1 hour to 30 days; and each replica's state now.
import { Activity, Cpu, HeartPulse, MemoryStick, RotateCw } from "lucide-react";
import { useMemo, useState } from "react";
import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { type App, type InstanceDetail, serviceOf, useStack } from "./api";
import { Dot, EmptyState, QueryError, ToneBadge } from "./components";
import { MetricChart } from "./metric-chart";
import { bridge, latest, type Line, type Metric, RANGES, type RangeId, rate, replicaLabel, toGrid, useMetric } from "./metrics";
import { bytes, percent } from "./util";

const CHARTS: { metric: Metric; title: string; format: (v: number | null) => string; floor: number }[] = [
  { metric: "cpu", title: "CPU", format: (v) => percent(v), floor: 5 },
  { metric: "memory", title: "Memory", format: (v) => bytes(v), floor: 1024 * 1024 },
  { metric: "net_rx", title: "Network in", format: rate, floor: 1024 },
  { metric: "net_tx", title: "Network out", format: rate, floor: 1024 },
  { metric: "disk_read", title: "Disk read", format: rate, floor: 1024 },
  { metric: "disk_write", title: "Disk write", format: rate, floor: 1024 },
];

const REFRESH: Record<RangeId, number> = { "1h": 10_000, "24h": 60_000, "7d": 300_000, "30d": 600_000 };

export function MonitoringTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack, 5000);
  const svc = serviceOf(stack.data, app.name);
  const [range, setRange] = useState<RangeId>("1h");
  const [split, setSplit] = useState(false);
  const instances = [...(svc?.instances ?? [])].sort((a, b) => a.slot - b.slot) as InstanceDetail[];

  if (stack.error) return <QueryError error={stack.error} />;
  const cpuNow = instances.reduce((n, i) => n + (i.cpu_pct ?? 0), 0);
  const memNow = instances.reduce((n, i) => n + (i.mem_bytes ?? 0), 0);
  const restarts = instances.reduce((n, i) => n + i.restarts, 0);
  const memLimit = parseMemory(app.resources?.memory);

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Stat icon={Cpu} label="CPU now, all replicas" value={svc ? percent(cpuNow) : "–"} hint="100% is one core" />
        <Stat icon={MemoryStick} label="Memory now" value={svc ? bytes(memNow) : "–"} hint={memLimit ? `limit ${bytes(memLimit)} each` : "no limit set"} />
        <Stat icon={HeartPulse} label="Healthy" value={svc ? `${svc.healthy}/${svc.replicas}` : "–"} hint={svc?.state ?? "not running"} />
        <Stat icon={RotateCw} label="Restarts" value={svc ? String(restarts) : "–"} hint="since each started" />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <div className="flex rounded-md border p-0.5" role="radiogroup" aria-label="Range">
          {RANGES.map((r) => (
            <button
              key={r.id}
              type="button"
              role="radio"
              aria-checked={range === r.id}
              onClick={() => setRange(r.id)}
              className={cn("rounded px-3 py-1 text-sm text-muted-foreground transition-colors hover:text-foreground", range === r.id && "bg-accent font-medium text-foreground")}
            >
              {r.id}
            </button>
          ))}
        </div>
        <div className="flex rounded-md border p-0.5" role="radiogroup" aria-label="Replicas">
          {([false, true] as const).map((s) => (
            <button
              key={String(s)}
              type="button"
              role="radio"
              aria-checked={split === s}
              onClick={() => setSplit(s)}
              className={cn("rounded px-3 py-1 text-sm text-muted-foreground transition-colors hover:text-foreground", split === s && "bg-accent font-medium text-foreground")}
            >
              {s ? "Per replica" : "Summed"}
            </button>
          ))}
        </div>
        <span className="text-xs text-muted-foreground">Kept 24 h at 10 s, 7 days at 1 min, 30 days at 10 min.</span>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        {CHARTS.map((c) => (
          <ChartCard key={c.metric} org={org} app={app.name} chart={c} range={range} split={split} max={c.metric === "memory" && split && memLimit ? memLimit : undefined} />
        ))}
      </div>

      {svc ? (
        <Card className="gap-0 overflow-hidden py-0">
          <div className="border-b px-5 py-3 text-sm font-medium">Replicas now</div>
          <ul className="divide-y">
            {instances.map((i) => {
              const tone = i.health === "unhealthy" ? "bad" : i.status !== "Running" ? "warn" : i.health === "starting" ? "busy" : "ok";
              return (
                <li key={i.name} className="grid grid-cols-[minmax(0,1fr)] gap-2 px-5 py-3 text-sm">
                  <div className="flex flex-wrap items-center gap-2">
                    <Dot tone={tone} />
                    <span className="font-medium">Replica {i.slot}</span>
                    <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{i.name}</span>
                    <span className="ml-auto">
                      <ToneBadge tone={i.in_rotation ? "ok" : "idle"}>{i.in_rotation ? "in rotation" : "out of rotation"}</ToneBadge>
                    </span>
                  </div>
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-6">
                    <Info k="Status" v={i.status} />
                    <Info k="Health" v={i.health === "none" ? "no check" : i.health} />
                    <Info k="CPU" v={percent(i.cpu_pct)} />
                    <Info k="Memory" v={bytes(i.mem_bytes)} />
                    <Info k="Address" v={i.ip ?? "–"} mono />
                    <Info k="Restarts" v={String(i.restarts)} />
                  </dl>
                  {i.last_probe && i.health === "unhealthy" && <pre className="max-h-24 overflow-auto rounded-md bg-muted p-2 font-mono text-xs whitespace-pre-wrap">{i.last_probe}</pre>}
                </li>
              );
            })}
          </ul>
        </Card>
      ) : (
        !stack.isLoading && (
          <Card className="py-0">
            <EmptyState icon={Activity} title="Not running">
              The charts show what was recorded while it ran.
            </EmptyState>
          </Card>
        )
      )}
    </div>
  );
}

function ChartCard({
  org,
  app,
  chart,
  range,
  split,
  max,
}: {
  org: string;
  app: string;
  chart: (typeof CHARTS)[number];
  range: RangeId;
  split: boolean;
  max?: number;
}) {
  const q = useMetric(org, app, chart.metric, range, REFRESH[range]);
  const shaped = useMemo(() => {
    if (!q.data) return null;
    const g = toGrid(q.data.series, q.data.from, q.data.to, q.data.step);
    const lines: Line[] = split
      ? g.lines.map((l, i) => ({ name: replicaLabel(l.name, q.data.series[i]?.service ?? app), values: bridge(l.values) }))
      : [{ name: "all replicas", values: bridge(g.total) }];
    return { times: g.times, lines, now: latest(g.total), step: q.data.step };
  }, [q.data, split, app]);

  return (
    <Card className="gap-3 px-5 py-4">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-sm font-medium">{chart.title}</span>
        <span className="text-xs text-muted-foreground tabular-nums">
          {shaped ? (
            <>
              latest <span className="text-base font-semibold text-foreground">{chart.format(shaped.now)}</span>
            </>
          ) : null}
        </span>
      </div>
      {q.error ? (
        <QueryError error={q.error} />
      ) : !shaped ? (
        <div className="h-44 animate-pulse rounded-md bg-muted/50" />
      ) : (
        <MetricChart times={shaped.times} lines={shaped.lines} format={chart.format} label={`${chart.title} of ${app}, last ${range}`} floor={chart.floor} max={max} filled={!split} />
      )}
      {shaped && <p className="text-[11px] text-muted-foreground">one point per {shaped.step >= 60 ? `${Math.round(shaped.step / 60)} min` : `${shaped.step} s`}{split && shaped.lines.length > 1 ? ` · ${shaped.lines.length} instances (a replaced replica is a new one)` : ""}</p>}
    </Card>
  );
}

function Info({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-muted-foreground">{k}</dt>
      <dd className={mono ? "truncate font-mono" : "truncate"}>{v}</dd>
    </div>
  );
}

function Stat({ icon: Icon, label, value, hint }: { icon: typeof Cpu; label: string; value: string; hint?: string }) {
  return (
    <Card className="gap-1 px-5 py-4">
      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="truncate">{label}</span>
        <Icon className="size-4 shrink-0" />
      </div>
      <div className="text-2xl font-semibold tabular-nums">{value}</div>
      {hint && <p className="truncate text-xs text-muted-foreground">{hint}</p>}
    </Card>
  );
}

/** `512m`, `2g`, `2GiB` -> bytes. */
export function parseMemory(s: string | undefined): number | null {
  if (!s) return null;
  const m = /^(\d+(?:\.\d+)?)\s*([kmgt])?(i?b)?$/i.exec(s.trim());
  if (!m) return null;
  const pow = { k: 1, m: 2, g: 3, t: 4 }[(m[2] ?? "").toLowerCase() as "k"] ?? 0;
  return Math.round(Number(m[1]) * 1024 ** pow);
}
