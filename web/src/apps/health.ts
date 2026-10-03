// Health of environments and projects, from the org's stacks.
import { useQuery } from "@tanstack/react-query";
import { callTool, type StackList, type StackStatus } from "@/api/tools";
import type { Project } from "./api";

export type Health = "healthy" | "degraded" | "failing" | "updating" | "idle";

/** Every stack the caller sees (shared with the org overview's query). */
export function useStackList() {
  return useQuery({
    queryKey: ["tool", "stack_list"],
    queryFn: () => callTool<StackList>("stack_list"),
    refetchInterval: 30_000,
  });
}

export function stackHealth(s: StackStatus | undefined): Health {
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

export function projectHealth(p: Project, stacks: StackStatus[], org: string): Health {
  return worst(p.environments.map((e) => stackHealth(stacks.find((s) => s.org === org && s.name === e.stack))));
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
