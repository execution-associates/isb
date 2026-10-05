// A compose stack's Deployments tab: its history in the app list's rows
// (stack_deployments), one deployment with the compose file it deployed and
// its events (stack_deployment_get), and Roll back to any finished one
// (stack_rollback with `to`). Each row says what deployed it (a compose file,
// a rollback, an environment or domains change) and the services it changed.
// One deployment is drawn by the app's deployment page (DeploymentView).
import { useQueryClient } from "@tanstack/react-query";
import { ChevronRight, FileCode, History, KeyRound, Layers } from "lucide-react";
import { useEffect, useReducer, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { currentOf, finished, keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError } from "@/apps/components";
import { DeploymentView } from "@/apps/deployment-view";
import { DeploymentRow, elapsed, elapsedText, TRIGGER_LABEL } from "@/apps/deployments-tab";
import { buildStepLabel } from "@/apps/follow";
import {
  asDeployment,
  type DeployResult,
  eventLine,
  eventServices,
  reusedSecrets,
  type StackDeployment,
  stackKeys,
  stackStages,
  useStackDeployment,
  useStackDeployments,
} from "./api";

/**
 * How a stack deployment came about, where an app's row says who triggered
 * it: a rollback names the deployment it put back; an environment or
 * domains change says so; a deploy of the compose file says how it started.
 */
export function stackDeploymentAction(d: Pick<StackDeployment, "action" | "rollback_of" | "trigger">): string {
  if (d.action === "rollback" || d.rollback_of) return d.rollback_of ? `Rollback to #${d.rollback_of}` : "Rollback";
  if (d.action === "env") return "Environment changed";
  if (d.action === "domains") return "Domains changed";
  return TRIGGER_LABEL[d.trigger ?? "api"] ?? "Manual";
}

/**
 * What a stack deployment changed, where an app's row names its image: the
 * services it created, changed, scaled or removed. None, once finished, is
 * "No changes"; before that, it is not known yet.
 */
export function stackDeploymentServices(d: Pick<StackDeployment, "services" | "status">): string | null {
  if (d.services?.length) return d.services.join(", ");
  return finished(d.status) ? null : "";
}

function Services({ d }: { d: StackDeployment }) {
  const text = stackDeploymentServices(d);
  return (
    <p className="flex min-w-0 items-center gap-1.5 text-sm">
      <Layers className="size-3.5 shrink-0 text-muted-foreground" />
      {text ? (
        <span className="truncate font-mono text-xs font-medium">{text}</span>
      ) : (
        <span className="text-muted-foreground">{text === null ? "No changes" : d.status === "queued" ? "Waiting to start" : "Rolling out"}</span>
      )}
    </p>
  );
}

/** The warning a deploy that reused secrets gets: which, and why. */
export function reusedSecretsLine(names: string[]): string {
  return `Secrets reused from an earlier deploy: ${names.join(", ")} — no new value was given`;
}

/** After a deploy started: say so, and warn when it reused secrets. */
export function deployToast(name: string, r: DeployResult | null | undefined, message = `Deploying ${name}`) {
  toast.success(message);
  const reused = reusedSecrets(r);
  if (reused.length) toast.warning(reusedSecretsLine(reused));
}

/** stack_rollback to one deployment: its compose file deployed again. */
function useRollback(org: string, name: string) {
  const qc = useQueryClient();
  return async (to: number) => {
    try {
      const r = await callTool<DeployResult, string>("stack_rollback", { name, to }, org);
      await Promise.all([qc.invalidateQueries({ queryKey: stackKeys.org(org) }), qc.invalidateQueries({ queryKey: keys.org(org) })]);
      deployToast(name, r, `Rolling ${name} back to #${to}`);
    } catch (e) {
      toast.error(errorMessage(e));
      throw e;
    }
  };
}

export function StackDeploymentsTab({
  org,
  name,
  writer,
  path,
  deploy,
}: {
  org: string;
  name: string;
  writer: boolean;
  /** The list, or one deployment's page. */
  path: (id?: number) => string;
  deploy?: { run: () => void; pending: boolean };
}) {
  const deps = useStackDeployments(org, name, 30, 5000);
  const rollbackTo = useRollback(org, name);
  const [rollback, setRollback] = useState<StackDeployment | null>(null);
  const list = deps.data?.deployments ?? [];
  const current = currentOf(list.map((d) => asDeployment(name, d)), deps.data?.current ?? null);

  if (deps.error) return <QueryError error={deps.error} />;
  return (
    <>
      <Card className="gap-0 overflow-hidden py-0">
        <div className="flex flex-col gap-1 border-b px-5 py-4 sm:flex-row sm:items-end sm:justify-between sm:gap-4">
          <div className="min-w-0 space-y-1">
            <h2 className="text-[15px] font-semibold tracking-tight">Deployments</h2>
            <p className="text-[13px] text-muted-foreground">The last 30, each with the compose file it deployed. Roll back to an earlier one to deploy its file again.</p>
          </div>
          {list.length > 0 && <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:inline">{list.length} shown</span>}
        </div>
        {deps.isLoading ? (
          <div className="grid gap-2 p-5" aria-busy>
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-10" />
            ))}
          </div>
        ) : list.length === 0 ? (
          <EmptyState
            icon={History}
            title="No deployments yet"
            action={
              writer &&
              deploy && (
                <Button onClick={deploy.run} disabled={deploy.pending}>
                  Deploy {name}
                </Button>
              )
            }
          >
            Each deploy of the compose file shows up here.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {list.map((d) => (
              <DeploymentRow
                key={d.id}
                d={asDeployment(name, d)}
                to={path(d.id)}
                current={d.id === current}
                summary={<Services d={d} />}
                action={stackDeploymentAction(d)}
                coarse
                onRollback={writer && d.status === "done" && d.id !== current && !!current ? () => setRollback(d) : undefined}
              />
            ))}
          </ul>
        )}
      </Card>
      <ConfirmDialog
        open={rollback !== null}
        onOpenChange={(open) => !open && setRollback(null)}
        destructive={false}
        title={`Roll back to deployment #${rollback?.id}?`}
        description={<>A new deployment puts back #{rollback?.id}'s compose file. The YAML tab then shows that file, and the next deploy starts from it.</>}
        confirmLabel="Roll back"
        onConfirm={async () => {
          if (rollback) await rollbackTo(rollback.id);
        }}
      />
    </>
  );
}

