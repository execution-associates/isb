// The tools over REST (POST /api/v1/tools/<tool>), typed from the daemon's
// OpenAPI document: `bun run gen:api` regenerates openapi.gen.ts from
// web/openapi.json (refresh that from a daemon's /api/v1/openapi.json).
// A tool the snapshot does not know yet still works, untyped: nothing here
// lists tools, so new ones need no UI-server change.
import { post } from "./client";
import type { paths } from "./openapi.gen";

type ToolPath = keyof paths & `/api/v1/tools/${string}`;
export type ToolName = ToolPath extends `/api/v1/tools/${infer N}` ? N : never;

type Body<P> = P extends { post: { requestBody?: { content: { "application/json": infer B } } } } ? B : never;
export type ToolArgs<N extends string> = N extends ToolName ? Body<paths[`/api/v1/tools/${N}`]> : Record<string, unknown>;

/** Call `tool`, in `org` when given (the org-bound endpoint pins it). */
export async function callTool<R = unknown, N extends string = ToolName>(
  tool: N,
  args: ToolArgs<N> = {} as ToolArgs<N>,
  org?: string,
): Promise<R> {
  const path = org
    ? `/orgs/${encodeURIComponent(org)}/api/v1/tools/${encodeURIComponent(tool)}`
    : `/api/v1/tools/${encodeURIComponent(tool)}`;
  const r = await post<{ result: R }>(path, args);
  return r.result;
}

// Result shapes. The OpenAPI document types arguments only (results are
// `{result: any}`), so the ones the UI reads are written out here, from
// src/stack/controller.rs and the overview tool.

export interface InstanceStatus {
  name: string;
  status?: string;
  healthy?: boolean;
  [k: string]: unknown;
}

export interface ServiceStatus {
  service: string;
  image: string;
  rev: string;
  replicas: number;
  running: number;
  healthy: number;
  /** starting, converged, updating, paused, waiting, failing */
  state: string;
  message?: string;
  instances: InstanceStatus[];
  ports: { listen?: string; target?: number; [k: string]: unknown }[];
  checked_at: number;
}

export interface StackStatus {
  name: string;
  org: string;
  deployed_at: number;
  deployed_by: string;
  has_previous: boolean;
  converged: boolean;
  services: ServiceStatus[];
}

export interface StackList {
  stacks: StackStatus[];
}

export interface StackEvent {
  seq: number;
  at: number;
  level: "info" | "warn" | "error";
  stack: string;
  service?: string;
  instance?: string;
  message: string;
}

// Secrets (src/daemon/secrets.rs). Values travel base64, and only
// secret_get returns one.

export interface SecretMeta {
  org: string;
  name: string;
  driver: string;
  version: number;
  created_at: number;
  updated_at: number;
  labels?: Record<string, string>;
  /** Deployed stacks whose services use it. */
  used_by: string[];
}

/** A driver reference a stack uses (such as 1Password's vault/item/field). */
export interface SecretReference {
  name: string;
  driver: string;
  version: number;
  used_by: string[];
}

export interface SecretList {
  secrets: SecretMeta[];
  references: SecretReference[];
}

// Orgs (src/daemon/orgs.rs, src/org.rs).

export interface OrgView {
  name: string;
  project: string;
  network: string | null;
  subnet: string | null;
  cpus: string | null;
  memory: string | null;
  disk: string | null;
  instances_limit: string | null;
  default_cpus: string | null;
  default_memory: string | null;
  bind_roots: string[];
  egress: string[];
  dns_dir: string | null;
  instances: number;
  /** Each limit's budget (`cpu`, `memory`, `disk`, `instances`), bytes for memory and disk; a limit the org does not set is absent. */
  allocation?: Record<string, { limit: number; allocated: number; free: number }>;
  domain: string;
  service_names: boolean;
  members: number;
  stacks: number;
  notes?: string[];
  /** Its workspace may run Docker (security.nesting): set by superadmins (org_nesting). */
  allow_nesting?: boolean;
}

// This host (server_status).

export interface ServerStatus {
  isb: string;
  incus: string;
  state_dir: string;
  routes: {
    route: string;
    listen: string;
    backends: { addr: string; active: number; down: boolean }[];
    accepted: number;
    failures: number;
    rejected: number;
  }[];
}

// Monitor (host_monitor, superadmin): live resource use of this host, its
// history, and its instances.

export interface MonitorInterface {
  name: string;
  /** Global-scope addresses. */
  addresses: string[];
  up: boolean;
  rx_bytes: number;
  tx_bytes: number;
  rx_rate: number | null;
  tx_rate: number | null;
}

export interface MonitorHost {
  hostname: string;
  cpus: number;
  /** 0-100 across every CPU. */
  cpu_pct: number | null;
  cpu_history: number[];
  /** Per-core busy, 0-100; empty when unknown. */
  cpu_cores: number[];
  load1: number;
  load5: number | null;
  load15: number | null;
  uptime_secs: number | null;
  mem_used: number;
  mem_total: number;
  swap_used: number | null;
  swap_total: number | null;
  disk_used: number;
  disk_total: number;
  pools: { name: string; driver: string; used: number; total: number }[];
  disk_read_rate: number | null;
  disk_write_rate: number | null;
  /** Sum of `interfaces`, bytes per second. */
  net_rx_rate: number | null;
  net_tx_rate: number | null;
  /** lo, veth* and tap* left out. */
  interfaces: MonitorInterface[];
  isb: string;
}

export interface HistoryPoint {
  t: number;
  cpu: number | null;
  mem_used: number | null;
  net_rx: number | null;
  net_tx: number | null;
}

export interface MonitorInstance {
  name: string;
  project: string;
  org: string | null;
  /** container, virtual-machine or oci */
  kind: string;
  status: string;
  ip: string | null;
  stack: string | null;
  /** Percent of one core: can pass 100. */
  cpu_pct: number | null;
  cpu_history: number[];
  mem_bytes: number | null;
  net_rx_rate: number | null;
  net_tx_rate: number | null;
  disk_read_rate: number | null;
  disk_write_rate: number | null;
}

export interface Monitor {
  /** Unix ms of the sample. */
  at: number;
  host: MonitorHost;
  /** `step` in seconds; points oldest first. */
  history: { step: number; points: HistoryPoint[] };
  instances: MonitorInstance[];
}
