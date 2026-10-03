// /orgs/:org/apps/:app/:tab: one app, with its header (state, Deploy, Stop)
// and tabs. Deployment logs live under the Deployments tab.
import { useQueryClient } from "@tanstack/react-query";
import { Activity, ArrowRight, ArrowUpRight, Boxes, CalendarClock, Database, DatabaseBackup, GitBranch, GitPullRequest, Globe, History, Loader2, Package, Play, Rocket, ScrollText, Server, Settings2, SlidersHorizontal, Square, TerminalSquare, Variable } from "lucide-react";
import { lazy, Suspense, useEffect, useReducer, useState } from "react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { type App, appState, type Deployment, finished, isGit, isNotFound, keys, serviceOf, useApp, useDeployments, useStack } from "./api";
import { AdvancedTab } from "./app-advanced";
import { DomainsTab } from "./app-domains";
import { EnvironmentTab } from "./app-environment";
import { GeneralTab } from "./app-general";
import { LogsTab } from "./app-logs";
import { MonitoringTab } from "./app-monitoring";
import { AppStateBadge, ConfirmDialog, Crumbs, EmptyState, QueryError, TabLinks } from "./components";
import { DeploymentPage } from "./deployment-page";
import { DeploymentsTab, elapsed } from "./deployments-tab";
import { splitStack, useLiveEvents, useOrgLive } from "./live";
import { deploymentLine, stripAnsi } from "./logstream";
import { engineLabel, isDatabase } from "@/data/api";
import { deploymentPath, useDeploy } from "./use-deploy";
import { duration, imageName } from "./util";
import { DEPLOYMENT_LABEL } from "@/lib/status";

const TABS = [
  { id: "database", label: "Database", icon: Database },
  { id: "backups", label: "Backups", icon: DatabaseBackup },
  { id: "general", label: "General", icon: Settings2 },
  { id: "environment", label: "Environment", icon: Variable },
  { id: "domains", label: "Domains", icon: Globe },
  { id: "deployments", label: "Deployments", icon: History },
  { id: "previews", label: "Previews", icon: GitPullRequest },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "monitoring", label: "Monitoring", icon: Activity },
  { id: "jobs", label: "Jobs", icon: CalendarClock },
  { id: "terminal", label: "Terminal", icon: TerminalSquare },
  { id: "advanced", label: "Advanced", icon: SlidersHorizontal },
] as const;

/** The tabs an app has: a database has no General or Domains; previews need git. */
function tabsOf(a: App) {
  const db = isDatabase(a);
  return TABS.filter((t) => {
    if (t.id === "database" || t.id === "backups") return db;
    if (t.id === "general" || t.id === "domains") return !db;
    if (t.id === "previews") return isGit(a.source);
    return true;
  });
}

// xterm.js is loaded only when the Terminal tab opens; the day-2 tabs too.
const TerminalTab = lazy(() => import("./app-terminal"));
const DatabaseTab = lazy(() => import("@/data/database-tab").then((m) => ({ default: m.DatabaseTab })));
const BackupsTab = lazy(() => import("@/data/backups-tab").then((m) => ({ default: m.BackupsTab })));
const JobsTab = lazy(() => import("@/jobs/jobs-tab").then((m) => ({ default: m.JobsTab })));
const PreviewsTab = lazy(() => import("@/previews/previews-tab").then((m) => ({ default: m.PreviewsTab })));

export type TabId = (typeof TABS)[number]["id"];

