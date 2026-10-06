// Platform > Monitor: live resource use of this host and every remote
// server, like `bottom`. One card per server; the selected one fills the
// panels below. host_monitor is polled every 2s while the tab is visible.
import { useQuery } from "@tanstack/react-query";
import { ServerOff, TriangleAlert } from "lucide-react";
import { useEffect, useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";
import { callTool, type HostMonitor, type MonitorServer } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Empty, Panel } from "@/components/confirm";
import { StatusBadge, StatusDot } from "@/components/status";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Skeleton } from "@/components/ui/skeleton";
import { Segmented } from "@/apps/segmented";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { bps, MONITOR_POLL, MONITOR_RANGES, parseRange, pctText, plate, serverKind, sparkPoints, STALE_AFTER } from "@/lib/monitor";
import { healthTone, meterTone, percent } from "@/lib/servers";
import { useMe } from "@/lib/session";
import { cn } from "@/lib/utils";
import { CpuPanel, DiskPanel, HostLine, InstancesPanel, MemoryPanel, NetworkPanel } from "@/pages/monitor-parts";
import { Sparkline } from "@/uptime/components";

function useMonitor(server: string | null, range: number, enabled: boolean) {
  return useQuery({
    enabled,
    queryKey: ["tool", "host_monitor", server, range],
    queryFn: () => callTool<HostMonitor, string>("host_monitor", server ? { server, range } : { range }),
    refetchInterval: MONITOR_POLL,
    // Another range of the same server keeps showing; another server does not.
    placeholderData: (prev, q) => (q && q.queryKey[2] === server ? prev : undefined),
  });
}

/** Now in ms, ticking each second. */
function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  return now;
}

export function MonitorPage() {
  const me = useMe().data!;
  const [params, setParams] = useSearchParams();
  const server = params.get("server");
  const range = parseRange(params.get("range"));
  const q = useMonitor(server, range, !!me.superadmin);
  if (!me.superadmin) return <Navigate to="/" replace />;

  const set = (key: string, value: string | null) =>
    setParams(
      (p) => {
        const next = new URLSearchParams(p);
        if (value === null) next.delete(key);
        else next.set(key, value);
        return next;
      },
      { replace: key === "range" },
    );
  const d = q.data;
  return (
    <>
      <PageHeader
        title={
          <>
            Monitor
            <StatusBadge tone="warning">Superadmin</StatusBadge>
          </>
        }
        description="CPU, memory, network and disk of this host and every connected server, live, and what each instance uses."
        actions={
          <>
            <Live updatedAt={q.dataUpdatedAt} />
            <Segmented
              label="Range"
              value={String(range)}
              onChange={(v) => set("range", v === String(300) ? null : v)}
              options={MONITOR_RANGES.map((r) => ({ value: String(r.value), label: r.label }))}
            />
          </>
        }
      />
      {q.error && !d ? (
        <Panel title="Monitor">
          <Empty icon={<ServerOff />} title="Couldn't load the monitor">
            {errorMessage(q.error)}
          </Empty>
        </Panel>
      ) : !d ? (
        <MonitorSkeleton />
      ) : (
        <div className="grid gap-6">
          {d.servers.length > 1 && <ServerCards servers={d.servers} selected={d.server} onSelect={(s) => set("server", s.local ? null : s.name)} />}
          {d.monitor ? (
            <>
              {d.partial && (
                <Alert role="note">
                  <TriangleAlert />
                  <AlertDescription>
                    <p>
                      {d.server}'s isb is too old for live detail: these are its heartbeat's numbers, without history or instances. Upgrade it from{" "}
                      <Link to="/admin/servers" className="font-medium text-foreground underline underline-offset-2">
                        Orgs, users, servers
                      </Link>
                      .
                    </p>
                  </AlertDescription>
                </Alert>
              )}
              <HostLine h={d.monitor.host} />
              <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
                <CpuPanel m={d.monitor} />
                <MemoryPanel m={d.monitor} />
              </div>
              <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
                <NetworkPanel m={d.monitor} />
                <DiskPanel m={d.monitor} />
              </div>
              <InstancesPanel key={d.server} m={d.monitor} partial={d.partial} />
            </>
          ) : (
            <Panel title={d.server}>
              <Empty icon={<ServerOff />} title={`Can't reach ${d.server}`}>
                {d.error ?? "The server did not answer."}
              </Empty>
            </Panel>
          )}
        </div>
      )}
    </>
  );
}

