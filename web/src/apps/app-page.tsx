// /orgs/:org/apps/:app/:tab: one app, with its header (state, Deploy, Stop)
// and tabs. Deployment logs live under the Deployments tab.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Boxes, CircleAlert, Database, GitBranch, Globe, Loader2, Package, Play, Rocket, Server, Square } from "lucide-react";
import { lazy, Suspense, useState } from "react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { type App, appState, type Deployment, finished, isGit, isNotFound, serviceOf, useApp, useDeployments, useIngress, useStack } from "./api";
import { autoHostLabel, ingressOff, NO_INGRESS_WARNING } from "./domains";
import { AdvancedTab } from "./app-advanced";
import { DomainsTab } from "./app-domains";
import { EnvironmentTab } from "./app-environment";
import { GeneralTab } from "./app-general";
import { LogsTab } from "./app-logs";
import { MonitoringTab } from "./app-monitoring";
import { YamlTab } from "./app-yaml";
import { AppStateBadge, ConfirmDialog, Crumbs, EmptyState, QueryError } from "./components";
import { DeploymentBanner, ServiceHeader, ServiceTabBar } from "./service-page";
import { DeploymentPage } from "./deployment-page";
import { DeploymentsTab } from "./deployments-tab";
import { splitStack, useLiveEvents } from "./live";
import { deploymentLine, stripAnsi } from "./logstream";
import { engineLabel, isDatabase } from "@/data/api";
import { deploymentPath, useDeploy } from "./use-deploy";
import { activeServiceTab, serviceTabs } from "./service-tabs";
import { imageName } from "./util";
import { invalidateOrg } from "@/lib/freshness";

// xterm.js is loaded only when the Terminal tab opens; the day-2 tabs too.
const TerminalTab = lazy(() => import("./app-terminal"));
const DatabaseTab = lazy(() => import("@/data/database-tab").then((m) => ({ default: m.DatabaseTab })));
const BackupsTab = lazy(() => import("@/data/backups-tab").then((m) => ({ default: m.BackupsTab })));
const JobsTab = lazy(() => import("@/jobs/jobs-tab").then((m) => ({ default: m.JobsTab })));
const PreviewsTab = lazy(() => import("@/previews/previews-tab").then((m) => ({ default: m.PreviewsTab })));

export function AppPage() {
  const { org = "", app: name = "", tab, id } = useParams();
  const app = useApp(org, name);
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
  const tabs = serviceTabs({ database: isDatabase(a), git: isGit(a.source), writer });
  const active = activeServiceTab(tab, tabs);
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
      <ServiceTabBar tabs={tabs} active={active} to={(t) => `/orgs/${o}/apps/${a.name}/${t}`} />
      {active === "yaml" && <YamlTab org={org} app={a} />}
      <Suspense fallback={<Skeleton className="h-64" />}>
        {active === "database" && <DatabaseTab org={org} app={a} />}
        {active === "backups" && <BackupsTab org={org} app={a} />}
        {active === "previews" && <PreviewsTab org={org} app={a} />}
        {active === "jobs" && <JobsTab org={org} target={{ app: a.name }} />}
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
  // Why it is not running, in the controller's words: "image ... not found", say.
  const problem = state === "failing" ? svc?.message : state === "failed" ? latest?.error : undefined;
  // Until both answer, "not deployed" would be a guess.
  const loading = stack.isLoading || deps.isLoading;
  const db = (app.source as { database?: { engine: string; version?: string } }).database;
  const src = isGit(app.source)
    ? `${app.source.git.url.replace(/^https?:\/\//, "").replace(/\.git$/, "")} @ ${app.source.git.ref}`
    : db
      ? `${engineLabel(db.engine)} ${db.version ?? ""}`.trim()
      : imageName(app.source.image);
  const url = (svc?.domains ?? []).map((d) => d.url).find(Boolean);
  const ingress = useIngress(org);
  const SourceIcon = isGit(app.source) ? GitBranch : db ? Database : Package;

  const start = async () => {
    setStarting(true);
    try {
      await callTool("stack_scale", { name: app.stack, service: app.name, replicas: Math.max(1, app.replicas) }, org);
      await invalidateOrg(qc, org);
      toast.success(`${app.name} starting`);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <>
      <ServiceHeader
        icon={db ? Database : Boxes}
        name={app.name}
        state={loading ? <Skeleton className="h-5 w-20 rounded-full" /> : <AppStateBadge state={state} />}
        details={
          <>
            <span className="flex min-w-0 items-center gap-1.5">
              <SourceIcon className="size-3.5 shrink-0" />
              <span className="truncate font-mono text-xs">{src}</span>
            </span>
            <span className="flex items-center gap-1.5">
              <Server className="size-3.5 shrink-0" />
              {loading ? <Skeleton className="h-3.5 w-20" /> : svc ? `${svc.healthy}/${svc.replicas} healthy` : "not running"}
            </span>
            {!url && ingressOff(ingress.data) && (app.domains ?? []).length > 0 && (
              <span className="flex min-w-0 items-center gap-1.5 text-warning" title={NO_INGRESS_WARNING}>
                <Globe className="size-3.5 shrink-0" />
                <span className="truncate">{autoHostLabel(String(app.domains?.[0]?.host ?? ""), undefined, true)}</span>
              </span>
            )}
            {url && (
              <a href={url} target="_blank" rel="noreferrer" className="flex min-w-0 items-center gap-1 font-medium text-foreground underline-offset-4 hover:underline">
                <span className="truncate">{url.replace(/^https?:\/\//, "")}</span>
                <ArrowUpRight className="size-3.5 shrink-0" />
              </a>
            )}
          </>
        }
        actions={
          writer && (
            <>
              {loading ? null : state === "stopping" || state === "starting" ? (
                <Button variant="outline" disabled>
                  <Loader2 className="animate-spin" />
                  {state === "stopping" ? "Stopping" : "Starting"}
                </Button>
              ) : state === "stopped" ? (
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
      {problem && (
        <p role="status" className="mb-5 flex animate-fade-up items-start gap-2 rounded-xl border border-destructive/25 bg-destructive/[0.06] px-4 py-3 text-sm text-destructive">
          <CircleAlert className="mt-0.5 size-4 shrink-0" />
          <span className="min-w-0 break-words">{problem}</span>
        </p>
      )}
      <ConfirmDialog
        open={stopOpen}
        onOpenChange={setStopOpen}
        title={`Stop ${app.name}?`}
        description="Its replicas are stopped (scaled to 0) and its domains answer 503 until you start it or deploy again. Settings and volumes are kept."
        confirmLabel="Stop app"
        onConfirm={async () => {
          await callTool("stack_scale", { name: app.stack, service: app.name, replicas: 0 }, org);
          await invalidateOrg(qc, org);
          toast.success(`${app.name} stopping`);
        }}
      />
    </>
  );
}

/** The app's deployment in progress, with the newest line of its log. */
function ActiveDeployment({ org, app, d }: { org: string; app: string; d: Deployment }) {
  const [line, setLine] = useState("");
  useLiveEvents((e) => {
    if (e.service !== app || splitStack(e.stack).org !== org) return;
    const l = deploymentLine(e.message, app, d.id);
    if (l !== null) setLine(stripAnsi(l));
  });
  return <DeploymentBanner d={d} to={deploymentPath(org, app, d.id)} line={line} />;
}
