// One deployment's page, however the service is deployed: a header with its
// status, clock and stage strip, what it deployed, how it ended, its log,
// and the recent deployments beside it. An app's page (deployment-page.tsx)
// and a compose stack's (stacks/stack-deployments.tsx) each adapt their
// records to these props, so both read and behave the same.
import { ArrowLeft, ArrowUpRight, Check, CircleAlert, CircleCheck, CircleSlash, CircleX, ExternalLink, Loader2, RotateCcw, Rocket, X } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Link } from "react-router";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { dateTime, relativeTime } from "@/lib/format";
import { DEPLOYMENT_TONE, inProgress, STEPS, type StepState, stepStates } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type Deployment, type DeploymentStatus, finished } from "./api";
import { ConfirmDialog, DeploymentBadge, QueryError } from "./components";
import { TRIGGER_LABEL } from "./deployments-tab";
import { LogView } from "./log-view";

/** The stages a deployment reached, for the strip after it failed. */
export interface Reached {
  building: boolean;
  deploying: boolean;
}

/** One cell of the what-was-deployed grid. */
export interface Fact {
  label: string;
  value: ReactNode;
  /** The value's classes (after its top margin). */
  className?: string;
  title?: string;
}

/** A row of the Recent deployments list. */
export interface RecentDeployment {
  id: number;
  status: DeploymentStatus;
  label: string;
  created_at: number;
}

export interface DeploymentViewProps {
  /** The record, as an app's Deployment (a stack's goes through asDeployment). */
  d: Deployment;
  current: number | null;
  /** The deployments list. */
  back: string;
  /** One deployment's page. */
  link: (id: number) => string;
  /** How it started, in place of the trigger's label (a stack's environment or domains change). */
  how?: string;
  /** The big clock. */
  clock: string;
  /** The stage strip: the status it is drawn for, the stages reached, and the middle stage's name. */
  steps: { status: DeploymentStatus; reached: Reached; middle: string };
  /** What it deployed: three cells. */
  facts: Fact[];
  /** The service, in "<name> is live with this deployment". */
  name: string;
  /** What a live deployment runs, in its panel. */
  liveText: string;
  urls?: string[];
  failureTitle: string;
  failureHint: string;
  writer: boolean;
  /** Deploy again, from the failure panel. */
  redeploy?: { run: () => void; pending: boolean };
  /** The newest successful deployment before this one, offered after a failure. */
  lastGood?: number;
  /** Roll back to a deployment; none hides Roll back. */
  onRollback?: (id: number) => unknown;
  rollbackDescription: string;
  /** The next apps a template deploys after this one. */
  then?: string[];
  error?: unknown;
  log: { lines: string[]; firstLine?: number; filename: string; title: ReactNode; empty: ReactNode };
  recent: RecentDeployment[];
  /** Below the log. */
  children?: ReactNode;
}

const STEP_ICON: Record<StepState, typeof Check> = { done: Check, current: Loader2, failed: X, waiting: Check, skipped: CircleSlash };

export function Progress({ status, reached, middle }: { status: DeploymentStatus; reached: Reached; middle: string }) {
  const states = stepStates(status, reached);
  const labels: Record<(typeof STEPS)[number], string> = { queued: "Queued", building: middle, deploying: "Roll out", done: "Live" };
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
  const Icon = d.status === "done" ? CircleCheck : d.status === "failed" ? CircleX : d.status === "superseded" || d.status === "cancelled" ? CircleSlash : Loader2;
  return (
    <span className={box}>
      <Icon className={cn("size-5", inProgress(d.status) && "animate-spin")} />
    </span>
  );
}

