// The org overview: stat tiles, projects with their health, recent
// deployments and the activity feed.
import { useQueries, useQuery } from "@tanstack/react-query";
import {
  ArrowRight,
  Boxes,
  ChevronRight,
  Code2,
  Cpu,
  FolderKanban,
  History,
  LayoutTemplate,
  MousePointerClick,
  Plus,
  Rocket,
  Terminal,
  Webhook,
} from "lucide-react";
import { type ReactNode, useState } from "react";
import { Link } from "react-router";
import { callTool, type OrgView } from "@/api/tools";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import { DEPLOYMENT_TONE, inProgress, type Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type App, type AppState, appState, type Deployment, keys, type Project, serviceOf, type StackDetail, useApps, useProjects } from "./api";
import { DeploymentBadge, Dot, EmptyState, QueryError, ToneBadge } from "./components";
import { HEALTH_LABEL, HEALTH_TONE, parseSize, projectHealth, usageOf, useOrgOverview } from "./health";
import { OrgActivity, actorLabel } from "./overview-activity";
import { NewProjectDialog } from "./project-dialogs";
import { bytes, duration, shortSha } from "./util";

const DAY = 86_400_000;

/** The last `limit` deployments of each app (shares useDeployments' cache at 30). */
export function useAppDeployments(org: string, apps: string[], limit = 30) {
  return useQueries({
    queries: apps.map((a) => ({
      queryKey: [...keys.deployments(org, a), limit],
      queryFn: () => callTool<{ current: number | null; deployments: Deployment[] }>("app_deployments", { name: a, limit }, org),
    })),
    combine: (rs) => {
      const m = new Map<string, Deployment[]>();
      rs.forEach((r, i) => m.set(apps[i], r.data?.deployments ?? []));
      return { byApp: m, loading: rs.some((r) => r.isLoading) };
    },
  });
}

/** Each app's state, from its stack's status and its latest deployment. */
export function appStates(apps: App[], stacks: StackDetail[], deps: Map<string, Deployment[]>): Map<string, AppState> {
  const m = new Map<string, AppState>();
  for (const a of apps) {
    const s = stacks.find((x) => x.name === a.stack);
    m.set(a.name, appState(serviceOf(s, a.name), deps.get(a.name)?.[0]));
  }
  return m;
}

const UP: AppState[] = ["running", "degraded", "updating"];
const TROUBLE: AppState[] = ["failing", "failed", "degraded"];

export function OrgDashboard({ org }: { org: string }) {
  const me = useMe().data!;
  const writer = canWrite(me, org);
  const projects = useProjects(org);
  const apps = useApps(org);
  const overview = useOrgOverview(org);
  const info = useQuery({
    queryKey: ["tool", "org_get", org],
    queryFn: () => callTool<OrgView>("org_get", {}, org),
    staleTime: 60_000,
  });
  const appList = apps.data ?? [];
  const deps = useAppDeployments(
    org,
    appList.map((a) => a.name),
  );
  const [newProject, setNewProject] = useState(false);
  const stacks = overview.data ?? [];
  const states = appStates(appList, stacks, deps.byApp);
  const list = projects.data ?? [];

  if (projects.error) return <QueryError error={projects.error} />;

  const fresh = !projects.isLoading && list.length === 0;

  return (
    <>
      {fresh ? (
        <Welcome org={org} writer={writer} onNew={() => setNewProject(true)} />
      ) : (
        <StatTiles
          loading={projects.isLoading || apps.isLoading}
          apps={appList}
          states={states}
          deps={deps.byApp}
          depsLoading={deps.loading}
          projects={list}
          stacks={stacks}
          usageLoading={overview.isLoading}
          info={info.data}
        />
      )}

      {fresh ? (
        <div className="mt-6 grid gap-6">
          <OrgActivity org={org} />
        </div>
      ) : (
        <div className="mt-6 grid items-start gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
          <div className="grid min-w-0 gap-6">
            <ProjectsCard org={org} projects={list} loading={projects.isLoading} stacks={stacks} deps={deps.byApp} apps={appList} />
            <RecentDeployments org={org} apps={appList} deps={deps.byApp} loading={apps.isLoading || deps.loading} />
          </div>
          <OrgActivity org={org} className="xl:sticky xl:top-20" />
        </div>
      )}
      <NewProjectDialog org={org} open={newProject} onOpenChange={setNewProject} />
    </>
  );
}

