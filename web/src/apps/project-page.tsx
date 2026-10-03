// /orgs/:org/projects/:project/:env: a project, its environments as tabs, and
// the apps of the one selected.
import { useQueryClient } from "@tanstack/react-query";
import { Boxes, ChevronRight, Globe, MoreHorizontal, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";
import { type App, appState, type Deployment, isGit, keys, serviceOf, type StackDetail, useApps, useLatestDeployments, useProjects, useStack } from "./api";
import { AppStateBadge, ConfirmDialog, Crumbs, DeploymentBadge, EmptyState, LiveIndicator, QueryError, TabLinks } from "./components";
import { useOrgLive } from "./live";
import { NewAppDialog } from "./new-app-dialog";
import { NewEnvironmentDialog } from "./project-dialogs";
import { imageName, shortSha } from "./util";

export function ProjectPage() {
  const { org = "", project = "", env } = useParams();
  const projects = useProjects(org);
  const apps = useApps(org);
  const live = useOrgLive(org);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [newApp, setNewApp] = useState(false);
  const [newEnv, setNewEnv] = useState(false);
  const [delEnv, setDelEnv] = useState(false);
  const [delProject, setDelProject] = useState(false);

  const p = projects.data?.find((x) => x.name === project);
  const environment = p?.environments.find((e) => e.name === env);
  const stack = useStack(org, environment?.stack);
  const envApps = (apps.data ?? []).filter((a) => a.project === project && a.environment === env);
  const latest = useLatestDeployments(
    org,
    envApps.map((a) => a.name),
  );
  const o = encodeURIComponent(org);

  if (projects.isLoading) return <Skeleton className="h-64" />;
  if (projects.error) return <QueryError error={projects.error} />;
  if (!p) {
    return (
      <Card className="py-0">
        <EmptyState icon={Boxes} title={`No project ${project} in ${org}`} action={<Button asChild variant="outline"><Link to={`/orgs/${o}/projects`}>All projects</Link></Button>} />
      </Card>
    );
  }
  if (!environment) return <Navigate to={`/orgs/${o}/projects/${project}/${p.environments[0]?.name ?? ""}`} replace />;

  const totalApps = p.environments.reduce((n, e) => n + e.apps.length, 0);

  return (
    <>
      <Crumbs items={[{ label: "Projects", to: `/orgs/${o}/projects` }, { label: project }]} />
      <PageHeader
        title={project}
        description={p.description || `Environments run as their own stacks; apps in one reach each other by name.`}
        actions={
          <>
            <LiveIndicator state={live} />
            <Button onClick={() => setNewApp(true)}>
              <Plus />
              New app
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" aria-label="Project actions">
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-56">
                <DropdownMenuItem onSelect={() => setNewEnv(true)}>
                  <Plus />
                  Add environment
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" disabled={environment.apps.length > 0 || p.environments.length < 2} onSelect={() => setDelEnv(true)}>
                  <Trash2 />
                  Delete {environment.name}
                </DropdownMenuItem>
                <DropdownMenuItem variant="destructive" disabled={totalApps > 0} onSelect={() => setDelProject(true)}>
                  <Trash2 />
                  Delete project
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </>
        }
      />
      <TabLinks
        active={environment.name}
        tabs={p.environments.map((e) => ({ id: e.name, label: `${e.name} · ${e.apps.length}`, to: `/orgs/${o}/projects/${project}/${e.name}` }))}
      />
      {apps.isLoading ? (
        <Skeleton className="h-40" />
      ) : apps.error ? (
        <QueryError error={apps.error} />
      ) : envApps.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={Boxes}
            title={`No apps in ${environment.name} yet`}
            action={
              <Button onClick={() => setNewApp(true)}>
                <Plus />
                New app
              </Button>
            }
          >
            Deploy a container image, or build one from a git repository. Apps here run as the stack{" "}
            <span className="font-mono">{environment.stack}</span>.
          </EmptyState>
        </Card>
      ) : (
        <AppList org={org} apps={envApps} stack={stack.data} latest={latest} />
      )}
      {(totalApps > 0 || p.environments.length < 2) && (
        <p className="mt-4 text-xs text-muted-foreground">
          {totalApps > 0 ? "A project or environment can be deleted once its apps are." : "A project keeps at least one environment."}
        </p>
      )}

      <NewAppDialog org={org} project={project} environment={environment.name} open={newApp} onOpenChange={setNewApp} />
      <NewEnvironmentDialog org={org} project={project} open={newEnv} onOpenChange={setNewEnv} />
      <ConfirmDialog
        open={delEnv}
        onOpenChange={setDelEnv}
        title={`Delete environment ${environment.name}?`}
        description={`It has no apps; this removes it from ${project}.`}
        confirmLabel="Delete environment"
        onConfirm={async () => {
          await callTool("environment_delete", { project, name: environment.name }, org);
          await qc.invalidateQueries({ queryKey: keys.projects(org) });
          toast.success(`Environment ${environment.name} deleted`);
          navigate(`/orgs/${o}/projects/${project}`);
        }}
      />
      <ConfirmDialog
        open={delProject}
        onOpenChange={setDelProject}
        title={`Delete project ${project}?`}
        description="It has no apps left. Its environments go with it."
        confirmLabel="Delete project"
        typed={project}
        onConfirm={async () => {
          await callTool("project_delete", { name: project }, org);
          await qc.invalidateQueries({ queryKey: keys.projects(org) });
          toast.success(`Project ${project} deleted`);
          navigate(`/orgs/${o}/projects`);
        }}
      />
    </>
  );
}

