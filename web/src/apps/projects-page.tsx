// /orgs/:org/projects: the org's projects as cards.
import { Boxes, Clock, Database, FolderKanban, LayoutTemplate, Plus } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import { type Deployment, type Project, useApps, useLatestDeployments, useProjects } from "./api";
import { Dot, EmptyState, QueryError, ToneBadge } from "./components";
import { HEALTH_LABEL, HEALTH_TONE, projectHealth, stackHealth, useStackList } from "./health";
import { useOrgLive } from "./live";
import { NewProjectDialog } from "./project-dialogs";
import { ComposeStacksSection } from "@/stacks/stack-list";

function lastDeploy(p: Project, latest: Map<string, Deployment[]>): Deployment | undefined {
  let best: Deployment | undefined;
  for (const e of p.environments)
    for (const a of e.apps) {
      const d = latest.get(a)?.[0];
      if (d && (!best || d.created_at > best.created_at)) best = d;
    }
  return best;
}

export function ProjectsPage() {
  const { org = "" } = useParams();
  const projects = useProjects(org);
  const apps = useApps(org);
  const stacks = useStackList();
  useOrgLive(org);
  const latest = useLatestDeployments(
    org,
    (apps.data ?? []).map((a) => a.name),
  );
  const [open, setOpen] = useState(false);
  const writer = canWrite(useMe().data!, org);
  const list = projects.data ?? [];
  const all = stacks.data?.stacks ?? [];
  const o = encodeURIComponent(org);
  const dbs = new Set((apps.data ?? []).filter((a) => "database" in a.source).map((a) => a.name));

  return (
    <>
      <PageHeader
        title="Projects"
        description="Each project holds environments, and each environment runs its apps side by side."
        actions={
          writer && (
            <Button onClick={() => setOpen(true)}>
              <Plus />
              New project
            </Button>
          )
        }
      />
      {projects.isLoading ? (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <Card key={i} className="gap-4 px-5 py-5">
              <div className="flex items-center gap-3">
                <Skeleton className="size-9 rounded-lg" />
                <div className="flex-1 space-y-1.5">
                  <Skeleton className="h-4 w-32" />
                  <Skeleton className="h-3 w-48" />
                </div>
              </div>
              <Skeleton className="h-6 w-40" />
              <Skeleton className="h-3 w-full" />
            </Card>
          ))}
        </div>
      ) : projects.error ? (
        <QueryError error={projects.error} />
      ) : list.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={FolderKanban}
            title="No projects yet"
            action={
              writer && (
                <>
                  <Button onClick={() => setOpen(true)}>
                    <Plus />
                    New project
                  </Button>
                  <Button asChild variant="outline">
                    <Link to={`/orgs/${o}/templates`}>
                      <LayoutTemplate />
                      Deploy a template
                    </Link>
                  </Button>
                </>
              )
            }
          >
            A project groups an app's environments, such as production and staging. Create one, then add apps from an image, a git
            repository or a database engine.
          </EmptyState>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {list.map((p) => {
            const names = p.environments.flatMap((e) => e.apps);
            const nDb = names.filter((a) => dbs.has(a)).length;
            const nApps = names.length - nDb;
            const h = projectHealth(p, all, org);
            const last = lastDeploy(p, latest);
            return (
              <Link
                key={p.name}
                to={`/orgs/${o}/projects/${p.name}/${p.environments[0]?.name ?? ""}`}
                className="group animate-fade-up rounded-xl focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
              >
                <Card className="h-full gap-0 px-5 py-5 transition-[border-color,box-shadow] group-hover:border-foreground/20 group-hover:shadow-md">
                  <div className="flex items-start gap-3">
                    <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-gradient-to-b from-muted/30 to-muted">
                      <FolderKanban className="size-4 text-muted-foreground" />
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center justify-between gap-2">
                        <h2 className="truncate text-[15px] font-semibold tracking-tight">{p.name}</h2>
                        <ToneBadge tone={HEALTH_TONE[h]}>{HEALTH_LABEL[h]}</ToneBadge>
                      </div>
                      <p className="mt-0.5 line-clamp-2 text-[13px] text-muted-foreground">{p.description || "No description"}</p>
                    </div>
                  </div>
                  <div className="mt-4 flex flex-wrap gap-1.5">
                    {p.environments.map((e) => {
                      const eh = stackHealth(all.find((s) => s.org === org && s.name === e.stack));
                      return (
                        <span key={e.name} className="inline-flex h-6 items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs font-medium">
                          <Dot tone={HEALTH_TONE[eh]} title={HEALTH_LABEL[eh]} />
                          {e.name}
                          <span className="font-normal text-muted-foreground tabular-nums">{e.apps.length}</span>
                        </span>
                      );
                    })}
                  </div>
                  <div className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-1 border-t pt-3 text-xs text-muted-foreground">
                    <span className="inline-flex items-center gap-1.5 tabular-nums">
                      <Boxes className="size-3.5" />
                      {nApps} app{nApps === 1 ? "" : "s"}
                    </span>
                    {nDb > 0 && (
                      <span className="inline-flex items-center gap-1.5 tabular-nums">
                        <Database className="size-3.5" />
                        {nDb} database{nDb === 1 ? "" : "s"}
                      </span>
                    )}
                    <span
                      className="ml-auto inline-flex items-center gap-1.5 tabular-nums"
                      title={last ? `Last deployed ${new Date(last.created_at).toLocaleString()}` : "Never deployed"}
                    >
                      <Clock className="size-3.5" />
                      {last ? relativeTime(last.created_at / 1000) : "Never deployed"}
                    </span>
                  </div>
                </Card>
              </Link>
            );
          })}
          {writer && (
            <button
              type="button"
              onClick={() => setOpen(true)}
              className="flex min-h-40 flex-col items-center justify-center gap-2 rounded-xl border border-dashed text-sm text-muted-foreground transition-colors hover:border-foreground/25 hover:bg-muted/30 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
            >
              <span className="flex size-9 items-center justify-center rounded-lg border bg-background">
                <Plus className="size-4" />
              </span>
              New project
            </button>
          )}
        </div>
      )}
      <ComposeStacksSection org={org} writer={writer} projects={list} />
      <NewProjectDialog org={org} open={open} onOpenChange={setOpen} />
    </>
  );
}
