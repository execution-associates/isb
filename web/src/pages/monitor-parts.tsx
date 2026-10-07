// The Monitor page's panels for one server: its summary line, CPU, memory,
// network, disk, and the instances table.
import { Boxes, Cpu, HardDrive, MemoryStick, Network } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import type { Monitor, MonitorHost, MonitorInstance } from "@/api/tools";
import { MetricChart } from "@/apps/metric-chart";
import { Empty, Panel } from "@/components/confirm";
import { StatusBadge } from "@/components/status";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  bps,
  historySeries,
  type InstanceSort,
  instanceLink,
  instanceOrgs,
  isRunning,
  load,
  pctText,
  pickInstances,
  plate,
  sparkPoints,
  uptime,
} from "@/lib/monitor";
import { meterTone, percent, size } from "@/lib/servers";
import { cn } from "@/lib/utils";
import { Tag } from "@/pages/org-ui";
import { Sparkline } from "@/uptime/components";

const sizeOrDash = (n: number | null | undefined) => (n == null ? "—" : size(n));

/** hostname · vCPUs · memory · load · uptime · isb. */
export function HostLine({ h }: { h: MonitorHost }) {
  const parts = [
    `${h.cpus} vCPU`,
    sizeOrDash(h.mem_total),
    `load ${load(h.load1)} ${load(h.load5)} ${load(h.load15)}`,
    `up ${uptime(h.uptime_secs)}`,
    h.isb ? `isb ${h.isb}` : null,
  ].filter(Boolean);
  return (
    <p className="-mb-2 flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-[13px] text-muted-foreground tabular-nums">
      <span className="font-semibold text-foreground">{h.hostname}</span>
      {parts.map((p) => (
        <span key={p} className="before:mr-2 before:content-['·']">
          {p}
        </span>
      ))}
    </p>
  );
}

/** A big number for a panel's header. */
function Figure({ children }: { children: React.ReactNode }) {
  return <span className="text-lg font-semibold tracking-tight tabular-nums">{children}</span>;
}

export function CpuPanel({ m }: { m: Monitor }) {
  const s = historySeries(m.history.points);
  const cores = m.host.cpu_cores;
  return (
    <Panel className={plate} title="CPU" icon={<Cpu />} action={<Figure>{pctText(m.host.cpu_pct)}</Figure>}>
      <div className="grid gap-4 p-5">
        <MetricChart times={s.times} lines={[{ name: "CPU", values: s.cpu }]} format={pctText} label="CPU use over the range" max={100} filled />
        {cores.length > 0 && (
          <div className="grid gap-1.5">
            <div className="flex items-baseline justify-between text-xs text-muted-foreground">
              <span>Per core</span>
              <span className="tabular-nums">{cores.length} cores</span>
            </div>
            <div className="flex h-10 items-end gap-[2px]" role="img" aria-label={`Use of ${cores.length} cores`}>
              {cores.map((c, i) => (
                <Tooltip key={i}>
                  <TooltipTrigger asChild>
                    <span className="flex h-full min-w-[3px] flex-1 items-end rounded-[2px] bg-muted">
                      <span className={cn("block w-full rounded-[2px]", meterTone(c))} style={{ height: `${Math.max(4, Math.min(100, c))}%` }} />
                    </span>
                  </TooltipTrigger>
                  <TooltipContent>
                    Core {i}: {pctText(c)}
                  </TooltipContent>
                </Tooltip>
              ))}
            </div>
          </div>
        )}
      </div>
    </Panel>
  );
}

/** A labelled usage bar with used / total under it. */
function UsageBar({ label, used, total, extra }: { label: React.ReactNode; used: number | null | undefined; total: number | null | undefined; extra?: React.ReactNode }) {
  const pct = percent(used, total);
  return (
    <div className="grid gap-1.5">
      <div className="flex min-w-0 items-center justify-between gap-2 text-[13px]">
        <span className="flex min-w-0 items-center gap-2 font-medium">{label}</span>
        <span className="tabular-nums">{pctText(pct)}</span>
      </div>
      <span className="h-2 overflow-hidden rounded-full bg-muted">
        {pct !== null && <span className={cn("block h-full rounded-full", meterTone(pct))} style={{ width: `${Math.max(pct, 2)}%` }} />}
      </span>
      <div className="flex justify-between gap-2 text-xs text-muted-foreground tabular-nums">
        <span>{total ? `${sizeOrDash(used)} / ${size(total)}` : "—"}</span>
        {extra}
      </div>
    </div>
  );
}