// ---- Stat tiles ----

function Tile({ label, icon: Icon, to, children, foot }: { label: string; icon: typeof Boxes; to?: string; children: ReactNode; foot?: ReactNode }) {
  const inner = (
    <Card className={cn("h-full gap-0 px-4 py-4 sm:px-5", to && "transition-colors group-hover:border-foreground/20")}>
      <div className="flex items-center justify-between gap-2 text-xs font-medium text-muted-foreground">
        <span className="truncate">{label}</span>
        <Icon className="size-4 shrink-0 opacity-70" />
      </div>
      <div className="mt-2.5 min-w-0">{children}</div>
      {foot && <div className="mt-auto pt-2.5 text-xs text-muted-foreground">{foot}</div>}
    </Card>
  );
  return to ? (
    <Link to={to} className="group rounded-xl focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none">
      {inner}
    </Link>
  ) : (
    inner
  );
}

function Big({ children, of }: { children: ReactNode; of?: ReactNode }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <span className="text-2xl font-semibold tracking-tight tabular-nums">{children}</span>
      {of !== undefined && <span className="text-sm text-muted-foreground tabular-nums">{of}</span>}
    </div>
  );
}

function Hint({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className="inline-flex min-w-0 items-center gap-1.5">
      <StatusDot tone={tone} className="size-1.5" />
      <span className="truncate">{children}</span>
    </span>
  );
}

function StatTiles({
  loading,
  apps,
  states,
  deps,
  depsLoading,
  projects,
  stacks,
  usageLoading,
  info,
}: {
  loading: boolean;
  apps: App[];
  states: Map<string, AppState>;
  deps: Map<string, Deployment[]>;
  depsLoading: boolean;
  projects: Project[];
  stacks: StackDetail[];
  usageLoading: boolean;
  info: OrgView | undefined;
}) {
  const s = [...states.values()];
  const up = s.filter((x) => UP.includes(x)).length;
  const trouble = s.filter((x) => TROUBLE.includes(x)).length;
  const deploying = s.filter((x) => x === "deploying" || x === "updating").length;
  const now = Date.now();
  const day = [...deps.values()].flat().filter((d) => now - d.created_at < DAY);
  const failed = day.filter((d) => d.status === "failed").length;
  const envs = projects.reduce((n, p) => n + p.environments.length, 0);
  const use = usageOf(stacks);
  const cpuLimit = info?.cpus ? Number(info.cpus) : null;
  const memLimit = parseSize(info?.memory);
  const instLimit = info?.instances_limit ? Number(info.instances_limit) : null;
  const sk = <Skeleton className="h-8 w-20" />;

  return (
    <div className="grid grid-cols-2 gap-3 sm:gap-4 lg:grid-cols-4">
      <Tile
        label="Apps running"
        icon={Boxes}
        foot={
          loading ? null : apps.length === 0 ? (
            <Hint tone="muted">No apps yet</Hint>
          ) : trouble > 0 ? (
            <Hint tone="danger">{trouble} need attention</Hint>
          ) : deploying > 0 ? (
            <Hint tone="info">{deploying} deploying</Hint>
          ) : up === apps.length ? (
            <Hint tone="success">All healthy</Hint>
          ) : (
            <Hint tone="neutral">{apps.length - up} not running</Hint>
          )
        }
      >
        {loading ? sk : <Big of={`/ ${apps.length}`}>{up}</Big>}
      </Tile>
      <Tile
        label="Deploys, last 24 h"
        icon={Rocket}
        foot={
          depsLoading ? null : day.length === 0 ? (
            <Hint tone="muted">None today</Hint>
          ) : failed > 0 ? (
            <Hint tone="danger">{failed} failed</Hint>
          ) : (
            <Hint tone="success">No failures</Hint>
          )
        }
      >
        {loading || depsLoading ? sk : (
          <div className="flex items-end justify-between gap-2">
            <Big>{day.length}</Big>
            <DayBars deps={day} now={now} />
          </div>
        )}
      </Tile>
      <Tile label="Projects" icon={FolderKanban} foot={loading ? null : `${envs} environment${envs === 1 ? "" : "s"}`}>
        {loading ? sk : <Big>{projects.length}</Big>}
      </Tile>
      <Tile
        label="Resources now"
        icon={Cpu}
        foot={
          usageLoading ? null : (
            <span className="tabular-nums">
              {use.instances} instance{use.instances === 1 ? "" : "s"}
              {instLimit ? ` of ${instLimit}` : ""}
            </span>
          )
        }
      >
        {usageLoading ? (
          <div className="space-y-2.5">
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-full" />
          </div>
        ) : (
          <div className="space-y-2">
            <Meter label="CPU" value={cpuLimit ? coresLabel(use.cpu) : `${coresLabel(use.cpu)} cores`} of={cpuLimit ? `${cpuLimit} cores` : undefined} ratio={cpuLimit ? use.cpu / 100 / cpuLimit : null} />
            <Meter label="Mem" value={bytes(use.mem)} of={memLimit ? bytes(memLimit) : undefined} ratio={memLimit ? use.mem / memLimit : null} />
          </div>
        )}
      </Tile>
    </div>
  );
}

