// The Deployments tab: history, newest first, with rollback.
import { ArrowRight, ChevronRight, CircleCheck, GitCommitHorizontal, History, Package, RotateCcw, Terminal, User, Webhook } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useReducer, useState } from "react";
import { Link, useNavigate } from "react-router";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import { DEPLOYMENT_TONE, inProgress } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type App, currentOf, type Deployment, finished, isGit, useDeployments } from "./api";
import { isMine, queuedEvent } from "./follow";
import { splitStack, useLiveEvents } from "./live";
import { deploymentPath, useDeploy } from "./use-deploy";
import { ConfirmDialog, DeploymentBadge, EmptyState, QueryError } from "./components";
import { duration, imageName, shortDigest, shortSha } from "./util";

export const TRIGGER_LABEL: Record<Deployment["trigger"], string> = { manual: "CLI", api: "Manual", webhook: "Webhook" };
const TRIGGER_ICON: Record<Deployment["trigger"], typeof User> = { manual: Terminal, api: User, webhook: Webhook };

/** Elapsed time of a deployment: until it finished, or until now; null when it finished at a time not recorded. */
export function elapsed(d: Deployment, now = Date.now()): number | null {
  const start = d.started_at ?? d.created_at;
  if (!start) return null;
  if (d.finished_at === undefined && finished(d.status)) return null;
  return (d.finished_at ?? now) - start;
}

/** A duration for a list or a header; `coarse` when the times are whole seconds, so under one is "<1s". */
export function elapsedText(ms: number, coarse?: boolean): string {
  return coarse && ms < 1000 ? "<1s" : duration(ms);
}

export function DeploymentsTab({ org, app }: { org: string; app: App }) {
  const deps = useDeployments(org, app.name, 30);
  const deploy = useDeploy(org, app.name);
  const writer = canWrite(useMe().data!, org);
  const [rollback, setRollback] = useState<Deployment | null>(null);
  const current = currentOf(deps.data?.deployments, deps.data?.current ?? app.current_deployment);
  const list = deps.data?.deployments ?? [];
  const o = encodeURIComponent(org);
  const navigate = useNavigate();
  const since = useMemo(() => Date.now(), []);
  const moving = list.some((d) => inProgress(d.status));

  // A deployment that starts while you look at the list (a webhook push, a
  // teammate) opens at once, its log following.
  useLiveEvents((e) => {
    if (e.at < since || e.service !== app.name || splitStack(e.stack).org !== org) return;
    const q = queuedEvent(e.message);
    if (q && q.app === app.name && !isMine(org, q.app, q.id)) navigate(deploymentPath(org, q.app, q.id));
  });

  // In-progress rows count their time up.
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!moving) return;
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [moving]);

  if (deps.error) return <QueryError error={deps.error} />;

  return (
    <>
      <Card className="gap-0 overflow-hidden py-0">
        <div className="flex flex-col gap-1 border-b px-5 py-4 sm:flex-row sm:items-end sm:justify-between sm:gap-4">
          <div className="min-w-0 space-y-1">
            <h2 className="text-[15px] font-semibold tracking-tight">Deployments</h2>
            <p className="text-[13px] text-muted-foreground">The last 30, each with its build and rollout log. Roll back to an earlier one without a build.</p>
          </div>
          {list.length > 0 && <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:inline">{list.length} shown</span>}
        </div>
        {deps.isLoading ? (
          <ul className="divide-y" aria-busy>
            {[0, 1, 2, 3].map((i) => (
              <li key={i} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 px-5 py-3.5 sm:grid-cols-[9.5rem_minmax(0,1fr)_auto]">
                <div className="flex items-center gap-2.5">
                  <Skeleton className="size-2 rounded-full" />
                  <Skeleton className="h-4 w-10" />
                  <Skeleton className="h-5 w-14 rounded-full" />
                </div>
                <div className="hidden space-y-2 sm:block">
                  <Skeleton className="h-4 w-64 max-w-full" />
                  <Skeleton className="h-3 w-40" />
                </div>
                <Skeleton className="h-3.5 w-16" />
              </li>
            ))}
          </ul>
        ) : list.length === 0 ? (
          <EmptyState
            icon={History}
            title="No deployments yet"
            action={
              writer && (
                <Button onClick={() => deploy.run().catch(() => {})} disabled={deploy.pending}>
                  Deploy {app.name}
                </Button>
              )
            }
          >
            Each deploy shows up here with its build and rollout log, live.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {list.map((d) => (
              <DeploymentRow
                key={d.id}
                d={d}
                to={`/orgs/${o}/apps/${encodeURIComponent(app.name)}/deployments/${d.id}`}
                current={d.id === current}
                onRollback={writer && d.status === "done" && d.id !== current && !!current ? () => setRollback(d) : undefined}
              />
            ))}
          </ul>
        )}
        {list.length > 0 && (
          <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1 border-t bg-muted/30 px-5 py-2.5 text-xs text-muted-foreground">
            <Webhook className="size-3.5 shrink-0" />
            {isGit(app.source) ? "A push to the branch deploys through the webhook." : "A registry or CI webhook can deploy too."}
            <Link to={`/orgs/${o}/apps/${encodeURIComponent(app.name)}/general`} className="inline-flex items-center gap-0.5 font-medium text-foreground underline-offset-4 hover:underline">
              Webhook settings
              <ArrowRight className="size-3" />
            </Link>
          </div>
        )}
      </Card>
      <ConfirmDialog
        open={rollback !== null}
        onOpenChange={(open) => !open && setRollback(null)}
        destructive={false}
        title={`Roll back to deployment #${rollback?.id}?`}
        description={
          <>
            A new deployment puts back #{rollback?.id}'s image
            {rollback?.digest ? ` (${shortDigest(rollback.digest)})` : ""} and the settings it ran with, without building. The app's saved settings stay as they
            are, so the next deploy applies them again.
          </>
        }
        confirmLabel="Roll back"
        onConfirm={async () => {
          if (rollback) await deploy.run("app_rollback", { deployment: rollback.id });
        }}
      />
    </>
  );
}

