// /orgs/:org/apps/:app/deployments/:id: one deployment, live. It renders
// from the record the deploy button put in the cache, reads the log from
// the first byte at once, and then pulls on every event the feed sends
// about this deployment, so the text and the status move together.
import { useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  ArrowUpRight,
  Check,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  CircleX,
  ExternalLink,
  GitCommitHorizontal,
  Loader2,
  Package,
  RotateCcw,
  Rocket,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { callTool } from "@/api/tools";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import { DEPLOYMENT_TONE, inProgress, STEPS, type StepState, stepStates } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type App, currentOf, type Deployment, finished, isGit, keys, serviceOf, useDeployments, useStack } from "./api";
import { ConfirmDialog, DeploymentBadge, QueryError } from "./components";
import { elapsed, TRIGGER_LABEL } from "./deployments-tab";
import { buildStepLabel, DeploymentFollow, type LogReply, queuedEvent } from "./follow";
import { splitStack, useLiveEvents } from "./live";
import { LogView } from "./log-view";
import { deploymentPath, useDeploy } from "./use-deploy";
import { duration, imageName, shortDigest, shortSha } from "./util";

/** Follow one deployment: its log and record, pulled when the feed says so. */
function useFollow(org: string, app: string, id: number) {
  const qc = useQueryClient();
  const seed = qc.getQueryData<Deployment>(keys.deployment(org, app, id));
  const follow = useMemo(() => new DeploymentFollow(seed), [org, app, id]); // eslint-disable-line react-hooks/exhaustive-deps -- a new follower per deployment; the cached record only seeds it
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [error, setError] = useState<unknown>(null);
  const inflight = useRef(false);
  const again = useRef(false);
  const lastEvent = useRef(0);

  const pull = useCallback(async () => {
    if (inflight.current) {
      again.current = true;
      return;
    }
    if (follow.finished) return;
    inflight.current = true;
    try {
      // Read until caught up (a long log comes in pieces).
      for (let i = 0; i < 50; i++) {
        const at = follow.log.offset;
        const r = await callTool<LogReply>("app_deployment_log", { name: app, deployment: id, offset: at }, org);
        const hadLines = follow.firstLineAt !== null;
        const wasDone = follow.finished;
        follow.apply(at, r);
        // Finished: the app's state, current deployment and replicas moved.
        if (!wasDone && follow.finished) void qc.invalidateQueries({ queryKey: keys.org(org) });
        if (!hadLines && follow.firstLineAt !== null) performance.mark("isb:first-log-line");
        if (follow.record) qc.setQueryData(keys.deployment(org, app, id), follow.record);
        setError(null);
        bump();
        if (r.finished || r.offset === at || !r.log) break;
      }
    } catch (e) {
      setError(e);
    } finally {
      inflight.current = false;
      if (again.current) {
        again.current = false;
        void pull();
      }
    }
  }, [org, app, id, follow, qc]);

  // Every log line and status change of this deployment is an event; each
  // one pulls the new text (coalesced while a pull is in flight).
  useLiveEvents((e) => {
    if (splitStack(e.stack).org !== org || e.service !== app) return;
    const kind = follow.onEvent(e.message, app, id);
    if (!kind) return;
    lastEvent.current = Date.now();
    bump();
    void pull();
    if (kind === "state") void qc.invalidateQueries({ queryKey: keys.deployments(org, app) });
  });

  useEffect(() => {
    void pull();
  }, [pull]);

  // The clock ticks each second while it runs; a quiet feed (dropped
  // connection, a slow step) still gets a pull every few seconds.
  const done = follow.finished;
  useEffect(() => {
    if (done) return;
    const t = setInterval(() => {
      bump();
      if (Date.now() - lastEvent.current > 3000) void pull();
    }, 1000);
    return () => clearInterval(t);
  }, [done, pull]);

  return { follow, pull, error };
}

const STEP_ICON: Record<StepState, typeof Check> = { done: Check, current: Loader2, failed: X, waiting: Check, skipped: CircleSlash };

