// Monitor > Orgs: each org's live use beside its limits. An org's limits
// are incus budgets on what its instances are allocated (stopped ones
// included), not on what they use, so each bar shows both: live use from
// host_monitor, allocated against the limit from org_list.
import { useQuery } from "@tanstack/react-query";
import { Building2, TriangleAlert } from "lucide-react";
import { type ReactNode, useMemo, useState } from "react";
import { Link } from "react-router";
import { callTool, type Monitor, type OrgView } from "@/api/tools";
import { Empty, Panel } from "@/components/confirm";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Segmented } from "@/apps/segmented";
import { errorMessage } from "@/lib/messages";
import { type Budget, bps, cores, meterTone, MONITOR_POLL, type OrgSort, type OrgUsage, orgUsage, pctText, pickOrgs, plate, size, sparkPoints } from "@/lib/monitor";
import { cn } from "@/lib/utils";
import { Sparkline } from "@/uptime/components";

/** The live window asked of host_monitor: the shortest, as only its latest sample is shown. */
const RANGE = 60;

const SORTS: { value: OrgSort; label: string }[] = [
  { value: "pressure", label: "Sort: pressure" },
  { value: "cpu", label: "Sort: CPU" },
  { value: "mem", label: "Sort: memory" },
  { value: "name", label: "Sort: name" },
];

type Host = { cpus: number; mem_total: number };

/** org_list and host_monitor, polled together. */
export function useOrgUsage(enabled: boolean) {
  const list = useQuery({
    enabled,
    queryKey: ["tool", "org_list"],
    queryFn: () => callTool<{ orgs: OrgView[] }>("org_list"),
    refetchInterval: 10_000,
  });
  const live = useQuery({
    enabled,
    queryKey: ["tool", "host_monitor", RANGE],
    queryFn: () => callTool<Monitor, string>("host_monitor", { range: RANGE }),
    refetchInterval: MONITOR_POLL,
  });
  const m = live.data;
  const usage = useMemo(() => orgUsage(list.data?.orgs ?? [], m?.instances ?? null), [list.data, m]);
  return { list, usage, host: m?.host, at: live.dataUpdatedAt || list.dataUpdatedAt };
}

export type OrgUsageQuery = ReturnType<typeof useOrgUsage>;