export function AppPage() {
  const { org = "", app: name = "", tab = "general", id } = useParams();
  const app = useApp(org, name);
  useOrgLive(org);
  const o = encodeURIComponent(org);
  // Viewers read: no terminal (the server refuses it to them anyway).
  const writer = canWrite(useMe().data!, org);

  if (app.isLoading) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-3.5">
          <Skeleton className="size-11 rounded-xl" />
          <div className="space-y-2">
            <Skeleton className="h-6 w-44" />
            <Skeleton className="h-4 w-72" />
          </div>
        </div>
        <Skeleton className="h-9 w-full max-w-2xl" />
        <Skeleton className="h-64" />
      </div>
    );
  }
  if (app.error || !app.data) {
    return isNotFound(app.error) ? (
      <Card className="py-0">
        <EmptyState
          icon={Boxes}
          title={`No app ${name} in ${org}`}
          action={
            <Button asChild variant="outline">
              <Link to={`/orgs/${o}/projects`}>All projects</Link>
            </Button>
          }
        >
          It may have been deleted.
        </EmptyState>
      </Card>
    ) : (
      <QueryError error={app.error} />
    );
  }
  const a = app.data;
  const tabs = tabsOf(a).filter((t) => writer || t.id !== "terminal");
  const active = (tabs.some((t) => t.id === tab) ? tab : tabs[0].id) as TabId;
  return (
    <>
      <Crumbs
        items={[
          { label: "Projects", to: `/orgs/${o}/projects` },
          { label: a.project, to: `/orgs/${o}/projects/${a.project}/${a.environment}` },
          { label: a.environment, to: `/orgs/${o}/projects/${a.project}/${a.environment}` },
          { label: a.name },
        ]}
      />
      <AppHeader org={org} app={a} writer={writer} viewing={active === "deployments" && id ? Number(id) : undefined} />
      <TabLinks active={active} tabs={tabs.map((t) => ({ ...t, to: `/orgs/${o}/apps/${a.name}/${t.id}` }))} />
      <Suspense fallback={<Skeleton className="h-64" />}>
        {active === "database" && <DatabaseTab org={org} app={a} />}
        {active === "backups" && <BackupsTab org={org} app={a} />}
        {active === "previews" && <PreviewsTab org={org} app={a} />}
        {active === "jobs" && <JobsTab org={org} app={a} />}
      </Suspense>
      {active === "general" && <GeneralTab org={org} app={a} />}
      {active === "environment" && <EnvironmentTab org={org} app={a} />}
      {active === "domains" && <DomainsTab org={org} app={a} />}
      {active === "deployments" && (id ? <DeploymentPage org={org} app={a} id={Number(id)} /> : <DeploymentsTab org={org} app={a} />)}
      {active === "logs" && <LogsTab org={org} app={a} />}
      {active === "monitoring" && <MonitoringTab org={org} app={a} />}
      {active === "terminal" && (
        <Suspense fallback={<Skeleton className="h-96" />}>
          <TerminalTab org={org} app={a} />
        </Suspense>
      )}
      {active === "advanced" && <AdvancedTab org={org} app={a} />}
    </>
  );
}

