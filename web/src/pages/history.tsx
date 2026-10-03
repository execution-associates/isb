import { useInfiniteQuery } from "@tanstack/react-query";
import {
  Bot,
  Boxes,
  ChevronDown,
  Download,
  Flag,
  Loader2,
  ScrollText,
  Search,
  Server,
  ShieldCheck,
  SlidersHorizontal,
  X,
} from "lucide-react";
import { Fragment, type ReactNode, useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { toast } from "sonner";
import {
  exportHistory,
  type HistoryFilters,
  type HistoryItem,
  itemKey,
  itemMatches,
  queryHistory,
  useHistoryTail,
} from "@/api/history";
import { PageHeader } from "@/components/app-shell";
import { Empty, Panel } from "@/components/confirm";
import { FormError } from "@/components/form";
import { StatusBadge, StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { canAudit } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { TONE_TEXT, type Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { useOrgPage } from "@/pages/org-common";

/** /orgs/:org/history: everything that happened in the org, for its members. */
export function OrgHistoryPage() {
  const { org, me, redirect } = useOrgPage();
  const [params] = useSearchParams();
  if (redirect) return redirect;
  return (
    <>
      <PageHeader
        title="History"
        description={
          canAudit(me, org)
            ? `What happened in ${org} and who did it: deploys and rollouts, incus changes (made through isb or not), and the audit log. Secret values are never recorded.`
            : `What happened in ${org}: deploys and rollouts, and incus changes made through isb or not. Owners and admins also see the audit log here.`
        }
      />
      <HistoryPanel org={org} initialSource={params.get("source") ?? undefined} initialObject={params.get("object") ?? undefined} />
    </>
  );
}

const SINCE = [
  { value: "any", label: "Any time", ms: 0 },
  { value: "1h", label: "Last hour", ms: 3_600_000 },
  { value: "24h", label: "Last 24 hours", ms: 86_400_000 },
  { value: "7d", label: "Last 7 days", ms: 7 * 86_400_000 },
  { value: "30d", label: "Last 30 days", ms: 30 * 86_400_000 },
];

const SOURCES = [
  { value: "all", label: "Every source" },
  { value: "audit", label: "Audit log" },
  { value: "controller", label: "isb events" },
  { value: "incus", label: "incus changes" },
  { value: "marker", label: "Gaps and restarts" },
];

const PLATFORM = "__platform__";
const ALL = "__all__";

/**
 * The timeline with filters, older pages on demand, and a live tail. `org`
 * fixes one org; without it (the Platform page) a scope picker covers
 * everything, host-level rows only, or one org of `orgs`.
 */
export function HistoryPanel({
  org,
  orgs = [],
  initialSource,
  initialObject,
}: {
  org?: string;
  orgs?: string[];
  initialSource?: string;
  initialObject?: string;
}) {
  const [scope, setScope] = useState(ALL);
  const [source, setSource] = useState(SOURCES.some((s) => s.value === initialSource) ? initialSource! : "all");
  const [draft, setDraft] = useState({ object: initialObject ?? "", kind: "", actor: "" });
  const [text, setText] = useState(draft);
  const [since, setSince] = useState("any");
  const [live, setLive] = useState(true);
  const [fresh, setFresh] = useState<HistoryItem[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [more, setMore] = useState(false);
  const sinceMs = SINCE.find((s) => s.value === since)!.ms;
  // Rounded to the minute so the query key stays put between renders.
  const sinceAt = sinceMs ? Math.floor((Date.now() - sinceMs) / 60_000) * 60_000 : undefined;

  const filters: HistoryFilters = useMemo(
    () => ({
      org: org ?? (scope !== ALL && scope !== PLATFORM ? scope : undefined),
      platform: !org && scope === PLATFORM,
      source: source === "all" ? undefined : source,
      object: text.object,
      kind: text.kind,
      actor: text.actor,
      since: sinceAt,
    }),
    [org, scope, source, text, sinceAt],
  );

  const pages = useInfiniteQuery({
    queryKey: ["history", filters],
    queryFn: ({ pageParam }) => queryHistory(filters, { before: pageParam, limit: 50, correlate: true }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (p) => p.next ?? undefined,
  });
  const loaded = pages.data?.pages.flatMap((p) => p.items) ?? [];
  const seen = new Set(loaded.map(itemKey));
  const rows = [...fresh.filter((i) => !seen.has(itemKey(i))), ...loaded];
  const freshKeys = new Set(fresh.map(itemKey));

  const tail = useHistoryTail(filters.platform ? undefined : filters.org, live && pages.isSuccess, (i) => {
    if (itemMatches(i, filters)) setFresh((prev) => [i, ...prev.filter((x) => itemKey(x) !== itemKey(i))].slice(0, 200));
  });

  const reset = () => setFresh([]);
  const apply = (e: React.FormEvent) => {
    e.preventDefault();
    reset();
    setText(draft);
  };
  const filtered = !!(text.object || text.kind || text.actor || source !== "all" || since !== "any" || scope !== ALL);
  const clear = () => {
    const empty = { object: "", kind: "", actor: "" };
    setDraft(empty);
    setText(empty);
    setSource("all");
    setSince("any");
    setScope(ALL);
    reset();
  };
  const extra = [draft.kind, draft.actor, source !== "all", since !== "any", !org && scope !== ALL].filter(Boolean).length;

  const download = async () => {
    setExporting(true);
    try {
      const { text: body, count } = await exportHistory(filters);
      const blob = new Blob([body], { type: "application/x-ndjson" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `isb-history-${org ?? (scope === ALL ? "all" : scope === PLATFORM ? "host" : scope)}-${new Date().toISOString().slice(0, 10)}.jsonl`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10_000);
      toast.success(`Exported ${count} ${count === 1 ? "entry" : "entries"}`);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setExporting(false);
    }
  };

  // Day groups, newest first (rows already are).
  const groups: { day: string; items: HistoryItem[] }[] = [];
  for (const i of rows) {
    const day = dayLabel(i.time);
    const last = groups[groups.length - 1];
    if (last?.day === day) last.items.push(i);
    else groups.push({ day, items: [i] });
  }

  return (
    <Panel
      title="Timeline"
      description={
        <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
          Newest first.
          <TailState state={tail} />
        </span>
      }
      action={
        <>
          <div className="flex h-8 items-center gap-2 rounded-md border px-2.5">
            <Switch id="history-live" checked={live} onCheckedChange={setLive} />
            <Label htmlFor="history-live" className="text-[13px] font-normal">
              Live
            </Label>
          </div>
          <Button variant="outline" size="sm" onClick={download} disabled={exporting}>
            {exporting ? <Loader2 className="animate-spin" /> : <Download />}
            Export JSONL
          </Button>
        </>
      }
    >
      <form onSubmit={apply} className="flex flex-col gap-2 border-b bg-muted/20 px-4 py-3 sm:px-5 lg:flex-row lg:items-center" aria-label="Filters">
        <div className="flex min-w-0 gap-2 lg:flex-1">
          <div className="relative min-w-0 flex-1">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              aria-label="Object"
              className="h-8 pl-8 text-[13px]"
              placeholder="Instance, app, stack, image…"
              value={draft.object}
              onChange={(e) => setDraft({ ...draft, object: e.target.value })}
            />
          </div>
          <Button type="button" variant="outline" size="sm" className="lg:hidden" aria-expanded={more} onClick={() => setMore((m) => !m)}>
            <SlidersHorizontal />
            Filters
            {extra > 0 && <span className="rounded-full bg-foreground px-1.5 text-[10px] font-semibold text-background tabular-nums">{extra}</span>}
          </Button>
        </div>
        <div className={cn("grid grid-cols-2 gap-2 lg:flex lg:items-center", !more && "hidden lg:flex")}>
          {!org && (
            <Select value={scope} onValueChange={(v) => (setScope(v), reset())}>
              <SelectTrigger size="sm" className="col-span-2 w-full text-[13px] lg:w-44" aria-label="Scope">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>Every org and the host</SelectItem>
                <SelectItem value={PLATFORM}>Host level only</SelectItem>
                {orgs.map((o) => (
                  <SelectItem key={o} value={o}>
                    Org: {o}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Select value={source} onValueChange={(v) => (setSource(v), reset())}>
            <SelectTrigger size="sm" className="w-full text-[13px] lg:w-36" aria-label="Source">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SOURCES.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={since} onValueChange={(v) => (setSince(v), reset())}>
            <SelectTrigger size="sm" className="w-full text-[13px] lg:w-36" aria-label="Time range">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SINCE.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input
            aria-label="Kind"
            className="h-8 font-mono text-[13px] placeholder:font-sans lg:w-40"
            placeholder="Kind, e.g. deploy.*"
            value={draft.kind}
            onChange={(e) => setDraft({ ...draft, kind: e.target.value })}
          />
          <Input
            aria-label="Actor"
            className="h-8 text-[13px] lg:w-36"
            placeholder="Who, e.g. *@acme.io"
            value={draft.actor}
            onChange={(e) => setDraft({ ...draft, actor: e.target.value })}
          />
        </div>
        <div className={cn("flex gap-2", !more && "hidden lg:flex")}>
          <Button type="submit" size="sm" variant="secondary" className="flex-1 lg:flex-none">
            Apply
          </Button>
          {filtered && (
            <Button type="button" size="sm" variant="ghost" onClick={clear}>
              <X />
              Clear
            </Button>
          )}
        </div>
      </form>
      {pages.isLoading ? (
        <TimelineSkeleton />
      ) : pages.error ? (
        <div className="p-5">
          <FormError>{errorMessage(pages.error)}</FormError>
        </div>
      ) : rows.length === 0 ? (
        <Empty
          icon={<ScrollText />}
          title={filtered ? "Nothing matches these filters" : "Nothing recorded yet"}
          action={
            filtered && (
              <Button size="sm" variant="outline" onClick={clear}>
                Clear filters
              </Button>
            )
          }
        >
          {filtered ? "Widen the time range or clear a filter." : "Deploys, rollouts, incus changes, sign-ins and secret reads show up here as they happen."}
        </Empty>
      ) : (
        <>
          <div>
            {groups.map((g) => (
              <section key={g.day} aria-label={g.day}>
                <h3 className="border-b bg-muted/40 px-4 py-1.5 text-xs font-medium text-muted-foreground sm:px-5">
                  {g.day}
                  <span className="ml-2 font-normal opacity-70 tabular-nums">{g.items.length}</span>
                </h3>
                <ul className="divide-y">
                  {g.items.map((i) => {
                    const k = itemKey(i);
                    return (
                      <Row
                        key={k}
                        i={i}
                        showOrg={!org}
                        open={open === k}
                        fresh={freshKeys.has(k)}
                        onToggle={() => setOpen(open === k ? null : k)}
                      />
                    );
                  })}
                </ul>
              </section>
            ))}
          </div>
          {pages.hasNextPage && (
            <div className="border-t p-3 text-center">
              <Button variant="ghost" size="sm" onClick={() => pages.fetchNextPage()} disabled={pages.isFetchingNextPage}>
                {pages.isFetchingNextPage && <Loader2 className="animate-spin" />}
                Load older entries
              </Button>
            </div>
          )}
        </>
      )}
    </Panel>
  );
}

function TimelineSkeleton() {
  return (
    <div className="divide-y">
      {[0, 1, 2, 3, 4].map((i) => (
        <div key={i} className="flex items-center gap-3 px-5 py-3">
          <Skeleton className="h-3 w-12" />
          <Skeleton className="size-7 rounded-full" />
          <div className="flex-1 space-y-1.5">
            <Skeleton className="h-3.5 w-48" />
            <Skeleton className="h-3 w-72 max-w-full" />
          </div>
          <Skeleton className="h-5 w-14 rounded-full" />
        </div>
      ))}
    </div>
  );
}

function dayLabel(ms: number): string {
  const d = new Date(ms);
  const today = new Date();
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((start(today) - start(d)) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    ...(d.getFullYear() !== today.getFullYear() ? { year: "numeric" } : {}),
  });
}

const clock = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" });

function Row({ i, showOrg, open, fresh, onToggle }: { i: HistoryItem; showOrg: boolean; open: boolean; fresh: boolean; onToggle: () => void }) {
  const tone = levelTone(i.level);
  return (
    <li className={cn(fresh && "animate-fade-up", open && "bg-muted/30")}>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        aria-label={`Details of ${i.source} entry ${i.id}`}
        className="grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 px-4 py-2.5 text-left transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none sm:grid-cols-[4.5rem_auto_minmax(0,1fr)_auto_auto] sm:px-5"
      >
        <span className="hidden font-mono text-xs text-muted-foreground tabular-nums sm:block" title={dateTime(i.time / 1000)}>
          {clock(i.time)}
        </span>
        <SourceIcon source={i.source} tone={tone} />
        <span className="min-w-0">
          <span className="flex min-w-0 items-center gap-2">
            <code className="truncate font-mono text-[13px] font-medium">{i.kind}</code>
            {i.object && <span className="hidden truncate font-mono text-xs text-muted-foreground sm:inline">{i.object}</span>}
            {showOrg && i.org && <span className="hidden shrink-0 rounded border px-1.5 text-[11px] text-muted-foreground sm:inline">{i.org}</span>}
          </span>
          {/* Phone: what, who, when in one line. */}
          <span className="block truncate text-xs text-muted-foreground sm:hidden">
            {[i.message ?? i.object, i.actor, relativeTime(i.time / 1000)].filter(Boolean).join(" · ")}
          </span>
          {(i.message || i.actor) && (
            <span className={cn("hidden truncate text-xs text-muted-foreground sm:block", !i.message && "lg:hidden")}>
              {[i.message, i.actor && <span key="actor" className="lg:hidden">{i.actor}</span>].filter(Boolean).map((x, n) => (
                <Fragment key={n}>
                  {n > 0 && <span className="lg:hidden"> · </span>}
                  {x}
                </Fragment>
              ))}
            </span>
          )}
        </span>
        <span className="hidden max-w-48 min-w-0 text-right text-xs lg:block">
          <span className="block truncate text-foreground/80">{i.actor ?? <span className="text-muted-foreground">—</span>}</span>
          {i.inferred && (
            <span className="block truncate text-muted-foreground" title={i.inferred.why}>
              likely {i.inferred.actor}
            </span>
          )}
        </span>
        <span className="flex items-center gap-2">
          <Outcome item={i} />
          <ChevronDown className={cn("hidden size-4 text-muted-foreground transition-transform sm:block", open && "rotate-180")} />
        </span>
      </button>
      {open && (
        <div className="animate-fade-up px-4 pb-4 sm:pr-5 sm:pl-[calc(4.5rem+2.75rem+0.75rem)]">
          <Details i={i} />
        </div>
      )}
    </li>
  );
}

function TailState({ state }: { state: string }) {
  if (state === "off") return <span className="text-xs">Live updates paused.</span>;
  const on = state === "live";
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-xs", on ? "text-success" : "text-muted-foreground")}>
      <StatusDot tone={on ? "success" : "muted"} pulse={on} />
      {on ? "Live" : state === "connecting" ? "Connecting…" : "Reconnecting…"}
    </span>
  );
}

const SOURCE_ICON = { audit: ShieldCheck, controller: Boxes, incus: Server, marker: Flag } as const;
const SOURCE_LABEL: Record<string, string> = { audit: "audit log", controller: "isb event", incus: "incus", marker: "marker" };

function levelTone(v: string | null): Tone {
  if (!v) return "neutral";
  if (v === "ok") return "success";
  if (v === "info") return "neutral";
  if (v === "ignored") return "muted";
  if (v === "error" || v === "forbidden" || v === "unauthorized") return "danger";
  return "warning";
}

const ICON_BG: Record<Tone, string> = {
  success: "border-success/25 bg-success/10",
  info: "border-info/25 bg-info/10",
  warning: "border-warning/30 bg-warning/10",
  danger: "border-destructive/25 bg-destructive/10",
  neutral: "bg-muted/50",
  muted: "bg-transparent",
};

function SourceIcon({ source, tone }: { source: string; tone: Tone }) {
  const Icon = SOURCE_ICON[source as keyof typeof SOURCE_ICON] ?? Bot;
  return (
    <span
      title={SOURCE_LABEL[source] ?? source}
      className={cn("flex size-7 shrink-0 items-center justify-center rounded-full border", ICON_BG[tone], tone === "neutral" ? "text-muted-foreground" : TONE_TEXT[tone])}
    >
      <Icon className="size-3.5" aria-label={SOURCE_LABEL[source] ?? source} />
    </span>
  );
}

function Outcome({ item }: { item: HistoryItem }) {
  const v = item.level;
  if (!v) return null;
  const tone = levelTone(v);
  return (
    <StatusBadge tone={tone === "neutral" ? "muted" : tone} className="max-w-28 truncate">
      {v.replace(/_/g, " ")}
    </StatusBadge>
  );
}

function scalar(v: unknown): string {
  return typeof v === "string" ? v : JSON.stringify(v);
}

function Details({ i }: { i: HistoryItem }) {
  const rows: [string, ReactNode][] = [
    ["Time", <span key="time" className="tabular-nums">{dateTime(i.time / 1000)}</span>],
    ["Source", SOURCE_LABEL[i.source] ?? i.source],
    ["Org", i.org ?? "host level"],
    ["What", <code key="what" className="font-mono text-xs">{i.kind}</code>],
  ];
  if (i.object)
    rows.push([
      "Object",
      <>
        {i.object_type && <span className="text-muted-foreground">{i.object_type} </span>}
        <code className="font-mono text-xs">{i.object}</code>
      </>,
    ]);
  if (i.actor) rows.push(["Who", i.actor]);
  if (i.message) rows.push(["Message", i.message]);
  if (i.inferred)
    rows.push([
      "Likely cause",
      <>
        {i.inferred.action} by {i.inferred.actor}, {i.inferred.seconds_before.toFixed(0)} s earlier{" "}
        <span className="text-muted-foreground">(audit #{i.inferred.audit_id}; inferred by time and name)</span>
      </>,
    ]);
  const details = Object.entries(i.details ?? {}).filter(([, v]) => v !== null && v !== "" && !(typeof v === "object" && v && Object.keys(v).length === 0));
  return (
    <div className="rounded-lg border bg-card p-4 shadow-xs">
      <dl className="grid gap-x-6 gap-y-2 text-[13px] sm:grid-cols-[7rem_minmax(0,1fr)]">
        {rows.map(([k, v]) => (
          <Fragment key={k}>
            <dt className="text-xs text-muted-foreground sm:pt-px">{k}</dt>
            <dd className="min-w-0 break-words">{v}</dd>
          </Fragment>
        ))}
        {details.length > 0 && (
          <>
            <dt className="text-xs text-muted-foreground sm:pt-1">Details</dt>
            <dd className="min-w-0 overflow-hidden rounded-md border bg-muted/30">
              <table className="w-full font-mono text-xs">
                <tbody className="divide-y">
                  {details.map(([k, v]) => (
                    <tr key={k}>
                      <td className="w-1/3 px-2.5 py-1.5 align-top text-muted-foreground">{k}</td>
                      <td className="px-2.5 py-1.5 break-all">{scalar(v)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </dd>
          </>
        )}
      </dl>
    </div>
  );
}
