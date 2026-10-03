// The Terminal tab: a shell in one of the app's replicas, xterm.js over the
// daemon's websocket (GET /orgs/<org>/api/v1/terminal, docs/reference/http-api.md#the-web-terminal).
// Loaded on demand: xterm is the biggest thing on this page.
import { TerminalSquare } from "lucide-react";
import { useState } from "react";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { type App, serviceOf, useStack } from "./api";
import { EmptyState } from "./components";
import { TerminalPane } from "./terminal-pane";
import { terminalUrl } from "./util";

export default function TerminalTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const [slot, setSlot] = useState("auto");

  if (stack.isLoading) return <Skeleton className="h-[min(60svh,32rem)] rounded-xl" />;
  if (!svc || svc.replicas === 0) {
    return (
      <Card className="py-0">
        <EmptyState icon={TerminalSquare} title="Not running">
          Deploy or start the app to open a shell in one of its replicas.
        </EmptyState>
      </Card>
    );
  }
  const slots = [...svc.instances].toSorted((a, b) => a.slot - b.slot);
  return (
    <TerminalPane
      url={(cols, rows) => terminalUrl(window.location, org, app.name, slot, cols, rows)}
      title={app.name}
      detail={slot === "auto" ? "any replica" : `replica ${slot}`}
      hint="A login shell (bash, else sh) as the image's user."
      idleText={`Open a shell in ${app.name}.`}
      controls={(live) => (
        <div className="flex items-center gap-2">
          <Label htmlFor="term-replica" className="text-xs font-normal text-muted-foreground">
            Replica
          </Label>
          <Select value={slot} onValueChange={setSlot} disabled={live}>
            <SelectTrigger id="term-replica" className="w-52">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">Any running replica</SelectItem>
              {slots.map((i) => (
                <SelectItem key={i.name} value={String(i.slot)}>
                  Replica {i.slot} ({i.status.toLowerCase()})
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}
    />
  );
}
