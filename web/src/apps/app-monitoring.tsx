// The Monitoring tab: the app's uptime monitors (uptime/cards.tsx), its metrics history (metrics_query, kept a
// month: docs/operations/metrics.md) for CPU, memory, network and disk, per replica or
// summed, over 1 hour to 30 days; and each replica's state now.
import { Activity, ArrowDownToLine, ArrowUpFromLine, Cpu, HardDriveDownload, HardDriveUpload, HeartPulse, MemoryStick, RotateCw } from "lucide-react";
import { Fragment, type ReactNode, useMemo, useState } from "react";
import { StatusBadge, StatusDot } from "@/components/status";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { TONE_TEXT, type Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { AppUptimeCard } from "@/uptime/cards";
import { type App, type InstanceDetail, serviceOf, type StackDetail, useStack } from "./api";
import { EmptyState, QueryError } from "./components";
import { MetricChart, SERIES } from "./metric-chart";
import { bridge, latest, type Line, type Metric, type MetricTarget, RANGES, type RangeId, rate, replicaLabel, targetLabel, toGrid, useMetric } from "./metrics";
import { Segmented } from "./segmented";
import { bytes, percent } from "./util";

const CHARTS: { metric: Metric; title: string; icon: typeof Cpu; format: (v: number | null) => string; floor: number }[] = [
  { metric: "cpu", title: "CPU", icon: Cpu, format: (v) => percent(v), floor: 5 },
  { metric: "memory", title: "Memory", icon: MemoryStick, format: (v) => bytes(v), floor: 1024 * 1024 },
  { metric: "net_rx", title: "Network in", icon: ArrowDownToLine, format: rate, floor: 1024 },
  { metric: "net_tx", title: "Network out", icon: ArrowUpFromLine, format: rate, floor: 1024 },
  { metric: "disk_read", title: "Disk read", icon: HardDriveDownload, format: rate, floor: 1024 },
  { metric: "disk_write", title: "Disk write", icon: HardDriveUpload, format: rate, floor: 1024 },
];

const REFRESH: Record<RangeId, number> = { "1h": 10_000, "24h": 60_000, "7d": 300_000, "30d": 600_000 };

export function MonitoringTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack, 5000);
  return (
    <ServiceMonitoring
      org={org}
      target={{ app: app.name }}
      svc={serviceOf(stack.data, app.name)}
      loading={stack.isLoading}
      error={stack.error}
      memLimit={parseMemory(app.resources?.memory)}
      top={<AppUptimeCard org={org} app={app} />}
    />
  );
}

/**
 * A service's numbers now, its metrics history, and its replicas: an app's
 * Monitoring tab and a compose stack service's. `svc` is its stack_status
 * entry (undefined when not running).
 */
