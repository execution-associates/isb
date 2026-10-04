// The org overview's first card: the workspace at a glance (status, live
// sessions, CPU and memory, last activity, sandboxes), or the way to make
// one.
import { ArrowRight, SquareTerminal } from "lucide-react";
import { Link } from "react-router";
import { AreaChart } from "@/apps/components";
import { bytes, percent } from "@/apps/util";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { useWorkspace } from "./api";
import { sessionsText } from "./resources-tab";
import { statusTone } from "./util";

export function WorkspaceCard({ org, admin }: { org: string; admin: boolean }) {
  const q = useWorkspace(org);
  const to = `/orgs/${encodeURIComponent(org)}/workspace`;
  if (q.isLoading) return <Skeleton className="mb-6 h-28 rounded-xl" />;
  // An older daemon, or no access: the overview goes on without it.
  if (q.error || !q.data) return null;
  const ws = q.data.workspace;
  if (!ws) {
    return (
      <Card className="mb-6 flex-row flex-wrap items-center gap-4 px-5 py-4">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-xl border border-dashed bg-muted/40">
          <SquareTerminal className="size-5 text-muted-foreground" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-[15px] font-semibold tracking-tight">No workspace yet</div>
          <p className="text-[13px] text-muted-foreground">
            {admin ? `Give ${org} its long-lived machine: a home that survives rebuilds, and an org token for the agents that live there.` : `${org}'s admins create its workspace, the machine where its people and agents work.`}
          </p>
        </div>
        {admin && (
          <Button asChild>
            <Link to={to}>Create the workspace</Link>
          </Button>
        )}
      </Card>
    );
  }
  const r = ws.resources;
  return (
    <Link to={to} className="group mb-6 block rounded-xl focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none">
      <Card className="gap-0 py-0 transition-colors group-hover:border-foreground/20">
        <div className="flex flex-wrap items-center gap-3 px-5 pt-4">
          <span className="flex size-10 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
            <SquareTerminal className="size-5 text-muted-foreground" />
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <span className="truncate text-[15px] font-semibold tracking-tight">Workspace · {ws.name}</span>
              <StatusBadge tone={statusTone(ws.status)} pulse={ws.status === "Running"}>
                {ws.status}
              </StatusBadge>
            </div>
            <div className="truncate font-mono text-xs text-muted-foreground">{ws.image}</div>
          </div>
          <ArrowRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
        </div>
        <dl className="grid grid-cols-2 gap-x-6 gap-y-3 px-5 pt-4 pb-4 text-sm sm:grid-cols-5">
          <Stat k="Sessions" v={sessionsText(ws)} />
          <Stat k="CPU" v={percent(r.cpu_pct)} />
          <Stat k="Memory" v={bytes(r.mem_bytes)} />
          <Stat k="Last activity" v={ws.last_activity ? relativeTime(ws.last_activity) : "–"} />
          <Stat k="Sandboxes" v={String(ws.sandboxes)} />
        </dl>
        {r.cpu_history.length > 1 && <AreaChart values={r.cpu_history} label="Workspace CPU" className="h-10 rounded-b-xl" />}
      </Card>
    </Link>
  );
}

function Stat({ k, v }: { k: string; v: string }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{k}</dt>
      <dd className="mt-0.5 truncate tabular-nums">{v}</dd>
    </div>
  );
}
