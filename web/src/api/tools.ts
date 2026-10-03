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
  domain: string;
  service_names: boolean;
  members: number;
  stacks: number;
  notes?: string[];
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