/** Marks the deployment that is running now. */
function CurrentChip({ className }: { className?: string }) {
  return (
    <span className={cn("h-5.5 shrink-0 items-center gap-1 rounded-full border bg-background px-2 text-xs font-medium text-foreground shadow-xs", className)}>
      <CircleCheck className="size-3 text-success" />
      Current
    </span>
  );
}

/**
 * One deployment in a history list: status, what was deployed and by whom,
 * when, and Roll back. `summary` replaces the what-was-deployed line (a
 * compose stack's deployment names its services, not an image), and
 * `action` the how line (a compose stack's environment or domains change).
 */
export function DeploymentRow({
  d,
  to,
  current,
  onRollback,
  summary,
  action,
  coarse,
}: {
  d: Deployment;
  to: string;
  current: boolean;
  onRollback?: () => void;
  summary?: ReactNode;
  action?: string;
  /** Its times are whole seconds. */
  coarse?: boolean;
}) {
  const live = inProgress(d.status);
  const ms = elapsed(d);
  const TriggerIcon = TRIGGER_ICON[d.trigger];
  return (
    <li
      className={cn(
        "group relative grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1.5 px-5 py-3.5 transition-colors hover:bg-muted/40 sm:grid-cols-[9.5rem_minmax(0,1fr)_auto]",
        live && "bg-info/[0.04]",
      )}
    >
      {live && <span className="absolute inset-y-0 left-0 w-0.5 bg-info" aria-hidden />}
      <Link to={to} className="absolute inset-0 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none focus-visible:ring-inset" aria-label={`Deployment ${d.id}`} />

      {/* Status: dot, #id, badge; the duration under it. */}
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex items-center gap-2.5">
          <StatusDot tone={DEPLOYMENT_TONE[d.status]} pulse={live} />
          <span className="font-mono text-sm font-semibold tabular-nums">#{d.id}</span>
          <DeploymentBadge status={d.status} />
          {current && <CurrentChip className="inline-flex sm:hidden" />}
        </div>
        <span className="pl-[18px] text-xs text-muted-foreground tabular-nums">
          {live ? (
            <span className="font-medium text-info">{ms !== null ? elapsedText(ms, coarse) : "Starting"}</span>
          ) : ms !== null && (ms || coarse) ? (
            elapsedText(ms, coarse)
          ) : (
            "–"
          )}
        </span>
      </div>

      {/* What was deployed, and who started it. */}
      <div className="col-span-2 row-start-2 min-w-0 space-y-1 pl-[18px] sm:col-span-1 sm:row-start-auto sm:pl-0">
        {summary ?? (d.commit ? (
          <p className="flex min-w-0 items-center gap-1.5 text-sm">
            <GitCommitHorizontal className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="shrink-0 font-mono text-xs text-muted-foreground">{shortSha(d.commit.sha)}</span>
            <span className="truncate font-medium">{d.commit.message}</span>
          </p>
        ) : d.image ? (
          <p className="flex min-w-0 items-center gap-1.5 text-sm">
            <Package className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="truncate font-mono text-xs font-medium">{imageName(d.image)}</span>
            {d.digest && <span className="hidden shrink-0 font-mono text-xs text-muted-foreground sm:inline">@{shortDigest(d.digest)}</span>}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">{d.status === "queued" ? "Waiting to start" : "No image recorded"}</p>
        ))}
        <p className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          {d.rollback_of ? <RotateCcw className="size-3.5 shrink-0" /> : <TriggerIcon className="size-3.5 shrink-0" />}
          <span className="truncate">
            {action ?? (d.rollback_of ? `Rollback to #${d.rollback_of}` : TRIGGER_LABEL[d.trigger])} by {d.by}
          </span>
        </p>
        {d.status === "failed" && d.error && <p className="line-clamp-2 text-xs break-words text-destructive">{d.error}</p>}
      </div>

      {/* When, the current marker, and rollback. */}
      <div className="col-start-2 row-start-1 flex shrink-0 items-center justify-end gap-3 sm:col-start-auto sm:row-start-auto">
        {current && <CurrentChip className="hidden sm:inline-flex" />}
        {onRollback && (
          <Button variant="outline" size="sm" className="relative z-10 hidden opacity-100 sm:inline-flex sm:opacity-0 sm:group-focus-within:opacity-100 sm:group-hover:opacity-100" onClick={onRollback}>
            <RotateCcw />
            Roll back
          </Button>
        )}
        <span className="min-w-20 text-right text-xs whitespace-nowrap text-muted-foreground tabular-nums" title={dateTime(d.created_at / 1000)}>
          {relativeTime(d.created_at / 1000)}
        </span>
        <ChevronRight className="hidden size-4 text-muted-foreground/60 transition-transform group-hover:translate-x-0.5 sm:block" />
      </div>
      {onRollback && (
        <div className="relative z-10 col-span-2 pl-[18px] sm:hidden">
          <Button variant="outline" size="sm" onClick={onRollback}>
            <RotateCcw />
            Roll back
          </Button>
        </div>
      )}
    </li>
  );
}
