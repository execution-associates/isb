import { useInfiniteQuery } from "@tanstack/react-query";
import {
  Bot,
  Boxes,
  ChevronDown,
  ChevronRight,
  Download,
  Flag,
  Loader2,
  Radio,
  ScrollText,
  Search,
  Server,
  ShieldCheck,
} from "lucide-react";
import { Fragment, useMemo, useState } from "react";
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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { canAudit } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
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

  const tail = useHistoryTail(filters.platform ? undefined : filters.org, live && pages.isSuccess, (i) => {
    if (itemMatches(i, filters)) setFresh((prev) => [i, ...prev.filter((x) => itemKey(x) !== itemKey(i))].slice(0, 200));
  });

  const reset = () => setFresh([]);
  const apply = (e: React.FormEvent) => {
    e.preventDefault();
    reset();
    setText(draft);
  };

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

  return (
    <Panel
      title="Timeline"
      description={
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          Newest first.
          <TailState state={tail} />
        </span>
      }
      action={
        <>
          <div className="flex items-center gap-2">
            <Switch id="history-live" checked={live} onCheckedChange={setLive} />
            <Label htmlFor="history-live" className="text-sm font-normal">
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
      <form onSubmit={apply} className="grid grid-cols-2 gap-3 border-b px-5 py-4 lg:grid-cols-4" aria-label="Filters">
        {!org && (
          <Select value={scope} onValueChange={(v) => (setScope(v), reset())}>
            <SelectTrigger className="col-span-2 w-full sm:col-span-1" aria-label="Scope">
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
          <SelectTrigger className="w-full" aria-label="Source">
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
          <SelectTrigger className="w-full" aria-label="Time range">
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
          aria-label="Object"
          className="col-span-2 sm:col-span-1"
          placeholder="Instance, app, stack, image…"
          value={draft.object}
          onChange={(e) => setDraft({ ...draft, object: e.target.value })}
        />
        <Input
          aria-label="Kind"
          className="col-span-2 font-mono placeholder:font-sans sm:col-span-1"
          placeholder="Kind, e.g. instance-* or deploy.*"
          value={draft.kind}
          onChange={(e) => setDraft({ ...draft, kind: e.target.value })}
        />
        <Input
          aria-label="Actor"
          className="col-span-2 sm:col-span-1"
          placeholder="Who, e.g. *@acme.io"
          value={draft.actor}
          onChange={(e) => setDraft({ ...draft, actor: e.target.value })}
        />
        <Button type="submit" variant="secondary" className="col-span-2 sm:col-span-1">
          <Search />
          Filter
        </Button>
      </form>
      {pages.isLoading ? (
        <div className="space-y-2 p-5">
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
        </div>
      ) : pages.error ? (
        <div className="p-5">
          <FormError>{errorMessage(pages.error)}</FormError>
        </div>
      ) : rows.length === 0 ? (
        <Empty icon={<ScrollText />} title="Nothing recorded yet">
          Deploys, rollouts, incus changes, sign-ins and secret reads show up here as they happen.
        </Empty>
      ) : (
        <>
          <Table className="table-fixed">
            <TableHeader>
              <TableRow>
                <TableHead className="w-8 pl-5" aria-label="Details" />
                <TableHead className="hidden w-36 sm:table-cell">When</TableHead>
                <TableHead>What</TableHead>
                <TableHead className="hidden md:table-cell">Who</TableHead>
                <TableHead className="w-24 pr-5 text-right sm:w-32">
                  <span className="sr-only sm:not-sr-only">Outcome</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((i) => {
                const k = itemKey(i);
                return (
                  <Fragment key={k}>
                    <TableRow className={cn("cursor-pointer", open === k && "border-b-0 bg-muted/40")} onClick={() => setOpen(open === k ? null : k)}>
                      <TableCell className="pl-5 text-muted-foreground">
                        <button type="button" className="flex items-center" aria-expanded={open === k} aria-label={`Details of ${i.source} entry ${i.id}`}>
                          {open === k ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
                        </button>
                      </TableCell>
                      <TableCell className="hidden text-muted-foreground sm:table-cell" title={dateTime(i.time / 1000)}>
                        {relativeTime(i.time / 1000)}
                      </TableCell>
                      <TableCell className="max-w-0">
                        <div className="flex min-w-0 items-center gap-2">
                          <SourceIcon source={i.source} />
                          <code className="truncate font-mono text-xs font-medium">{i.kind}</code>
                          {!org && i.org && (
                            <Badge variant="secondary" className="hidden font-normal sm:inline-flex">
                              {i.org}
                            </Badge>
                          )}
                        </div>
                        <div className="truncate text-xs text-muted-foreground">
                          {i.message ?? i.object ?? "—"}
                          {i.message && i.object ? ` · ${i.object}` : ""}
                          {i.actor && <span className="md:hidden"> · {i.actor}</span>}
                          <span className="sm:hidden"> · {relativeTime(i.time / 1000)}</span>
                        </div>
                      </TableCell>
                      <TableCell className="hidden max-w-0 md:table-cell">
                        <div className="truncate">{i.actor ?? "—"}</div>
                        {i.inferred && (
                          <div className="truncate text-xs text-muted-foreground" title={i.inferred.why}>
                            likely {i.inferred.action} by {i.inferred.actor}
                          </div>
                        )}
                      </TableCell>
                      <TableCell className="pr-5 text-right">
                        <Outcome item={i} />
                      </TableCell>
                    </TableRow>
                    {open === k && (
                      <TableRow className="bg-muted/40 hover:bg-muted/40">
                        <TableCell colSpan={5} className="px-5 pt-0 pb-4 whitespace-normal">
                          <Details i={i} />
                        </TableCell>
                      </TableRow>
                    )}
                  </Fragment>
                );
              })}
            </TableBody>
          </Table>
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

function TailState({ state }: { state: string }) {
  if (state === "off") return <span className="text-xs">Live updates paused.</span>;
  const on = state === "live";
  return (
    <span className={cn("inline-flex items-center gap-1.5 text-xs", on ? "text-success" : "text-muted-foreground")}>
      <Radio className="size-3.5" />
      {on ? "Live" : state === "connecting" ? "Connecting…" : "Reconnecting…"}
    </span>
  );
}

const SOURCE_ICON = { audit: ShieldCheck, controller: Boxes, incus: Server, marker: Flag } as const;
const SOURCE_LABEL: Record<string, string> = { audit: "audit log", controller: "isb event", incus: "incus", marker: "marker" };

function SourceIcon({ source }: { source: string }) {
  const Icon = SOURCE_ICON[source as keyof typeof SOURCE_ICON] ?? Bot;
  return (
    <span title={SOURCE_LABEL[source] ?? source} className="shrink-0 text-muted-foreground">
      <Icon className="size-3.5" aria-label={SOURCE_LABEL[source] ?? source} />
    </span>
  );
}

function Outcome({ item }: { item: HistoryItem }) {
  const v = item.level;
  if (!v) return null;
  const tone =
    v === "ok" || v === "info"
      ? "border-success/30 bg-success/15 text-success"
      : v === "ignored"
        ? "bg-muted text-muted-foreground"
        : v === "error" || v === "forbidden" || v === "unauthorized"
          ? "border-destructive/30 bg-destructive/15 text-destructive"
          : "border-warning/30 bg-warning/15 text-warning";
  return (
    <Badge variant="outline" className={cn("max-w-full truncate font-normal", tone)}>
      {v.replace(/_/g, " ")}
    </Badge>
  );
}

function scalar(v: unknown): string {
  return typeof v === "string" ? v : JSON.stringify(v);
}

function Details({ i }: { i: HistoryItem }) {
  const rows: [string, React.ReactNode][] = [
    ["Time", dateTime(i.time / 1000)],
    ["Source", SOURCE_LABEL[i.source] ?? i.source],
    ["Org", i.org ?? "host level"],
    ["What", <code className="font-mono text-xs">{i.kind}</code>],
  ];
  if (i.object) rows.push(["Object", `${i.object_type ? `${i.object_type} ` : ""}${i.object}`]);
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
    <dl className="grid gap-x-6 gap-y-1.5 text-sm sm:grid-cols-[8rem_1fr]">
      {rows.map(([k, v]) => (
        <Fragment key={k}>
          <dt className="text-muted-foreground">{k}</dt>
          <dd className="min-w-0 break-words">{v}</dd>
        </Fragment>
      ))}
      {details.length > 0 && (
        <>
          <dt className="text-muted-foreground">Details</dt>
          <dd className="flex min-w-0 flex-wrap gap-1.5">
            {details.map(([k, v]) => (
              <Badge key={k} variant="secondary" className="max-w-full font-mono text-xs font-normal break-all whitespace-normal">
                {k}={scalar(v)}
              </Badge>
            ))}
          </dd>
        </>
      )}
    </dl>
  );
}