function sourceLabel(a: App, d?: Deployment) {
  if (isGit(a.source)) {
    const repo = a.source.git.url.replace(/^https?:\/\//, "").replace(/\.git$/, "");
    return { main: repo, sub: d?.commit ? `${a.source.git.ref} · ${shortSha(d.commit.sha)} ${d.commit.message}` : a.source.git.ref };
  }
  return { main: imageName(a.source.image), sub: d?.digest ? d.digest.slice(0, 19) : "" };
}

function AppList({
  org,
  apps,
  stack,
  latest,
}: {
  org: string;
  apps: App[];
  stack: StackDetail | null | undefined;
  latest: Map<string, Deployment[]>;
}) {
  const o = encodeURIComponent(org);
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="hidden grid-cols-[minmax(0,1.3fr)_minmax(0,1.6fr)_6rem_minmax(0,1.2fr)_minmax(0,1fr)_1.5rem] gap-4 border-b bg-muted/30 px-5 py-2.5 text-xs font-medium text-muted-foreground lg:grid">
        <span>App</span>
        <span>Source</span>
        <span>Replicas</span>
        <span>Domains</span>
        <span>Last deploy</span>
        <span />
      </div>
      <ul className="divide-y">
        {apps.map((a) => {
          const svc = serviceOf(stack, a.name);
          const ds = latest.get(a.name) ?? [];
          const last = ds[0];
          const state = appState(svc, last);
          const src = sourceLabel(a, ds.find((d) => d.id === a.current_deployment) ?? last);
          const urls = (svc?.domains ?? []).map((d) => d.url).filter(Boolean) as string[];
          const hosts = urls.length ? urls : (a.domains ?? []).map((d) => String(d.host));
          return (
            <li key={a.name}>
              <Link
                to={`/orgs/${o}/apps/${a.name}`}
                className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1.5 px-5 py-4 transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none lg:grid-cols-[minmax(0,1.3fr)_minmax(0,1.6fr)_6rem_minmax(0,1.2fr)_minmax(0,1fr)_1.5rem] lg:items-center"
              >
                <div className="flex min-w-0 items-center gap-2.5">
                  <span className="truncate font-medium">{a.name}</span>
                  <AppStateBadge state={state} className="lg:hidden" />
                </div>
                <ChevronRight className="size-4 self-center text-muted-foreground lg:order-last" />
                <div className="col-span-2 min-w-0 lg:col-span-1">
                  <p className="truncate font-mono text-xs">{src.main}</p>
                  {src.sub && <p className="truncate text-xs text-muted-foreground">{src.sub}</p>}
                </div>
                <div className="hidden lg:block">
                  <AppStateBadge state={state} />
                  <p className="mt-1 text-xs text-muted-foreground tabular-nums">{svc ? `${svc.healthy}/${svc.replicas} healthy` : `${a.replicas} wanted`}</p>
                </div>
                <div className="col-span-2 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground lg:col-span-1">
                  {hosts.length > 0 && <Globe className="size-3.5 shrink-0" />}
                  <span className="truncate">{hosts.length ? hosts.map((h) => h.replace(/^https?:\/\//, "")).join(", ") : <span className="lg:hidden">no domains</span>}</span>
                </div>
                <div className={cn("col-span-2 flex items-center gap-2 text-xs text-muted-foreground lg:col-span-1")}>
                  {last ? (
                    <>
                      <DeploymentBadge status={last.status} />
                      <span className="truncate">
                        #{last.id} {relativeTime(last.created_at / 1000)}
                      </span>
                    </>
                  ) : (
                    "never deployed"
                  )}
                  <span className="ml-auto tabular-nums lg:hidden">{svc ? `${svc.healthy}/${svc.replicas}` : ""}</span>
                </div>
              </Link>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