export function DeploymentView(p: DeploymentViewProps) {
  const { d, current, link, writer, then = [] } = p;
  const [rollback, setRollback] = useState<number | null>(null);
  const done = finished(d.status);

  return (
    <div className="grid animate-fade-up gap-4 xl:grid-cols-[minmax(0,1fr)_16rem]">
      <div className="grid min-w-0 content-start gap-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Button asChild variant="ghost" size="sm" className="-ml-2 text-muted-foreground">
            <Link to={p.back}>
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
                    <Link className="font-medium text-foreground underline-offset-4 hover:underline" to={link(d.rollback_of)}>
                      #{d.rollback_of}
                    </Link>{" "}
                    ·{" "}
                  </>
                ) : null}
                {p.how ?? TRIGGER_LABEL[d.trigger]} by <span className="font-medium text-foreground">{d.by}</span> ·{" "}
                <span title={dateTime(d.created_at / 1000)}>{relativeTime(d.created_at / 1000)}</span>
              </p>
            </div>
            <div className="text-right">
              <div className="font-mono text-2xl font-semibold tracking-tight tabular-nums" aria-label="Duration">
                {p.clock}
              </div>
              <div className="text-xs text-muted-foreground">{done ? "Total" : "Elapsed"}</div>
            </div>
          </div>
          <Progress {...p.steps} />
          <dl className="grid grid-cols-2 gap-x-6 gap-y-3 border-t pt-4 text-sm sm:grid-cols-3">
            {p.facts.map((f, i) => (
              <div key={f.label} className={i === 0 ? "col-span-2 min-w-0 sm:col-span-1" : "min-w-0"}>
                <dt className="text-xs text-muted-foreground">{f.label}</dt>
                <dd className={cn("mt-0.5", f.className)} title={f.title}>
                  {f.value}
                </dd>
              </div>
            ))}
          </dl>
        </Card>

        {d.status === "done" && (
          <Outcome tone="success" icon={CircleCheck} title={d.id === current ? `${p.name} is live with this deployment` : "Deployed"}>
            <p>{d.id === current ? p.liveText : `A newer deployment (#${current}) replaced it since; roll back to put this one back.`}</p>
            <div className="mt-3 flex flex-wrap gap-2">
              {(p.urls ?? []).slice(0, 2).map((u) => (
                <Button key={u} asChild size="sm" variant="outline" className="bg-background">
                  <a href={u} target="_blank" rel="noreferrer">
                    <ExternalLink />
                    {u.replace(/^https?:\/\//, "")}
                  </a>
                </Button>
              ))}
              {writer && p.onRollback && d.id !== current && (
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
          <Outcome tone="danger" icon={CircleAlert} title={p.failureTitle}>
            {d.error && <p className="font-mono text-xs break-words whitespace-pre-wrap">{d.error}</p>}
            <p className="mt-1">{p.failureHint}</p>
            {writer && (
              <div className="mt-3 flex flex-wrap gap-2">
                {p.redeploy && (
                  <Button size="sm" onClick={p.redeploy.run} disabled={p.redeploy.pending}>
                    {p.redeploy.pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                    Deploy again
                  </Button>
                )}
                {p.lastGood && p.onRollback && (
                  <Button size="sm" variant="outline" className="bg-background" onClick={() => setRollback(p.lastGood!)}>
                    <RotateCcw />
                    Roll back to #{p.lastGood}
                  </Button>
                )}
                {p.lastGood && (
                  <Button asChild size="sm" variant="ghost">
                    <Link to={link(p.lastGood)}>
                      View #{p.lastGood}
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

        {d.status === "cancelled" && (
          <Outcome tone="neutral" icon={CircleSlash} title="Cancelled">
            {d.error ?? "Closed before it started."}
          </Outcome>
        )}

        {p.error ? <QueryError error={p.error} /> : null}
        <LogView
          lines={p.log.lines}
          firstLine={p.log.firstLine}
          live={!done}
          filename={p.log.filename}
          title={p.log.title}
          status={!done ? <span className="text-[11px] text-zinc-500">{d.status === "queued" ? "waiting" : "streaming"}</span> : undefined}
          empty={p.log.empty}
        />
        {p.children}
      </div>

      <aside className="hidden xl:block">
        <div className="sticky top-16 rounded-xl border bg-card">
          <div className="border-b px-4 py-3 text-xs font-medium tracking-wide text-muted-foreground uppercase">Recent deployments</div>
          <ul className="max-h-[70svh] overflow-y-auto p-1.5">
            {p.recent.map((x) => (
              <li key={x.id}>
                <Link
                  to={link(x.id)}
                  replace
                  className={cn("flex items-center gap-2.5 rounded-md px-2.5 py-2 text-sm transition-colors hover:bg-accent", x.id === d.id && "bg-accent font-medium")}
                >
                  <StatusDot tone={DEPLOYMENT_TONE[x.status]} pulse={inProgress(x.status)} />
                  <span className="font-mono text-xs">#{x.id}</span>
                  <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">{x.label}</span>
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
        description={p.rollbackDescription}
        confirmLabel="Roll back"
        onConfirm={async () => {
          if (rollback !== null) await p.onRollback?.(rollback);
        }}
      />
    </div>
  );
}

function Outcome({ tone, icon: Icon, title, children }: { tone: "success" | "danger" | "neutral"; icon: typeof Check; title: string; children: ReactNode }) {
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