export function ServiceMonitoring({
  org,
  target,
  svc,
  loading,
  error,
  memLimit,
  top,
}: {
  org: string;
  target: MetricTarget;
  svc: StackDetail["services"][number] | undefined;
  loading: boolean;
  error: unknown;
  /** Each replica's memory limit, in bytes. */
  memLimit: number | null;
  /** Above the numbers (the app's uptime monitors). */
  top?: ReactNode;
}) {
  const [range, setRange] = useState<RangeId>("1h");
  const [split, setSplit] = useState<"sum" | "split">("sum");
  const instances = [...(svc?.instances ?? [])].toSorted((a, b) => a.slot - b.slot) as InstanceDetail[];

  if (error) return <QueryError error={error} />;
  const cpuNow = instances.reduce((n, i) => n + (i.cpu_pct ?? 0), 0);
  const memNow = instances.reduce((n, i) => n + (i.mem_bytes ?? 0), 0);
  const restarts = instances.reduce((n, i) => n + i.restarts, 0);

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      {top}
      <div className="grid grid-cols-2 gap-3 sm:gap-4 lg:grid-cols-4">
        <Stat loading={loading} icon={Cpu} label="CPU now" value={svc ? percent(cpuNow) : "–"} hint="All replicas; 100% is one core" />
        <Stat loading={loading} icon={MemoryStick} label="Memory now" value={svc ? bytes(memNow) : "–"} hint={memLimit ? `Limit ${bytes(memLimit)} each` : "No limit set"} />
        <Stat
          loading={loading}
          icon={HeartPulse}
          label="Healthy"
          value={svc ? `${svc.healthy}/${svc.replicas}` : "–"}
          hint={svc?.state ?? "Not running"}
          tone={!svc ? undefined : svc.healthy === svc.replicas ? "success" : svc.healthy === 0 ? "danger" : "warning"}
        />
        <Stat loading={loading} icon={RotateCw} label="Restarts" value={svc ? String(restarts) : "–"} hint="Since each replica started" tone={restarts > 0 ? "warning" : undefined} />
      </div>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Segmented value={range} onChange={setRange} label="Range" options={RANGES.map((r) => ({ value: r.id, label: r.id }))} />
        <Segmented
          value={split}
          onChange={setSplit}
          label="Replicas"
          options={[
            { value: "sum", label: "Summed" },
            { value: "split", label: "Per replica" },
          ]}
        />
        <span className="text-xs text-muted-foreground sm:ml-auto">Kept 24 h at 10 s, 7 days at 1 min, 30 days at 10 min.</span>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        {CHARTS.map((c) => (
          <ChartCard
            key={c.metric}
            org={org}
            target={target}
            chart={c}
            range={range}
            split={split === "split"}
            max={c.metric === "memory" && split === "split" && memLimit ? memLimit : undefined}
          />
        ))}
      </div>

      {svc ? (
        <Card className="gap-0 overflow-hidden py-0">
          <div className="flex items-center justify-between gap-3 border-b px-5 py-4">
            <div className="space-y-1">
              <h2 className="text-[15px] font-semibold tracking-tight">Replicas now</h2>
              <p className="text-[13px] text-muted-foreground">Each replica's state, refreshed every 5 seconds.</p>
            </div>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{instances.length} total</span>
          </div>
          {/* Phones: one stacked row per replica. */}
          <ul className="divide-y sm:hidden">
            {instances.map((i) => (
              <li key={i.name} className="grid gap-2 px-5 py-3 text-sm">
                <div className="flex items-center gap-2">
                  <StatusDot tone={replicaTone(i)} />
                  <span className="font-medium">Replica {i.slot}</span>
                  <span className="ml-auto">
                    <RotationBadge on={i.in_rotation} />
                  </span>
                </div>
                <dl className="grid grid-cols-3 gap-x-4 gap-y-1.5 text-xs">
                  <Info k="Status" v={i.status} />
                  <Info k="CPU" v={percent(i.cpu_pct)} />
                  <Info k="Memory" v={bytes(i.mem_bytes)} />
                  <Info k="Health" v={i.health === "none" ? "No check" : i.health} />
                  <Info k="Restarts" v={String(i.restarts)} />
                  <Info k="Address" v={i.ip ?? "–"} mono />
                </dl>
                {i.last_probe && i.health === "unhealthy" && <Probe text={i.last_probe} />}
              </li>
            ))}
          </ul>
          <div className="hidden sm:block">
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead className="pl-5">Replica</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Health</TableHead>
                  <TableHead className="text-right">CPU</TableHead>
                  <TableHead className="text-right">Memory</TableHead>
                  <TableHead className="text-right">Restarts</TableHead>
                  <TableHead>Address</TableHead>
                  <TableHead className="pr-5 text-right">Traffic</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {instances.map((i) => (
                  <Fragment key={i.name}>
                    <TableRow>
                      <TableCell className="pl-5">
                        <div className="flex items-center gap-2.5">
                          <StatusDot tone={replicaTone(i)} />
                          <div className="min-w-0">
                            <p className="font-medium">Replica {i.slot}</p>
                            <p className="max-w-56 truncate font-mono text-xs text-muted-foreground">{i.name}</p>
                          </div>
                        </div>
                      </TableCell>
                      <TableCell>{i.status}</TableCell>
                      <TableCell className="text-muted-foreground">{i.health === "none" ? "No check" : i.health}</TableCell>
                      <TableCell className="text-right tabular-nums">{percent(i.cpu_pct)}</TableCell>
                      <TableCell className="text-right tabular-nums">{bytes(i.mem_bytes)}</TableCell>
                      <TableCell className="text-right tabular-nums">{i.restarts}</TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">{i.ip ?? "–"}</TableCell>
                      <TableCell className="pr-5 text-right">
                        <RotationBadge on={i.in_rotation} />
                      </TableCell>
                    </TableRow>
                    {i.last_probe && i.health === "unhealthy" && (
                      <TableRow className="hover:bg-transparent">
                        <TableCell colSpan={8} className="px-5 pt-0 whitespace-normal">
                          <Probe text={i.last_probe} />
                        </TableCell>
                      </TableRow>
                    )}
                  </Fragment>
                ))}
              </TableBody>
            </Table>
          </div>
        </Card>
      ) : (
        !loading && (
          <Card className="py-0">
            <EmptyState icon={Activity} title="Not running" compact>
              The charts show what was recorded while it ran.
            </EmptyState>
          </Card>
        )
      )}
    </div>
  );
}

