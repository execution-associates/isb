// The Deployments tab: history, newest first, with rollback.
import { ChevronRight, GitCommitHorizontal, History, Package, RotateCcw } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { dateTime, relativeTime } from "@/lib/format";
import { type App, currentOf, type Deployment, finished, useDeployments } from "./api";
import { isMine, queuedEvent } from "./follow";
import { splitStack, useLiveEvents } from "./live";
import { deploymentPath, useDeploy } from "./use-deploy";
import { ConfirmDialog, DeploymentBadge, EmptyState, QueryError } from "./components";
import { duration, imageName, shortDigest, shortSha } from "./util";

export const TRIGGER_LABEL: Record<Deployment["trigger"], string> = { manual: "CLI", api: "Manual", webhook: "Webhook" };

/** Elapsed time of a deployment: until it finished, or until now. */
export function elapsed(d: Deployment, now = Date.now()): number | null {
  const start = d.started_at ?? d.created_at;
  if (!start) return null;
  return (d.finished_at ?? (finished(d.status) ? start : now)) - start;
}

export function DeploymentsTab({ org, app }: { org: string; app: App }) {
  const deps = useDeployments(org, app.name, 30);
  const deploy = useDeploy(org, app.name);
  const [rollback, setRollback] = useState<Deployment | null>(null);
  const current = currentOf(deps.data?.deployments, deps.data?.current ?? app.current_deployment);
  const list = deps.data?.deployments ?? [];
  const o = encodeURIComponent(org);
  const navigate = useNavigate();
  const since = useMemo(() => Date.now(), []);

  // A deployment that starts while you look at the list (a webhook push, a
  // teammate) opens at once, its log following.
  useLiveEvents((e) => {
    if (e.at < since || e.service !== app.name || splitStack(e.stack).org !== org) return;
    const q = queuedEvent(e.message);
    if (q && q.app === app.name && !isMine(org, q.app, q.id)) navigate(deploymentPath(org, q.app, q.id));
  });

  if (deps.isLoading) return <Skeleton className="h-64" />;
  if (deps.error) return <QueryError error={deps.error} />;

  return (
    <>
      <Card className="gap-0 overflow-hidden py-0">
        <div className="border-b px-5 py-4">
          <h2 className="text-base font-semibold">Deployments</h2>
          <p className="text-sm text-muted-foreground">The last 30, with their logs. Roll back to any earlier successful one: its image and settings, without a build.</p>
        </div>
        {list.length === 0 ? (
          <EmptyState icon={History} title="No deployments yet">
            Deploy the app and its build and rollout log show up here, live.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {list.map((d) => {
              const canRollback = d.status === "done" && d.id !== current && !!current;
              return (
                <li key={d.id} className="relative flex flex-col gap-2 px-5 py-4 transition-colors hover:bg-muted/40 sm:flex-row sm:items-center sm:gap-4">
                  <Link to={`/orgs/${o}/apps/${app.name}/deployments/${d.id}`} className="absolute inset-0" aria-label={`Deployment ${d.id}`} />
                  <div className="flex w-36 shrink-0 items-center gap-2">
                    <span className="font-mono text-sm font-medium tabular-nums">#{d.id}</span>
                    <DeploymentBadge status={d.status} />
                  </div>
                  <div className="min-w-0 flex-1 space-y-1">
                    {d.commit ? (
                      <p className="flex min-w-0 items-center gap-1.5 text-sm">
                        <GitCommitHorizontal className="size-4 shrink-0 text-muted-foreground" />
                        <span className="font-mono text-xs">{shortSha(d.commit.sha)}</span>
                        <span className="truncate">{d.commit.message}</span>
                      </p>
                    ) : d.image ? (
                      <p className="flex min-w-0 items-center gap-1.5 text-sm">
                        <Package className="size-4 shrink-0 text-muted-foreground" />
                        <span className="truncate font-mono text-xs">{imageName(d.image)}</span>
                        {d.digest && <span className="shrink-0 font-mono text-xs text-muted-foreground">@{shortDigest(d.digest)}</span>}
                      </p>
                    ) : (
                      <p className="text-sm text-muted-foreground">{d.status === "queued" ? "Waiting to start" : "No image recorded"}</p>
                    )}
                    <p className="truncate text-xs text-muted-foreground">
                      {d.rollback_of ? `Rollback to #${d.rollback_of} · ` : ""}
                      {TRIGGER_LABEL[d.trigger]} by {d.by} · <span title={dateTime(d.created_at / 1000)}>{relativeTime(d.created_at / 1000)}</span>
                      {finished(d.status) && elapsed(d) ? ` · ${duration(elapsed(d))}` : ""}
                      {d.id === current ? " · running now" : ""}
                    </p>
                    {d.status === "failed" && d.error && <p className="line-clamp-2 text-xs break-words text-destructive">{d.error}</p>}
                  </div>
                  <div className="relative z-10 flex shrink-0 items-center gap-2 self-end sm:self-auto">
                    {canRollback && (
                      <Button variant="outline" size="sm" onClick={() => setRollback(d)}>
                        <RotateCcw />
                        Roll back
                      </Button>
                    )}
                    <ChevronRight className="hidden size-4 text-muted-foreground sm:block" />
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </Card>
      <ConfirmDialog
        open={rollback !== null}
        onOpenChange={(o) => !o && setRollback(null)}
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
