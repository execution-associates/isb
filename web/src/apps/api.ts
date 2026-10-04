// The app tools (docs/guides/deploy-apps.md) as typed calls and queries. Results are not in
// the OpenAPI document (it types arguments only), so their shapes are written
// out here from src/daemon/apps.rs and src/app/.
import { useQueries, useQuery } from "@tanstack/react-query";
import { ApiError } from "@/api/client";
import { callTool, type ServiceStatus } from "@/api/tools";
import type { DomainSpec, DomainStatus } from "./domains";

export interface EnvironmentInfo {
  name: string;
  /** `<project>-<env>` */
  stack: string;
  apps: string[];
}

export interface Project {
  name: string;
  description?: string;
  created_at: number;
  environments: EnvironmentInfo[];
}

export type GitAuth = { token_secret: string; username?: string } | { ssh_key_secret: string } | null;

export interface GitSource {
  url: string;
  ref: string;
  subdir?: string;
  auth?: GitAuth;
  submodules?: boolean;
}

export type AppSource = { image: string } | { git: GitSource };

export type Builder =
  | { type: "railpack" }
  | { type: "nixpacks" }
  | { type: "dockerfile"; path?: string; target?: string }
  | { type: "buildpacks"; builder?: string };

export interface BuildSettings {
  builder: Builder;
  args?: Record<string, string>;
  untrusted?: boolean;
}

export interface Healthcheck {
  test?: string | string[];
  interval?: string;
  timeout?: string;
  retries?: number;
  start_period?: string;
  [k: string]: unknown;
}

export interface App {
  name: string;
  project: string;
  environment: string;
  source: AppSource;
  build?: BuildSettings;
  /** `.env` text. */
  env: string;
  env_vars: Record<string, string | { secret: string }>;
  domains?: DomainSpec[];
  volumes?: string[];
  ports?: string[];
  replicas: number;
  port?: number;
  healthcheck?: Healthcheck;
  resources?: { cpus?: string; memory?: string };
  command?: string | string[];
  stack: string;
  service_name: string;
  current_deployment: number | null;
  /** Unix seconds. */
  created_at: number;
  updated_at: number;
  /** The webhook's path on this server. */
  webhook: string;
  domains_served: boolean;
}

export type DeploymentStatus = "queued" | "building" | "deploying" | "done" | "failed" | "superseded" | "cancelled";

export interface Deployment {
  id: number;
  app: string;
  trigger: "manual" | "api" | "webhook";
  by: string;
  status: DeploymentStatus;
  requested?: string;
  rollback_of?: number;
  commit?: { sha: string; message: string };
  image?: string;
  digest?: string;
  error?: string;
  /** Unix milliseconds. */
  created_at: number;
  started_at?: number;
  finished_at?: number;
}

/**
 * The deployment running now: the last one that finished done (deploys run
 * one at a time per app, so ids finish in order). Fresher than the app
 * record's `current_deployment` while events are still arriving.
 */
export function currentOf(ds: Deployment[] | undefined, fallback: number | null): number | null {
  const done = (ds ?? []).filter((d) => d.status === "done").map((d) => d.id);
  return done.length ? Math.max(...done, fallback ?? 0) : fallback;
}

export const isGit = (s: AppSource): s is { git: GitSource } => "git" in s;

export const finished = (s: DeploymentStatus) => s === "done" || s === "failed" || s === "superseded" || s === "cancelled";

/** The stack_status of one stack, with each service's domains. */
export interface StackDetail {
  name: string;
  org: string;
  converged: boolean;
  services: (ServiceStatus & {
    domains?: DomainStatus[];
    instances: InstanceDetail[];
  })[];
}

export interface InstanceDetail {
  name: string;
  slot: number;
  rev: string;
  status: string;
  health: string;
  ip?: string | null;
  in_rotation: boolean;
  restarts: number;
  last_probe?: string;
  cpu_pct?: number | null;
  cpu_history: number[];
  mem_bytes?: number | null;
  [k: string]: unknown;
}

