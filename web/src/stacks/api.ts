// Compose stacks: the stacks an org has that no project's apps own, as
// documents (stack_export, stack_validate, stack_deploy).
import { useQuery } from "@tanstack/react-query";
import { ApiError } from "@/api/client";
import { callTool, type StackStatus } from "@/api/tools";
import type { Project } from "@/apps/api";

export interface StackExport {
  name: string;
  /** The compose file, resolved, as YAML. */
  yaml: string;
  services: string[];
  /** `apps` when a project's environment owns the stack; `ingress` for isb's tunnel. */
  managed_by: "apps" | "ingress" | null;
  deployed_at: number;
  deployed_by: string;
}

export const stackKeys = {
  org: (org: string) => ["stacks", org] as const,
  export: (org: string, name: string) => ["stacks", org, "export", name] as const,
  status: (org: string, name: string) => ["stacks", org, "status", name] as const,
};

/** A starting point for a new stack. */
export const NEW_STACK_TEMPLATE = `services:
  web:
    image: docker:traefik/whoami:latest
    ports: ["127.0.0.1:18080:80"]
    deploy:
      replicas: 2
`;

/** The stack names projects' apps own: <project>-<env> (a pull request's is <project>-<env>-pr-<n>). */
export function appStackNames(projects: Project[]): Set<string> {
  return new Set(projects.flatMap((p) => p.environments.map((e) => e.stack)));
}

/**
 * Whether a stack is a compose stack: one written as a file, not the stack
 * a project's environment renders its apps to (or one of its previews), and
 * not isb's own ingress tunnel.
 */
export function isComposeStack(name: string, appStacks: Set<string>): boolean {
  if (name === "isb-tunnel") return false;
  if (appStacks.has(name)) return false;
  return ![...appStacks].some((s) => name.startsWith(`${s}-pr-`));
}

export function useStackExport(org: string, name: string) {
  return useQuery({
    queryKey: stackKeys.export(org, name),
    queryFn: () => callTool<StackExport>("stack_export", { name }, org),
    retry: (n, e) => !(e instanceof ApiError && e.status === 404) && n < 2,
  });
}

/** A stack's status, or null when it is not deployed. */
export function useStackStatus(org: string, name: string, refetchInterval?: number) {
  return useQuery({
    queryKey: stackKeys.status(org, name),
    refetchInterval,
    queryFn: async () => {
      try {
        return await callTool<StackStatus>("stack_status", { name }, org);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
  });
}
