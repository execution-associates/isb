// /orgs/:org/projects/:project/:env: a project, its environments as tabs, and
// the services (apps, databases and compose stacks) of the one selected, as cards.
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Box, Boxes, ChevronDown, Database, GitBranch, Globe, Layers, LayoutTemplate, MoreHorizontal, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool, type StackStatus } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { StatusDot } from "@/components/status";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
  useIngress,
  useLatestDeployments,
  useProjects,
  useStack,
} from "./api";
import { composePath } from "@/stacks/api";
import { AppStateBadge, ConfirmDialog, Crumbs, DeploymentBadge, EmptyState, QueryError, TabLinks, ToneBadge } from "./components";
import { autoHostLabel, ingressOff } from "./domains";
import { HEALTH_LABEL, HEALTH_TONE, stackHealth, useStackList } from "./health";
import { NewAppDialog } from "./new-app-dialog";
import { NewEnvironmentDialog } from "./project-dialogs";
import { imageName, shortSha } from "./util";

export function ProjectPage() {
  const { org = "", project = "", env } = useParams();
  const writer = canWrite(useMe().data!, org);
  const projects = useProjects(org);
  const apps = useApps(org);
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
  const stacks = useStackList();
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

  const total = p.environments.reduce((n, e) => n + e.apps.length + e.compose.length, 0);
  const templateLink = `/orgs/${o}/templates?project=${encodeURIComponent(project)}&env=${encodeURIComponent(environment.name)}`;
  const composeLink = `/orgs/${o}/projects/${encodeURIComponent(project)}/${encodeURIComponent(environment.name)}/compose/new`;
  const compose = environment.compose.map((c) => ({ ...c, status: (stacks.data?.stacks ?? []).find((s) => s.org === org && s.name === c.name) }));
  const nServices = envApps.length + compose.length;
  const svcs = [...(stack.data?.services ?? []), ...compose.flatMap((c) => c.status?.services ?? [])];
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
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button>
                    <Plus />
                    Create service
                    <ChevronDown className="opacity-70" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-56">
                  <DropdownMenuItem onSelect={() => setNewApp(true)}>
                    <Box />
                    App
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={() => setNewDb(true)}>
                    <Database />
                    Database
                  </DropdownMenuItem>
                  <DropdownMenuItem asChild>
                    <Link to={composeLink}>
                      <Layers />
                      Compose
                    </Link>
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem asChild>
                    <Link to={templateLink}>
                      <LayoutTemplate />
                      From a template
                    </Link>
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
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
                  <DropdownMenuItem variant="destructive" disabled={environment.apps.length + environment.compose.length > 0 || p.environments.length < 2} onSelect={() => setDelEnv(true)}>
                    <Trash2 />
                    Delete {environment.name}
                  </DropdownMenuItem>
                  <DropdownMenuItem variant="destructive" disabled={total > 0} onSelect={() => setDelProject(true)}>
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
        tabs={p.environments.map((e) => ({ id: e.name, label: `${e.name} · ${e.apps.length + e.compose.length}`, to: `/orgs/${o}/projects/${project}/${e.name}` }))}
      />
      {(environment.conflicts?.length ?? 0) > 0 && (
        <Alert role="status" className="mb-5 border-warning/40 bg-warning/5">
          <AlertTriangle />
          <AlertTitle>Service names clash in {environment.name}</AlertTitle>
          <AlertDescription>
            <ul className="grid gap-0.5">
              {environment.conflicts?.map((c) => (
                <li key={`${c.stack}/${c.service}`}>
                  <span className="font-mono">{c.service}</span> in <span className="font-mono">{c.stack}</span> is shadowed: the name reaches{" "}
                  <span className="font-mono">{c.winner}</span>.
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      )}
      {apps.isLoading ? (
        <CardsSkeleton />
      ) : apps.error ? (
        <QueryError error={apps.error} />
      ) : nServices === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={Boxes}
            title={`Nothing in ${environment.name} yet`}
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
                    <Link to={composeLink}>
                      <Layers />
                      New compose
                    </Link>
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
            Deploy a container image, build one from a git repository, start a database, or paste a compose file. Apps and databases here run as the
            stack <span className="font-mono text-foreground/80">{environment.stack}</span>.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <span>
              {nServices} service{nServices === 1 ? "" : "s"}
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
            {compose.map((c) => (
              <ComposeCard key={c.name} to={composePath(org, { project, environment: environment.name }, c.name)} name={c.name} services={c.services} status={c.status} />
            ))}
          </div>
        </>
      )}
      {writer && (total > 0 || p.environments.length < 2) && (
        <p className="mt-6 text-xs text-muted-foreground">
          {total > 0 ? "A project or environment can be deleted once its services are." : "A project keeps at least one environment."}
        </p>
      )}

      <NewAppDialog org={org} project={project} environment={environment.name} open={newApp} onOpenChange={setNewApp} />
      <NewEnvironmentDialog org={org} project={project} open={newEnv} onOpenChange={setNewEnv} />
      <NewDatabaseDialog org={org} project={project} environment={environment.name} open={newDb} onOpenChange={setNewDb} />
      <ConfirmDialog
        open={delEnv}
        onOpenChange={setDelEnv}
        title={`Delete environment ${environment.name}?`}
        description={`It has no services; this removes it from ${project}.`}
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
        description="It has no services left. Its environments go with it."
        confirmLabel="Delete project"
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
  const off = ingressOff(useIngress(org).data);
  const last = deployments[0];
  const state = appState(svc, last);
  const src = sourceOf(a, deployments.find((d) => d.id === a.current_deployment) ?? last);
  const Icon = KIND_ICON[src.kind];
  const urls = (svc?.domains ?? []).map((d) => d.url).filter(Boolean) as string[];
  const pending = urls.length ? [] : (a.domains ?? []).map((d) => (off ? autoHostLabel(String(d.host), undefined, true) : d.host === "auto" ? "generated name" : String(d.host)));
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
          <span className={cn("truncate", off && "text-warning")} title={off ? "No ingress: domains aren't served" : undefined}>
            {pending.join(", ")}
          </span>
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

/** A compose stack in the environment: its services and how they are doing. */
function ComposeCard({ to, name, services, status }: { to: string; name: string; services: string[]; status: StackStatus | undefined }) {
  const h = stackHealth(status);
  const replicas = status?.services.reduce((n, s) => n + s.replicas, 0) ?? 0;
  const healthy = status?.services.reduce((n, s) => n + s.healthy, 0) ?? 0;
  return (
    <Card className="group relative h-full gap-0 py-0 transition-[border-color,box-shadow] hover:border-foreground/20 hover:shadow-md has-[a.card-link:focus-visible]:ring-[3px] has-[a.card-link:focus-visible]:ring-ring/50">
      <div className="flex items-start gap-3 px-5 pt-4 pb-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-gradient-to-b from-muted/30 to-muted text-muted-foreground">
          <Layers className="size-4" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-2">
            <Link to={to} className="card-link truncate text-[15px] font-semibold tracking-tight after:absolute after:inset-0 after:rounded-xl focus-visible:outline-none">
              {name}
            </Link>
            <ToneBadge tone={HEALTH_TONE[h]} pulse={h === "updating"}>
              {HEALTH_LABEL[h]}
            </ToneBadge>
          </div>
          <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
            COMPOSE · {services.length} service{services.length === 1 ? "" : "s"}
          </p>
        </div>
      </div>
      <div className="flex min-h-6 items-center gap-1.5 px-5 pb-3 text-xs text-muted-foreground">
        <Boxes className="size-3.5 shrink-0" />
        <span className="truncate" title={services.join(", ")}>
          {services.join(", ") || "No services"}
        </span>
      </div>
      <div className="mt-auto flex items-center justify-between gap-3 border-t px-5 py-2.5 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1.5 tabular-nums">
          <StatusDot tone={HEALTH_DOT[h]} className="size-1.5" />
          {status ? `${healthy}/${replicas} healthy` : "Not running"}
        </span>
        {status ? (
          <span className="truncate tabular-nums" title={new Date(status.deployed_at * 1000).toLocaleString()}>
            {relativeTime(status.deployed_at)}
          </span>
        ) : (
          <span>Never deployed</span>
        )}
      </div>
    </Card>
  );
}

const HEALTH_DOT: Record<keyof typeof HEALTH_TONE, Tone> = {
  healthy: "success",
  degraded: "warning",
  failing: "danger",
  updating: "info",
  idle: "muted",
};

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
