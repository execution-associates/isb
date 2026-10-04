// The Logs tab: the running replicas' recent output (stack_logs: the
// supervised command's journal, or an OCI image's console).
import { useQuery } from "@tanstack/react-query";
import { RefreshCw, ScrollText } from "lucide-react";
import { useState } from "react";
import { callTool } from "@/api/tools";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import type { Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type App, type InstanceDetail, keys, serviceOf, useStack } from "./api";
import { EmptyState, QueryError } from "./components";
import { LogView } from "./log-view";
import { Segmented } from "./segmented";

function replicaTone(i: InstanceDetail | undefined): Tone {
  if (!i) return "muted";
  if (i.health === "unhealthy") return "danger";
  if (i.status !== "Running") return "warning";
  if (i.health === "starting") return "info";
  return "success";
}

export function LogsTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const [slot, setSlot] = useState<string>("all");
  const [lines, setLines] = useState("200");
  const [auto, setAuto] = useState(true);
  const logs = useQuery({
    queryKey: [...keys.org(org), "logs", app.name, slot, lines],
    enabled: !!svc,
    refetchInterval: auto ? 5000 : false,
    queryFn: () =>
      callTool<{ logs: Record<string, string> }>(
        "stack_logs",
        { name: app.stack, service: app.name, lines: Number(lines), ...(slot === "all" ? {} : { slot: Number(slot) }) },
        org,
      ).then((r) => r.logs),
  });

  if (stack.isLoading) {
    return (
      <div className="grid gap-4">
        <Skeleton className="h-9 w-full max-w-md" />
        <Skeleton className="h-96 rounded-xl" />
      </div>
    );
  }
  if (!svc) {
    return (
      <Card className="py-0">
        <EmptyState icon={ScrollText} title="Not running">
          Deploy the app and its replicas' output shows up here. Build and rollout output is under Deployments.
        </EmptyState>
      </Card>
    );
  }
  const instances = Object.entries(logs.data ?? {});
  const slots = [...svc.instances].toSorted((a, b) => a.slot - b.slot);
  const byName = new Map(slots.map((i) => [i.name, i]));
  const replicaOptions = [
    { value: "all", label: "All replicas" },
    ...slots.map((i) => ({
      value: String(i.slot),
      label: (
        <>
          <StatusDot tone={replicaTone(i)} className="size-1.5" />
          Replica {i.slot}
        </>
      ),
    })),
  ];
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        {slots.length <= 6 ? (
          <Segmented value={slot} onChange={setSlot} options={replicaOptions} label="Replica" />
        ) : (
          <Select value={slot} onValueChange={setSlot}>
            <SelectTrigger aria-label="Replica" className="w-44">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All replicas</SelectItem>
              {slots.map((i) => (
                <SelectItem key={i.name} value={String(i.slot)}>
                  Replica {i.slot}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <div className="flex items-center gap-2">
          <Label htmlFor="log-lines" className="text-xs font-normal text-muted-foreground">
            Last
          </Label>
          <Select value={lines} onValueChange={setLines}>
            <SelectTrigger id="log-lines" size="sm" className="w-24 tabular-nums">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {["100", "200", "1000", "5000"].map((n) => (
                <SelectItem key={n} value={n}>
                  {Number(n).toLocaleString()}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <span className="text-xs text-muted-foreground">lines</span>
        </div>
        <div className="ml-auto flex items-center gap-3">
          <div className="flex items-center gap-2">
            <Switch id="log-auto" checked={auto} onCheckedChange={setAuto} />
            <Label htmlFor="log-auto" className="text-[13px] font-normal">
              Auto-refresh
            </Label>
          </div>
          <Button variant="outline" size="sm" onClick={() => logs.refetch()} disabled={logs.isFetching} aria-label="Refresh now">
            <RefreshCw className={cn(logs.isFetching && "animate-spin")} />
            <span className="hidden sm:inline">Refresh</span>
          </Button>
        </div>
      </div>
      {logs.error ? <QueryError error={logs.error} /> : null}
      {logs.isLoading ? (
        <LogView lines={[]} live title={<span className="font-mono">{app.name}</span>} empty="Loading output..." />
      ) : instances.length === 0 ? (
        <Card className="py-0">
          <EmptyState icon={ScrollText} title="No replicas to read from" compact>
            The replica may have been replaced. Pick another, or refresh.
          </EmptyState>
        </Card>
      ) : (
        instances.map(([name, text]) => {
          const inst = byName.get(name);
          return (
            <LogView
              key={name}
              lines={text.trim() ? text.replace(/\n$/, "").split("\n") : []}
              live={auto}
              filename={`${name}.log`}
              title={
                <span className="inline-flex items-center gap-2">
                  {inst ? `Replica ${inst.slot}` : "Replica"}
                  <span className="font-mono font-normal text-zinc-500">{name}</span>
                </span>
              }
              status={inst && <StatusDot tone={replicaTone(inst)} pulse={auto} className="ml-1" title={inst.status} />}
              empty="No output yet: the app has not written to stdout or stderr."
            />
          );
        })
      )}
    </div>
  );
}