/** A stack deployment's line in Recent deployments, as an app's says its commit or trigger. */
export function stackRecentLabel(d: StackDeployment): string {
  if (d.action === "rollback" || d.rollback_of) return d.rollback_of ? `rollback to #${d.rollback_of}` : "rollback";
  if (d.action === "env") return "environment changed";
  if (d.action === "domains") return "domains changed";
  return d.services?.length ? d.services.join(", ") : TRIGGER_LABEL[d.trigger ?? "api"].toLowerCase();
}

/** What a stack deployment deployed, for the Source cell. */
export function stackSource(d: Pick<StackDeployment, "action" | "rollback_of">): string {
  if (d.action === "rollback" || d.rollback_of) return d.rollback_of ? `Rollback to #${d.rollback_of}` : "Rollback";
  if (d.action === "env") return "Environment changed";
  if (d.action === "domains") return "Domains changed";
  return "Compose file";
}

/**
 * One stack deployment, on the page an app deployment has: its record read
 * every 2 s until it finishes (stack_deployment_get), its events as the log,
 * and the compose file it deployed below.
 */
export function StackDeploymentPage({
  org,
  name,
  id,
  writer,
  path,
  deploy,
  urls,
}: {
  org: string;
  name: string;
  id: number;
  writer: boolean;
  path: (id?: number) => string;
  /** Deploy the stack's compose file as it is now (the header's Deploy). */
  deploy?: { run: () => void; pending: boolean };
  urls?: string[];
}) {
  const q = useStackDeployment(org, name, id);
  const history = useStackDeployments(org, name, 30);
  const rollbackTo = useRollback(org, name);
  const [service, setService] = useState("");
  const r = q.data?.record;
  const done = r ? finished(r.status) : false;
  const list = history.data?.deployments ?? [];
  const current = currentOf(list.map((d) => asDeployment(name, d)), history.data?.current ?? null);

  // The clock ticks each second while it runs.
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (done) return;
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [done]);

  if (!q.data || !r) {
    return q.error ? (
      <QueryError error={q.error} />
    ) : (
      <div className="grid gap-4">
        <Skeleton className="h-36" />
        <Skeleton className="h-96" />
      </div>
    );
  }
  const d = asDeployment(name, r);
  const ms = elapsed(d);
  const events = q.data.events;
  const services = eventServices(events);
  const shown = service ? events.filter((e) => e.service === service) : null;
  const changed = stackDeploymentServices(r);
  const reused = r.reused_secrets ?? [];
  const lastGood = list.find((x) => x.status === "done" && x.id < d.id)?.id;

  return (
    <DeploymentView
      d={d}
      current={current}
      back={path()}
      link={(x) => path(x)}
      how={r.action === "env" || r.action === "domains" ? stackDeploymentAction(r) : undefined}
      clock={ms !== null ? elapsedText(ms, true) : "–"}
      steps={{ ...stackStages(r, events), middle: buildStepLabel(d, false) }}
      facts={[
        {
          label: "Source",
          className: "flex min-w-0 items-center gap-1.5",
          value: (
            <>
              <FileCode className="size-4 shrink-0 text-muted-foreground" />
              <span className="truncate">{stackSource(r)}</span>
            </>
          ),
        },
        {
          label: "Services",
          className: "truncate font-mono text-xs",
          title: changed || undefined,
          value: changed || <span className="font-sans text-sm text-muted-foreground">{changed === null ? "No changes" : "–"}</span>,
        },
        {
          label: "Secrets",
          className: "flex min-w-0 items-center gap-1.5",
          title: reused.length ? reusedSecretsLine(reused) : undefined,
          value: reused.length ? (
            <>
              <KeyRound className="size-3.5 shrink-0 text-warning" />
              <span className="truncate text-warning">
                Reused: <span className="font-mono text-xs">{reused.join(", ")}</span>
              </span>
            </>
          ) : (
            <span className="text-muted-foreground">–</span>
          ),
        },
      ]}
      name={name}
      liveText="Every service runs this compose file and these settings."
      urls={urls}
      failureTitle={stackStages(r, events).reached.deploying ? "The rollout failed" : "The deployment failed before it rolled out"}
      failureHint="The log below has the details; fix the cause and deploy again."
      writer={writer}
      redeploy={deploy}
      lastGood={lastGood}
      onRollback={current ? (x) => rollbackTo(x) : undefined}
      rollbackDescription="A new deployment puts back that one's compose file. The YAML tab then shows that file, and the next deploy starts from it."
      error={q.error}
      log={{
        lines: shown ? shown.map((e) => eventLine(e, service)) : q.data.lines,
        filename: `${name}${service ? `-${service}` : ""}-deployment-${d.id}.log`,
        title:
          services.length > 0 ? (
            <span className="inline-flex min-w-0 items-center gap-1.5">
              <span className="truncate">{`${name} · #${d.id}`}</span>
              <select
                value={service}
                onChange={(e) => setService(e.target.value)}
                aria-label="Service"
                className="rounded border border-white/10 bg-transparent px-1 py-0.5 text-[11px] text-zinc-300 outline-none focus-visible:ring-1 focus-visible:ring-white/30"
              >
                <option value="" className="bg-zinc-900">
                  all services
                </option>
                {services.map((s) => (
                  <option key={s} value={s} className="bg-zinc-900">
                    {s}
                  </option>
                ))}
              </select>
            </span>
          ) : (
            `${name} · #${d.id}`
          ),
        empty: d.status === "queued" ? "Queued: waiting for the deploy before it to finish..." : done ? "No events recorded." : "Waiting for the first line...",
      }}
      recent={list.slice(0, 12).map((x) => ({ id: x.id, status: x.status, created_at: x.created_at, label: stackRecentLabel(x) }))}
    >
      {q.data.source && (
        <details className="group rounded-xl border bg-card">
          <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-[13px] font-medium text-muted-foreground select-none hover:text-foreground">
            <ChevronRight className="size-4 transition-transform group-open:rotate-90" />
            Compose file
            <span className="font-normal">· as this deployment deployed it</span>
          </summary>
          <pre className="max-h-[60svh] overflow-auto border-t bg-muted/30 p-4 font-mono text-xs leading-relaxed">{q.data.source}</pre>
        </details>
      )}
    </DeploymentView>
  );
}
