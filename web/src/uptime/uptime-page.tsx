// /orgs/:org/uptime: what users see of the org's apps. Every monitor with
// its status, 24 hours of uptime bars and its latency; the recent
// incidents; and whether apps get monitors of their own.
import { useQueryClient } from "@tanstack/react-query";
import { Activity, Plus, Sparkles } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { EmptyState, QueryError, Section } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { downtimeText, type Incident, type Monitor, type MonitorList, msText, STATUS_TONE, ukeys, uptimeText, uptimeTone, useMonitors } from "./api";
import { MonitorBadge, Sparkline, Stat, UptimeBars } from "./components";
import { MonitorDialog } from "./monitor-dialog";
import { TONE_TEXT } from "@/lib/status";

export function UptimePage() {
  const { org = "" } = useParams();
  const list = useMonitors(org);
  const canWrite = useCanWrite(org);
  const [creating, setCreating] = useState(false);
  const ms = list.data?.monitors ?? [];

  return (
    <>
      <PageHeader
        title="Uptime"
        description="What users see: each app's public URL (DNS, TLS, ingress and the app's answer), checked from this server. Channels that hear monitor events are told when one goes down and comes back."
        actions={
          canWrite && (
            <Button onClick={() => setCreating(true)}>
              <Plus />
              New monitor
            </Button>
          )
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
        {list.isLoading ? (
          <Card className="gap-0 py-0">
            {[0, 1, 2].map((i) => (
              <div key={i} className="flex items-center gap-4 border-b px-5 py-4 last:border-0">
                <Skeleton className="size-2 rounded-full" />
                <Skeleton className="h-4 w-40" />
                <Skeleton className="ml-auto h-6 w-64" />
              </div>
            ))}
          </Card>
        ) : list.error ? (
          <QueryError error={list.error} />
        ) : ms.length === 0 ? (
          <Card className="py-0">
            <EmptyState
              icon={Activity}
              title="Nothing is watched yet"
              action={
                canWrite && (
                  <Button onClick={() => setCreating(true)}>
                    <Plus />
                    New monitor
                  </Button>
                )
              }
            >
              Every app with a domain gets a monitor of its own within a minute of being served{list.data?.settings.auto_monitors === false ? " (turned off for this org)" : ""}. Add one for any URL or TCP port too.
            </EmptyState>
          </Card>
        ) : (
          <>
            <Summary list={list.data!} />
            <Card className="gap-0 overflow-hidden py-0">
              <ul className="divide-y">
                {ms.map((m) => (
                  <MonitorRow key={m.name} org={org} m={m} />
                ))}
              </ul>
            </Card>
          </>
        )}
        {list.data && list.data.incidents.length > 0 && <Incidents org={org} incidents={list.data.incidents} />}
        {list.data && <AutoSettings org={org} list={list.data} canWrite={canWrite} />}
      </div>
      <MonitorDialog org={org} open={creating} onOpenChange={setCreating} />
    </>
  );
}

function Summary({ list }: { list: MonitorList }) {
  const ms = list.monitors.filter((m) => m.status !== "paused");
  const up = ms.filter((m) => m.status === "up").length;
  const ups = ms.map((m) => m.uptime["24h"]).filter((u): u is number => u !== null);
  const avg = ups.length ? ups.reduce((a, b) => a + b, 0) / ups.length : null;
  const open = list.incidents.filter((i) => !i.ended).length;
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <Stat label="Up" value={`${up}/${ms.length}`} tone={up === ms.length && ms.length > 0 ? "success" : undefined} />
      <Stat label="Down" value={String(list.down)} tone={list.down > 0 ? "danger" : undefined} />
      <Stat label="Uptime, 24 h" value={uptimeText(avg)} hint="Average of the monitors" />
      <Stat label="Open incidents" value={String(open)} tone={open > 0 ? "danger" : undefined} />
    </div>
  );
}