// Query keys: everything under ["apps", org] is refetched when the org's
// events say something changed.
export const keys = {
  org: (org: string) => ["apps", org] as const,
  projects: (org: string) => ["apps", org, "projects"] as const,
  apps: (org: string) => ["apps", org, "apps"] as const,
  app: (org: string, app: string) => ["apps", org, "app", app] as const,
  deployments: (org: string, app: string) => ["apps", org, "deployments", app] as const,
  deployment: (org: string, app: string, id: number) => ["apps", org, "deployment", app, id] as const,
  stack: (org: string, stack: string) => ["apps", org, "stack", stack] as const,
  env: (org: string, app: string) => ["apps", org, "env", app] as const,
  yaml: (org: string, app: string) => ["apps", org, "yaml", app] as const,
  secrets: (org: string) => ["apps", org, "secrets"] as const,
  ingress: (org: string) => ["apps", org, "ingress"] as const,
};

export const isNotFound = (e: unknown) => e instanceof ApiError && e.status === 404;

export function useProjects(org: string) {
  return useQuery({
    queryKey: keys.projects(org),
    queryFn: () => callTool<{ projects: Project[] }>("project_list", {}, org).then((r) => r.projects),
  });
}

export function useApps(org: string) {
  return useQuery({
    queryKey: keys.apps(org),
    queryFn: () => callTool<{ apps: App[] }>("app_list", {}, org).then((r) => r.apps),
  });
}

export function useApp(org: string, name: string) {
  return useQuery({
    queryKey: keys.app(org, name),
    queryFn: () => callTool<App>("app_get", { name }, org),
  });
}

export function useDeployments(org: string, app: string, limit = 30) {
  return useQuery({
    queryKey: [...keys.deployments(org, app), limit],
    queryFn: () =>
      callTool<{ current: number | null; deployments: Deployment[] }>("app_deployments", { name: app, limit }, org),
  });
}

/** The latest deployment of each app, for listings. */
export function useLatestDeployments(org: string, apps: string[]) {
  return useQueries({
    queries: apps.map((a) => ({
      queryKey: [...keys.deployments(org, a), 5],
      queryFn: () =>
        callTool<{ current: number | null; deployments: Deployment[] }>("app_deployments", { name: a, limit: 5 }, org),
    })),
    combine: (rs) => {
      const m = new Map<string, Deployment[]>();
      rs.forEach((r, i) => m.set(apps[i], r.data?.deployments ?? []));
      return m;
    },
  });
}

/** A stack's status, or null when it is not deployed (no app has run yet). */
export function useStack(org: string, stack: string | undefined, refetchInterval?: number) {
  return useQuery({
    queryKey: keys.stack(org, stack ?? ""),
    enabled: !!stack,
    refetchInterval,
    queryFn: async () => {
      try {
        return await callTool<StackDetail>("stack_status", { name: stack ?? "" }, org);
      } catch (e) {
        if (isNotFound(e)) return null;
        throw e;
      }
    },
  });
}

export function useSecretNames(org: string) {
  return useQuery({
    queryKey: keys.secrets(org),
    queryFn: () =>
      callTool<{ secrets: { name: string }[] } | { name: string }[]>("secret_list", {}, org).then((r) =>
        (Array.isArray(r) ? r : (r.secrets ?? [])).map((s) => s.name),
      ),
    staleTime: 30_000,
  });
}

export interface IngressInfo {
  enabled: boolean;
  message?: string;
  [k: string]: unknown;
}

export function useIngress(org: string) {
  return useQuery({
    queryKey: keys.ingress(org),
    queryFn: () => callTool<IngressInfo>("ingress_status", {}, org),
    staleTime: 60_000,
  });
}

/** What an app is doing now, from its service and its latest deployment. */
export type AppState =
  | "not-deployed"
  | "deploying"
  | "running"
  | "degraded"
  | "updating"
  | "failing"
  | "stopped"
  | "failed";

export function appState(svc: StackDetail["services"][number] | undefined, latest: Deployment | undefined): AppState {
  if (latest && !finished(latest.status)) return "deploying";
  if (!svc) return latest?.status === "failed" ? "failed" : "not-deployed";
  if (svc.replicas === 0) return "stopped";
  if (svc.state === "failing") return "failing";
  if (svc.state === "updating" || svc.state === "starting") return "updating";
  if (svc.healthy < svc.replicas) return svc.healthy === 0 ? "failing" : "degraded";
  return "running";
}

/** The service an app runs as in its stack's status. */
export const serviceOf = (stack: StackDetail | null | undefined, app: string) =>
  stack?.services.find((s) => s.service === app);
