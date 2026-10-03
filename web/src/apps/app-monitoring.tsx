// The Monitoring tab: CPU and memory per replica. The daemon samples every
// 2 s and keeps 40 CPU samples per instance; memory history is gathered
// here, from each poll, while the tab is open.
import { Activity, Cpu, HeartPulse, MemoryStick, RotateCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Card } from "@/components/ui/card";
import { type App, type InstanceDetail, serviceOf, useStack } from "./api";
import { AreaChart, Dot, EmptyState, QueryError, ToneBadge } from "./components";
import { bytes, percent } from "./util";

const KEEP = 60;

export function MonitoringTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack, 3000);
  const svc = serviceOf(stack.data, app.name);
  const mem = useRef(new Map<string, number[]>());
  const [, setV] = useState(0);
  const instances = [...(svc?.instances ?? [])].sort((a, b) => a.slot - b.slot) as InstanceDetail[];

  useEffect(() => {
    if (!stack.dataUpdatedAt) return;
    for (const i of instances) {
      if (i.mem_bytes === null || i.mem_bytes === undefined) continue;
      const h = mem.current.get(i.name) ?? [];
      h.push(i.mem_bytes);
      if (h.length > KEEP) h.shift();
      mem.current.set(i.name, h);
    }
    setV((v) => v + 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stack.dataUpdatedAt]);

  if (stack.error) return <QueryError error={stack.error} />;
  if (!stack.isLoading && !svc) {
    return (
      <Card className="py-0">
        <EmptyState icon={Activity} title="Not running">
          Deploy the app to see its replicas' CPU and memory.
        </EmptyState>
      </Card>
    );
  }
  const cpuNow = instances.reduce((n, i) => n + (i.cpu_pct ?? 0), 0);
  const memNow = instances.reduce((n, i) => n + (i.mem_bytes ?? 0), 0);
  const restarts = instances.reduce((n, i) => n + i.restarts, 0);
  const memLimit = parseMemory(app.resources?.memory);

  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Stat icon={Cpu} label="CPU, all replicas" value={percent(cpuNow)} hint="100% is one core" />
        <Stat icon={MemoryStick} label="Memory, all replicas" value={bytes(memNow)} hint={memLimit ? `limit ${bytes(memLimit)} each` : "no limit set"} />
        <Stat icon={HeartPulse} label="Healthy" value={svc ? `${svc.healthy}/${svc.replicas}` : "–"} hint={svc?.state} />
        <Stat icon={RotateCw} label="Restarts" value={String(restarts)} hint="since each replica started" />
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        {instances.map((i) => {
          const m = mem.current.get(i.name) ?? [];
          const tone = i.health === "unhealthy" ? "bad" : i.status !== "Running" ? "warn" : i.health === "starting" ? "busy" : "ok";
          return (
            <Card key={i.name} className="gap-4 px-5 py-5">
              <div className="flex flex-wrap items-center gap-2">
                <Dot tone={tone} />
                <span className="font-medium">Replica {i.slot}</span>
                <span className="truncate font-mono text-xs text-muted-foreground">{i.name}</span>
                <span className="ml-auto flex gap-1.5">
                  <ToneBadge tone={i.in_rotation ? "ok" : "idle"}>{i.in_rotation ? "in rotation" : "out of rotation"}</ToneBadge>
                </span>
              </div>
              <div className="grid gap-4 sm:grid-cols-2">
                <div>
                  <div className="mb-1 flex items-baseline justify-between text-xs text-muted-foreground">
                    <span>CPU · last {Math.round((i.cpu_history.length * 2) / 60) || 1} min</span>
                    <span className="text-base font-semibold text-foreground tabular-nums">{percent(i.cpu_pct)}</span>
                  </div>
                  <AreaChart values={i.cpu_history} max={Math.max(5, ...i.cpu_history) * 1.25} label={`CPU of replica ${i.slot}`} tone="sky" />
                </div>
                <div>
                  <div className="mb-1 flex items-baseline justify-between text-xs text-muted-foreground">
                    <span>Memory · since opened</span>
                    <span className="text-base font-semibold text-foreground tabular-nums">{bytes(i.mem_bytes)}</span>
                  </div>
                  <AreaChart values={m} max={memLimit ?? Math.max(...m, 1) * 1.5} label={`Memory of replica ${i.slot}`} tone="violet" />
                </div>
              </div>
              <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-4">
                <Info k="Status" v={i.status} />
                <Info k="Health" v={i.health === "none" ? "no check" : i.health} />
                <Info k="Address" v={i.ip ?? "–"} mono />
                <Info k="Restarts" v={String(i.restarts)} />
              </dl>
              {i.last_probe && i.health === "unhealthy" && (
                <pre className="max-h-24 overflow-auto rounded-md bg-muted p-2 font-mono text-xs whitespace-pre-wrap">{i.last_probe}</pre>
              )}
            </Card>
          );
        })}
      </div>
    </div>
  );
}

function Info({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-muted-foreground">{k}</dt>
      <dd className={mono ? "truncate font-mono" : "truncate"}>{v}</dd>
    </div>
  );
}

function Stat({ icon: Icon, label, value, hint }: { icon: typeof Cpu; label: string; value: string; hint?: string }) {
  return (
    <Card className="gap-1 px-5 py-4">
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        {label}
        <Icon className="size-4" />
      </div>
      <div className="text-2xl font-semibold tabular-nums">{value}</div>
      {hint && <p className="truncate text-xs text-muted-foreground">{hint}</p>}
    </Card>
  );
}

/** `512m`, `2g`, `2GiB` -> bytes. */
export function parseMemory(s: string | undefined): number | null {
  if (!s) return null;
  const m = /^(\d+(?:\.\d+)?)\s*([kmgt])?(i?b)?$/i.exec(s.trim());
  if (!m) return null;
  const pow = { k: 1, m: 2, g: 3, t: 4 }[(m[2] ?? "").toLowerCase() as "k"] ?? 0;
  return Math.round(Number(m[1]) * 1024 ** pow);
}
