// /orgs/:org/uptime/:name: one monitor. Its status and last answer, uptime
// over 24 hours to 30 days, latency, the certificate, a history chart over
// 24 hours to 90 days, its incidents and its latest checks.
import { useQueryClient } from "@tanstack/react-query";
import { Pause, Pencil, Play, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { AreaChart, ConfirmDialog, Crumbs, QueryError, Section } from "@/apps/components";
import { Segmented } from "@/apps/segmented";
import { PageHeader } from "@/components/app-shell";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { type Bar, type Monitor, msText, NEVER_UP_HINT, PENDING_HINT, type Range, RANGES, statusTone, targetText, ukeys, uptimeText, uptimeTone, useHistory, useMonitor } from "./api";
import { MonitorBadge, Stat, UptimeBars } from "./components";
import { MonitorDialog } from "./monitor-dialog";
import { Incidents } from "./uptime-page";

const toneOf = (u: number | null | undefined) => {
  const t = uptimeTone(u);
  return t === "success" || t === "danger" || t === "warning" ? t : undefined;
};

export function MonitorPage() {
  const { org = "", name = "" } = useParams();
  const o = encodeURIComponent(org);
  const m = useMonitor(org, name);
  const canWrite = useCanWrite(org);
  const [edit, setEdit] = useState(false);
  const [del, setDel] = useState(false);
  const qc = useQueryClient();
  const nav = useNavigate();

  if (m.error) return <QueryError error={m.error} />;
  if (!m.data) return <Skeleton className="h-40 rounded-xl" />;
  const x = m.data;
  const refresh = () => qc.invalidateQueries({ queryKey: ukeys.all(org) });
  const pause = async () => {
    try {
      await callTool(x.paused ? "monitor_resume" : "monitor_pause", { name }, org);
      await refresh();
      toast.success(x.paused ? `Checking ${name} again` : `${name} paused`);
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };
  return (
    <>
      <Crumbs items={[{ label: "Uptime", to: `/orgs/${o}/uptime` }, { label: name }]} />
      <PageHeader
        title={
          <>
            {name}
            <MonitorBadge m={x} />
          </>
        }
        description={
          <span className="font-mono text-xs">
            {x.type === "app" && x.app ? (
              <>
                app <Link className="underline underline-offset-2" to={`/orgs/${o}/apps/${encodeURIComponent(x.app)}`}>{x.app}</Link>
                {x.last?.url ? ` · ${x.last.url}` : ""}
              </>
            ) : (
              targetText(x)
            )}
          </span>
        }
        actions={
          canWrite && (
            <>
              <Button variant="outline" onClick={pause}>
                {x.paused ? <Play /> : <Pause />}
                {x.paused ? "Resume" : "Pause"}
              </Button>
              <Button variant="outline" onClick={() => setEdit(true)}>
                <Pencil />
                Edit
              </Button>
              <Button variant="ghost" size="icon" aria-label={`Delete ${name}`} onClick={() => setDel(true)}>
                <Trash2 />
              </Button>
            </>
          )
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
        <LastCheck m={x} />
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
          <Stat label="Uptime, 24 h" value={uptimeText(x.uptime["24h"])} tone={toneOf(x.uptime["24h"])} />
          <Stat label="7 days" value={uptimeText(x.uptime["7d"])} tone={toneOf(x.uptime["7d"])} />
          <Stat label="30 days" value={uptimeText(x.uptime["30d"])} tone={toneOf(x.uptime["30d"])} />
          <Stat label="Latency p50 / p95" value={`${msText(x.latency.p50)} / ${msText(x.latency.p95)}`} hint="Last 24 h" />
          <Stat
            label="Certificate"
            value={x.cert_expires ? `${Math.floor((x.cert_expires * 1000 - Date.now()) / 86_400_000)} days` : "–"}
            hint={x.cert_expires ? `Expires ${new Date(x.cert_expires * 1000).toLocaleDateString()}` : "No HTTPS answer yet"}
            tone={x.cert_expires && x.cert_expires * 1000 - Date.now() < x.cert_expiry_days * 86_400_000 ? "warning" : undefined}
          />
        </div>
        <HistoryCard org={org} name={name} />
        {x.incidents && x.incidents.length > 0 && <Incidents org={org} incidents={x.incidents} />}
        <Checks m={x} />
        <Settings m={x} />
      </div>
      <MonitorDialog org={org} existing={x} open={edit} onOpenChange={setEdit} />
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete monitor ${name}?`}
        description={x.auto ? `Its history goes too, and ${x.type === "service" ? `${x.stack}/${x.service}` : x.app} gets no monitor of its own from now on (turn that back on under Uptime).` : "Its checks and incidents go too."}
        confirmLabel="Delete monitor"
        onConfirm={async () => {
          await callTool("monitor_delete", { name }, org);
          await refresh();
          toast.success(`Deleted ${name}`);
          nav(`/orgs/${o}/uptime`);
        }}
      />
    </>
  );
}

function LastCheck({ m }: { m: Monitor }) {
  const l = m.last;
  if (!l) return <p className="text-sm text-muted-foreground">{m.paused ? "Paused before its first check." : "The first check is on its way."}</p>;
  if (m.status === "pending" && !l.ok) {
    return (
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border px-4 py-3 text-sm">
        <StatusDot tone={statusTone(m)} />
        <span className="font-medium">{m.never_up ? NEVER_UP_HINT : PENDING_HINT}</span>
        {l.error && <span className="min-w-0 basis-full font-mono text-xs break-words text-muted-foreground sm:basis-auto">{l.error}</span>}
        <span className="ml-auto text-xs text-muted-foreground" title={new Date(l.at).toLocaleString()}>
          {relativeTime(l.at / 1000)}
        </span>
      </div>
    );
  }
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border px-4 py-3 text-sm">
      <StatusDot tone={l.ok ? "success" : "danger"} />
      <span className="font-medium">{l.ok ? "Last check passed" : "Last check failed"}</span>
      {l.status !== undefined && <span className="font-mono text-xs">HTTP {l.status}</span>}
      {l.latency_ms !== undefined && <span className="text-muted-foreground tabular-nums">{msText(l.latency_ms)}</span>}
      {l.error && <span className="min-w-0 basis-full font-mono text-xs break-words text-destructive sm:basis-auto">{l.error}</span>}
      {l.via && l.via !== "public" && <span className="text-xs text-muted-foreground">via {l.via.replace(/^internal: /, "")}</span>}
      <span className="ml-auto text-xs text-muted-foreground" title={new Date(l.at).toLocaleString()}>
        {relativeTime(l.at / 1000)}
      </span>
      {l.note && <p className="basis-full text-xs text-muted-foreground">{l.note}</p>}
    </div>
  );
}

function HistoryCard({ org, name }: { org: string; name: string }) {
  const [range, setRange] = useState<Range>("24h");
  const h = useHistory(org, name, range);
  const lat = (h.data?.buckets ?? []).map((b) => b.p95 ?? 0);
  return (
    <Section
      title="History"
      description={h.data ? `${uptimeText(h.data.uptime)} up over ${range}.` : "Uptime and latency over time."}
      actions={<Segmented value={range} onChange={setRange} label="Range" options={RANGES.map((r) => ({ value: r, label: r }))} />}
    >
      {h.error ? (
        <QueryError error={h.error} />
      ) : !h.data ? (
        <Skeleton className="h-28" />
      ) : (
        <div className="grid gap-4">
          <UptimeBars bars={h.data.buckets.map((b): Bar => [b.at, b.uptime, b.pending])} stepMs={h.data.step_ms} className="h-9" />
          <div>
            <div className="mb-1 text-xs text-muted-foreground">Latency, p95 per bar (up to {msText(Math.max(0, ...lat))})</div>
            <AreaChart values={lat} label="Latency p95" tone="sky" />
          </div>
        </div>
      )}
    </Section>
  );
}

function Checks({ m }: { m: Monitor }) {
  const cs = m.checks ?? [];
  if (!cs.length) return null;
  return (
    <Section title="Latest checks" description="Raw checks are kept 7 days, hourly rollups 90.">
      <div className="-mx-5 -mb-5 overflow-x-auto">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="pl-5">When</TableHead>
              <TableHead>Result</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Latency</TableHead>
              <TableHead className="pr-5">Error</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {cs.map((c) => (
              <TableRow key={c.at}>
                <TableCell className="pl-5 text-muted-foreground" title={new Date(c.at).toLocaleString()}>
                  {relativeTime(c.at / 1000)}
                </TableCell>
                <TableCell>
                  <span className="inline-flex items-center gap-2">
                    <StatusDot tone={c.ok ? "success" : c.pending ? "neutral" : "danger"} />
                    {c.ok ? "Up" : c.pending ? "Pending" : "Down"}
                  </span>
                </TableCell>
                <TableCell className="font-mono text-xs">{c.status ?? "–"}</TableCell>
                <TableCell className="tabular-nums">{msText(c.latency_ms)}</TableCell>
                <TableCell className="max-w-96 truncate pr-5 font-mono text-xs text-muted-foreground" title={c.error}>
                  {c.error ?? ""}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </Section>
  );
}

function Settings({ m }: { m: Monitor }) {
  const rows: [string, string][] = [
    ["Every", `${m.interval} s (timeout ${m.timeout} s)`],
    ["Down after", `${m.failure_threshold} failed check${m.failure_threshold === 1 ? "" : "s"} in a row`],
    ["Up after", `${m.recovery_threshold} good check${m.recovery_threshold === 1 ? "" : "s"} in a row`],
  ];
  if (m.type !== "tcp") {
    rows.push(["Up when", `${m.method} answers ${m.expected_status}${m.keyword ? `, containing “${m.keyword}”` : ""}${m.keyword_absent ? `, without “${m.keyword_absent}”` : ""}`]);
    rows.push(["Redirects", m.follow_redirects ? "followed" : "judged as they are"]);
    if (m.headers?.length) rows.push(["Headers", m.headers.map((h) => `${h.name}${h.secret ? ` (secret ${h.secret})` : ""}`).join(", ")]);
    rows.push(["Certificate warning", m.cert_expiry_days ? `${m.cert_expiry_days} days ahead` : "off"]);
  }
  return (
    <Section title="Settings" description={m.auto ? "Made for the app because it has a domain; edit it like any other." : undefined}>
      <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[10rem_1fr]">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="min-w-0 break-words">{v}</dd>
          </div>
        ))}
      </dl>
    </Section>
  );
}
