// Health of environments and projects, from the org's stacks.
import { useQuery } from "@tanstack/react-query";
import { callTool, type StackList, type StackStatus } from "@/api/tools";
import { LIVE_POLL } from "@/lib/freshness";
import { type EnvironmentInfo, keys, type Project, type StackDetail } from "./api";

export type Health = "healthy" | "degraded" | "failing" | "updating" | "idle";

/** Every stack the caller sees (shared with the org overview's query). */
export function useStackList() {
  return useQuery({
    queryKey: ["tool", "stack_list"],
    queryFn: () => callTool<StackList>("stack_list"),
    refetchInterval: LIVE_POLL,
  });
}

export function stackHealth(s: Pick<StackStatus, "services"> | undefined): Health {
  if (!s || s.services.length === 0) return "idle";
  const live = s.services.filter((x) => x.replicas > 0);
  if (live.length === 0) return "idle";
  if (live.some((x) => x.state === "failing" || x.healthy === 0)) return "failing";
  if (live.some((x) => x.state === "updating" || x.state === "starting")) return "updating";
  if (live.some((x) => x.healthy < x.replicas)) return "degraded";
  return "healthy";
}

const RANK: Health[] = ["failing", "degraded", "updating", "healthy", "idle"];

/** The worst of several. */
export function worst(hs: Health[]): Health {
  for (const h of RANK) if (hs.includes(h)) return h;
  return "idle";
}

/** The stacks an environment runs: its apps' stack and its compose stacks. */
export function envStacks(e: EnvironmentInfo): string[] {
  return [e.stack, ...e.compose.map((c) => c.name)];
}

/** An environment's health: the worst of its apps' stack and its compose stacks. */
export function envHealth(e: EnvironmentInfo, stacks: Pick<StackStatus, "name" | "org" | "services">[], org: string): Health {
  return worst(envStacks(e).map((n) => stackHealth(stacks.find((s) => s.org === org && s.name === n))));
}

export function projectHealth(p: Project, stacks: Pick<StackStatus, "name" | "org" | "services">[], org: string): Health {
  return worst(p.environments.map((e) => envHealth(e, stacks, org)));
}

export const HEALTH_TONE = {
  healthy: "ok",
  degraded: "warn",
  failing: "bad",
  updating: "busy",
  idle: "idle",
} as const;

export const HEALTH_LABEL: Record<Health, string> = {
  healthy: "Healthy",
  degraded: "Degraded",
  failing: "Failing",
  updating: "Updating",
  idle: "Nothing running",
};

/** The overview tool's view of the org: its stacks with each instance's CPU and memory. */
export function useOrgOverview(org: string) {
  return useQuery({
    queryKey: [...keys.org(org), "overview"],
    queryFn: () => callTool<{ stacks?: StackDetail[] }>("overview", {}, org),
    refetchInterval: LIVE_POLL,
    select: (r) => (r.stacks ?? []).filter((s) => s.org === org),
  });
}

/** CPU (percent of one core, summed) and memory (bytes) of the instances now. */
export function usageOf(stacks: StackDetail[]): { cpu: number; mem: number; instances: number } {
  let cpu = 0;
  let mem = 0;
  let instances = 0;
  for (const s of stacks)
    for (const svc of s.services)
      for (const i of svc.instances ?? []) {
        instances++;
        cpu += i.cpu_pct ?? 0;
        mem += i.mem_bytes ?? 0;
      }
  return { cpu, mem, instances };
}

const UNITS: Record<string, number> = {
  "": 1,
  b: 1,
  kb: 1e3,
  mb: 1e6,
  gb: 1e9,
  tb: 1e12,
  kib: 2 ** 10,
  mib: 2 ** 20,
  gib: 2 ** 30,
  tib: 2 ** 40,
};

/** An incus size ("8GiB", "512MB") in bytes, or null. */
export function parseSize(v: string | null | undefined): number | null {
  const m = v?.trim().match(/^([\d.]+)\s*([a-zA-Z]*)$/);
  if (!m) return null;
  const mul = UNITS[m[2].toLowerCase()];
  return mul ? Number(m[1]) * mul : null;
}