function Progress({ d, reached, git }: { d: Deployment; reached: DeploymentFollow["reached"]; git: boolean }) {
  const states = stepStates(d.status, reached);
  const labels: Record<(typeof STEPS)[number], string> = { queued: "Queued", building: buildStepLabel(d, git), deploying: "Roll out", done: "Live" };
  return (
    <ol className="flex items-start gap-1.5 sm:items-center" aria-label="Progress">
      {STEPS.map((s, i) => {
        const st = states[s];
        const Icon = STEP_ICON[st];
        return (
          <li key={s} className="flex min-w-0 flex-1 flex-col items-center gap-1 sm:flex-row sm:gap-1.5">
            <span
              className={cn(
                "flex size-5 shrink-0 items-center justify-center rounded-full border text-[10px] transition-colors",
                st === "done" && "border-success/40 bg-success/15 text-success",
                st === "current" && "border-info/40 bg-info/15 text-info",
                st === "failed" && "border-destructive/40 bg-destructive/15 text-destructive",
                (st === "waiting" || st === "skipped") && "border-border bg-muted text-muted-foreground/60",
              )}
              aria-label={`${labels[s]}: ${st}`}
            >
              {st === "waiting" ? <span className="size-1.5 rounded-full bg-current" /> : <Icon className={cn("size-3", st === "current" && "animate-spin")} strokeWidth={2.5} />}
            </span>
            <span className={cn("truncate text-[11px] font-medium sm:text-xs", st === "waiting" || st === "skipped" ? "text-muted-foreground" : "text-foreground")}>{labels[s]}</span>
            {i < STEPS.length - 1 && (
              <span className={cn("hidden h-px min-w-3 flex-1 rounded-full sm:block", st === "done" ? "bg-success/50" : "bg-border")} aria-hidden />
            )}
          </li>
        );
      })}
    </ol>
  );
}

function StatusGlyph({ d }: { d: Deployment }) {
  const tone = DEPLOYMENT_TONE[d.status];
  const box = cn(
    "flex size-10 shrink-0 items-center justify-center rounded-xl border",
    tone === "success" && "border-success/30 bg-success/10 text-success",
    tone === "info" && "border-info/30 bg-info/10 text-info",
    tone === "danger" && "border-destructive/30 bg-destructive/10 text-destructive",
    (tone === "neutral" || tone === "muted") && "bg-muted text-muted-foreground",
  );
  const Icon = d.status === "done" ? CircleCheck : d.status === "failed" ? CircleX : d.status === "superseded" ? CircleSlash : Loader2;
  return (
    <span className={box}>
      <Icon className={cn("size-5", inProgress(d.status) && "animate-spin")} />
    </span>
  );
}

