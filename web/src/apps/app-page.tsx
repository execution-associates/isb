// /orgs/:org/apps/:app/:tab: one app, with its header (state, Deploy, Stop)
// and tabs. Deployment logs live under the Deployments tab.
import { useQueryClient } from "@tanstack/react-query";
import { Activity, Boxes, Globe, History, Loader2, Play, Rocket, ScrollText, Settings2, SlidersHorizontal, Square, TerminalSquare, Variable } from "lucide-react";
import { lazy, Suspense, useState } from "react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { type App, appState, isGit, isNotFound, keys, serviceOf, useApp, useDeployments, useStack } from "./api";
import { AdvancedTab } from "./app-advanced";
import { DomainsTab } from "./app-domains";
import { EnvironmentTab } from "./app-environment";
import { GeneralTab } from "./app-general";
import { LogsTab } from "./app-logs";
import { MonitoringTab } from "./app-monitoring";
import { AppStateBadge, ConfirmDialog, Crumbs, EmptyState, LiveIndicator, QueryError, TabLinks } from "./components";
import { DeploymentPage } from "./deployment-page";
import { DeploymentsTab } from "./deployments-tab";
import { useOrgLive } from "./live";
import { useDeploy } from "./use-deploy";
import { imageName } from "./util";

const TABS = [
  { id: "general", label: "General", icon: Settings2 },
  { id: "environment", label: "Environment", icon: Variable },
  { id: "domains", label: "Domains", icon: Globe },
  { id: "deployments", label: "Deployments", icon: History },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "monitoring", label: "Monitoring", icon: Activity },
  { id: "terminal", label: "Terminal", icon: TerminalSquare },
  { id: "advanced", label: "Advanced", icon: SlidersHorizontal },
] as const;

// xterm.js is loaded only when the Terminal tab opens.
const TerminalTab = lazy(() => import("./app-terminal"));

export type TabId = (typeof TABS)[number]["id"];

export function AppPage() {
  const { org = "", app: name = "", tab = "general", id } = useParams();
  const app = useApp(org, name);
  const live = useOrgLive(org);
  const o = encodeURIComponent(org);

  if (app.isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-6 w-60" />
        <Skeleton className="h-10 w-80" />
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
  const active = (TABS.some((t) => t.id === tab) ? tab : "general") as TabId;
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
      <AppHeader org={org} app={a} live={<LiveIndicator state={live} />} />
      <TabLinks active={active} tabs={TABS.map((t) => ({ ...t, to: `/orgs/${o}/apps/${a.name}/${t.id}` }))} />
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

function AppHeader({ org, app, live }: { org: string; app: App; live: React.ReactNode }) {
  const stack = useStack(org, app.stack);
  const deps = useDeployments(org, app.name, 5);
  const qc = useQueryClient();
  const deploy = useDeploy(org, app.name);
  const [stopOpen, setStopOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const svc = serviceOf(stack.data, app.name);
  const latest = deps.data?.deployments[0];
  const state = appState(svc, latest);
  const src = isGit(app.source) ? `${app.source.git.url} @ ${app.source.git.ref}` : imageName(app.source.image);

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
        title={
          <>
            <span className="truncate">{app.name}</span>
            <AppStateBadge state={state} />
          </>
        }
        description={
          <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
            <span className="truncate font-mono text-xs">{src}</span>
            <span className="text-xs">
              {svc ? `${svc.healthy}/${svc.replicas} healthy` : "not running"} · service <span className="font-mono">{app.service_name}</span>
            </span>
          </span>
        }
        actions={
          <>
            {live}
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
        }
      />
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
