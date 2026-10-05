// /orgs/:org/apps/:app/deployments/:id: one deployment, live. It renders
// from the record the deploy button put in the cache, reads the log from
// the first byte at once, and then pulls on every event the feed sends
// about this deployment, so the text and the status move together.
import { useQueryClient } from "@tanstack/react-query";
import { GitCommitHorizontal, Package } from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { callTool } from "@/api/tools";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";
import { type App, currentOf, type Deployment, finished, isGit, keys, serviceOf, useDeployments, useStack } from "./api";
import { QueryError } from "./components";
import { DeploymentView } from "./deployment-view";
import { elapsed, TRIGGER_LABEL } from "./deployments-tab";
import { buildStepLabel, DeploymentFollow, type LogReply, queuedEvent } from "./follow";
import { splitStack, useLiveEvents } from "./live";
import { deploymentPath, useDeploy } from "./use-deploy";
import { duration, imageName, shortDigest, shortSha } from "./util";
import { invalidateOrg } from "@/lib/freshness";

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
        if (!wasDone && follow.finished) void invalidateOrg(qc, org);
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

  const done = finished(d.status);
  // Finished at a time not recorded reads 0s, as it always has here.
  const ms = elapsed(d) ?? (done && (d.started_at ?? d.created_at) ? 0 : null);
  const lastGood = history.data?.deployments.find((x) => x.status === "done" && x.id !== d.id && x.id < d.id);
  const urls = (serviceOf(stack.data, app.name)?.domains ?? []).map((x) => x.url).filter(Boolean) as string[];
  const recent = (history.data?.deployments ?? []).slice(0, 12).map((x) => ({
    id: x.id,
    status: x.status,
    created_at: x.created_at,
    label: x.rollback_of ? `rollback to #${x.rollback_of}` : x.commit ? x.commit.message : TRIGGER_LABEL[x.trigger].toLowerCase(),
  }));
  const git = isGit(app.source);

  return (
    <DeploymentView
      d={d}
      current={current}
      back={`/orgs/${o}/apps/${app.name}/deployments`}
      link={(x) => deploymentPath(org, app.name, x)}
      clock={ms !== null ? duration(ms) : "–"}
      steps={{ status: d.status, reached: follow.reached, middle: buildStepLabel(d, git) }}
      facts={[
        {
          label: "Source",
          className: "flex min-w-0 items-center gap-1.5",
          value: d.commit ? (
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
          ),
        },
        {
          label: "Digest",
          className: "truncate font-mono text-xs",
          title: d.digest,
          value: d.digest ? shortDigest(d.digest) : <span className="font-sans text-sm text-muted-foreground">–</span>,
        },
        { label: "Requested", className: "truncate", value: d.requested ?? <span className="text-muted-foreground">–</span> },
      ]}
      name={app.name}
      liveText="Every replica runs this image and these settings."
      urls={urls}
      failureTitle={failureTitle(d)}
      failureHint={failureHint(d, app)}
      writer={writer}
      redeploy={{ run: () => void deploy.run().catch(() => {}), pending: deploy.pending }}
      lastGood={lastGood?.id}
      onRollback={(x) => deploy.run("app_rollback", { deployment: x })}
      rollbackDescription="A new deployment puts back that one's image and settings, without building. The app's saved settings stay as they are."
      then={then}
      error={error}
      log={{
        lines: follow.log.buf.all(),
        firstLine: follow.log.buf.dropped + 1,
        filename: `${app.name}-deployment-${d.id}.log`,
        title: `${app.name} · #${d.id}`,
        empty: d.status === "queued" ? "Queued: waiting for the deploy before it to finish..." : "Waiting for the first line...",
      }}
      recent={recent}
    />
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