export function DeploymentPage({ org, app, id }: { org: string; app: App; id: number }) {
  const { follow, error } = useFollow(org, app.name, id);
  const history = useDeployments(org, app.name, 30);
  const stack = useStack(org, app.stack);
  const current = currentOf(history.data?.deployments, app.current_deployment);
  const deploy = useDeploy(org, app.name);
  const writer = canWrite(useMe().data!, org);
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const then = (params.get("then") ?? "").split(",").filter(Boolean);
  const [rollback, setRollback] = useState<number | null>(null);
  const o = encodeURIComponent(org);
  const d = follow.record;
  const openedAt = useMemo(() => Date.now(), [id]); // eslint-disable-line react-hooks/exhaustive-deps -- the clock restarts when the page moves to another deployment

  // A template deploys its apps one after another: when this one is done,
  // follow the next as soon as its deployment is queued.
  useLiveEvents((e) => {
    if (!then.length || e.at < openedAt) return;
    if (splitStack(e.stack).org !== org) return;
    const q = queuedEvent(e.message);
    if (q && q.app === then[0]) navigate(deploymentPath(org, q.app, q.id, then.slice(1)), { replace: true });
  });

  // A newer deployment of this app while you watch a finished one (a push
  // to the webhook, a teammate): follow it.
  const newest = history.data?.deployments[0];
  useEffect(() => {
    if (d && finished(d.status) && newest && newest.id > d.id && !finished(newest.status) && newest.created_at > openedAt) {
      navigate(deploymentPath(org, app.name, newest.id), { replace: true });
    }
  }, [d, newest, org, app.name, navigate, openedAt]);

  if (!d) {
    return error ? (
      <QueryError error={error} />
    ) : (
      <div className="grid gap-4">
        <Skeleton className="h-36" />
        <Skeleton className="h-96" />
      </div>
    );
  }

  const lines = follow.log.buf.all();
  const first = follow.log.buf.dropped + 1;
  const ms = elapsed(d);
  const done = finished(d.status);
  const lastGood = history.data?.deployments.find((x) => x.status === "done" && x.id !== d.id && x.id < d.id);
  const urls = (serviceOf(stack.data, app.name)?.domains ?? []).map((x) => x.url).filter(Boolean) as string[];
  const recent = (history.data?.deployments ?? []).slice(0, 12);
  const git = isGit(app.source);

  return (
    <div className="grid animate-fade-up gap-4 xl:grid-cols-[minmax(0,1fr)_16rem]">
      <div className="grid min-w-0 content-start gap-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Button asChild variant="ghost" size="sm" className="-ml-2 text-muted-foreground">
            <Link to={`/orgs/${o}/apps/${app.name}/deployments`}>
              <ArrowLeft />
              All deployments
            </Link>
          </Button>
        </div>
        <Card className="gap-5 px-5 py-5">
          <div className="flex flex-wrap items-start gap-4">
            <StatusGlyph d={d} />
            <div className="min-w-0 flex-1 space-y-1">
              <div className="flex flex-wrap items-center gap-2">
                <h2 className="text-lg font-semibold tracking-tight">
                  Deployment <span className="font-mono">#{d.id}</span>
                </h2>
                <DeploymentBadge status={d.status} />
                {d.id === current && <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">Current</span>}
              </div>
              <p className="text-[13px] text-muted-foreground">
                {d.rollback_of ? (
                  <>
                    Rollback to{" "}
                    <Link className="font-medium text-foreground underline-offset-4 hover:underline" to={deploymentPath(org, app.name, d.rollback_of)}>
                      #{d.rollback_of}
                    </Link>{" "}
                    ·{" "}
                  </>
                ) : null}
                {TRIGGER_LABEL[d.trigger]} by <span className="font-medium text-foreground">{d.by}</span> ·{" "}
                <span title={dateTime(d.created_at / 1000)}>{relativeTime(d.created_at / 1000)}</span>
              </p>
            </div>
            <div className="text-right">
              <div className="font-mono text-2xl font-semibold tracking-tight tabular-nums" aria-label="Duration">
                {ms !== null ? duration(ms) : "–"}
              </div>
              <div className="text-xs text-muted-foreground">{done ? "Total" : "Elapsed"}</div>
            </div>
          </div>
          <Progress d={d} reached={follow.reached} git={git} />
          <dl className="grid grid-cols-2 gap-x-6 gap-y-3 border-t pt-4 text-sm sm:grid-cols-3">
            <div className="col-span-2 min-w-0 sm:col-span-1">
              <dt className="text-xs text-muted-foreground">Source</dt>
              <dd className="mt-0.5 flex min-w-0 items-center gap-1.5">
                {d.commit ? (
                  <>
                    <GitCommitHorizontal className="size-4 shrink-0 text-muted-foreground" />
                    <span className="font-mono text-xs">{shortSha(d.commit.sha)}</span>
                    <span className="truncate" title={d.commit.message}>
                      {d.commit.message}
                    </span>
                  </>
                ) : d.image ? (
                  <>
                    <Package className="size-4 shrink-0 text-muted-foreground" />
                    <span className="truncate font-mono text-xs" title={d.image}>
                      {imageName(d.image)}
                    </span>
                  </>
                ) : (
                  <span className="text-muted-foreground">{git ? "Not fetched yet" : "Resolving image"}</span>
                )}
              </dd>
            </div>
            <div className="min-w-0">
              <dt className="text-xs text-muted-foreground">Digest</dt>
              <dd className="mt-0.5 truncate font-mono text-xs" title={d.digest}>
                {d.digest ? shortDigest(d.digest) : <span className="font-sans text-sm text-muted-foreground">–</span>}
              </dd>
            </div>
            <div className="min-w-0">
              <dt className="text-xs text-muted-foreground">Requested</dt>
              <dd className="mt-0.5 truncate">{d.requested ?? <span className="text-muted-foreground">–</span>}</dd>
            </div>
          </dl>
        </Card>

        {d.status === "done" && (
          <Outcome tone="success" icon={CircleCheck} title={d.id === current ? `${app.name} is live with this deployment` : "Deployed"}>
            <p>
              {d.id === current
                ? "Every replica runs this image and these settings."
                : `A newer deployment (#${current}) replaced it since; roll back to put this one back.`}
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              {urls.slice(0, 2).map((u) => (
                <Button key={u} asChild size="sm" variant="outline" className="bg-background">
                  <a href={u} target="_blank" rel="noreferrer">
                    <ExternalLink />
                    {u.replace(/^https?:\/\//, "")}
                  </a>
                </Button>
              ))}
              {writer && d.id !== current && (
                <Button size="sm" variant="outline" className="bg-background" onClick={() => setRollback(d.id)}>
                  <RotateCcw />
                  Roll back to #{d.id}
                </Button>
              )}
              {then.length > 0 && (
                <span className="inline-flex h-8 items-center gap-2 text-xs text-muted-foreground">
                  <Loader2 className="size-3.5 animate-spin" />
                  Next: {then[0]}
                </span>
              )}
            </div>
          </Outcome>
        )}
        {d.status === "failed" && (
          <Outcome tone="danger" icon={CircleAlert} title={failureTitle(d)}>
            {d.error && <p className="font-mono text-xs break-words whitespace-pre-wrap">{d.error}</p>}
            <p className="mt-1">{failureHint(d, app)}</p>
            {writer && (
              <div className="mt-3 flex flex-wrap gap-2">
                <Button size="sm" onClick={() => deploy.run().catch(() => {})} disabled={deploy.pending}>
                  {deploy.pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                  Deploy again
                </Button>
                {lastGood && (
                  <Button size="sm" variant="outline" className="bg-background" onClick={() => setRollback(lastGood.id)}>
                    <RotateCcw />
                    Roll back to #{lastGood.id}
                  </Button>
                )}
                {lastGood && (
                  <Button asChild size="sm" variant="ghost">
                    <Link to={deploymentPath(org, app.name, lastGood.id)}>
                      View #{lastGood.id}
                      <ArrowUpRight />
                    </Link>
                  </Button>
                )}
              </div>
            )}
          </Outcome>
        )}
        {d.status === "superseded" && (
          <Outcome tone="neutral" icon={CircleSlash} title="Superseded">
            A newer deploy replaced this one before it started{d.error ? `: ${d.error}` : ""}.
          </Outcome>
        )}

        {error ? <QueryError error={error} /> : null}
        <LogView
          lines={lines}
          firstLine={first}
          live={!done}
          filename={`${app.name}-deployment-${d.id}.log`}
          title={`${app.name} · #${d.id}`}
          status={!done ? <span className="text-[11px] text-zinc-500">{d.status === "queued" ? "waiting" : "streaming"}</span> : undefined}
          empty={d.status === "queued" ? "Queued: waiting for the deploy before it to finish..." : "Waiting for the first line..."}
        />
      </div>

      <aside className="hidden xl:block">
        <div className="sticky top-16 rounded-xl border bg-card">
          <div className="border-b px-4 py-3 text-xs font-medium tracking-wide text-muted-foreground uppercase">Recent deployments</div>
          <ul className="max-h-[70svh] overflow-y-auto p-1.5">
            {recent.map((x) => (
              <li key={x.id}>
                <Link
                  to={deploymentPath(org, app.name, x.id)}
                  replace
                  className={cn(
                    "flex items-center gap-2.5 rounded-md px-2.5 py-2 text-sm transition-colors hover:bg-accent",
                    x.id === d.id && "bg-accent font-medium",
                  )}
                >
                  <StatusDot tone={DEPLOYMENT_TONE[x.status]} pulse={inProgress(x.status)} />
                  <span className="font-mono text-xs">#{x.id}</span>
                  <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                    {x.rollback_of ? `rollback to #${x.rollback_of}` : x.commit ? x.commit.message : TRIGGER_LABEL[x.trigger].toLowerCase()}
                  </span>
                  <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums">{relativeTime(x.created_at / 1000).replace(" ago", "")}</span>
                </Link>
              </li>
            ))}
          </ul>
        </div>
      </aside>

      <ConfirmDialog
        open={rollback !== null}
        onOpenChange={(v) => !v && setRollback(null)}
        destructive={false}
        title={`Roll back to deployment #${rollback}?`}
        description="A new deployment puts back that one's image and settings, without building. The app's saved settings stay as they are."
        confirmLabel="Roll back"
        onConfirm={() => deploy.run("app_rollback", { deployment: rollback })}
      />
    </div>
  );
}

function Outcome({
  tone,
  icon: Icon,
  title,
  children,
}: {
  tone: "success" | "danger" | "neutral";
  icon: typeof Check;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div
      role={tone === "danger" ? "alert" : "status"}
      className={cn(
        "flex animate-fade-up gap-3 rounded-xl border px-4 py-3.5 text-[13px]",
        tone === "success" && "border-success/25 bg-success/[0.06]",
        tone === "danger" && "border-destructive/30 bg-destructive/[0.06]",
        tone === "neutral" && "bg-muted/50",
      )}
    >
      <Icon className={cn("mt-0.5 size-4 shrink-0", tone === "success" && "text-success", tone === "danger" && "text-destructive", tone === "neutral" && "text-muted-foreground")} />
      <div className="min-w-0 flex-1">
        <p className="font-semibold">{title}</p>
        <div className="mt-0.5 text-muted-foreground">{children}</div>
      </div>
    </div>
  );
}

function failureTitle(d: Deployment): string {
  if (!d.image && !d.commit) return "The deployment failed before it had an image";
  if (!d.image) return "The build failed";
  return "The rollout failed";
}

function failureHint(d: Deployment, app: App): string {
  const e = d.error ?? "";
  if (/builds are not implemented/i.test(e)) return "This server cannot build from git yet. Deploy a prebuilt image instead (General, Source).";
  if (/auth|permission denied|could not read|403|401/i.test(e) && isGit(app.source)) return "Check the repository's access: the deploy key or token secret in General, Source.";
  if (!d.image && isGit(app.source)) return "The full build output is in the log below.";
  return "The log below has the details; fix the cause and deploy again.";
}
