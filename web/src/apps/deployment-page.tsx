// /orgs/:org/apps/:app/deployments/:id: one deployment, its log live.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, CircleAlert, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { Link } from "react-router";
import { callTool } from "@/api/tools";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { dateTime, relativeTime } from "@/lib/format";
import { type App, currentOf, type Deployment, finished, isGit, keys, useDeployments } from "./api";
import { ConfirmDialog, DeploymentBadge, Meta, QueryError } from "./components";
import { elapsed, TRIGGER_LABEL } from "./deployments-tab";
import { useLiveEvents, splitStack } from "./live";
import { LogView } from "./log-view";
import { concernsDeployment, LogFollower } from "./logstream";
import { useDeploy } from "./use-deploy";
import { duration, imageName, shortDigest, shortSha } from "./util";

/** Follow a deployment's log: fetch from the last offset when told to. */
function useDeploymentLog(org: string, app: string, id: number) {
  const follower = useRef(new LogFollower());
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [error, setError] = useState<unknown>(null);
  const inflight = useRef(false);
  const again = useRef(false);

  useEffect(() => {
    follower.current = new LogFollower();
    bump();
  }, [org, app, id]);

  const pull = useCallback(async () => {
    if (inflight.current) {
      again.current = true;
      return;
    }
    const f = follower.current;
    if (f.finished) return;
    inflight.current = true;
    try {
      // Read until caught up (a long log comes in pieces).
      for (let i = 0; i < 50; i++) {
        const at = f.offset;
        const r = await callTool<{ log: string; offset: number; finished: boolean }>("app_deployment_log", { name: app, deployment: id, offset: at }, org);
        if (f !== follower.current) return;
        f.apply(at, r);
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
  }, [org, app, id]);

  return { follower: follower.current, pull, error };
}

export function DeploymentPage({ org, app, id }: { org: string; app: App; id: number }) {
  const qc = useQueryClient();
  const dep = useQuery({
    queryKey: keys.deployment(org, app.name, id),
    queryFn: async () => {
      const r = await callTool<{ deployments: Deployment[] }>("app_deployments", { name: app.name, limit: 100 }, org);
      const d = r.deployments.find((x) => x.id === id);
      if (!d) throw new Error(`There is no deployment #${id} of ${app.name} (only the last 30 are kept).`);
      return d;
    },
  });
  const log = useDeploymentLog(org, app.name, id);
  const history = useDeployments(org, app.name, 30);
  const current = currentOf(history.data?.deployments, app.current_deployment);
  const deploy = useDeploy(org, app.name);
  const [rollback, setRollback] = useState(false);
  const [, tick] = useReducer((n: number) => n + 1, 0);
  const d = dep.data;
  const done = d ? finished(d.status) : false;
  const o = encodeURIComponent(org);

  // Every log line and status change of this deployment arrives on the
  // event feed; each one pulls the new text (coalesced), and a status
  // change refreshes the record.
  useLiveEvents((e) => {
    if (splitStack(e.stack).org !== org || e.service !== app.name) return;
    if (!concernsDeployment(e.message, app.name, id)) return;
    void log.pull();
    if (e.level !== "log") qc.invalidateQueries({ queryKey: keys.deployment(org, app.name, id) });
  });

  useEffect(() => {
    void log.pull();
  }, [log.pull]); // eslint-disable-line react-hooks/exhaustive-deps

  // A safety net while it runs (a dropped event stream, a slow first line),
  // and the elapsed clock.
  useEffect(() => {
    if (done) {
      void log.pull();
      return;
    }
    const t = setInterval(() => {
      void log.pull();
      tick();
    }, 2000);
    return () => clearInterval(t);
  }, [done, log.pull]); // eslint-disable-line react-hooks/exhaustive-deps

  // The record says finished but the log follower has not seen it yet.
  useEffect(() => {
    if (done && !log.follower.finished) void log.pull();
  });

  if (dep.isLoading) return <Skeleton className="h-96" />;
  if (dep.error || !d) return <QueryError error={dep.error} />;

  const lines = log.follower.buf.all();
  const first = log.follower.buf.dropped + 1;
  const ms = elapsed(d);
  const canRollback = d.status === "done" && current !== null && d.id !== current;

  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <Button asChild variant="ghost" size="sm" className="-ml-2">
          <Link to={`/orgs/${o}/apps/${app.name}/deployments`}>
            <ArrowLeft />
            All deployments
          </Link>
        </Button>
        {canRollback && (
          <Button variant="outline" size="sm" onClick={() => setRollback(true)}>
            <RotateCcw />
            Roll back to #{d.id}
          </Button>
        )}
      </div>
      <Card className="gap-4 px-5 py-5">
        <div className="flex flex-wrap items-center gap-3">
          <h2 className="font-mono text-lg font-semibold">#{d.id}</h2>
          <DeploymentBadge status={d.status} />
          {d.id === current && <span className="text-xs text-muted-foreground">running now</span>}
          <span className="ml-auto text-sm text-muted-foreground tabular-nums">{ms !== null ? duration(ms) : ""}</span>
        </div>
        <Meta
          items={[
            ["Triggered", `${TRIGGER_LABEL[d.trigger]} by ${d.by}`],
            ["Queued", <span title={dateTime(d.created_at / 1000)}>{relativeTime(d.created_at / 1000)}</span>],
            d.rollback_of ? ["Rollback to", <Link className="underline" to={`/orgs/${o}/apps/${app.name}/deployments/${d.rollback_of}`}>#{d.rollback_of}</Link>] : ["Requested", d.requested ?? ""],
            [
              "Commit",
              d.commit ? (
                <span title={d.commit.sha}>
                  <span className="font-mono">{shortSha(d.commit.sha)}</span> {d.commit.message}
                </span>
              ) : isGit(app.source) ? (
                "not fetched yet"
              ) : (
                ""
              ),
            ],
            ["Image", d.image ? <span className="font-mono text-xs" title={d.image}>{imageName(d.image)}</span> : ""],
            ["Digest", d.digest ? <span className="font-mono text-xs" title={d.digest}>{shortDigest(d.digest)}</span> : ""],
          ]}
        />
      </Card>
      {d.status === "failed" && d.error && (
        <Alert variant="destructive" className="border-destructive/40 bg-destructive/5">
          <CircleAlert />
          <AlertTitle>{failureTitle(d)}</AlertTitle>
          <AlertDescription>
            <p className="font-mono text-xs break-words whitespace-pre-wrap">{d.error}</p>
            <p className="mt-1 text-xs">{failureHint(d, app)}</p>
          </AlertDescription>
        </Alert>
      )}
      {d.status === "superseded" && (
        <Alert>
          <AlertTitle>Superseded</AlertTitle>
          <AlertDescription>A newer deploy replaced this one before it started{d.error ? `: ${d.error}` : ""}.</AlertDescription>
        </Alert>
      )}
      {log.error ? <QueryError error={log.error} /> : null}
      <LogView
        lines={lines}
        firstLine={first}
        live={!done}
        filename={`${app.name}-deployment-${d.id}.log`}
        title={
          <span>
            {app.name} · deployment #{d.id} {done ? "" : "· live"}
          </span>
        }
        empty={d.status === "queued" ? "Waiting for the deploy before it to finish..." : "No output yet."}
      />
      <ConfirmDialog
        open={rollback}
        onOpenChange={setRollback}
        destructive={false}
        title={`Roll back to deployment #${d.id}?`}
        description="A new deployment puts back this one's image and settings, without building. The app's saved settings stay as they are."
        confirmLabel="Roll back"
        onConfirm={() => deploy.run("app_rollback", { deployment: d.id })}
      />
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
