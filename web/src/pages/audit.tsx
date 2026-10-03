import { useInfiniteQuery } from "@tanstack/react-query";
import { Bot, ChevronDown, ChevronRight, Download, Loader2, Radio, ScrollText, Search, Terminal, User, Webhook } from "lucide-react";
import { Fragment, useMemo, useState } from "react";
import { Navigate } from "react-router";
import { toast } from "sonner";
import { type AuditEntry, type AuditFilters, exportJsonl, listAudit, matches, useAuditTail } from "@/api/audit";
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

/** /orgs/:org/audit: the org's log, for its owners and admins. */
export function OrgAuditPage() {
  const { org, me, redirect } = useOrgPage();
  if (redirect) return redirect;
  if (!canAudit(me, org)) return <Navigate to={`/orgs/${encodeURIComponent(org)}`} replace />;
  return (
    <>
      <PageHeader
        title="Audit log"
        description={`Who changed what in ${org}, through which door, and how it went. Secret values and arguments are never recorded.`}
      />
      <AuditPanel org={org} />
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

const OUTCOMES = [
  { value: "any", label: "Any outcome" },
  { value: "ok", label: "Succeeded" },
  { value: "error", label: "Failed or refused" },
  { value: "forbidden", label: "Refused" },
];

const PLATFORM = "__platform__";
const ALL = "__all__";

/**
 * The log with filters, older pages on demand, and a live tail. `org` fixes
 * one org; without it (the Platform page) a scope picker covers every org,
 * platform-level entries only, or one org of `orgs`.
 */
export function AuditPanel({ org, orgs = [] }: { org?: string; orgs?: string[] }) {
  const [scope, setScope] = useState(ALL);
  const [draft, setDraft] = useState({ actor: "", action: "", target: "" });
  const [text, setText] = useState(draft);
  const [outcome, setOutcome] = useState("any");
  const [since, setSince] = useState("any");
  const [live, setLive] = useState(true);
  const [fresh, setFresh] = useState<AuditEntry[]>([]);
  const [open, setOpen] = useState<number | null>(null);
  const [exporting, setExporting] = useState(false);
  const sinceMs = SINCE.find((s) => s.value === since)!.ms;
  // Rounded to the minute so the query key stays put between renders.
  const sinceAt = sinceMs ? Math.floor((Date.now() - sinceMs) / 60_000) * 60_000 : undefined;

  const filters: AuditFilters = useMemo(
    () => ({
      org: org ?? (scope !== ALL && scope !== PLATFORM ? scope : undefined),
      platform: !org && scope === PLATFORM,
      actor: text.actor,
      action: text.action,
      target: text.target,
      outcome: outcome === "any" ? undefined : outcome,
      since: sinceAt,
    }),
    [org, scope, text, outcome, sinceAt],
  );

  const pages = useInfiniteQuery({
    queryKey: ["audit", filters],
    queryFn: ({ pageParam }) => listAudit(filters, { before: pageParam, limit: 50 }),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (p) => p.next_before ?? undefined,
  });
  const loaded = pages.data?.pages.flatMap((p) => p.entries) ?? [];
  const head = pages.data?.pages[0]?.head ?? 0;
  const seen = new Set(loaded.map((e) => e.id));
  const rows = [...fresh.filter((e) => !seen.has(e.id)), ...loaded];

  const tail = useAuditTail(filters.platform ? undefined : filters.org, head, live && pages.isSuccess, (e) => {
    if (matches(e, filters)) setFresh((prev) => [e, ...prev.filter((x) => x.id !== e.id)].slice(0, 200));
  });

  const apply = (e: React.FormEvent) => {
    e.preventDefault();
    setFresh([]);
    setText(draft);
  };

  const download = async () => {
    setExporting(true);
    try {
      const { text: body, count } = await exportJsonl(filters);
      const blob = new Blob([body], { type: "application/x-ndjson" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `isb-audit-${org ?? (scope === ALL ? "all" : scope === PLATFORM ? "platform" : scope)}-${new Date().toISOString().slice(0, 10)}.jsonl`;
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
      title="Entries"
      description={
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          Newest first.
          <TailState state={tail} />
        </span>
      }
      action={
        <>
          <div className="flex items-center gap-2">
            <Switch id="audit-live" checked={live} onCheckedChange={setLive} />
            <Label htmlFor="audit-live" className="text-sm font-normal">
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
          <Select value={scope} onValueChange={(v) => (setScope(v), setFresh([]))}>
            <SelectTrigger className="col-span-2 w-full sm:col-span-1" aria-label="Scope">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>Every org and the platform</SelectItem>
              <SelectItem value={PLATFORM}>Platform level only</SelectItem>
              {orgs.map((o) => (
                <SelectItem key={o} value={o}>
                  Org: {o}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <Input
          aria-label="Actor"
          className="col-span-2 sm:col-span-1"
          placeholder="Actor or email, e.g. *@acme.io"
          value={draft.actor}
          onChange={(e) => setDraft({ ...draft, actor: e.target.value })}
        />
        <Input
          aria-label="Action"
          placeholder="Action, e.g. secret_* or auth.*"
          className="col-span-2 font-mono placeholder:font-sans sm:col-span-1"
          value={draft.action}
          onChange={(e) => setDraft({ ...draft, action: e.target.value })}
        />
        <Input
          aria-label="Target"
          className="col-span-2 sm:col-span-1"
          placeholder="Target, e.g. web*"
          value={draft.target}
          onChange={(e) => setDraft({ ...draft, target: e.target.value })}
        />
        <Select value={outcome} onValueChange={(v) => (setOutcome(v), setFresh([]))}>
          <SelectTrigger className="w-full" aria-label="Outcome">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {OUTCOMES.map((o) => (
              <SelectItem key={o.value} value={o.value}>
                {o.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={since} onValueChange={(v) => (setSince(v), setFresh([]))}>
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
        <Button type="submit" variant="secondary" className={cn("col-span-2 sm:col-span-1", org && "lg:col-start-4")}>
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
          Changes, sign-ins, secret reads and terminal sessions show up here as they happen.
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
                <TableHead className="hidden w-20 lg:table-cell">Via</TableHead>
                <TableHead className="w-24 pr-5 text-right sm:w-32">
                  <span className="sr-only sm:not-sr-only">Outcome</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((e) => (
                <Fragment key={e.id}>
                  <TableRow
                    className={cn("cursor-pointer", open === e.id && "border-b-0 bg-muted/40")}
                    onClick={() => setOpen(open === e.id ? null : e.id)}
                  >
                    <TableCell className="pl-5 text-muted-foreground">
                      <button
                        type="button"
                        className="flex items-center"
                        aria-expanded={open === e.id}
                        aria-label={`Details of entry ${e.id}`}
                      >
                        {open === e.id ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
                      </button>
                    </TableCell>
                    <TableCell className="hidden text-muted-foreground sm:table-cell" title={dateTime(e.time / 1000)}>
                      {relativeTime(e.time / 1000)}
                    </TableCell>
                    <TableCell className="max-w-0">
                      <div className="flex min-w-0 items-center gap-2">
                        <code className="truncate font-mono text-xs font-medium">{e.action}</code>
                        {!org && e.org && (
                          <Badge variant="secondary" className="hidden font-normal sm:inline-flex">
                            {e.org}
                          </Badge>
                        )}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {e.target ?? "—"}
                        <span className="md:hidden"> · {e.actor}</span>
                        <span className="sm:hidden"> · {relativeTime(e.time / 1000)}</span>
                      </div>
                    </TableCell>
                    <TableCell className="hidden max-w-0 md:table-cell">
                      <Actor e={e} />
                    </TableCell>
                    <TableCell className="hidden text-muted-foreground lg:table-cell">{e.surface}</TableCell>
                    <TableCell className="pr-5 text-right">
                      <Outcome outcome={e.outcome} />
                    </TableCell>
                  </TableRow>
                  {open === e.id && (
                    <TableRow className="bg-muted/40 hover:bg-muted/40">
                      <TableCell colSpan={6} className="px-5 pt-0 pb-4 whitespace-normal">
                        <Details e={e} />
                      </TableCell>
                    </TableRow>
                  )}
                </Fragment>
              ))}
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

const KIND_ICON = { person: User, agent: Bot, local: Terminal, webhook: Webhook, anonymous: User } as const;

function Actor({ e }: { e: AuditEntry }) {
  const Icon = KIND_ICON[e.actor_kind] ?? User;
  return (
    <div className="flex min-w-0 items-center gap-2" title={e.actor_kind}>
      <Icon className="size-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0">
        <div className="truncate">{e.actor}</div>
        {e.token_name && <div className="truncate text-xs text-muted-foreground">token “{e.token_name}”</div>}
      </div>
    </div>
  );
}

function Outcome({ outcome }: { outcome: string }) {
  const tone =
    outcome === "ok"
      ? "border-success/30 bg-success/15 text-success"
      : outcome === "ignored"
        ? "bg-muted text-muted-foreground"
        : outcome === "forbidden" || outcome === "unauthorized"
          ? "border-destructive/30 bg-destructive/15 text-destructive"
          : "border-warning/30 bg-warning/15 text-warning";
  return (
    <Badge variant="outline" className={cn("max-w-full truncate font-normal", tone)}>
      {outcome === "ok" ? "ok" : outcome.replace(/_/g, " ")}
    </Badge>
  );
}

function Details({ e }: { e: AuditEntry }) {
  const rows: [string, React.ReactNode][] = [
    ["Time", dateTime(e.time / 1000)],
    ["Org", e.org ?? "platform"],
    [
      "Actor",
      <>
        {e.actor} <span className="text-muted-foreground">({e.actor_kind}{e.user_id ? `, user ${e.user_id}` : ""}{e.token_id ? `, token ${e.token_id}` : ""})</span>
      </>,
    ],
    ["Via", e.surface],
    ["From", [e.ip, e.user_agent].filter(Boolean).join(" · ") || "—"],
    ["Request", e.request_id ?? "—"],
    ["Entry", <span className="font-mono text-xs break-all">#{e.id} · {e.hash.slice(0, 16)}…</span>],
  ];
  const details = Object.entries(e.details ?? {});
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
              <Badge key={k} variant="secondary" className="font-mono text-xs font-normal">
                {k}={String(v)}
              </Badge>
            ))}
          </dd>
        </>
      )}
    </dl>
  );
}