export function OrgsMonitor({ data, layout, setLayout }: { data: OrgUsageQuery; layout: "cards" | "table"; setLayout: (l: "cards" | "table") => void }) {
  const { list, usage, host } = data;
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<OrgSort>("pressure");
  const rows = useMemo(() => pickOrgs(usage, q, sort), [usage, q, sort]);

  if (list.error && !list.data)
    return (
      <Panel title="Orgs">
        <Empty icon={<Building2 />} title="Couldn't load the orgs">
          {errorMessage(list.error)}
        </Empty>
      </Panel>
    );
  if (!list.data)
    return (
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
        {Array.from({ length: 3 }, (_, i) => (
          <Skeleton key={i} className="h-72 rounded-xl" />
        ))}
      </div>
    );

  const running = usage.reduce((a, o) => a + o.running, 0);
  const total = usage.reduce((a, o) => a + o.total, 0);
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-[13px] text-muted-foreground tabular-nums">
          {usage.length} {usage.length === 1 ? "org" : "orgs"} · {total} instances ({running} running)
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter orgs" className="h-8 w-full sm:w-48" aria-label="Filter orgs" />
          <Select value={sort} onValueChange={(v) => setSort(v as OrgSort)}>
            <SelectTrigger size="sm" aria-label="Sort">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SORTS.map((s) => (
                <SelectItem key={s.value} value={s.value}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Segmented
            label="Layout"
            value={layout}
            onChange={setLayout}
            options={[
              { value: "cards", label: "Cards" },
              { value: "table", label: "Table" },
            ]}
          />
        </div>
      </div>
      {rows.length === 0 ? (
        <Panel title="Orgs">
          <Empty icon={<Building2 />} title={q ? "Nothing matches" : "No orgs"} />
        </Panel>
      ) : layout === "cards" ? (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
          {rows.map((o) => (
            <OrgCard key={o.name} o={o} host={host} />
          ))}
        </div>
      ) : (
        <OrgTable rows={rows} host={host} />
      )}
      <Legend />
    </div>
  );
}

/** The host tab, its instances filtered to the org. */
const instancesLink = (o: OrgUsage) => `/monitor?${new URLSearchParams({ org: o.name })}`;

function PressureTag({ pct }: { pct: number | null }) {
  if (pct === null) return <span className="text-xs text-muted-foreground">no limits</span>;
  const hot = pct >= 90;
  return (
    <span
      className={cn("inline-flex items-center gap-1 text-xs font-medium tabular-nums", hot ? "text-destructive" : pct >= 75 ? "text-warning" : "text-muted-foreground")}
      title="The fullest limit: what is allocated against it, of the limit"
    >
      {hot && <TriangleAlert className="size-3.5" />}
      {pctText(pct)}
    </span>
  );
}

function OrgCard({ o, host }: { o: OrgUsage; host?: Host }) {
  return (
    <Link
      to={instancesLink(o)}
      className={cn(
        plate,
        "flex min-w-0 flex-col gap-3 rounded-xl border bg-card px-4 py-3.5 shadow-xs transition-colors hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
      )}
    >
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="truncate font-medium">{o.name}</span>
        <PressureTag pct={o.pressure} />
      </div>
      <div className="-mt-2 text-xs text-muted-foreground tabular-nums">
        {o.total} {o.total === 1 ? "instance" : "instances"}
        {o.live && ` · ${o.running} running`}
      </div>
      <div className="grid gap-2.5">
        <Resource label="CPU" used={o.cpu.used} budget={o.cpu.budget} scale={host?.cpus} text={cpuText(o, host)} />
        <Resource label="Memory" used={o.mem.used} budget={o.mem.budget} scale={host?.mem_total} text={memText(o, host)} />
        <Resource label="Disk" used={null} budget={o.disk.budget} text={o.disk.budget ? `${size(o.disk.budget.allocated)} / ${size(o.disk.budget.limit)} allocated` : "no limit"} />
        <Resource label="Instances" used={null} budget={o.instances.budget} text={o.instances.budget ? `${o.instances.budget.allocated} / ${o.instances.budget.limit}` : `${o.total} · no limit`} />
      </div>
      {o.live && o.running === 0 ? (
        <div className="border-t pt-2.5 text-xs text-muted-foreground">Nothing running</div>
      ) : o.live ? (
        <div className="grid gap-1 border-t pt-2.5 text-xs text-muted-foreground tabular-nums">
          <div className="flex items-center gap-2">
            <span className="w-8 shrink-0">cpu</span>
            <Sparkline points={sparkPoints(o.cpu_history)} max={Math.max(100, ...o.cpu_history)} label={`${o.name} CPU over the last samples`} className="h-5 min-w-0 flex-1 text-brand" />
          </div>
          <div className="truncate">
            net ↓ {bps(o.net_rx)} <span className="ml-1">↑ {bps(o.net_tx)}</span>
          </div>
        </div>
      ) : (
        <div className="border-t pt-2.5 text-xs text-muted-foreground">No live use yet</div>
      )}
    </Link>
  );
}

function cpuText(o: OrgUsage, host?: Host): string {
  const used = o.cpu.used === null ? "—" : cores(o.cpu.used);
  const b = o.cpu.budget;
  if (b) return `${used} used · ${b.allocated} / ${b.limit} allocated`;
  return host ? `${used} used of ${host.cpus} on the host` : `${used} used`;
}

function memText(o: OrgUsage, host?: Host): string {
  const used = o.mem.used === null ? "—" : size(o.mem.used);
  const b = o.mem.budget;
  if (b) return `${used} used · ${size(b.allocated)} / ${size(b.limit)} allocated`;
  return host ? `${used} used of ${size(host.mem_total)} on the host` : `${used} used`;
}

function Resource({ label, used, budget, scale, text }: { label: string; used: number | null; budget?: Budget; scale?: number; text: string }) {
  return (
    <div className="grid gap-1">
      <div className="flex items-baseline justify-between gap-2 text-[13px]">
        <span className="font-medium">{label}</span>
        <span className="truncate text-xs text-muted-foreground tabular-nums">{text}</span>
      </div>
      <UsageBar used={used} budget={budget} scale={scale} />
    </div>
  );
}

/**
 * Live use solid over what is allocated, faint, against the limit (the
 * bar's full width). Without a limit, use is against `scale` (the host's
 * size); with neither, there is no bar.
 */
function UsageBar({ used, budget, scale }: { used: number | null; budget?: Budget; scale?: number }) {
  const whole = budget?.limit || scale;
  if (!whole) return <div className="h-1.5 rounded-full bg-muted/50" />;
  const pct = (n: number) => Math.min(100, (n / whole) * 100);
  const alloc = budget ? pct(budget.allocated) : null;
  const live = used === null ? null : pct(used);
  return (
    <div className="relative h-1.5 overflow-hidden rounded-full bg-muted">
      {alloc !== null && <div className={cn("absolute inset-y-0 left-0 rounded-full opacity-35", meterTone(alloc))} style={{ width: `${alloc}%` }} />}
      {live !== null && <div className="absolute inset-y-0 left-0 rounded-full bg-brand" style={{ width: `${live > 0 ? Math.max(live, 2) : 0}%` }} />}
    </div>
  );
}

function Legend() {
  return (
    <p className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
      <span className="inline-flex items-center gap-1.5">
        <span className="h-1.5 w-4 rounded-full bg-brand" /> live use
      </span>
      <span className="inline-flex items-center gap-1.5">
        <span className="h-1.5 w-4 rounded-full bg-brand opacity-35" /> allocated
      </span>
      <span className="inline-flex items-center gap-1.5">
        <span className="h-1.5 w-4 rounded-full bg-muted" /> free, up to the limit
      </span>
      <span>Limits cap what instances are allocated, stopped ones included, not what they use.</span>
    </p>
  );
}

function OrgTable({ rows, host }: { rows: OrgUsage[]; host?: Host }) {
  return (
    <Panel title="Orgs" className={plate}>
      <Table className="min-w-[56rem]">
        <TableHeader className="bg-muted/30">
          <TableRow className="hover:bg-transparent">
            <TableHead className="pl-5">Org</TableHead>
            <TableHead className="text-right">Instances</TableHead>
            <TableHead className="w-48">CPU</TableHead>
            <TableHead className="w-72">Memory</TableHead>
            <TableHead className="w-40">Disk allocated</TableHead>
            <TableHead className="text-right">Net ↓ / ↑</TableHead>
            <TableHead className="pr-5 text-right">Pressure</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((o) => {
            const ib = o.instances.budget;
            return (
              <TableRow key={o.name}>
                <TableCell className="py-2 pl-5">
                  <Link to={instancesLink(o)} className="font-medium hover:underline focus-visible:underline focus-visible:outline-none">
                    {o.name}
                  </Link>
                </TableCell>
                <TableCell className="text-right text-[13px] whitespace-nowrap tabular-nums">
                  {o.live ? `${o.running}/${o.total}` : o.total}
                  {ib && <span className="text-muted-foreground"> of {ib.limit}</span>}
                </TableCell>
                <TableCell>
                  <Cell text={cpuText(o, host)}>
                    <UsageBar used={o.cpu.used} budget={o.cpu.budget} scale={host?.cpus} />
                  </Cell>
                </TableCell>
                <TableCell>
                  <Cell text={memText(o, host)}>
                    <UsageBar used={o.mem.used} budget={o.mem.budget} scale={host?.mem_total} />
                  </Cell>
                </TableCell>
                <TableCell>
                  <Cell text={o.disk.budget ? `${size(o.disk.budget.allocated)} / ${size(o.disk.budget.limit)}` : "no limit"}>
                    <UsageBar used={null} budget={o.disk.budget} />
                  </Cell>
                </TableCell>
                <TableCell className="text-right text-xs whitespace-nowrap tabular-nums">
                  {bps(o.net_rx)} / {bps(o.net_tx)}
                </TableCell>
                <TableCell className="pr-5 text-right">
                  <PressureTag pct={o.pressure} />
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </Panel>
  );
}

function Cell({ text, children }: { text: string; children: ReactNode }) {
  return (
    <div className="grid gap-1">
      <span className="truncate text-xs text-muted-foreground tabular-nums">{text}</span>
      {children}
    </div>
  );
}
