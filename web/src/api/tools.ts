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
  /** The server it runs on (`local` for this daemon), on a control plane. */
  server?: string;
  placement?: Placement;
  /** Its workspace may run Docker (security.nesting): set by superadmins (org_nesting). */
  allow_nesting?: boolean;
}

/** Where an org runs and how it is kept apart (src/daemon/servers.rs placement_view). */
export interface Placement {
  kind: "local" | "server" | "vm";
  server: string;
  isolation: "shared-kernel" | "own-host" | "own-kernel";
  vm?: { cpus: number; memory: string; disk: string; project: string; instance: string };
}

// Servers (src/daemon/servers.rs, src/servers/).

export interface Heartbeat {
  isb?: string;
  incus?: string | null;
  host?: {
    hostname?: string;
    cpus?: number;
    cpu_pct?: number;
    load1?: number;
    mem_used?: number;
    mem_total?: number;
    disk_used?: number;
    disk_total?: number;
  };
  orgs?: string[];
  stacks?: number;
  last_error?: { at: number; stack: string; message: string } | null;
}

export interface ServerView {
  name: string;
  kind: "ssh" | "vm";
  address: string;
  port: number;
  ssh: string;
  ssh_port: number;
  added_at: number;
  fingerprint: string;
  cert_not_after: number | null;
  isb_version: string;
  allow_from: string[];
  vm?: { org: string; project: string; instance: string; cpus: number; memory: string; disk: string };
  orgs: string[];
  version?: ServerVersion;
  health: {
    state: "unknown" | "up" | "unreachable";
    failures: number;
    last_ok: number | null;
    last_checked: number | null;
    last_error: string | null;
    heartbeat: Heartbeat | null;
  };
}

/** What a server's agent runs next to what this control plane runs. */
export interface ServerVersion {
  isb: string | null;
  /** SHA-256 of the agent's binary. */
  build: string | null;
  protocol: number | null;
  control_plane: { isb: string; build: string; protocol: number };
  /** A different version or build than this control plane's. */
  skew: boolean;
  /** Calls are forwarded to it (its protocol is one this control plane speaks). */
  compatible: boolean;
  /** SSH sessions are forwarded to it. */
  ssh: boolean;
  /** server_upgrade can replace it (a dedicated VM, or a server with the upgrade helper). */
  upgradable: boolean;
  last_upgrade: { state: string; sha256: string; at: number; message: string } | null;
}

export interface ServerUpgrade {
  name: string;
  upgraded: boolean;
  from?: { isb: string | null; build: string | null };
  to?: { isb: string | null; build: string | null };
  note?: string;
}

export type StepState = "pending" | "running" | "done" | "failed";

export interface ProvisionView {
  name: string;
  kind: "ssh" | "vm";
  org?: string;
  state: "running" | "done" | "failed";
  started_at: number;
  finished_at: number | null;
  steps: { id: string; title: string; state: StepState; started_at: number | null; finished_at: number | null }[];
  log: string[];
  /** Lines dropped off the top of `log` (the server keeps the last 400). */
  log_start?: number;
  error: string | null;
  request: Record<string, unknown>;
  result?: unknown;
}

export interface ServerList {
  servers: ServerView[];
  provisions: ProvisionView[];
  dedicated_vm: { supported: boolean; reason?: string };
  suggested_allow_from: { address: string; via: string }[];
}

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

// Monitor (host_monitor, superadmin): live resource use of this host or one
// remote server, its history, and its instances.

/** A server's heartbeat numbers; an older agent leaves fields out. */
export interface HostCard {
  hostname?: string;
  cpus?: number;
  cpu_pct?: number | null;
  /** The last ~40 samples, 2s apart, 0-100. */
  cpu_history?: number[];
  mem_used?: number;
  mem_total?: number;
  disk_used?: number;
  disk_total?: number;
  /** Bytes per second. */
  net_rx_rate?: number | null;
  net_tx_rate?: number | null;
  load1?: number;
}

export interface MonitorServer {
  name: string;
  local: boolean;
  kind: "local" | "ssh" | "vm";
  /** For a dedicated VM, the org it is for. */
  vm_org: string | null;
  state: "up" | "unreachable" | "unknown";
  /** Unix seconds of the last good heartbeat (null for this host). */
  last_ok: number | null;
  /** Null until a first heartbeat. */
  host: HostCard | null;
}

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

export interface HostMonitor {
  /** This host first, then every remote server. */
  servers: MonitorServer[];
  /** The selected entry's name. */
  server: string;
  /** Null when the server could not be reached. */
  monitor: Monitor | null;
  /** Why `monitor` is null or partial. */
  error: string | null;
  /** The agent is too old for live detail: host numbers only, no history or instances. */
  partial: boolean;
}