/** Percent of one core, as cores: "0.42", "<0.01", "0". */
function coresLabel(pct: number): string {
  const c = pct / 100;
  if (c === 0) return "0";
  if (c < 0.01) return "<0.01";
  return c < 10 ? c.toFixed(2) : c.toFixed(1);
}

function Meter({ label, value, of, ratio }: { label: string; value: string; of?: string; ratio: number | null }) {
  const pct = ratio === null ? null : Math.min(100, Math.max(0, ratio * 100));
  const fill = pct === null ? "" : pct > 90 ? "bg-destructive" : pct > 75 ? "bg-warning" : "bg-info";
  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className="text-muted-foreground">{label}</span>
        <span className="truncate tabular-nums">
          <span className="font-medium">{value}</span>
          {of && <span className="text-muted-foreground"> / {of}</span>}
        </span>
      </div>
      {pct !== null && (
        <div className="h-1.5 overflow-hidden rounded-full bg-muted" role="meter" aria-label={label} aria-valuenow={Math.round(pct)} aria-valuemin={0} aria-valuemax={100}>
          <div className={cn("h-full rounded-full transition-[width]", fill)} style={{ width: `${Math.max(pct, 1.5)}%` }} />
        </div>
      )}
    </div>
  );
}

/** Deploys per hour over the last day, as 24 thin bars. */
function DayBars({ deps, now }: { deps: Deployment[]; now: number }) {
  const buckets = Array.from({ length: 24 }, () => ({ n: 0, failed: false }));
  for (const d of deps) {
    const i = 23 - Math.floor((now - d.created_at) / 3_600_000);
    if (i < 0 || i > 23) continue;
    buckets[i].n++;
    if (d.status === "failed") buckets[i].failed = true;
  }
  const max = Math.max(1, ...buckets.map((b) => b.n));
  return (
    <div className="flex h-7 w-20 shrink-0 items-end gap-px sm:w-24" aria-hidden>
      {buckets.map((b, i) => (
        <span
          key={i}
          className={cn("flex-1 rounded-[1px]", b.n === 0 ? "bg-muted" : b.failed ? "bg-destructive/80" : "bg-foreground/45")}
          style={{ height: b.n === 0 ? "3px" : `${Math.max(20, (b.n / max) * 100)}%` }}
        />
      ))}
    </div>
  );
}

// ---- Projects ----

function lastDeployOf(p: Project, deps: Map<string, Deployment[]>): Deployment | undefined {
  let best: Deployment | undefined;
  for (const e of p.environments)
    for (const a of e.apps) {
      const d = deps.get(a)?.[0];
      if (d && (!best || d.created_at > best.created_at)) best = d;
    }
  return best;
}