function MonitorRow({ org, m }: { org: string; m: Monitor }) {
  const to = `/orgs/${encodeURIComponent(org)}/uptime/${encodeURIComponent(m.name)}`;
  const u = m.uptime["24h"];
  return (
    <li className={cn("relative grid grid-cols-[minmax(0,1fr)] items-center gap-x-5 gap-y-2 px-5 py-3.5 transition-colors hover:bg-muted/30 md:grid-cols-[minmax(0,1.3fr)_minmax(0,1.2fr)_7rem_6rem]", m.status === "paused" && "opacity-60")}>
      <div className="flex min-w-0 items-center gap-3">
        <StatusDot tone={STATUS_TONE[m.status]} pulse={m.status === "down"} className="size-2.5" />
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <Link to={to} className="truncate text-[15px] font-semibold tracking-tight after:absolute after:inset-0 focus-visible:underline focus-visible:outline-none">
              {m.name}
            </Link>
            {m.auto && (
              <span className="inline-flex items-center gap-1 rounded-md border bg-muted/40 px-1.5 text-[11px] text-muted-foreground" title="Made for an app with a domain">
                <Sparkles className="size-3" />
                auto
              </span>
            )}
            {m.status !== "up" && <MonitorBadge m={m} />}
          </div>
          <p className="truncate font-mono text-xs text-muted-foreground" title={m.last?.url ?? m.target}>
            {m.last?.url ?? m.target}
            {m.status === "down" && m.last?.error ? <span className="text-destructive"> · {m.last.error}</span> : null}
          </p>
        </div>
      </div>
      <UptimeBars bars={m.bars} className="relative z-10 h-6" />
      <div className="hidden text-right md:block">
        <Sparkline points={m.spark} />
        <div className="text-xs text-muted-foreground tabular-nums">{msText(m.latency.p50)}</div>
      </div>
      <div className="flex items-baseline justify-between gap-2 md:block md:text-right">
        <div className={cn("text-sm font-semibold tabular-nums", TONE_TEXT[uptimeTone(u)])}>{uptimeText(u)}</div>
        <div className="text-xs text-muted-foreground">{m.last ? relativeTime(m.last.at / 1000) : "not checked"}</div>
      </div>
    </li>
  );
}

export function Incidents({ org, incidents, title = "Incidents" }: { org: string; incidents: Incident[]; title?: string }) {
  return (
    <Section title={title} description="Each time a monitor went down, and how long it stayed down. Kept 90 days.">
      <ul className="-my-2 divide-y">
        {incidents.map((i) => (
          <li key={i.id} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 py-2.5 text-sm">
            <StatusDot tone={i.ended ? "muted" : "danger"} pulse={!i.ended} />
            <Link className="font-medium hover:underline" to={`/orgs/${encodeURIComponent(org)}/uptime/${encodeURIComponent(i.monitor)}`}>
              {i.monitor}
            </Link>
            <span className="text-muted-foreground">{i.ended ? `down ${downtimeText(i.duration_ms)}` : `down for ${downtimeText(i.duration_ms)}, ongoing`}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground" title={i.error}>
              {i.error}
            </span>
            <span className="text-xs text-muted-foreground" title={new Date(i.started).toLocaleString()}>
              {relativeTime(i.started / 1000)}
            </span>
          </li>
        ))}
      </ul>
    </Section>
  );
}

function AutoSettings({ org, list, canWrite }: { org: string; list: MonitorList; canWrite: boolean }) {
  const qc = useQueryClient();
  const s = list.settings;
  const save = async (p: Record<string, unknown>, done: string) => {
    try {
      await callTool("monitor_settings", p, org);
      await qc.invalidateQueries({ queryKey: ukeys.all(org) });
      toast.success(done);
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };
  const excluded = s.exclude_apps ?? [];
  return (
    <Section title="Apps' own monitors" description="Every app with a served domain gets a monitor named app-<name>, so a down app is noticed with no setup. Deleting one keeps it away for that app.">
      <div className="grid gap-3 text-sm">
        <div className="flex items-center gap-3">
          <Switch id="uptime-auto" checked={s.auto_monitors} disabled={!canWrite} onCheckedChange={(v) => save({ auto_monitors: v }, v ? "Apps get their own monitors" : "Apps' own monitors turned off")} />
          <Label htmlFor="uptime-auto" className="font-normal">
            Give apps with a domain their own monitor
          </Label>
        </div>
        {excluded.length > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-muted-foreground">Left out:</span>
            {excluded.map((a) => (
              <span key={a} className="inline-flex items-center gap-1 rounded-md border bg-muted/40 py-0.5 pr-1 pl-2 text-xs">
                {a}
                {canWrite && (
                  <button type="button" className="rounded px-1 text-muted-foreground hover:text-foreground" aria-label={`Watch ${a} again`} onClick={() => save({ exclude_apps: excluded.filter((x) => x !== a) }, `${a} gets its monitor back`)}>
                    ×
                  </button>
                )}
              </span>
            ))}
          </div>
        )}
      </div>
    </Section>
  );
}
