// A compose stack's Terminal tab: a shell in one replica of one service,
// opened by instance name (GET /orgs/<org>/api/v1/terminal?instance=).
// Loaded on demand: xterm is the biggest thing on the page.
import { TerminalSquare } from "lucide-react";
import { useState } from "react";
import { useSearchParams } from "react-router";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/apps/components";
import { TerminalPane } from "@/apps/terminal-pane";
import { ServicePicker, type StackServices, useServicePick } from "./stack-tabs";

/** The terminal websocket for one instance of the org (a stack service's replica). */
export function instanceTerminalUrl(loc: { protocol: string; host: string }, org: string, instance: string, cols: number, rows: number): string {
  const q = new URLSearchParams({ instance, cols: String(cols), rows: String(rows) });
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/orgs/${encodeURIComponent(org)}/api/v1/terminal?${q}`;
}

export function StackTerminalTab({ org, services, status, loading }: { org: string; services: string[]; status: StackServices | undefined; loading: boolean }) {
  const [service, pick] = useServicePick(services);
  // `&replica=N` opens it on that replica (the General tab's Terminal).
  const [params] = useSearchParams();
  const [slot, setSlot] = useState(() => (/^\d+$/.test(params.get("replica") ?? "") ? params.get("replica")! : "auto"));
  const svc = status?.find((s) => s.service === service);

  if (loading) return <Skeleton className="h-[min(60svh,32rem)] rounded-xl" />;
  const slots = [...(svc?.instances ?? [])].toSorted((a, b) => a.slot - b.slot);
  // "Any" is the first running replica in rotation, as an app's terminal picks.
  const auto = slots.filter((i) => i.status === "Running").toSorted((a, b) => Number(b.in_rotation) - Number(a.in_rotation))[0];
  const inst = slot === "auto" ? auto : slots.find((i) => String(i.slot) === slot);
  return (
    <div className="grid gap-4">
      <ServicePicker
        services={services}
        value={service}
        onChange={(s) => {
          setSlot("auto");
          pick(s);
        }}
      />
      {!svc || svc.replicas === 0 || !inst ? (
        <Card className="py-0">
          <EmptyState icon={TerminalSquare} title="Not running">
            Deploy the stack, or scale <span className="font-mono">{service}</span> up, to open a shell in one of its replicas.
          </EmptyState>
        </Card>
      ) : (
        <TerminalPane
          key={`${service}-${inst.name}`}
          url={(cols, rows) => instanceTerminalUrl(window.location, org, inst.name, cols, rows)}
          title={service}
          detail={slot === "auto" ? "any replica" : `replica ${slot}`}
          hint="A login shell (bash, else sh) in the replica, as root."
          idleText={`Open a shell in ${service}.`}
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
      )}
    </div>
  );
}