function AppHeader({ org, app, writer, viewing }: { org: string; app: App; writer: boolean; viewing?: number }) {
  const stack = useStack(org, app.stack);
  const deps = useDeployments(org, app.name, 5);
  const qc = useQueryClient();
  const deploy = useDeploy(org, app.name);
  const [stopOpen, setStopOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const svc = serviceOf(stack.data, app.name);
  const latest = deps.data?.deployments[0];
  const state = appState(svc, latest);
  const db = (app.source as { database?: { engine: string; version?: string } }).database;
  const src = isGit(app.source)
    ? `${app.source.git.url.replace(/^https?:\/\//, "").replace(/\.git$/, "")} @ ${app.source.git.ref}`
    : db
      ? `${engineLabel(db.engine)} ${db.version ?? ""}`.trim()
      : imageName(app.source.image);
  const url = (svc?.domains ?? []).map((d) => d.url).find(Boolean);
  const SourceIcon = isGit(app.source) ? GitBranch : db ? Database : Package;

  const start = async () => {
    setStarting(true);
    try {
      await callTool("stack_scale", { name: app.stack, service: app.name, replicas: Math.max(1, app.replicas) }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`${app.name} starting`);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <>
      <PageHeader
        icon={
          <span className="flex size-11 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
            {db ? <Database className="size-5 text-muted-foreground" /> : <Boxes className="size-5 text-muted-foreground" />}
          </span>
        }
        title={
          <>
            <span className="truncate">{app.name}</span>
            <AppStateBadge state={state} />
          </>
        }
        description={
          <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
            <span className="flex min-w-0 items-center gap-1.5">
              <SourceIcon className="size-3.5 shrink-0" />
              <span className="truncate font-mono text-xs">{src}</span>
            </span>
            <span className="flex items-center gap-1.5">
              <Server className="size-3.5 shrink-0" />
              {svc ? `${svc.healthy}/${svc.replicas} healthy` : "not running"}
            </span>
            {url && (
              <a href={url} target="_blank" rel="noreferrer" className="flex min-w-0 items-center gap-1 font-medium text-foreground underline-offset-4 hover:underline">
                <span className="truncate">{url.replace(/^https?:\/\//, "")}</span>
                <ArrowUpRight className="size-3.5 shrink-0" />
              </a>
            )}
          </span>
        }
        actions={
          writer && (
            <>
              {state === "stopped" ? (
                <Button variant="outline" onClick={start} disabled={starting}>
                  {starting ? <Loader2 className="animate-spin" /> : <Play />}
                  Start
                </Button>
              ) : (
                svc && (
                  <Button variant="outline" onClick={() => setStopOpen(true)}>
                    <Square />
                    Stop
                  </Button>
                )
              )}
              <Button onClick={() => deploy.run().catch(() => {})} disabled={deploy.pending}>
                {deploy.pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                {app.current_deployment ? "Redeploy" : "Deploy"}
              </Button>
            </>
          )
        }
      />
      {latest && !finished(latest.status) && latest.id !== viewing && <ActiveDeployment org={org} app={app.name} d={latest} />}
      <ConfirmDialog
        open={stopOpen}
        onOpenChange={setStopOpen}
        title={`Stop ${app.name}?`}
        description="Its replicas are stopped (scaled to 0) and its domains answer 503 until you start it or deploy again. Settings and volumes are kept."
        confirmLabel="Stop app"
        onConfirm={async () => {
          await callTool("stack_scale", { name: app.stack, service: app.name, replicas: 0 }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`${app.name} stopped`);
        }}
      />
    </>
  );
}

/**
 * A deployment in progress while you are on another tab (or it came from a
 * webhook): its stage, clock and newest log line, one click from its log.
 */
function ActiveDeployment({ org, app, d }: { org: string; app: string; d: Deployment }) {
  const [line, setLine] = useState("");
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useLiveEvents((e) => {
    if (e.service !== app || splitStack(e.stack).org !== org) return;
    const l = deploymentLine(e.message, app, d.id);
    if (l !== null) setLine(stripAnsi(l));
  });
  useEffect(() => {
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, []);
  const ms = elapsed(d);
  return (
    <Link
      to={deploymentPath(org, app, d.id)}
      className="group mb-5 flex animate-fade-up items-center gap-3 rounded-xl border border-info/25 bg-info/[0.06] px-4 py-3 text-sm transition-colors hover:border-info/40 hover:bg-info/10"
    >
      <Loader2 className="size-4 shrink-0 animate-spin text-info" />
      <span className="shrink-0 font-medium">
        Deployment <span className="font-mono">#{d.id}</span> · {DEPLOYMENT_LABEL[d.status]}
      </span>
      <span className="hidden min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground sm:block">{line}</span>
      <span className="ml-auto shrink-0 font-mono text-xs text-muted-foreground tabular-nums sm:ml-0">{ms !== null ? duration(ms) : ""}</span>
      <span className="flex shrink-0 items-center gap-1 text-xs font-medium text-info">
        <span className="hidden sm:inline">View log</span>
        <ArrowRight className="size-3.5 transition-transform group-hover:translate-x-0.5" />
      </span>
    </Link>
  );
}