function replicaTone(i: InstanceDetail): Tone {
  if (i.health === "unhealthy") return "danger";
  if (i.status !== "Running") return "warning";
  if (i.health === "starting") return "info";
  return "success";
}

function RotationBadge({ on }: { on: boolean }) {
  return <StatusBadge tone={on ? "success" : "muted"}>{on ? "In rotation" : "Out of rotation"}</StatusBadge>;
}

function Probe({ text }: { text: string }) {
  return <pre className="max-h-24 overflow-auto rounded-md border border-destructive/20 bg-destructive/5 p-2 font-mono text-xs whitespace-pre-wrap text-destructive">{text}</pre>;
}

function ChartCard({
  org,
  target,
  chart,
  range,
  split,
  max,
}: {
  org: string;
  target: MetricTarget;
  chart: (typeof CHARTS)[number];
  range: RangeId;
  split: boolean;
  max?: number;
}) {
  const q = useMetric(org, target, chart.metric, range, REFRESH[range]);
  const app = targetLabel(target);
  const shaped = useMemo(() => {
    if (!q.data) return null;
    const g = toGrid(q.data.series, q.data.from, q.data.to, q.data.step);
    const lines: Line[] = split
      ? g.lines.map((l, i) => ({ name: replicaLabel(l.name, q.data.series[i]?.service ?? app), values: bridge(l.values) }))
      : [{ name: "All replicas", values: bridge(g.total) }];
    return { times: g.times, lines, now: latest(g.total), step: q.data.step };
  }, [q.data, split, app]);
  const Icon = chart.icon;

  return (
    <Card className="gap-4 px-5 py-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <p className="flex items-center gap-1.5 text-[13px] font-medium text-muted-foreground">
            <Icon className="size-3.5" />
            {chart.title}
          </p>
          {shaped ? <p className="text-2xl font-semibold tracking-tight tabular-nums">{chart.format(shaped.now)}</p> : <Skeleton className="h-8 w-24" />}
        </div>
        {shaped && (
          <span className="shrink-0 pt-0.5 text-[11px] text-muted-foreground tabular-nums">
            {range} · per {shaped.step >= 60 ? `${Math.round(shaped.step / 60)} min` : `${shaped.step} s`}
          </span>
        )}
      </div>
      {q.error ? (
        <QueryError error={q.error} />
      ) : !shaped ? (
        <Skeleton className="h-[180px] rounded-md" />
      ) : (
        <MetricChart times={shaped.times} lines={shaped.lines} format={chart.format} label={`${chart.title} of ${app}, last ${range}`} floor={chart.floor} max={max} filled={!split} />
      )}
      {shaped && split && shaped.lines.length > 0 && (
        <ul className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground" aria-label="Legend">
          {shaped.lines.map((l, i) => (
            <li key={l.name} className="flex min-w-0 items-center gap-1.5">
              <span className={cn("size-2 shrink-0 rounded-full bg-current", SERIES[i % SERIES.length])} />
              <span className="truncate">{l.name}</span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function Info({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-muted-foreground">{k}</dt>
      <dd className={cn("truncate tabular-nums", mono && "font-mono")}>{v}</dd>
    </div>
  );
}

function Stat({ icon: Icon, label, value, hint, tone, loading }: { icon: typeof Cpu; label: string; value: string; hint?: string; tone?: Tone; loading?: boolean }) {
  return (
    <Card className="gap-1.5 px-4 py-4 sm:px-5">
      <div className="flex items-center justify-between gap-2 text-[13px] font-medium text-muted-foreground">
        <span className="truncate">{label}</span>
        <Icon className="size-4 shrink-0" />
      </div>
      {loading ? (
        <Skeleton className="h-8 w-20" />
      ) : (
        <div className={cn("text-2xl font-semibold tracking-tight tabular-nums", tone && tone !== "success" && TONE_TEXT[tone])}>{value}</div>
      )}
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
