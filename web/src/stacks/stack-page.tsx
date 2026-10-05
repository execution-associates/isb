// /orgs/:org/projects/:project/:env/compose/:stack/:tab: a compose stack in a
// project environment, on the same page an app has: the shared header (with
// Stop/Start, Deploy and the deployment in progress) and the shared tab list
// (service-tabs.ts). YAML is the compose file in an editor; deploying it is
// stack_deploy, the same call `isb stack deploy` makes. The tabs are in
// stack-general.tsx, stack-tabs.tsx, stack-deployments.tsx, stack-yaml.tsx,
// stack-jobs.tsx and stack-terminal.tsx. The old Compose and Services tabs
// redirect to YAML and General. /orgs/:org/stacks/:stack redirects here, and
// still shows the stacks no project owns (isb's tunnel, an apps stack).
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, FileCode2, Layers, Loader2, Play, Rocket, Server, Square } from "lucide-react";
import { lazy, Suspense, useState } from "react";
import { Link, Navigate, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { finished, isNotFound, keys, useProjects, useStack } from "@/apps/api";
import { ConfirmDialog, Crumbs, EmptyState, QueryError, ToneBadge } from "@/apps/components";
import { HEALTH_LABEL, HEALTH_TONE, stackHealth } from "@/apps/health";
import { useOrgLive } from "@/apps/live";
import { DeploymentBanner, ServiceHeader, ServiceTabBar } from "@/apps/service-page";
import { activeServiceTab, serviceTabs, stackTab } from "@/apps/service-tabs";
import {
  afterDeploy,
  asDeployment,
  composePath,
  type DeployResult,
  ownerArgs,
  ownerOf,
  sourceReplicas,
  type StackOwner,
  stackKeys,
  stackRedirect,
  useStackDeployments,
  useStackExport,
} from "./api";
import { deployToast, StackDeploymentPage, StackDeploymentsTab } from "./stack-deployments";
import { StackAdvancedTab, StackGeneralTab } from "./stack-general";
import { type StackServices, StackDomainsTab, StackEnvironmentTab, StackLogsTab, StackMonitoringTab } from "./stack-tabs";
import { StackYamlTab } from "./stack-yaml";

// xterm.js is loaded only when the Terminal tab opens; the jobs tab too.
const StackTerminalTab = lazy(() => import("./stack-terminal").then((m) => ({ default: m.StackTerminalTab })));
const StackJobsTab = lazy(() => import("./stack-jobs").then((m) => ({ default: m.StackJobsTab })));

function StackSkeleton() {
  return (
    <div className="space-y-6">
      <Skeleton className="h-11 w-64" />
      <Skeleton className="h-9 w-full max-w-md" />
      <Skeleton className="h-96 rounded-xl" />
    </div>
  );
}

/** /orgs/:org/stacks/:stack(/:tab(/:id)): to the stack's page under its project environment, or the page here when no project owns it. */
export function StackRedirect() {
  const { org = "", stack: name = "", tab: seg, id } = useParams();
  const tab = seg && id ? `${stackTab(seg)}/${id}` : seg;
  const projects = useProjects(org);
  const known = projects.data ? ownerOf(projects.data, name) : null;
  // The export is only needed when the projects' compose lists don't name the stack.
  const exp = useStackExport(org, name);
  if (projects.isLoading || (!known && exp.isLoading)) return <StackSkeleton />;
  const to = stackRedirect(org, name, tab, projects.data ?? [], exp.data);
  return to ? <Navigate to={to} replace /> : <StackPage />;
}

export function StackPage() {
  const { org = "", project, env, stack: name = "", tab, id } = useParams();
  const [params] = useSearchParams();
  const exp = useStackExport(org, name);
  const status = useStack(org, name, 3000);
  const projects = useProjects(org);
  const writer = canWrite(useMe().data!, org);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [deploying, setDeploying] = useState(false);
  useOrgLive(org);
  const o = encodeURIComponent(org);

  // The page's old tab names: Compose is YAML, Services is General.
  const legacy = stackTab(tab);
  if (tab && legacy !== tab && project && env) {
    return <Navigate to={`${composePath(org, { project, environment: env }, name, legacy)}${params.size ? `?${params}` : ""}`} replace />;
  }
  if (exp.isLoading) return <StackSkeleton />;
  if (exp.error || !exp.data) {
    return isNotFound(exp.error) ? (
      <Card className="py-0">
        <EmptyState
          icon={Layers}
          title={`No stack ${name} in ${org}`}
          action={
            <Button asChild variant="outline">
              <Link to={`/orgs/${o}/projects`}>All projects</Link>
            </Button>
          }
        >
          It may have been removed.
        </EmptyState>
      </Card>
    ) : (
      <QueryError error={exp.error} />
    );
  }
  const e = exp.data;
  const tabs = serviceTabs({ writer });
  const active = activeServiceTab(legacy, tabs);
  const services = status.data?.services;
  const health = stackHealth(status.data ?? undefined);
  const healthy = (services ?? []).reduce((n, s) => n + s.healthy, 0);
  const replicas = (services ?? []).reduce((n, s) => n + s.replicas, 0);
  const url = (services ?? []).flatMap((s) => s.domains ?? []).map((d) => d.url).find(Boolean);
  const appsOwner = e.managed_by === "apps" ? (projects.data ?? []).find((p) => p.environments.some((x) => x.stack === name || name.startsWith(`${x.stack}-pr-`))) : undefined;
  // The project environment this compose stack is deployed into: the URL's, else the projects' or the export's say.
  const owner: StackOwner | null =
    project && env
      ? { project, environment: env }
      : (ownerOf(projects.data ?? [], name) ?? (!e.managed_by && e.project && e.environment ? { project: e.project, environment: e.environment } : null));
  const base = (t: string) => (owner ? composePath(org, owner, name, t) : `/orgs/${o}/stacks/${encodeURIComponent(name)}/${t}`);
  // The picked service rides along between tabs.
  const picked = params.get("service");
  const tabPath = (t: string, service = picked ?? undefined) => `${base(t)}${service ? `?service=${encodeURIComponent(service)}` : ""}`;
  const deploymentsPath = (d?: number) => base(d ? `deployments/${d}` : "deployments");
  const envPath = owner && `/orgs/${o}/projects/${encodeURIComponent(owner.project)}/${encodeURIComponent(owner.environment)}`;

  // Deploy the compose file as it is now: what the YAML tab's Deploy does
  // with no edits, so environment or domain changes saved without deploying
  // go out. Not for a stack a project's apps or isb itself manage.
  const deploy = e.managed_by
    ? undefined
    : {
        pending: deploying,
        run: async () => {
          setDeploying(true);
          try {
            const r = await callTool<DeployResult>("stack_deploy", { name, compose: e.yaml, ...ownerArgs(owner) }, org);
            await afterDeploy(qc, org, name);
            deployToast(name, r);
            if (r?.deployment?.id) navigate(deploymentsPath(r.deployment.id));
          } catch (err) {
            toast.error(errorMessage(err));
          } finally {
            setDeploying(false);
          }
        },
      };

  return (
    <>
      <Crumbs
        items={
          owner && envPath
            ? [
                { label: "Projects", to: `/orgs/${o}/projects` },
                { label: owner.project, to: `/orgs/${o}/projects/${encodeURIComponent(owner.project)}` },
                { label: owner.environment, to: envPath },
                { label: name },
              ]
            : [{ label: "Projects", to: `/orgs/${o}/projects` }, { label: name }]
        }
      />
      <ServiceHeader
        icon={Layers}
        name={name}
        state={
          status.isLoading ? (
            <Skeleton className="h-5 w-20 rounded-full" />
          ) : (
            <ToneBadge tone={HEALTH_TONE[health]} pulse={health === "updating"}>
              {HEALTH_LABEL[health]}
            </ToneBadge>
          )
        }
        details={
          <>
            <span className="flex min-w-0 items-center gap-1.5">
              <FileCode2 className="size-3.5 shrink-0" />
              <span className="truncate font-mono text-xs">
                compose · {e.services.length} service{e.services.length === 1 ? "" : "s"}
              </span>
            </span>
            <span className="flex items-center gap-1.5">
              <Server className="size-3.5 shrink-0" />
              {status.isLoading ? <Skeleton className="h-3.5 w-20" /> : services?.length ? `${healthy}/${replicas} healthy` : "not running"}
            </span>
            {e.deployed_at > 0 && (
              <span title={new Date(e.deployed_at * 1000).toLocaleString()}>
                Deployed {relativeTime(e.deployed_at)} by {e.deployed_by || "someone"}
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
          writer &&
          deploy && (
            <>
              {!status.isLoading && <StopStart org={org} name={name} yaml={e.yaml} names={e.services} services={services} />}
              <Button onClick={deploy.run} disabled={deploy.pending}>
                {deploy.pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                {e.deployed_at ? "Redeploy" : "Deploy"}
              </Button>
            </>
          )
        }
      />
      <StackActiveDeployment org={org} name={name} path={deploymentsPath} viewing={active === "deployments" && id ? Number(id) : undefined} />
      {e.managed_by === "apps" && (
        <Alert className="mb-5">
          <AlertTitle>This stack belongs to {appsOwner ? `the project ${appsOwner.name}` : "a project"}'s apps</AlertTitle>
          <AlertDescription>
            Its services are apps. Change them from their app pages (or their YAML tab), not from a compose file.{" "}
            {appsOwner && (
              <Link to={`/orgs/${o}/projects/${appsOwner.name}`} className="font-medium underline underline-offset-4">
                Open {appsOwner.name}
              </Link>
            )}
          </AlertDescription>
        </Alert>
      )}
      <ServiceTabBar tabs={tabs} active={active} to={(t) => tabPath(t)} />
      {active === "yaml" && <StackYamlTab org={org} name={name} exp={e} owner={owner} writer={writer} deploymentsPath={deploymentsPath} generalPath={tabPath("general")} />}
      {active === "general" && (
        <StackGeneralTab
          org={org}
          name={name}
          exp={e}
          services={services}
          loading={status.isLoading}
          writer={writer}
          tabPath={tabPath}
          deploy={deploy}
          removable={!e.managed_by}
          onRemoved={() => navigate(envPath || `/orgs/${o}/projects`)}
        />
      )}
      {active === "environment" && <StackEnvironmentTab org={org} name={name} deploymentsPath={deploymentsPath} />}
      {active === "domains" && (
        <StackDomainsTab org={org} name={name} services={e.services} status={services} statusLoading={status.isLoading} deploymentsPath={deploymentsPath} yamlPath={tabPath("yaml")} />
      )}
      {active === "deployments" &&
        (id ? (
          <StackDeploymentPage
            org={org}
            name={name}
            id={Number(id)}
            writer={writer}
            path={deploymentsPath}
            deploy={deploy}
            urls={(services ?? []).flatMap((s) => s.domains ?? []).map((d) => d.url).filter((u): u is string => !!u)}
          />
        ) : (
          <StackDeploymentsTab org={org} name={name} writer={writer} path={deploymentsPath} deploy={deploy} />
        ))}
      {active === "logs" && <StackLogsTab org={org} name={name} services={(services ?? []).map((s) => s.service)} />}
      {active === "monitoring" && <StackMonitoringTab org={org} name={name} services={e.services} status={services} loading={status.isLoading} error={status.error} />}
      <Suspense fallback={<Skeleton className="h-64" />}>
        {active === "jobs" && <StackJobsTab org={org} name={name} services={e.services} />}
        {active === "terminal" && <StackTerminalTab org={org} services={e.services} status={services} loading={status.isLoading} />}
      </Suspense>
      {active === "advanced" && <StackAdvancedTab org={org} name={name} services={services} loading={status.isLoading} writer={writer} />}
    </>
  );
}

/**
 * The stack's deployment in progress (stack_deployments' newest, not
 * finished), read every 2 s while it runs and every 10 s otherwise, so one
 * started elsewhere shows up too. Hidden on that deployment's own page.
 */
function StackActiveDeployment({ org, name, path, viewing }: { org: string; name: string; path: (id?: number) => string; viewing?: number }) {
  const deps = useStackDeployments(org, name, 5, (latest) => (latest && !finished(latest.status) ? 2000 : 10000));
  const latest = deps.data?.deployments[0];
  if (!latest || finished(latest.status) || latest.id === viewing) return null;
  return <DeploymentBanner d={asDeployment(name, latest)} to={path(latest.id)} coarse />;
}

/**
 * Stop and Start for the whole stack, as an app has for its service. Stop
 * scales every service to 0 (stack_scale); Start, shown once every service
 * is at 0, scales each back to the replicas its compose file asks for.
 */
function StopStart({ org, name, yaml, names, services }: { org: string; name: string; yaml: string; names: string[]; services: StackServices | undefined }) {
  const qc = useQueryClient();
  const [stopOpen, setStopOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  if (!services?.length) return null;
  const refresh = () => Promise.all([qc.invalidateQueries({ queryKey: stackKeys.org(org) }), qc.invalidateQueries({ queryKey: keys.org(org) })]);
  const stopped = services.every((s) => s.replicas === 0);
  const start = async () => {
    setStarting(true);
    try {
      const want = sourceReplicas(yaml, names.length ? names : services.map((s) => s.service));
      for (const [service, replicas] of Object.entries(want)) await callTool("stack_scale", { name, service, replicas }, org);
      await refresh();
      toast.success(`${name} starting`);
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setStarting(false);
    }
  };
  return (
    <>
      {stopped ? (
        <Button variant="outline" onClick={start} disabled={starting}>
          {starting ? <Loader2 className="animate-spin" /> : <Play />}
          Start
        </Button>
      ) : (
        <Button variant="outline" onClick={() => setStopOpen(true)}>
          <Square />
          Stop
        </Button>
      )}
      <ConfirmDialog
        open={stopOpen}
        onOpenChange={setStopOpen}
        title={`Stop ${name}?`}
        description="Every service is scaled to 0 replicas and its domains answer 503 until you start it or deploy again. The compose file, settings and volumes are kept."
        confirmLabel="Stop stack"
        onConfirm={async () => {
          for (const s of services) if (s.replicas > 0) await callTool("stack_scale", { name, service: s.service, replicas: 0 }, org);
          await refresh();
          toast.success(`${name} stopped`);
        }}
      />
    </>
  );
}
