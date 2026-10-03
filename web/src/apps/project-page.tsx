// /orgs/:org/projects/:project/:env: a project, its environments as tabs, and
// the services (apps and databases) of the one selected, as cards.
import { useQueryClient } from "@tanstack/react-query";
import { Box, Boxes, Database, GitBranch, Globe, LayoutTemplate, MoreHorizontal, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { StatusDot } from "@/components/status";
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
import { engineLabel } from "@/data/api";
import { NewDatabaseDialog } from "@/data/new-database";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import {
  type App,
  type AppState,
  appState,
  type Deployment,
  isGit,
  keys,
  serviceOf,
  type StackDetail,
  useApps,
  useLatestDeployments,
  useProjects,
  useStack,
} from "./api";
import { AppStateBadge, ConfirmDialog, Crumbs, DeploymentBadge, EmptyState, QueryError, TabLinks } from "./components";
import { useOrgLive } from "./live";
import { NewAppDialog } from "./new-app-dialog";
import { NewEnvironmentDialog } from "./project-dialogs";
import { imageName, shortSha } from "./util";

export function ProjectPage() {
  const { org = "", project = "", env } = useParams();
  const writer = canWrite(useMe().data!, org);
  const projects = useProjects(org);
  const apps = useApps(org);
  useOrgLive(org);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [newApp, setNewApp] = useState(false);
  const [newEnv, setNewEnv] = useState(false);
  const [newDb, setNewDb] = useState(false);
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

  if (projects.isLoading) return <ProjectSkeleton />;
  if (projects.error) return <QueryError error={projects.error} />;
  if (!p) {
    return (
      <Card className="py-0">
        <EmptyState
          icon={Boxes}
          title={`No project ${project} in ${org}`}
          action={
            <Button asChild variant="outline">
              <Link to={`/orgs/${o}/projects`}>All projects</Link>
            </Button>
          }
        />
      </Card>
    );
  }
  if (!environment) return <Navigate to={`/orgs/${o}/projects/${project}/${p.environments[0]?.name ?? ""}`} replace />;

  const totalApps = p.environments.reduce((n, e) => n + e.apps.length, 0);
  const templateLink = `/orgs/${o}/templates?project=${encodeURIComponent(project)}&env=${encodeURIComponent(environment.name)}`;
  const svcs = stack.data?.services ?? [];
  const replicas = svcs.reduce((n, s) => n + s.replicas, 0);
  const healthy = svcs.reduce((n, s) => n + s.healthy, 0);

  return (
    <>
      <Crumbs items={[{ label: "Projects", to: `/orgs/${o}/projects` }, { label: project }]} />
      <PageHeader
        title={project}
        description={p.description || "Environments run as their own stacks; apps in one reach each other by name."}
        actions={
          writer && (
            <>
              <Button onClick={() => setNewApp(true)}>
                <Plus />
                New app
              </Button>
              <Button variant="outline" onClick={() => setNewDb(true)} className="hidden sm:inline-flex">
                <Database />
                New database
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="icon" aria-label="Project actions">
                    <MoreHorizontal />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-56">
                  <DropdownMenuItem onSelect={() => setNewDb(true)} className="sm:hidden">
                    <Database />
                    New database
                  </DropdownMenuItem>
                  <DropdownMenuItem asChild>
                    <Link to={templateLink}>
                      <LayoutTemplate />
                      From a template
                    </Link>
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
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
          )
        }
      />
      <TabLinks
        active={environment.name}
        tabs={p.environments.map((e) => ({ id: e.name, label: `${e.name} · ${e.apps.length}`, to: `/orgs/${o}/projects/${project}/${e.name}` }))}
      />
      {apps.isLoading ? (
        <CardsSkeleton />
      ) : apps.error ? (
        <QueryError error={apps.error} />
      ) : envApps.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={Boxes}
            title={`No apps in ${environment.name} yet`}
            action={
              writer && (
                <>
                  <Button onClick={() => setNewApp(true)}>
                    <Plus />
                    New app
                  </Button>
                  <Button variant="outline" onClick={() => setNewDb(true)}>
                    <Database />
                    New database
                  </Button>
                  <Button asChild variant="outline">
                    <Link to={templateLink}>
                      <LayoutTemplate />
                      From a template
                    </Link>
                  </Button>
                </>
              )
            }
          >
            Deploy a container image, build one from a git repository, or start a database. Everything here runs as the stack{" "}
            <span className="font-mono text-foreground/80">{environment.stack}</span>.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <span>
              {envApps.length} service{envApps.length === 1 ? "" : "s"}
            </span>
            <span aria-hidden>·</span>
            <span>
              stack <span className="font-mono">{environment.stack}</span>
            </span>
            {replicas > 0 && (
              <>
                <span aria-hidden>·</span>
                <span className="tabular-nums">
                  {healthy}/{replicas} replicas healthy
                </span>
              </>
            )}
          </div>
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            {envApps.map((a) => (
              <ServiceCard key={a.name} org={org} app={a} stack={stack.data} deployments={latest.get(a.name) ?? []} />
            ))}
          </div>
        </>
      )}
      {writer && (totalApps > 0 || p.environments.length < 2) && (
        <p className="mt-6 text-xs text-muted-foreground">
          {totalApps > 0 ? "A project or environment can be deleted once its apps are." : "A project keeps at least one environment."}
        </p>
      )}

      <NewAppDialog org={org} project={project} environment={environment.name} open={newApp} onOpenChange={setNewApp} />
      <NewEnvironmentDialog org={org} project={project} open={newEnv} onOpenChange={setNewEnv} />
      <NewDatabaseDialog org={org} project={project} environment={environment.name} open={newDb} onOpenChange={setNewDb} />
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

type Source = { kind: "database" | "git" | "image"; main: string; sub: string };

function sourceOf(a: App, d?: Deployment): Source {
  const db = (a.source as { database?: { engine: string; version?: string } }).database;
  if (db) return { kind: "database", main: `${engineLabel(db.engine)} ${db.version ?? ""}`.trim(), sub: "" };
  if (isGit(a.source)) {
    const repo = a.source.git.url.replace(/^https?:\/\//, "").replace(/^git@([^:]+):/, "$1/").replace(/\.git$/, "");
    return { kind: "git", main: `${repo}@${a.source.git.ref}`, sub: d?.commit ? `${shortSha(d.commit.sha)} ${d.commit.message}` : "" };
  }
  return { kind: "image", main: imageName(a.source.image).replace(/^docker:/, ""), sub: "" };
}

const KIND_ICON = { database: Database, git: GitBranch, image: Box } as const;

const REPLICA_TONE: Record<AppState, Tone> = {
  running: "success",
  degraded: "warning",
  updating: "info",
  deploying: "info",
  failing: "danger",
  failed: "danger",
  stopped: "muted",
  "not-deployed": "muted",
};

function ServiceCard({ org, app: a, stack, deployments }: { org: string; app: App; stack: StackDetail | null | undefined; deployments: Deployment[] }) {
  const o = encodeURIComponent(org);
  const svc = serviceOf(stack, a.name);
  const last = deployments[0];
  const state = appState(svc, last);
  const src = sourceOf(a, deployments.find((d) => d.id === a.current_deployment) ?? last);
  const Icon = KIND_ICON[src.kind];
  const urls = (svc?.domains ?? []).map((d) => d.url).filter(Boolean) as string[];
  const pending = urls.length ? [] : (a.domains ?? []).map((d) => (d.host === "auto" ? "generated name" : String(d.host)));
  const to = `/orgs/${o}/apps/${a.name}`;

  return (
    <Card className="group relative h-full gap-0 py-0 transition-[border-color,box-shadow] hover:border-foreground/20 hover:shadow-md has-[a.card-link:focus-visible]:ring-[3px] has-[a.card-link:focus-visible]:ring-ring/50">
      <div className="flex items-start gap-3 px-5 pt-4 pb-3">
        <span
          className={cn(
            "flex size-9 shrink-0 items-center justify-center rounded-lg border",
            src.kind === "database" ? "bg-info/8 text-info" : "bg-gradient-to-b from-muted/30 to-muted text-muted-foreground",
          )}
        >
          <Icon className="size-4" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-2">
            <Link to={to} className="card-link truncate text-[15px] font-semibold tracking-tight after:absolute after:inset-0 after:rounded-xl focus-visible:outline-none">
              {a.name}
            </Link>
            <AppStateBadge state={state} />
          </div>
          <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground" title={src.main}>
            {src.main}
          </p>
          {src.sub && (
            <p className="truncate text-xs text-muted-foreground" title={src.sub}>
              {src.sub}
            </p>
          )}
        </div>
      </div>
      <div className="flex min-h-6 items-center gap-1.5 px-5 pb-3 text-xs text-muted-foreground">
        <Globe className="size-3.5 shrink-0" />
        {urls.length > 0 ? (
          <span className="flex min-w-0 items-center gap-1.5">
            <a
              href={urls[0]}
              target="_blank"
              rel="noreferrer"
              className="relative z-10 truncate text-foreground/85 underline-offset-2 hover:text-foreground hover:underline"
            >
              {urls[0].replace(/^https?:\/\//, "")}
            </a>
            {urls.length > 1 && <span className="shrink-0">+{urls.length - 1}</span>}
          </span>
        ) : pending.length > 0 ? (
          <span className="truncate">{pending.join(", ")}</span>
        ) : (
          <span>{src.kind === "database" ? "Internal only" : "No domain"}</span>
        )}
      </div>
      <div className="mt-auto flex items-center justify-between gap-3 border-t px-5 py-2.5 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1.5 tabular-nums">
          <StatusDot tone={REPLICA_TONE[state]} className="size-1.5" />
          {svc ? `${svc.healthy}/${svc.replicas} healthy` : `${a.replicas} replica${a.replicas === 1 ? "" : "s"} wanted`}
        </span>
        {last ? (
          <span className="inline-flex min-w-0 items-center gap-2">
            <DeploymentBadge status={last.status} />
            <span className="truncate tabular-nums" title={new Date(last.created_at).toLocaleString()}>
              {relativeTime(last.created_at / 1000)}
            </span>
          </span>
        ) : (
          <span>Never deployed</span>
        )}
      </div>
    </Card>
  );
}

function CardsSkeleton() {
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
      {[0, 1, 2].map((i) => (
        <Card key={i} className="gap-3 px-5 py-4">
          <div className="flex items-center gap-3">
            <Skeleton className="size-9 rounded-lg" />
            <div className="flex-1 space-y-1.5">
              <Skeleton className="h-4 w-24" />
              <Skeleton className="h-3 w-40" />
            </div>
          </div>
          <Skeleton className="h-3 w-32" />
          <Skeleton className="h-4 w-full" />
        </Card>
      ))}
    </div>
  );
}

function ProjectSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-4 w-72" />
      </div>
      <Skeleton className="h-9 w-64" />
      <CardsSkeleton />
    </div>
  );
}
