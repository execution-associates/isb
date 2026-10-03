// Uptime where people already look: a card on each app's Monitoring tab,
// and a banner on the org overview while any monitor is down.
import { Activity, CircleAlert, Plus } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import type { App } from "@/apps/api";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { relativeTime } from "@/lib/format";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { TONE_TEXT } from "@/lib/status";
import { downtimeText, monitorsOfApp, uptimeText, uptimeTone, useMonitors } from "./api";
import { latencyLine, MonitorBadge, UptimeBars } from "./components";
import { MonitorDialog } from "./monitor-dialog";

export function AppUptimeCard({ org, app }: { org: string; app: App }) {
  const list = useMonitors(org);
  const canWrite = useCanWrite(org);
  const [adding, setAdding] = useState(false);
  const o = encodeURIComponent(org);
  const ms = monitorsOfApp(list.data?.monitors, app.name);
  if (list.isLoading || list.error) return null;
  return (
    <Card className="gap-0 py-0">
      <div className="flex flex-wrap items-center gap-3 border-b px-5 py-3">
        <Activity className="size-4 text-muted-foreground" />
        <span className="text-[15px] font-semibold tracking-tight">Uptime</span>
        <span className="text-xs text-muted-foreground">What users see, checked from outside the app</span>
        {canWrite && (
          <Button variant="outline" size="sm" className="ml-auto" onClick={() => setAdding(true)}>
            <Plus />
            Monitor
          </Button>
        )}
      </div>
      {ms.length === 0 ? (
        <p className="px-5 py-4 text-sm text-muted-foreground">
          {app.domains?.length
            ? list.data?.settings.auto_monitors === false || list.data?.settings.exclude_apps?.includes(app.name)
              ? "No monitor watches this app: its own monitor is turned off."
              : "Its own monitor appears within a minute of its domain being served."
            : "No monitor watches this app. Give it a domain and it gets one of its own, or add one for its endpoint."}
        </p>
      ) : (
        <ul className="divide-y">
          {ms.map((m) => (
            <li key={m.name} className="grid items-center gap-x-5 gap-y-2 px-5 py-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)_6rem]">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <Link className="truncate font-medium hover:underline" to={`/orgs/${o}/uptime/${encodeURIComponent(m.name)}`}>
                    {m.name}
                  </Link>
                  <MonitorBadge m={m} />
                </div>
                <p className="truncate font-mono text-xs text-muted-foreground" title={m.last?.error ?? m.last?.url}>
                  {m.status === "down" && m.last?.error ? m.last.error : (m.last?.url ?? m.target)}
                </p>
              </div>
              <UptimeBars bars={m.bars} className="h-6" />
              <div className="text-sm md:text-right">
                <div className={cn("font-semibold tabular-nums", TONE_TEXT[uptimeTone(m.uptime["24h"])])}>{uptimeText(m.uptime["24h"])}</div>
                <div className="text-xs text-muted-foreground">{latencyLine(m)}</div>
              </div>
            </li>
          ))}
        </ul>
      )}
      <MonitorDialog org={org} app={app.name} open={adding} onOpenChange={setAdding} />
    </Card>
  );
}

export function DownMonitorsBanner({ org }: { org: string }) {
  const list = useMonitors(org);
  const down = (list.data?.monitors ?? []).filter((m) => m.status === "down");
  if (!down.length) return null;
  const o = encodeURIComponent(org);
  return (
    <Alert variant="destructive" className="border-destructive/30 bg-destructive/5">
      <CircleAlert />
      <AlertTitle>
        {down.length === 1 ? `${down[0].name} is down` : `${down.length} monitors are down`}
      </AlertTitle>
      <AlertDescription>
        <ul className="grid gap-1">
          {down.map((m) => (
            <li key={m.name}>
              <Link className="font-medium underline underline-offset-2" to={`/orgs/${o}/uptime/${encodeURIComponent(m.name)}`}>
                {m.name}
              </Link>
              {m.incident ? ` for ${downtimeText(m.incident.duration_ms)}` : m.since ? ` since ${relativeTime(m.since / 1000)}` : ""}
              {m.last?.error ? `: ${m.last.error}` : ""}
            </li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  );
}
