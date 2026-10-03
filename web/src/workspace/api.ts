// The org's workspace (docs/workspaces.md) over the workspace_* and
// sandbox_* tools: result shapes from src/daemon/workspaces.rs (`view`) and
// the sandbox_list tool, and the queries the pages share.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { Role } from "@/api/auth";

export interface WorkspaceSettings {
  max_workspaces: number;
  /** A new sandbox's lifetime, e.g. `24h`. */
  sandbox_expiry: string;
  /** Idle timeout for sandboxes, e.g. `2h`, or `none`. */
  sandbox_idle: string;
}

export interface WorkspaceSessions {
  /** Web terminals open through the daemon. */
  terminals: number;
  /** Established SSH connections inside the machine, when it could be asked. */
  ssh?: number | null;
  total?: number;
}

export type WorkspaceHome =
  | { volume: string; pool: string | null; path: string; size: string; exists: boolean; bind?: undefined }
  | { bind: string; path: string; volume?: undefined; pool?: undefined; size?: undefined; exists?: undefined };

export interface Workspace {
  name: string;
  id: string;
  org: string;
  image: string;
  user: string;
  cpus?: number | null;
  memory?: string | null;
  root_size?: string | null;
  home_size: string;
  env?: Record<string, string>;
  secrets?: string[];
  labels?: Record<string, string>;
  token_role: Exclude<Role, "owner">;
  home_bind?: string | null;
  created_at: number;
  created_by: string;
  updated_at?: number;
  rebuilt_at?: number | null;
  home_dir: string;
  /** Running, Stopped, ... or Missing when the instance is gone. */
  status: string;
  instance: { name: string; status: string; type: string; created_at: string; ip: string | null } | null;
  resources: {
    cpu_pct: number | null;
    cpu_history: number[];
    mem_bytes: number | null;
    disk_bytes: number | null;
    cpus: string | null;
    memory: string | null;
  };
  home: WorkspaceHome;
  sessions: WorkspaceSessions;
  last_activity: number | null;
  token: { id: string; role: string; created_at: number; last_used: number | null; path: string } | null;
  connect: { url: string | null; mcp_url: string | null; org: string; user: string; token_path: string; env: string[] };
  /** Sandboxes in the org. */
  sandboxes: number;
}

export interface WorkspaceGet {
  org: string;
  settings: WorkspaceSettings;
  workspace: Workspace | null;
}

export interface Sandbox {
  name: string;
  status: string;
  type: string;
  kind: "sandbox" | "workspace" | "replica" | "build";
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

export const wsKeys = {
  workspace: (org: string) => ["workspace", org] as const,
  sandboxes: (org: string) => ["workspace-sandboxes", org] as const,
};

export function useWorkspace(org: string) {
  return useQuery({
    queryKey: wsKeys.workspace(org),
    queryFn: () => callTool<WorkspaceGet>("workspace_get", {}, org),
    refetchInterval: 10_000,
  });
}

export function useSandboxes(org: string) {
  return useQuery({
    queryKey: wsKeys.sandboxes(org),
    queryFn: () => callTool<{ sandboxes: Sandbox[] }>("sandbox_list", { kind: "sandbox" }, org).then((r) => r.sandboxes),
    refetchInterval: 15_000,
  });
}

/** Call a workspace tool with loosely typed arguments (the snapshot types them strictly). */
export const wsCall = <R = unknown>(tool: string, args: Record<string, unknown>, org: string) => callTool<R, string>(tool, args, org);