/** A dot that pulses while the last answer is fresh, and says so when it is not. */
function Live({ updatedAt }: { updatedAt: number }) {
  const now = useNow();
  const fresh = updatedAt > 0 && now - updatedAt <= STALE_AFTER;
  return (
    <span
      className="inline-flex h-8 items-center gap-2 rounded-md border px-2.5 text-[13px] text-muted-foreground tabular-nums"
      title={updatedAt ? `Last updated ${new Date(updatedAt).toLocaleTimeString()}` : "Waiting for the first sample"}
    >
      <StatusDot tone={fresh ? "success" : "muted"} pulse={fresh} />
      {fresh ? "live" : "stale"}
      <span className="text-muted-foreground/70">{MONITOR_POLL / 1000}s</span>
    </span>
  );
}

function ServerCards({ servers, selected, onSelect }: { servers: MonitorServer[]; selected: string; onSelect: (s: MonitorServer) => void }) {
  return (
    <div className="grid grid-cols-1 gap-3 min-[420px]:grid-cols-2 md:grid-cols-[repeat(auto-fill,minmax(13.5rem,1fr))]" role="radiogroup" aria-label="Server">
      {servers.map((s) => (
        <ServerCard key={s.name} s={s} active={s.name === selected} onClick={() => onSelect(s)} />
      ))}
    </div>
  );
}

function ServerCard({ s, active, onClick }: { s: MonitorServer; active: boolean; onClick: () => void }) {
  const t = healthTone(s.state);
  const h = s.host;
  const mem = percent(h?.mem_used, h?.mem_total);
  const down = s.state !== "up";
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      onClick={onClick}
      className={cn(
        plate,
        "flex min-w-0 flex-col gap-2 rounded-xl border bg-card px-3.5 py-3 text-left shadow-xs transition-colors hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
        active && "border-brand/50 ring-2 ring-brand/25",
      )}
    >
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="truncate font-medium">{s.name}</span>
        <span className="inline-flex h-5 shrink-0 items-center rounded border bg-muted/50 px-1.5 font-mono text-[11px] text-muted-foreground">{serverKind(s)}</span>
      </div>
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <StatusDot tone={t.tone} pulse={s.state === "up"} />
        {t.label}
        {down && s.last_ok ? <span className="truncate">· last ok {relativeTime(s.last_ok)}</span> : null}
      </div>
      {h && !down ? (
        <div className="grid gap-1.5 text-xs tabular-nums">
          <div className="flex items-center gap-2">
            <span className="w-7 shrink-0 text-muted-foreground">cpu</span>
            <Sparkline points={sparkPoints(h.cpu_history ?? [])} max={100} label="CPU over the last samples" className="h-5 min-w-0 flex-1 text-brand" />
            <span className="w-9 shrink-0 text-right">{pctText(h.cpu_pct)}</span>
            <span className="w-8 shrink-0 text-right text-muted-foreground">{h.cpus ? `${h.cpus}c` : ""}</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="w-7 shrink-0 text-muted-foreground">mem</span>
            <span className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted">
              {mem !== null && <span className={cn("block h-full rounded-full", meterTone(mem))} style={{ width: `${Math.max(mem, 3)}%` }} />}
            </span>
            <span className="w-9 shrink-0 text-right">{pctText(mem)}</span>
            <span className="w-8 shrink-0" />
          </div>
          <div className="truncate text-muted-foreground">
            ↓ {bps(h.net_rx_rate)} <span className="ml-1">↑ {bps(h.net_tx_rate)}</span>
          </div>
        </div>
      ) : (
        <div className="text-xs text-muted-foreground">{down ? "No live numbers" : "No heartbeat yet"}</div>
      )}
    </button>
  );
}

function MonitorSkeleton() {
  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-1 gap-3 min-[420px]:grid-cols-2 md:grid-cols-4">
        {Array.from({ length: 3 }, (_, i) => (
          <Skeleton key={i} className="h-32 rounded-xl" />
        ))}
      </div>
      <Skeleton className="h-4 w-80 max-w-full" />
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Skeleton className="h-64 rounded-xl" />
        <Skeleton className="h-64 rounded-xl" />
      </div>
    </div>
  );
}
