// Platform > Monitor: live resource use of this host, like `bottom`.
// host_monitor is polled every 2s while the tab is visible. The Orgs tab
// (monitor-orgs.tsx) shows each org's use against its limits.
import { useQuery } from "@tanstack/react-query";
import { Building2, Server, ServerOff } from "lucide-react";
import { useEffect, useState } from "react";
import { Navigate, useParams, useSearchParams } from "react-router";
import { callTool, type Monitor } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Empty, Panel } from "@/components/confirm";
import { StatusBadge, StatusDot } from "@/components/status";
import { Skeleton } from "@/components/ui/skeleton";
import { TabLinks } from "@/apps/components";
import { Segmented } from "@/apps/segmented";
import { errorMessage } from "@/lib/messages";
import { MONITOR_POLL, MONITOR_RANGES, parseRange, STALE_AFTER } from "@/lib/monitor";
import { useMe } from "@/lib/session";
import { CpuPanel, DiskPanel, HostLine, InstancesPanel, MemoryPanel, NetworkPanel } from "@/pages/monitor-parts";
import { OrgsMonitor, useOrgUsage } from "@/pages/monitor-orgs";

function useMonitor(range: number, enabled: boolean) {
  return useQuery({
    enabled,
    queryKey: ["tool", "host_monitor", range],
    queryFn: () => callTool<Monitor, string>("host_monitor", { range }),
    refetchInterval: MONITOR_POLL,
    // Another range keeps showing the last answer until its own arrives.
    placeholderData: (prev) => prev,
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

const TABS = [
  { id: "host", label: "This host", to: "/monitor", icon: Server },
  { id: "orgs", label: "Orgs", to: "/monitor/orgs", icon: Building2 },
];

export function MonitorPage() {
  const me = useMe().data!;
  const { tab = "host" } = useParams();
  const [params, setParams] = useSearchParams();
  const range = parseRange(params.get("range"));
  const orgs = tab === "orgs";
  const q = useMonitor(range, !!me.superadmin && !orgs);
  const o = useOrgUsage(!!me.superadmin && orgs);
  if (!me.superadmin) return <Navigate to="/" replace />;
  if (!TABS.some((t) => t.id === tab)) return <Navigate to="/monitor" replace />;

  const set = (key: string, value: string | null) =>
    setParams(
      (p) => {
        const next = new URLSearchParams(p);
        if (value === null) next.delete(key);
        else next.set(key, value);
        return next;
      },
      { replace: key === "range" || key === "layout" },
    );
  const m = q.data;
  return (
    <>
      <PageHeader
        title={
          <>
            Monitor
            <StatusBadge tone="warning">Superadmin</StatusBadge>
          </>
        }
        description={
          orgs
            ? "What each org's instances use now, beside what they are allocated against the org's limits."
            : "CPU, memory, network and disk of this host, live, and what each instance uses."
        }
        actions={
          <>
            <Live updatedAt={orgs ? o.at : q.dataUpdatedAt} />
            {!orgs && <Segmented
              label="Range"
              value={String(range)}
              onChange={(v) => set("range", v === String(300) ? null : v)}
              options={MONITOR_RANGES.map((r) => ({ value: String(r.value), label: r.label }))}
            />}
          </>
        }
      />
      <TabLinks tabs={TABS} active={tab} />
      {orgs ? (
        <OrgsMonitor data={o} layout={params.get("layout") === "table" ? "table" : "cards"} setLayout={(l) => set("layout", l === "cards" ? null : l)} />
      ) : q.error && !m ? (
        <Panel title="Monitor">
          <Empty icon={<ServerOff />} title="Couldn't load the monitor">
            {errorMessage(q.error)}
          </Empty>
        </Panel>
      ) : !m ? (
        <MonitorSkeleton />
      ) : (
        <div className="grid gap-6">
          <HostLine h={m.host} />
          <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
            <CpuPanel m={m} />
            <MemoryPanel m={m} />
          </div>
          <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
            <NetworkPanel m={m} />
            <DiskPanel m={m} />
          </div>
          <InstancesPanel key={params.get("org")} m={m} org={params.get("org")} />
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

function MonitorSkeleton() {
  return (
    <div className="grid gap-6">
      <Skeleton className="h-4 w-80 max-w-full" />
      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Skeleton className="h-64 rounded-xl" />
        <Skeleton className="h-64 rounded-xl" />
      </div>
    </div>
  );
}
