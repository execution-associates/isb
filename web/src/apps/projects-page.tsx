// /orgs/:org/projects: the org's projects as cards.
import { Boxes, FolderKanban, Layers, Plus } from "lucide-react";
import { useState } from "react";
import { Link, useParams } from "react-router";
import { PageHeader } from "@/components/app-shell";
import { Card } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { useProjects } from "./api";
import { Dot, EmptyState, LiveIndicator, QueryError, ToneBadge } from "./components";
import { HEALTH_LABEL, HEALTH_TONE, projectHealth, stackHealth, useStackList } from "./health";
import { useOrgLive } from "./live";
import { NewProjectDialog } from "./project-dialogs";

export function ProjectsPage() {
  const { org = "" } = useParams();
  const projects = useProjects(org);
  const stacks = useStackList();
  const live = useOrgLive(org);
  const [open, setOpen] = useState(false);
  const list = projects.data ?? [];
  const all = stacks.data?.stacks ?? [];

  return (
    <>
      <PageHeader
        title="Projects"
        description="Each project holds environments, and each environment runs its apps."
        actions={
          <>
            <LiveIndicator state={live} />
            <Button onClick={() => setOpen(true)}>
              <Plus />
              New project
            </Button>
          </>
        }
      />
      {projects.isLoading ? (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          <Skeleton className="h-40" />
          <Skeleton className="h-40" />
          <Skeleton className="h-40" />
        </div>
      ) : projects.error ? (
        <QueryError error={projects.error} />
      ) : list.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={FolderKanban}
            title="No projects yet"
            action={
              <Button onClick={() => setOpen(true)}>
                <Plus />
                Create a project
              </Button>
            }
          >
            A project groups an app's environments (production, staging, ...). Create one, then add apps from an image or a git
            repository.
          </EmptyState>
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {list.map((p) => {
            const apps = p.environments.reduce((n, e) => n + e.apps.length, 0);
            const h = projectHealth(p, all, org);
            return (
              <Link
                key={p.name}
                to={`/orgs/${encodeURIComponent(org)}/projects/${p.name}/${p.environments[0]?.name ?? ""}`}
                className="group rounded-xl focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
              >
                <Card className="h-full gap-4 px-5 py-5 transition-colors group-hover:border-foreground/25">
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <h2 className="truncate text-base font-semibold">{p.name}</h2>
                      <p className="line-clamp-2 text-sm text-muted-foreground">{p.description || `Created ${relativeTime(p.created_at)}`}</p>
                    </div>
                    <ToneBadge tone={HEALTH_TONE[h]}>{HEALTH_LABEL[h]}</ToneBadge>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {p.environments.map((e) => {
                      const eh = stackHealth(all.find((s) => s.org === org && s.name === e.stack));
                      return (
                        <span key={e.name} className="inline-flex items-center gap-1.5 rounded-md border bg-muted/40 px-2 py-0.5 text-xs">
                          <Dot tone={HEALTH_TONE[eh]} title={HEALTH_LABEL[eh]} />
                          {e.name}
                          <span className="text-muted-foreground">{e.apps.length}</span>
                        </span>
                      );
                    })}
                  </div>
                  <div className="mt-auto flex gap-4 text-xs text-muted-foreground">
                    <span className="inline-flex items-center gap-1.5">
                      <Layers className="size-3.5" />
                      {p.environments.length} environment{p.environments.length === 1 ? "" : "s"}
                    </span>
                    <span className="inline-flex items-center gap-1.5">
                      <Boxes className="size-3.5" />
                      {apps} app{apps === 1 ? "" : "s"}
                    </span>
                  </div>
                </Card>
              </Link>
            );
          })}
        </div>
      )}
      <NewProjectDialog org={org} open={open} onOpenChange={setOpen} />
    </>
  );
}