export function MemoryPanel({ m }: { m: Monitor }) {
  const h = m.host;
  const s = historySeries(m.history.points);
  return (
    <Panel className={plate} title="Memory" icon={<MemoryStick />} action={<Figure>{pctText(percent(h.mem_used, h.mem_total))}</Figure>}>
      <div className="grid gap-4 p-5">
        <UsageBar label="RAM" used={h.mem_used} total={h.mem_total} />
        <UsageBar label="Swap" used={h.swap_used} total={h.swap_total} extra={h.swap_total === 0 ? <span>no swap</span> : undefined} />
        <MetricChart times={s.times} lines={[{ name: "Used", values: s.mem }]} format={sizeOrDash} label="Memory used over the range" max={h.mem_total || undefined} filled />
      </div>
    </Panel>
  );
}

export function NetworkPanel({ m }: { m: Monitor }) {
  const h = m.host;
  const s = historySeries(m.history.points);
  return (
    <Panel
      className={plate}
      title="Network"
      icon={<Network />}
      action={
        <span className="flex gap-3 text-[13px] font-medium tabular-nums">
          <span>↓ {bps(h.net_rx_rate)}</span>
          <span>↑ {bps(h.net_tx_rate)}</span>
        </span>
      }
    >
      <div className="p-5 pb-3">
        <MetricChart
          times={s.times}
          lines={[
            { name: "↓ Received", values: s.rx },
            { name: "↑ Sent", values: s.tx },
          ]}
          format={bps}
          label="Network traffic over the range"
          floor={1000}
          filled
        />
      </div>
      {h.interfaces.length > 0 && (
        <div className="divide-y border-t">
          {h.interfaces.map((i) => (
            <div key={i.name} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-0.5 px-5 py-2 text-[13px] sm:grid-cols-[8rem_minmax(0,1fr)_auto]">
              <span className="flex min-w-0 items-center gap-2">
                <span className="truncate font-mono text-[12px]">{i.name}</span>
                {!i.up && <StatusBadge tone="muted">down</StatusBadge>}
              </span>
              <span className="order-last col-span-2 truncate font-mono text-[12px] text-muted-foreground sm:order-none sm:col-span-1" title={i.addresses.join("\n")}>
                {i.addresses[0] ?? "no address"}
                {i.addresses.length > 1 && ` +${i.addresses.length - 1}`}
              </span>
              <span className="flex justify-end gap-3 text-xs text-muted-foreground tabular-nums">
                <span>↓ {bps(i.rx_rate)}</span>
                <span>↑ {bps(i.tx_rate)}</span>
              </span>
            </div>
          ))}
        </div>
      )}
    </Panel>
  );
}

export function DiskPanel({ m }: { m: Monitor }) {
  const h = m.host;
  const pools = h.pools.length ? h.pools : [{ name: "Disk", driver: "", used: h.disk_used, total: h.disk_total }];
  return (
    <Panel className={plate} title="Disk" icon={<HardDrive />} action={<Figure>{pctText(percent(h.disk_used, h.disk_total))}</Figure>}>
      <div className="grid gap-4 p-5">
        {pools.map((p) => (
          <UsageBar
            key={p.name}
            label={
              <>
                <span className="truncate font-mono text-[12px]">{p.name}</span>
                {p.driver && <Tag mono>{p.driver}</Tag>}
              </>
            }
            used={p.used}
            total={p.total}
          />
        ))}
      </div>
      <div className="grid grid-cols-2 divide-x border-t text-[13px] tabular-nums">
        <div className="px-5 py-2.5">
          <div className="text-xs text-muted-foreground">Read</div>
          <div className="font-medium">{bps(h.disk_read_rate)}</div>
        </div>
        <div className="px-5 py-2.5">
          <div className="text-xs text-muted-foreground">Write</div>
          <div className="font-medium">{bps(h.disk_write_rate)}</div>
        </div>
      </div>
    </Panel>
  );
}

const SORTS: { value: InstanceSort; label: string }[] = [
  { value: "cpu", label: "Sort: CPU" },
  { value: "mem", label: "Sort: memory" },
  { value: "net", label: "Sort: network" },
  { value: "name", label: "Sort: name" },
];

const ALL_ORGS = "*";

function kindTag(kind: string): string | null {
  if (kind === "virtual-machine") return "VM";
  if (kind === "oci") return "OCI";
  return null;
}

