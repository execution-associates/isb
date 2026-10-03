// The Logs tab: the running replicas' recent output (stack_logs: the
// supervised command's journal, or an OCI image's console).
import { useQuery } from "@tanstack/react-query";
import { RefreshCw, ScrollText } from "lucide-react";
import { useState } from "react";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { type App, keys, serviceOf, useStack } from "./api";
import { EmptyState, QueryError } from "./components";
import { LogView } from "./log-view";

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

  if (stack.isLoading) return null;
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
  const slots = [...svc.instances].sort((a, b) => a.slot - b.slot);
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1.5">
          <Label htmlFor="log-replica" className="text-xs text-muted-foreground">
            Replica
          </Label>
          <Select value={slot} onValueChange={setSlot}>
            <SelectTrigger id="log-replica" className="w-44">
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
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="log-lines" className="text-xs text-muted-foreground">
            Lines
          </Label>
          <Select value={lines} onValueChange={setLines}>
            <SelectTrigger id="log-lines" className="w-28">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {["100", "200", "1000", "5000"].map((n) => (
                <SelectItem key={n} value={n}>
                  {n}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="flex h-9 items-center gap-2">
          <Switch id="log-auto" checked={auto} onCheckedChange={setAuto} />
          <Label htmlFor="log-auto" className="font-normal">
            Refresh every 5s
          </Label>
        </div>
        <Button variant="outline" className="ml-auto" onClick={() => logs.refetch()} disabled={logs.isFetching}>
          <RefreshCw className={cn(logs.isFetching && "animate-spin")} />
          Refresh
        </Button>
      </div>
      {logs.error ? <QueryError error={logs.error} /> : null}
      {logs.isLoading ? (
        <LogView lines={[]} live={false} title="Loading..." empty="Loading..." />
      ) : instances.length === 0 ? (
        <Card className="py-0">
          <EmptyState icon={ScrollText} title="No replicas to read from" />
        </Card>
      ) : (
        instances.map(([name, text]) => (
          <LogView
            key={name}
            lines={text.trim() ? text.replace(/\n$/, "").split("\n") : []}
            live={auto}
            filename={`${name}.log`}
            title={<span className="font-mono">{name}</span>}
            empty="No output yet: the app has not written to stdout or stderr."
          />
        ))
      )}
    </div>
  );
}
