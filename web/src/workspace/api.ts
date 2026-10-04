// The org's workspace (docs/concepts/workspaces.md) over the workspace_* and
// sandbox_* tools: result shapes from src/daemon/workspaces.rs (`view`) and
// the sandbox_list tool, and the queries the pages share.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { Role } from "@/api/auth";
import type { Quota } from "./util";

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
  | { volume: string; pool: string | null; driver: string | null; cow: boolean | null; path: string; size: string; exists: boolean; bind?: undefined }
  | { bind: string; path: string; volume?: undefined; pool?: undefined; driver?: undefined; cow?: undefined; size?: undefined; exists?: undefined };

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
  /** The first-boot script, run once as root after each create or rebuild. */
  setup?: string | null;
  setup_state?: { status: "pending" | "running" | "succeeded" | "failed"; at: number; runs: number; exit_code?: number | null; message?: string | null } | null;
  /** Published ports (workspace_port_add). */
  ports?: { port: number; host?: string | null; auto?: boolean; added_by: string; added_at: number }[];
  /** Whether its org lets it run Docker (security.nesting), and whether the machine has it now. */
  nesting?: { allowed: boolean; active: boolean; warning: string | null };
}

/** One published port as workspace_port_list shows it. */
export interface WorkspacePort {
  port: number;
  /** The ingress hostname, when it has one. */
  host?: string | null;
  auto?: boolean;
  added_by: string;
  added_at: number;
  /** The preview's own host label, `<port>-<workspace>-<org>`. */
  preview_host: string;
  url?: string | null;
  domain?: { host: string; state: string; cert: string; url?: string | null; message?: string | null }[];
}

export interface PortList {
  org: string;
  name: string;
  ports: WorkspacePort[];
  /** This server runs an ingress, so ports can have hostnames. */
  ingress: boolean;
  preview_domain: boolean;
}

/** An image a workspace can be made from: local to this host, or remote. */
export interface WorkspaceImage {
  image: string;
  description: string;
  source: "local" | "remote";
}

/** With no workspace yet: what one can be made from, and the org's quota. */
export interface WorkspaceCreateOptions {
  images: WorkspaceImage[];
  default_image: string;
  /** isb's default workspace image (built from its recipe) and whether this host has it. */
  default_recipe?: { image: string; exists: boolean };
  quota: Quota;
}

export interface WorkspaceGet {
  org: string;
  settings: WorkspaceSettings;
  workspace: Workspace | null;
  create?: WorkspaceCreateOptions | null;
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
