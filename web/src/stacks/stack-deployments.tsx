// A compose stack's Deployments tab: its history in the app list's rows
// (stack_deployments), one deployment with the compose file it deployed and
// its events (stack_deployment_get), and Roll back to any finished one
// (stack_rollback with `to`). Each row says what deployed it (a compose file,
// a rollback, an environment or domains change) and the services it changed.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, History, KeyRound, Layers, Loader2, RotateCcw } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { currentOf, finished, keys } from "@/apps/api";
import { ConfirmDialog, DeploymentBadge, EmptyState, QueryError, Section } from "@/apps/components";
import { DeploymentRow, elapsed, elapsedText, TRIGGER_LABEL } from "@/apps/deployments-tab";
import { LogView } from "@/apps/log-view";
import { asDeployment, type DeployResult, reusedSecrets, type StackDeployment, stackKeys, useStackDeployment, useStackDeployments } from "./api";

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

export function StackDeploymentPage({ org, name, id, writer, path }: { org: string; name: string; id: number; writer: boolean; path: (id?: number) => string }) {
  const q = useStackDeployment(org, name, id);
  const history = useStackDeployments(org, name, 30);
  const rollbackTo = useRollback(org, name);
  const [rollback, setRollback] = useState(false);
  const r = q.data?.record;
  const done = r ? finished(r.status) : false;
  const current = currentOf((history.data?.deployments ?? []).map((d) => asDeployment(name, d)), history.data?.current ?? null);

  if (q.error) return <QueryError error={q.error} />;
  if (!q.data || !r) {
    return (
      <div className="grid gap-4">
        <Skeleton className="h-36" />
        <Skeleton className="h-96" />
      </div>
    );
  }
  const d = asDeployment(name, r);
  const ms = elapsed(d);
  return (
    <div className="grid animate-fade-up gap-4">
      <div>
        <Button asChild variant="ghost" size="sm" className="-ml-2 text-muted-foreground">
          <Link to={path()}>
            <ArrowLeft />
            All deployments
          </Link>
        </Button>
      </div>
      <Card className="gap-4 px-5 py-5">
        <div className="flex flex-wrap items-start gap-4">
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
                  <Link className="font-medium text-foreground underline-offset-4 hover:underline" to={path(d.rollback_of)}>
                    #{d.rollback_of}
                  </Link>
                </>
              ) : (
                stackDeploymentAction(r)
              )}{" "}
              by <span className="font-medium text-foreground">{d.by}</span> ·{" "}
              <span title={dateTime(d.created_at / 1000)}>{relativeTime(d.created_at / 1000)}</span>
            </p>
          </div>
          <div className="text-right">
            <div className="font-mono text-2xl font-semibold tracking-tight tabular-nums" aria-label="Duration">
              {ms !== null ? elapsedText(ms, true) : "–"}
            </div>
            <div className="text-xs text-muted-foreground">{done ? "Total" : "Elapsed"}</div>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-3 border-t pt-4">
          <Services d={r} />
          {writer && d.status === "done" && d.id !== current && !!current && (
            <Button size="sm" variant="outline" className="ml-auto" onClick={() => setRollback(true)}>
              <RotateCcw />
              Roll back to #{d.id}
            </Button>
          )}
        </div>
        {d.status === "failed" && d.error && <p className="font-mono text-xs break-words whitespace-pre-wrap text-destructive">{d.error}</p>}
        {!!r.reused_secrets?.length && (
          <p className="flex items-start gap-1.5 text-[13px] text-warning">
            <KeyRound className="mt-0.5 size-3.5 shrink-0" />
            <span>
              Secrets reused from an earlier deploy: <span className="font-mono">{r.reused_secrets.join(", ")}</span> — no new value was given
            </span>
          </p>
        )}
      </Card>
      <LogView
        lines={q.data.lines}
        live={!done}
        filename={`${name}-deployment-${d.id}.log`}
        title={`${name} · #${d.id}`}
        status={!done ? <Loader2 className="size-3 animate-spin text-zinc-500" /> : undefined}
        empty={d.status === "queued" ? "Queued: waiting for the deploy before it to finish..." : "No events recorded."}
      />
      {q.data.source && (
        <Section title="Compose file" description="The file this deployment deployed, as it was then.">
          <pre className="max-h-[60svh] overflow-auto rounded-lg border bg-muted/30 p-3 font-mono text-xs leading-relaxed">{q.data.source}</pre>
        </Section>
      )}
      <ConfirmDialog
        open={rollback}
        onOpenChange={setRollback}
        destructive={false}
        title={`Roll back to deployment #${d.id}?`}
        description="A new deployment puts back this one's compose file."
        confirmLabel="Roll back"
        onConfirm={() => rollbackTo(d.id)}
      />
    </div>
  );
}
