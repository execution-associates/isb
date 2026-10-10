// /orgs/:org/projects/:project/:env: a project, its environments as tabs, and
// the services (apps, databases and compose stacks) of the one selected, as cards.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Box, Boxes, ChevronDown, Database, GitBranch, Globe, Layers, LayoutTemplate, Loader2, MoreHorizontal, Play, Plus, Square, Trash2 } from "lucide-react";
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
import { canWrite, maxGrant } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { invalidateOrg } from "@/lib/freshness";
import { useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import {
  type App,
  type AppState,
  appState,
  type Deployment,
  isGit,
  isStarting,
  isStopping,
  keys,
  serviceOf,
  type StackDetail,
  useApps,
  useIngress,
  useLatestDeployments,
  useProjects,
  useStack,
} from "./api";
import { composePath, type StackExport, sourceReplicas } from "@/stacks/api";
import { AppStateBadge, ConfirmDialog, Crumbs, DeploymentBadge, EmptyState, QueryError, TabLinks, ToneBadge } from "./components";
import { autoHostLabel, ingressOff } from "./domains";
import { HEALTH_LABEL, HEALTH_TONE, stackHealth, useStackList } from "./health";
import { NewAppDialog } from "./new-app-dialog";
import { NewEnvironmentDialog } from "./project-dialogs";
import { imageName, shortSha } from "./util";

export function ProjectPage() {
  const { org = "", project = "", env } = useParams();
  const me = useMe().data!;
  const writer = canWrite(me, org);
  // Deleting volumes is for org admins and owners, as volume_delete is.
  const admin = maxGrant(me, org) !== null;
  const projects = useProjects(org);
  const apps = useApps(org);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [newApp, setNewApp] = useState(false);
  const [newEnv, setNewEnv] = useState(false);
  const [newDb, setNewDb] = useState(false);
  const [delEnv, setDelEnv] = useState(false);
  const [delProject, setDelProject] = useState(false);
  const [wipe, setWipe] = useState(false);

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

  const servicesOf = (e: (typeof p.environments)[number]): Doomed[] => [
    ...e.apps.map((name) => ({ name, kind: "app" as const, env: e.name })),
    ...e.compose.map((c) => ({ name: c.name, kind: "compose" as const, env: e.name })),
  ];
  const envServices = servicesOf(environment);
  const projectServices = p.environments.flatMap(servicesOf);
  const refresh = () =>
    Promise.all([
      qc.invalidateQueries({ queryKey: keys.projects(org) }),
      qc.invalidateQueries({ queryKey: keys.apps(org) }),
      qc.invalidateQueries({ queryKey: ["tool", "stack_list"] }),
    ]);
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
              <EnvStopStart org={org} env={environment.name} stack={environment.stack} apps={envApps} appStatus={stack.data} compose={compose} />
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
                  {p.environments.length > 1 && (
                    <DropdownMenuItem variant="destructive" onSelect={() => setDelEnv(true)}>
                      <Trash2 />
                      Delete {environment.name}
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuItem variant="destructive" onSelect={() => setDelProject(true)}>
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
      <NewAppDialog org={org} project={project} environment={environment.name} open={newApp} onOpenChange={setNewApp} />
      <NewEnvironmentDialog org={org} project={project} open={newEnv} onOpenChange={setNewEnv} />
      <NewDatabaseDialog org={org} project={project} environment={environment.name} open={newDb} onOpenChange={setNewDb} />
      <ConfirmDialog
        open={delEnv}
        onOpenChange={(v) => {
          setDelEnv(v);
          if (!v) setWipe(false);
        }}
        title={`Delete environment ${environment.name}?`}
        description={envServices.length ? "Its services are deleted with it." : `It has no services; this removes it from ${project}.`}
        confirmLabel="Delete environment"
        typed={envServices.length ? environment.name : undefined}
        onConfirm={async () => {
          const force = envServices.length > 0;
          await callTool("environment_delete", { project, name: environment.name, force, volumes: force && wipe }, org);
          await refresh();
          toast.success(`Environment ${environment.name} deleted`);
          navigate(`/orgs/${o}/projects/${project}`);
        }}
      >
        <DoomedServices
          org={org}
          tool="environment_delete"
          args={{ project, name: environment.name }}
          services={envServices}
          admin={admin}
          wipe={wipe}
          onWipe={setWipe}
        />
      </ConfirmDialog>
      <ConfirmDialog
        open={delProject}
        onOpenChange={(v) => {
          setDelProject(v);
          if (!v) setWipe(false);
        }}
        title={`Delete project ${project}?`}
        description={
          projectServices.length
            ? `Its ${p.environments.length === 1 ? "environment" : `${p.environments.length} environments`} and every service in them are deleted with it.`
            : "It has no services left. Its environments go with it."
        }
        confirmLabel="Delete project"
        typed={projectServices.length ? project : undefined}
        onConfirm={async () => {
          const force = projectServices.length > 0;
          await callTool("project_delete", { name: project, force, volumes: force && wipe }, org);
          await refresh();
          toast.success(`Project ${project} deleted`);
          navigate(`/orgs/${o}/projects`);
        }}
      >
        <DoomedServices org={org} tool="project_delete" args={{ name: project }} services={projectServices} admin={admin} wipe={wipe} onWipe={setWipe} />
      </ConfirmDialog>
    </>
  );
}

/**
 * Stop and Start for every service in the environment: its apps and
 * databases (in its own stack) and its compose stacks. Stop scales each to 0
 * replicas; Start scales apps back to the replicas they ask for and compose
 * services to what their compose file says, as their own pages' Start does.
 * stack_scale returns before the instances stop or start, so while any is
 * still on its way the button says so and waits.
 */
function EnvStopStart({
  org,
  env,
  stack,
  apps,
  appStatus,
  compose,
}: {
  org: string;
  env: string;
  stack: string;
  apps: App[];
  appStatus: StackDetail | null | undefined;
  compose: { name: string; services: string[]; status: StackStatus | undefined }[];
}) {
  const qc = useQueryClient();
  const [stopOpen, setStopOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const appSvcs = apps.map((a) => ({ app: a, svc: serviceOf(appStatus, a.name) })).filter((x) => x.svc);
  const all = [...appSvcs.map((x) => x.svc!), ...compose.flatMap((c) => c.status?.services ?? [])];
  if (all.length === 0) return null;
  const refresh = () => Promise.all([invalidateOrg(qc, org), qc.invalidateQueries({ queryKey: ["tool", "stack_list"] }), qc.invalidateQueries({ queryKey: ["stacks", org] })]);
  if (all.some(isStopping) || all.some(isStarting)) {
    return (
      <Button variant="outline" disabled>
        <Loader2 className="animate-spin" />
        {all.some(isStopping) ? "Stopping" : "Starting"}
      </Button>
    );
  }
  const running = all.some((s) => s.replicas > 0);
  const start = async () => {
    setStarting(true);
    try {
      for (const { app } of appSvcs) await callTool("stack_scale", { name: stack, service: app.name, replicas: Math.max(1, app.replicas) }, org);
      for (const c of compose) {
        if (!c.status?.services.length) continue;
        const exp = await callTool<StackExport>("stack_export", { name: c.name }, org);
        const want = sourceReplicas(exp.yaml, c.services.length ? c.services : c.status.services.map((s) => s.service));
        for (const [service, replicas] of Object.entries(want)) await callTool("stack_scale", { name: c.name, service, replicas }, org);
      }
      await refresh();
      toast.success(`${env} starting`);
    } catch (err) {
      toast.error(errorMessage(err));
      await refresh();
    } finally {
      setStarting(false);
    }
  };
  return (
    <>
      {running ? (
        <Button variant="outline" onClick={() => setStopOpen(true)}>
          <Square />
          Stop
        </Button>
      ) : (
        <Button variant="outline" onClick={start} disabled={starting}>
          {starting ? <Loader2 className="animate-spin" /> : <Play />}
          Start
        </Button>
      )}
      <ConfirmDialog
        open={stopOpen}
        onOpenChange={setStopOpen}
        title={`Stop everything in ${env}?`}
        description={`Every app, database and compose stack in ${env} is scaled to 0 replicas, and their domains answer 503 until you start them or deploy again. Settings, compose files and volumes are kept.`}
        confirmLabel={`Stop ${env}`}
        onConfirm={async () => {
          try {
            for (const { app, svc } of appSvcs) if (svc!.replicas > 0) await callTool("stack_scale", { name: stack, service: app.name, replicas: 0 }, org);
            for (const c of compose) for (const s of c.status?.services ?? []) if (s.replicas > 0) await callTool("stack_scale", { name: c.name, service: s.service, replicas: 0 }, org);
          } finally {
            await refresh();
          }
          toast.success(`${env} stopping`);
        }}
      />
    </>
  );
}

type Doomed = { name: string; kind: "app" | "compose"; env: string };

type DryRun = { volumes: string[]; volumes_kept: { name: string; reason: string }[] };

/** What a delete takes with it, so the confirmation names it, and the
 * choice to delete the data in their volumes too (kept by default). */
function DoomedServices({
  org,
  tool,
  args,
  services,
  admin,
  wipe,
  onWipe,
}: {
  org: string;
  tool: "project_delete" | "environment_delete";
  args: Record<string, string>;
  services: Doomed[];
  admin: boolean;
  wipe: boolean;
  onWipe: (v: boolean) => void;
}) {
  // The server knows which volumes go: the ones these stacks made that no other stack uses.
  const plan = useQuery({
    queryKey: ["tool", tool, "dry_run", org, args],
    queryFn: () => callTool<DryRun>(tool, { ...args, force: true, volumes: true, dry_run: true }, org),
    enabled: services.length > 0,
  });
  if (!services.length) return null;
  const envs = new Set(services.map((s) => s.env)).size;
  const volumes = plan.data?.volumes ?? [];
  const shared = plan.data?.volumes_kept ?? [];
  return (
    <div className="grid gap-3 text-sm">
      <div className="grid gap-2">
        <p className="text-muted-foreground">
          {services.length} service{services.length === 1 ? "" : "s"} will be deleted:
        </p>
        <ul className="max-h-48 divide-y overflow-y-auto rounded-md border">
          {services.map((s) => (
            <li key={`${s.env}/${s.kind}/${s.name}`} className="flex items-center gap-2 px-3 py-1.5">
              {s.kind === "compose" ? <Layers className="size-3.5 shrink-0 text-muted-foreground" /> : <Box className="size-3.5 shrink-0 text-muted-foreground" />}
              <span className="truncate font-mono text-xs">{s.name}</span>
              {envs > 1 && <span className="ml-auto shrink-0 text-xs text-muted-foreground">{s.env}</span>}
            </li>
          ))}
        </ul>
      </div>
      {plan.isLoading ? (
        <Skeleton className="h-14 w-full" />
      ) : volumes.length > 0 && !admin ? (
        <p className="text-xs text-muted-foreground">
          Their data is kept: <span className="font-mono">{volumes.join(", ")}</span>. Only org admins and owners can delete volumes.
        </p>
      ) : volumes.length > 0 ? (
        <label className="flex items-start gap-3 rounded-md border p-3">
          <input
            type="checkbox"
            className="mt-0.5 size-4 accent-destructive"
            checked={wipe}
            onChange={(e) => onWipe(e.target.checked)}
          />
          <span className="min-w-0">
            Also delete their data
            <span className="block text-xs text-muted-foreground">
              {wipe ? "Deleted for good: " : "Kept unless you tick this: "}
              <span className="font-mono">{volumes.join(", ")}</span>.
              {!wipe && " A new service with the same name picks the old data up again."}
            </span>
            {shared.length > 0 && (
              <span className="block text-xs text-muted-foreground">
                Kept either way, other stacks use them: <span className="font-mono">{shared.map((v) => v.name).join(", ")}</span>.
              </span>
            )}
          </span>
        </label>
      ) : (
        <p className="text-xs text-muted-foreground">They have no named volumes.</p>
      )}
    </div>
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
  starting: "info",
  stopping: "info",
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
            <ToneBadge tone={HEALTH_TONE[h]} pulse={HEALTH_TONE[h] === "busy"}>
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
  starting: "info",
  updating: "info",
  stopping: "info",
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