/** `org` starts the org filter there (the Orgs tab links here with it). */
export function InstancesPanel({ m, partial, org: initialOrg = null }: { m: Monitor; partial: boolean; org?: string | null }) {
  const [q, setQ] = useState("");
  const [org, setOrg] = useState<string | null>(initialOrg);
  const [sort, setSort] = useState<InstanceSort>("cpu");
  const [all, setAll] = useState(false);
  const navigate = useNavigate();
  const orgs = useMemo(() => instanceOrgs(m.instances), [m.instances]);
  const rows = useMemo(() => pickInstances(m.instances, { q, org, all }, sort), [m.instances, q, org, all, sort]);
  const running = m.instances.filter(isRunning).length;
  return (
    <Panel
      className={plate}
      title={
        <>
          Instances
          <span className="text-[13px] font-normal text-muted-foreground tabular-nums">{running} running</span>
        </>
      }
      icon={<Boxes />}
      action={
        <>
          <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter by name, org, stack" className="h-8 w-full sm:w-56" aria-label="Filter instances" />
          <Select value={org ?? ALL_ORGS} onValueChange={(v) => setOrg(v === ALL_ORGS ? null : v)}>
            <SelectTrigger size="sm" aria-label="Org" className="min-w-28">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL_ORGS}>All orgs</SelectItem>
              {orgs.map((o) => (
                <SelectItem key={o} value={o}>
                  {o}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={sort} onValueChange={(v) => setSort(v as InstanceSort)}>
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
          <div className="flex h-8 items-center gap-2 rounded-md border px-2.5">
            <Switch id="monitor-all" checked={all} onCheckedChange={setAll} />
            <Label htmlFor="monitor-all" className="text-[13px] font-normal">
              Stopped too
            </Label>
          </div>
        </>
      }
    >
      {partial ? (
        <Empty icon={<Boxes />} title="No instance detail">
          This server's isb is too old to report its instances.
        </Empty>
      ) : rows.length === 0 ? (
        <Empty icon={<Boxes />} title={q || org || (!all && m.instances.length) ? "Nothing matches" : "No instances"} />
      ) : (
        <Table className="min-w-[46rem]">
          <TableHeader className="bg-muted/30">
            <TableRow className="hover:bg-transparent">
              <TableHead className="pl-5">Instance</TableHead>
              <TableHead>Org</TableHead>
              <TableHead className="w-44">CPU</TableHead>
              <TableHead className="text-right">Memory</TableHead>
              <TableHead className="text-right">Net ↓ / ↑</TableHead>
              <TableHead className="pr-5 text-right">Disk read / write</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((i) => (
              <InstanceRow key={`${i.project}/${i.name}`} i={i} onOpen={(to) => navigate(to)} />
            ))}
          </TableBody>
        </Table>
      )}
    </Panel>
  );
}

function InstanceRow({ i, onOpen }: { i: MonitorInstance; onOpen: (to: string) => void }) {
  const to = instanceLink(i);
  const kind = kindTag(i.kind);
  const top = Math.max(100, ...i.cpu_history);
  return (
    <TableRow className={cn(to && "cursor-pointer", !isRunning(i) && "text-muted-foreground")} onClick={to ? () => onOpen(to) : undefined}>
      <TableCell className="max-w-64 py-2 pl-5">
        <div className="flex min-w-0 items-center gap-2">
          {to ? (
            <Link to={to} onClick={(e) => e.stopPropagation()} className="truncate font-medium hover:underline focus-visible:underline focus-visible:outline-none">
              {i.name}
            </Link>
          ) : (
            <span className="truncate font-medium">{i.name}</span>
          )}
          {kind && <Tag>{kind}</Tag>}
          {!isRunning(i) && <StatusBadge tone="muted">{i.status}</StatusBadge>}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {i.stack ? <span className="font-mono">stack {i.stack}</span> : <span className="font-mono">{i.project}</span>}
          {i.ip && <span className="font-mono"> · {i.ip}</span>}
        </div>
      </TableCell>
      <TableCell className="text-[13px]">{i.org ?? <span className="text-muted-foreground">—</span>}</TableCell>
      <TableCell>
        <div className="flex items-center gap-2">
          <Sparkline points={sparkPoints(i.cpu_history)} max={top} label={`${i.name} CPU over the last samples`} className="h-5 w-24 text-brand" />
          <span className="w-12 text-right text-[13px] tabular-nums">{pctText(i.cpu_pct)}</span>
        </div>
      </TableCell>
      <TableCell className="text-right text-[13px] tabular-nums">{sizeOrDash(i.mem_bytes)}</TableCell>
      <TableCell className="text-right text-xs whitespace-nowrap tabular-nums">
        {bps(i.net_rx_rate)} / {bps(i.net_tx_rate)}
      </TableCell>
      <TableCell className="pr-5 text-right text-xs whitespace-nowrap tabular-nums">
        {bps(i.disk_read_rate)} / {bps(i.disk_write_rate)}
      </TableCell>
    </TableRow>
  );
}
