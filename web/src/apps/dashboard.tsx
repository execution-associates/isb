// The org overview's projects summary and recent deployments.
import { ArrowRight, FolderKanban, History, Plus } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { useApps, useLatestDeployments, useProjects } from "./api";
import { DeploymentBadge, Dot, EmptyState } from "./components";
import { HEALTH_LABEL, HEALTH_TONE, projectHealth, useStackList } from "./health";
import { useOrgLive } from "./live";
import { NewProjectDialog } from "./project-dialogs";

export function OrgAppsOverview({ org }: { org: string }) {
  const projects = useProjects(org);
  const apps = useApps(org);
  const stacks = useStackList();
  useOrgLive(org);
  const latest = useLatestDeployments(
    org,
    (apps.data ?? []).map((a) => a.name),
  );
  const [open, setOpen] = useState(false);
  const o = encodeURIComponent(org);
  const recent = [...latest.values()]
    .flat()
    .sort((a, b) => b.created_at - a.created_at)
    .slice(0, 8);
  const appInfo = new Map((apps.data ?? []).map((a) => [a.name, a]));

  return (
    <div className="mt-6 grid items-start gap-6 xl:grid-cols-2">
      <Card className="gap-0 overflow-hidden py-0">
        <CardHeader className="flex flex-row items-center justify-between border-b px-5 py-3 [.border-b]:pb-3">
          <CardTitle className="text-base">Projects</CardTitle>
          <Button asChild variant="ghost" size="sm">
            <Link to={`/orgs/${o}/projects`}>
              All projects
              <ArrowRight />
            </Link>
          </Button>
        </CardHeader>
        {projects.isLoading ? (
          <div className="space-y-2 p-5">
            <Skeleton className="h-8" />
            <Skeleton className="h-8" />
          </div>
        ) : (projects.data ?? []).length === 0 ? (
          <EmptyState
            icon={FolderKanban}
            title="No projects yet"
            action={
              <Button size="sm" onClick={() => setOpen(true)}>
                <Plus />
                New project
              </Button>
            }
          >
            Projects hold your apps: an image or a git repository, deployed with a click.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {(projects.data ?? []).slice(0, 6).map((p) => {
              const h = projectHealth(p, stacks.data?.stacks ?? [], org);
              const n = p.environments.reduce((k, e) => k + e.apps.length, 0);
              return (
                <li key={p.name}>
                  <Link
                    to={`/orgs/${o}/projects/${p.name}/${p.environments[0]?.name ?? ""}`}
                    className="flex items-center gap-3 px-5 py-3 text-sm transition-colors hover:bg-muted/40"
                  >
                    <Dot tone={HEALTH_TONE[h]} title={HEALTH_LABEL[h]} />
                    <span className="min-w-0 flex-1 truncate font-medium">{p.name}</span>
                    <span className="shrink-0 text-xs text-muted-foreground">
                      {p.environments.length} env · {n} app{n === 1 ? "" : "s"}
                    </span>
                  </Link>
                </li>
              );
            })}
          </ul>
        )}
      </Card>
      <Card className="gap-0 overflow-hidden py-0">
        <CardHeader className="border-b px-5 py-3 [.border-b]:pb-3">
          <CardTitle className="flex h-8 items-center text-base">Recent deployments</CardTitle>
        </CardHeader>
        {recent.length === 0 ? (
          <EmptyState icon={History} title="Nothing deployed yet">
            Deployments of every app in {org} show up here.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {recent.map((d) => {
              const a = appInfo.get(d.app);
              return (
                <li key={`${d.app}-${d.id}`}>
                  <Link
                    to={`/orgs/${o}/apps/${d.app}/deployments/${d.id}`}
                    className="flex items-center gap-3 px-5 py-3 text-sm transition-colors hover:bg-muted/40"
                  >
                    <DeploymentBadge status={d.status} />
                    <span className="min-w-0 flex-1 truncate">
                      <span className="font-medium">{d.app}</span>
                      <span className="text-muted-foreground"> #{d.id}</span>
                      {a && <span className="hidden text-muted-foreground sm:inline"> · {a.project}/{a.environment}</span>}
                    </span>
                    <span className="shrink-0 text-xs text-muted-foreground">{relativeTime(d.created_at / 1000)}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        )}
      </Card>
      <NewProjectDialog org={org} open={open} onOpenChange={setOpen} />
    </div>
  );
}
