// The org's compose stacks, under the projects: stacks written as a file
// rather than rendered from apps. They live beside projects, not in them: a
// compose file has no project or environment.
import { Layers, Plus } from "lucide-react";
import { Link } from "react-router";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { relativeTime } from "@/lib/format";
import type { Project } from "@/apps/api";
import { ToneBadge } from "@/apps/components";
import { HEALTH_LABEL, HEALTH_TONE, stackHealth, useStackList } from "@/apps/health";
import { appStackNames, isComposeStack } from "./api";

export function ComposeStacksSection({ org, writer, projects }: { org: string; writer: boolean; projects: Project[] }) {
  const stacks = useStackList();
  const owned = appStackNames(projects);
  const mine = (stacks.data?.stacks ?? []).filter((s) => s.org === org && isComposeStack(s.name, owned)).toSorted((a, b) => a.name.localeCompare(b.name));
  const o = encodeURIComponent(org);
  return (
    <section id="compose-stacks" className="mt-10 scroll-mt-20" aria-labelledby="compose-stacks-title">
      <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
        <div className="space-y-1">
          <h2 id="compose-stacks-title" className="font-display text-lg font-semibold tracking-tight">
            Compose stacks
          </h2>
          <p className="max-w-2xl text-[13px] leading-relaxed text-muted-foreground">
            Stacks you write as a compose file, deployed whole, with no project around them. Apps in a project run as stacks too; those stay with their project.
          </p>
        </div>
        {writer && (
          <Button asChild variant="outline">
            <Link to={`/orgs/${o}/stacks/new`}>
              <Plus />
              New compose stack
            </Link>
          </Button>
        )}
      </div>
      {mine.length === 0 ? (
        <Card className="items-center gap-1 border-dashed px-5 py-8 text-center text-sm text-muted-foreground shadow-none">
          <Layers className="mb-1 size-5" />
          No compose stacks in {org}.
        </Card>
      ) : (
        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          {mine.map((s) => {
            const h = stackHealth(s);
            return (
              <Link
                key={s.name}
                to={`/orgs/${o}/stacks/${encodeURIComponent(s.name)}`}
                className="group rounded-xl focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
              >
                <Card className="gap-2 px-4 py-3.5 transition-[border-color,box-shadow] group-hover:border-foreground/20 group-hover:shadow-md">
                  <div className="flex items-center justify-between gap-2">
                    <span className="flex min-w-0 items-center gap-2">
                      <Layers className="size-4 shrink-0 text-muted-foreground" />
                      <span className="truncate font-mono text-sm font-semibold">{s.name}</span>
                    </span>
                    <ToneBadge tone={HEALTH_TONE[h]}>{HEALTH_LABEL[h]}</ToneBadge>
                  </div>
                  <p className="truncate text-xs text-muted-foreground">
                    {s.services.length} service{s.services.length === 1 ? "" : "s"}: {s.services.map((x) => x.service).join(", ")} · deployed {relativeTime(s.deployed_at)}
                  </p>
                </Card>
              </Link>
            );
          })}
        </div>
      )}
    </section>
  );
}