function ProjectsCard({
  org,
  projects,
  loading,
  stacks,
  deps,
  apps,
}: {
  org: string;
  projects: Project[];
  loading: boolean;
  stacks: StackDetail[];
  deps: Map<string, Deployment[]>;
  apps: App[];
}) {
  const o = encodeURIComponent(org);
  const shown = projects.slice(0, 6);
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <CardHeader className="flex flex-row items-center justify-between border-b px-5 py-3 [.border-b]:pb-3">
        <CardTitle className="text-[15px] font-semibold tracking-tight">Projects</CardTitle>
        <Button asChild variant="ghost" size="sm" className="-mr-2 text-muted-foreground">
          <Link to={`/orgs/${o}/projects`}>
            {projects.length > shown.length ? `All ${projects.length}` : "All projects"}
            <ArrowRight />
          </Link>
        </Button>
      </CardHeader>
      {loading ? (
        <RowSkeletons n={2} />
      ) : (
        <ul className="divide-y">
          {shown.map((p) => {
            const h = projectHealth(p, stacks, org);
            const n = p.environments.reduce((k, e) => k + e.apps.length, 0);
            const last = lastDeployOf(p, deps);
            const dbs = apps.filter((a) => a.project === p.name && "database" in a.source).length;
            const nc = p.environments.reduce((k, e) => k + e.compose.length, 0);
            return (
              <li key={p.name}>
                <Link
                  to={`/orgs/${o}/projects/${p.name}/${p.environments[0]?.name ?? ""}`}
                  className="flex items-center gap-3 px-5 py-3 transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none"
                >
                  <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border bg-muted/50">
                    <FolderKanban className="size-4 text-muted-foreground" />
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-2">
                      <span className="truncate text-sm font-semibold">{p.name}</span>
                      <Dot tone={HEALTH_TONE[h]} title={HEALTH_LABEL[h]} className="sm:hidden" />
                    </span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {p.environments.map((e) => e.name).join(", ")} · {n - dbs} app{n - dbs === 1 ? "" : "s"}
                      {dbs > 0 && `, ${dbs} database${dbs === 1 ? "" : "s"}`}
                      {nc > 0 && `, ${nc} compose`}
                    </span>
                  </span>
                  <span className="hidden shrink-0 text-right text-xs text-muted-foreground sm:block">
                    <ToneBadge tone={HEALTH_TONE[h]}>{HEALTH_LABEL[h]}</ToneBadge>
                  </span>
                  <span className="hidden w-28 shrink-0 text-right text-xs text-muted-foreground tabular-nums md:block">
                    {last ? relativeTime(last.created_at / 1000) : "never deployed"}
                  </span>
                  <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" />
                </Link>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

// ---- Recent deployments ----

const TRIGGER: Record<Deployment["trigger"], [typeof Boxes, string]> = {
  manual: [MousePointerClick, "Manual"],
  api: [Code2, "API"],
  webhook: [Webhook, "Git push"],
};

function triggerOf(d: Deployment): [typeof Boxes, string] {
  if (d.by.startsWith("local(")) return [Terminal, "CLI"];
  return TRIGGER[d.trigger] ?? [Rocket, d.trigger];
}

function RecentDeployments({ org, apps, deps, loading }: { org: string; apps: App[]; deps: Map<string, Deployment[]>; loading: boolean }) {
  const o = encodeURIComponent(org);
  const info = new Map(apps.map((a) => [a.name, a]));
  const recent = [...deps.values()]
    .flat()
    .toSorted((a, b) => b.created_at - a.created_at)
    .slice(0, 8);
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <CardHeader className="flex flex-row items-center justify-between border-b px-5 py-3 [.border-b]:pb-3">
        <CardTitle className="flex h-8 items-center text-[15px] font-semibold tracking-tight">Recent deployments</CardTitle>
      </CardHeader>
      {loading && recent.length === 0 ? (
        <RowSkeletons n={3} />
      ) : recent.length === 0 ? (
        <EmptyState icon={History} title="Nothing deployed yet" compact>
          Every app's deployments show up here, newest first.
        </EmptyState>
      ) : (
        <ul className="divide-y">
          {recent.map((d) => {
            const a = info.get(d.app);
            const [TIcon, tlabel] = triggerOf(d);
            const who = actorLabel(d.by);
            const took = d.finished_at && d.started_at ? duration(d.finished_at - d.started_at) : "";
            return (
              <li key={`${d.app}-${d.id}`}>
                <Link
                  to={`/orgs/${o}/apps/${d.app}/deployments/${d.id}`}
                  className="flex items-center gap-3 px-5 py-3 transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none"
                >
                  <StatusDot tone={DEPLOYMENT_TONE[d.status]} pulse={inProgress(d.status)} className="hidden sm:inline-flex" />
                  <span className="min-w-0 flex-1">
                    <span className="flex min-w-0 items-baseline gap-1.5">
                      <span className="truncate text-sm font-semibold">{d.app}</span>
                      <span className="shrink-0 font-mono text-xs text-muted-foreground">#{d.id}</span>
                      {a && (
                        <span className="hidden truncate text-xs text-muted-foreground sm:inline">
                          {a.project} / {a.environment}
                        </span>
                      )}
                    </span>
                    <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                      <TIcon className="size-3.5 shrink-0" />
                      <span className="shrink-0">{tlabel}</span>
                      {who && who !== "CLI on the server" && <span className="truncate">· {who}</span>}
                      {d.commit && (
                        <span className="hidden truncate md:inline">
                          · <span className="font-mono">{shortSha(d.commit.sha)}</span> {d.commit.message}
                        </span>
                      )}
                    </span>
                  </span>
                  <span className="flex shrink-0 flex-col items-end gap-1">
                    <DeploymentBadge status={d.status} />
                    <span className="text-xs text-muted-foreground tabular-nums" title={new Date(d.created_at).toLocaleString()}>
                      {relativeTime(d.created_at / 1000)}
                      {took && <span className="hidden sm:inline"> · {took}</span>}
                    </span>
                  </span>
                </Link>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

// ---- A brand-new org ----

function Welcome({ org, writer, onNew }: { org: string; writer: boolean; onNew: () => void }) {
  const o = encodeURIComponent(org);
  const steps: [typeof Boxes, string, string][] = [
    [FolderKanban, "Create a project", "A project holds environments such as production and staging."],
    [Boxes, "Add an app or a database", "Deploy an image, build a git repository, or start Postgres, MySQL or Redis."],
    [Rocket, "Deploy and watch it live", "Each deploy streams its log; domains get certificates on their own."],
  ];
  return (
    <Card className="animate-fade-up gap-0 overflow-hidden py-0">
      <div className="grid gap-6 px-5 py-8 sm:px-8 sm:py-10 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] lg:items-center">
        <div className="space-y-4">
          <div className="flex size-11 items-center justify-center rounded-xl border bg-gradient-to-b from-muted/40 to-muted shadow-xs">
            <FolderKanban className="size-5 text-muted-foreground" />
          </div>
          <div className="space-y-1.5">
            <h2 className="text-lg font-semibold tracking-tight">Nothing in {org} yet</h2>
            <p className="max-w-md text-sm leading-relaxed text-muted-foreground">
              {writer
                ? "Start with a project and add apps to it, or deploy a ready-made template in a few clicks."
                : "When someone in the org creates a project, its apps and their health show up here."}
            </p>
          </div>
          {writer && (
            <div className="flex flex-wrap gap-2">
              <Button onClick={onNew}>
                <Plus />
                New project
              </Button>
              <Button asChild variant="outline">
                <Link to={`/orgs/${o}/templates`}>
                  <LayoutTemplate />
                  Deploy a template
                </Link>
              </Button>
            </div>
          )}
        </div>
        <ol className="grid gap-3">
          {steps.map(([Icon, title, text], i) => (
            <li key={title} className="flex gap-3 rounded-lg border bg-muted/30 px-4 py-3">
              <span className="flex size-7 shrink-0 items-center justify-center rounded-full border bg-background text-xs font-semibold tabular-nums">{i + 1}</span>
              <span className="min-w-0">
                <span className="flex items-center gap-1.5 text-sm font-medium">
                  <Icon className="size-3.5 text-muted-foreground" />
                  {title}
                </span>
                <span className="block text-[13px] text-muted-foreground">{text}</span>
              </span>
            </li>
          ))}
        </ol>
      </div>
    </Card>
  );
}

function RowSkeletons({ n }: { n: number }) {
  return (
    <div className="divide-y">
      {Array.from({ length: n }, (_, i) => (
        <div key={i} className="flex items-center gap-3 px-5 py-3.5">
          <Skeleton className="size-8 rounded-lg" />
          <div className="flex-1 space-y-1.5">
            <Skeleton className="h-3.5 w-40" />
            <Skeleton className="h-3 w-24" />
          </div>
          <Skeleton className="h-5 w-16 rounded-full" />
        </div>
      ))}
    </div>
  );
}
