// The org's sandboxes over the sandbox_* tools (docs/concepts/sandboxes.md):
// result shapes from the sandbox_list and sandbox_settings tools, and the
// queries the pages share.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";

/** The org's defaults for new sandboxes (sandbox_settings). */
export interface SandboxSettings {
  /** A new sandbox's lifetime, e.g. `24h`. */
  sandbox_expiry: string;
  /** Idle timeout for sandboxes, e.g. `2h`, or `none`. */
  sandbox_idle: string;
}

export interface Sandbox {
  name: string;
  status: string;
  type: string;
  kind: "sandbox" | "replica" | "build";
  labels: Record<string, string>;
  owner: string | null;
  /** Created by the caller, who may extend it. */
  mine: boolean;
  created_at: number | null;
  age_secs: number | null;
  expires_at: number | null;
  /** Seconds; 0 or null: none. */
  idle_timeout: number | null;
  last_active: number | null;
  cpus: string | null;
  memory: string | null;
  cpu_pct: number | null;
  mem_bytes: number | null;
  ip: string | null;
}

export const sbKeys = {
  sandboxes: (org: string) => ["sandboxes", org] as const,
  settings: (org: string) => ["sandbox-settings", org] as const,
};

export function useSandboxes(org: string) {
  return useQuery({
    queryKey: sbKeys.sandboxes(org),
    queryFn: () => sbCall<{ sandboxes: Sandbox[] }>("sandbox_list", { kind: "sandbox" }, org).then((r) => r.sandboxes),
    refetchInterval: 15_000,
  });
}

export function useSandboxSettings(org: string) {
  return useQuery({
    queryKey: sbKeys.settings(org),
    queryFn: () => sbCall<{ org: string; settings: SandboxSettings }>("sandbox_settings", {}, org).then((r) => r.settings),
  });
}

/** Call a sandbox tool with loosely typed arguments (the snapshot types them strictly). */
export const sbCall = <R = unknown>(tool: string, args: Record<string, unknown>, org: string) => callTool<R, string>(tool, args, org);
