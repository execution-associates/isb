// A server being added (over SSH, or an org's dedicated VM): its steps and
// its log, polled from server_provision_get until it ends.
import { useQuery } from "@tanstack/react-query";
import { Check, Circle, Loader2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { callTool, type ProvisionView } from "@/api/tools";
import { LogView } from "@/apps/log-view";
import { FormError } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { errorMessage } from "@/lib/messages";
import { duration, STEP_TONE } from "@/lib/servers";
import { TONE_TEXT } from "@/lib/status";
import { cn } from "@/lib/utils";

/** Follow `name`'s provisioning; polls while it runs. */
export function useProvision(name: string | null, initial?: ProvisionView) {
  return useQuery({
    queryKey: ["tool", "server_provision_get", name],
    queryFn: () => callTool<ProvisionView>("server_provision_get", { name: name! }),
    enabled: !!name,
    initialData: initial,
    refetchInterval: (q) => (q.state.data?.state === "running" || !q.state.data ? 1500 : false),
  });
}

/** Now, in unix seconds, ticking while `live`. */
function useNow(live: boolean): number {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, [live]);
  return now;
}

export function ProvisionStateBadge({ state }: { state: ProvisionView["state"] }) {
  return state === "running" ? (
    <StatusBadge tone="info" pulse>
      Running
    </StatusBadge>
  ) : state === "done" ? (
    <StatusBadge tone="success">Done</StatusBadge>
  ) : (
    <StatusBadge tone="danger">Failed</StatusBadge>
  );
}

export function ProvisionProgress({ p, error, className }: { p: ProvisionView | undefined; error?: unknown; className?: string }) {
  const live = p?.state === "running";
  const now = useNow(live);
  if (!p) {
    return error ? (
      <FormError>{errorMessage(error)}</FormError>
    ) : (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" /> Starting…
      </div>
    );
  }
  return (
    <div className={cn("grid min-w-0 gap-4", className)}>
      <ol className="grid gap-1.5" aria-label="Steps">
        {p.steps.map((s) => {
          const Icon = s.state === "done" ? Check : s.state === "failed" ? X : s.state === "running" ? Loader2 : Circle;
          const took = s.started_at ? duration(s.started_at, s.finished_at ?? (s.state === "running" ? now : null)) : "";
          return (
            <li key={s.id} className="flex min-w-0 items-center gap-2.5 text-sm">
              <Icon
                aria-hidden
                className={cn(
                  "size-4 shrink-0",
                  TONE_TEXT[STEP_TONE[s.state]],
                  s.state === "running" && "animate-spin",
                  s.state === "pending" && "size-3 opacity-50",
                )}
              />
              <span className={cn("min-w-0 flex-1 truncate", s.state === "pending" && "text-muted-foreground")}>{s.title}</span>
              <span className="sr-only">{s.state}</span>
              {took && <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{took}</span>}
            </li>
          );
        })}
      </ol>
      {p.state === "failed" && p.error && <FormError title="It stopped here">{p.error}</FormError>}
      <LogView
        lines={p.log}
        firstLine={(p.log_start ?? 0) + 1}
        live={live}
        title="Log"
        status={<ProvisionStateBadge state={p.state} />}
        height="max-h-64"
        filename={`${p.name}-provision.txt`}
        empty={<span className="text-muted-foreground">Nothing logged yet.</span>}
      />
    </div>
  );
}
