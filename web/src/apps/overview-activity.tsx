// The org overview's Activity card: the history (controller events and
// audit rows), humanized, with the rollout chatter left out and repeats
// folded together.
import { useQuery } from "@tanstack/react-query";
import {
  Activity,
  ArrowRight,
  CircleCheck,
  CircleX,
  HeartCrack,
  HeartPulse,
  KeyRound,
  LoaderCircle,
  LogIn,
  Pencil,
  Plus,
  Rocket,
  Trash2,
  TriangleAlert,
  UserPlus,
} from "lucide-react";
import { Link } from "react-router";
import type { HistoryItem, HistoryPage } from "@/api/history";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { TONE_TEXT, type Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { keys } from "./api";
import { EmptyState, QueryError } from "./components";

export type ActivityKind =
  | "deploy-ok"
  | "deploy-failed"
  | "deploy-running"
  | "health-bad"
  | "health-ok"
  | "create"
  | "delete"
  | "update"
  | "member"
  | "secret"
  | "auth"
  | "warn"
  | "info";

export interface ActivityEntry {
  key: string;
  kind: ActivityKind;
  /** Text before and after the subject, which is shown in bold. */
  before: string;
  subject?: string;
  after?: string;
  detail?: string;
  actor?: string;
  /** Unix milliseconds. */
  time: number;
  count: number;
  /** Where the entry leads, relative to /orgs/ORG. */
  to?: string;
}

const KIND_STYLE: Record<ActivityKind, [typeof Activity, Tone]> = {
  "deploy-ok": [Rocket, "success"],
  "deploy-failed": [CircleX, "danger"],
  "deploy-running": [LoaderCircle, "info"],
  "health-bad": [HeartCrack, "warning"],
  "health-ok": [HeartPulse, "success"],
  create: [Plus, "info"],
  delete: [Trash2, "neutral"],
  update: [Pencil, "neutral"],
  member: [UserPlus, "info"],
  secret: [KeyRound, "neutral"],
  auth: [LogIn, "muted"],
  warn: [TriangleAlert, "warning"],
  info: [CircleCheck, "neutral"],
};

const TILE_BG: Record<Tone, string> = {
  success: "bg-success/10",
  info: "bg-info/10",
  warning: "bg-warning/12",
  danger: "bg-destructive/10",
  neutral: "bg-muted",
  muted: "bg-muted/60",
};

const VERBS: Record<string, string> = {
  create: "Created",
  add: "Added",
  delete: "Deleted",
  remove: "Removed",
  update: "Updated",
  set: "Set",
  unset: "Removed",
  rename: "Renamed",
  deploy: "Deployed",
  rollback: "Rolled back",
  restart: "Restarted",
  stop: "Stopped",
  start: "Started",
  scale: "Scaled",
  invite: "Invited",
  revoke: "Revoked",
  rotate: "Rotated",
  restore: "Restored",
  run: "Ran",
  backup: "Backed up",
  import: "Imported",
  promote: "Promoted",
  accept: "Accepted",
  join: "Joined",
};

/** `local(uid 1000)` is the CLI on the server. */
export function actorLabel(a: string | null | undefined): string | undefined {
  if (!a || a === "isb") return undefined;
  if (a.startsWith("local(")) return "CLI on the server";
  return a;
}

/** Words in an audit action that make it a read. */
const READS = new Set(["list", "get", "status", "query", "overview", "events", "export", "validate", "logs", "top", "show", "inspect", "search"]);

/** Account events: [kind, text before the object]; null leaves it out (personal, not the org's news). */
const AUTH: Record<string, [ActivityKind, string] | null> = {
  invitation_create: ["member", "Invited"],
  invitation_revoke: ["member", "Revoked the invitation for"],
  invitation_accept: ["member", "Joined the org"],
  member_remove: ["member", "Removed member"],
  role_change: ["member", "Changed the role of"],
  token_create: ["secret", "Created API token"],
  token_revoke: ["secret", "Revoked API token"],
};

function auditEntry(i: HistoryItem): ActivityEntry | null {
  const action = i.kind;
  // Deploys are told by the controller's own events, with their outcome.
  if (action === "app_deploy" || action === "app_redeploy" || action.endsWith(".deploy")) return null;
  const failed = i.level !== null && i.level !== "ok";
  const subject = i.object ?? undefined;
  const base = { key: `a:${i.id}`, actor: actorLabel(i.actor), time: i.time, count: 1, after: failed ? ` (${i.level})` : undefined };
  if (action.startsWith("auth.")) {
    const a = AUTH[action.slice(5)];
    if (!a) return null;
    if (action === "auth.invitation_accept" && !subject && i.actor) {
      return { ...base, kind: "member", before: "", subject: i.actor, after: " joined the org", actor: undefined };
    }
    return { ...base, kind: failed ? "warn" : a[0], before: subject ? `${a[1]} ` : a[1], subject };
  }
  // A read is evidence, not news: the daemon marks them, and older rows are
  // told by their action's name (monitor_list, overview, ...).
  const parts = action.split("_");
  if (i.details?.read_only === true || parts.some((p) => READS.has(p))) return null;
  const verbAt = parts.findIndex((p) => p in VERBS);
  let kind: ActivityKind = "update";
  let before: string;
  if (verbAt >= 0) {
    const verb = parts[verbAt];
    const noun = [...parts.slice(0, verbAt), ...parts.slice(verbAt + 1)].join(" ");
    before = `${VERBS[verb]} ${noun}`.trim();
    kind = verb === "create" || verb === "add" ? "create" : verb === "delete" || verb === "remove" || verb === "unset" ? "delete" : "update";
  } else if (subject && parts.length > 1) {
    // app_webhook on web: "Changed the webhook of app web".
    before = `Changed the ${parts.slice(1).join(" ")} of ${parts[0]}`;
  } else {
    before = `Changed ${parts.join(" ")}`;
  }
  if (action.startsWith("secret")) kind = kind === "update" ? "secret" : kind;
  if (failed) kind = "warn";
  return { ...base, kind, before: subject ? `${before} ` : before, subject };
}

const NOISE = /^(slot \d+:|rolling out|rollout of rev .* complete|app \S+: deployment \d+: (building|deploying|done|failed))/;

function controllerEntry(i: HistoryItem, terminal: Set<string>): ActivityEntry | null {
  const msg = i.message ?? "";
  const service = (i.details?.service as string | undefined) ?? undefined;
  let m: RegExpMatchArray | null;
  if (i.kind === "deploy.succeeded" && (m = msg.match(/^app (\S+): deployment (\d+)/))) {
    return { key: `c:${i.id}`, kind: "deploy-ok", before: "", subject: m[1], after: ` deployed · #${m[2]}`, time: i.time, count: 1, to: `apps/${m[1]}/deployments/${m[2]}` };
  }
  if (i.kind === "deploy.failed" && (m = msg.match(/^app (\S+): deployment (\d+) failed:?\s*(.*)$/s))) {
    return {
      key: `c:${i.id}`,
      kind: "deploy-failed",
      before: "",
      subject: m[1],
      after: ` failed to deploy · #${m[2]}`,
      detail: m[3] || undefined,
      time: i.time,
      count: 1,
      to: `apps/${m[1]}/deployments/${m[2]}`,
    };
  }
  if (i.kind === "health.unhealthy") {
    return { key: `c:${i.id}`, kind: "health-bad", before: "", subject: service ?? i.object ?? undefined, after: " became unhealthy", detail: msg || undefined, time: i.time, count: 1 };
  }
  if (i.kind === "health.recovered") {
    return { key: `c:${i.id}`, kind: "health-ok", before: "", subject: service ?? i.object ?? undefined, after: " is healthy again", time: i.time, count: 1 };
  }
  if ((m = msg.match(/^app (\S+): deployment (\d+) queued by (.+)$/))) {
    // Started: only worth a line while it has no outcome yet.
    if (terminal.has(`${m[1]}#${m[2]}`)) return null;
    return { key: `c:${i.id}`, kind: "deploy-running", before: "", subject: m[1], after: ` deploying · #${m[2]}`, actor: actorLabel(m[3]), time: i.time, count: 1, to: `apps/${m[1]}/deployments/${m[2]}` };
  }
  if (i.level === "info" || i.level === null) {
    if (NOISE.test(msg) || !msg) return null;
    return { key: `c:${i.id}`, kind: "info", before: msg.charAt(0).toUpperCase() + msg.slice(1), time: i.time, count: 1, actor: undefined, subject: undefined };
  }
  return {
    key: `c:${i.id}`,
    kind: i.level === "error" ? "deploy-failed" : "warn",
    before: service ? "" : msg,
    subject: service,
    after: service ? `: ${msg}` : undefined,
    time: i.time,
    count: 1,
  };
}

/** Newest-first history items as at most `max` readable entries. */
export function humanize(items: HistoryItem[], max = 10): ActivityEntry[] {
  const terminal = new Set<string>();
  for (const i of items) {
    const m = (i.kind === "deploy.succeeded" || i.kind === "deploy.failed") && (i.message ?? "").match(/^app (\S+): deployment (\d+)/);
    if (m) terminal.add(`${m[1]}#${m[2]}`);
  }
  const out: ActivityEntry[] = [];
  for (const i of items) {
    if (i.source !== "controller" && i.source !== "audit") continue;
    const e = i.source === "audit" ? auditEntry(i) : controllerEntry(i, terminal);
    if (!e) continue;
    const prev = out[out.length - 1];
    if (prev && prev.kind === e.kind && prev.before === e.before && prev.subject === e.subject && prev.after === e.after && prev.actor === e.actor && !e.to) {
      prev.count++;
      continue;
    }
    out.push(e);
    if (out.length >= max) break;
  }
  return out;
}

export function OrgActivity({ org, className }: { org: string; className?: string }) {
  const o = encodeURIComponent(org);
  // Under the org's keys: useLiveSync refetches it as events arrive.
  const q = useQuery({
    queryKey: [...keys.org(org), "activity"],
    queryFn: () => callTool<HistoryPage>("history_query", { limit: 200, source: "controller,audit" }, org),
    refetchInterval: 60_000,
  });
  const entries = humanize(q.data?.items ?? []);
  return (
    <Card className={cn("gap-0 overflow-hidden py-0", className)}>
      <CardHeader className="flex flex-row items-center justify-between border-b px-5 py-3 [.border-b]:pb-3">
        <CardTitle className="text-[15px] font-semibold tracking-tight">Activity</CardTitle>
        <Button asChild variant="ghost" size="sm" className="-mr-2 text-muted-foreground">
          <Link to={`/orgs/${o}/history`}>
            View all
            <ArrowRight />
          </Link>
        </Button>
      </CardHeader>
      {q.isLoading ? (
        <div className="space-y-4 p-5">
          {[0, 1, 2, 3, 4].map((k) => (
            <div key={k} className="flex gap-3">
              <Skeleton className="size-7 rounded-md" />
              <div className="flex-1 space-y-1.5">
                <Skeleton className="h-3.5 w-3/4" />
                <Skeleton className="h-3 w-1/3" />
              </div>
            </div>
          ))}
        </div>
      ) : q.error ? (
        <div className="p-5">
          <QueryError error={q.error} />
        </div>
      ) : entries.length === 0 ? (
        <EmptyState icon={Activity} title="No activity yet" compact>
          Deploys, health changes and changes people make show up here.
        </EmptyState>
      ) : (
        <ol className="max-h-[27rem] overflow-y-auto py-1.5">
          {entries.map((e) => (
            <ActivityRow key={e.key} e={e} org={o} />
          ))}
        </ol>
      )}
    </Card>
  );
}

function ActivityRow({ e, org }: { e: ActivityEntry; org: string }) {
  const [Icon, tone] = KIND_STYLE[e.kind];
  const body = (
    <>
      <span className={cn("mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md", TILE_BG[tone])}>
        <Icon className={cn("size-3.5", TONE_TEXT[tone], e.kind === "deploy-running" && "motion-safe:animate-spin")} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-[13px] leading-snug break-words">
          {e.before}
          {e.subject && <span className="font-semibold">{e.subject}</span>}
          {e.after}
          {e.count > 1 && <span className="ml-1.5 rounded bg-muted px-1 py-px text-[11px] font-medium text-muted-foreground tabular-nums">×{e.count}</span>}
        </span>
        {e.detail && <span className="mt-0.5 line-clamp-2 block font-mono text-[11px] text-muted-foreground">{e.detail}</span>}
        <span className="mt-0.5 block truncate text-xs text-muted-foreground">
          {e.actor && <>{e.actor} · </>}
          <span className="tabular-nums" title={new Date(e.time).toLocaleString()}>
            {relativeTime(e.time / 1000)}
          </span>
        </span>
      </span>
    </>
  );
  const cls = "flex gap-3 px-5 py-2.5";
  return (
    <li>
      {e.to ? (
        <Link to={`/orgs/${org}/${e.to}`} className={cn(cls, "transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none")}>
          {body}
        </Link>
      ) : (
        <div className={cls}>{body}</div>
      )}
    </li>
  );
}
