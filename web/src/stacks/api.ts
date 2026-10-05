// Compose stacks: stacks written as a compose file and deployed into a
// project's environment, beside its apps, as documents (stack_export,
// stack_validate, stack_deploy), with the same day-2 surface an app has:
// environment, domains, deployments and rollback.
import { type QueryClient, useQuery } from "@tanstack/react-query";
import { ApiError } from "@/api/client";
import { callTool } from "@/api/tools";
import { type Deployment, type DeploymentStatus, finished, type Project } from "@/apps/api";
import type { DomainSpec } from "@/apps/domains";
import { stackTab } from "@/apps/service-tabs";
import { invalidateOrg } from "@/lib/freshness";

export interface StackExport {
  name: string;
  /** The compose file, resolved, as YAML. */
  yaml: string;
  services: string[];
  /** `apps` when a project's environment owns the stack; `ingress` for isb's tunnel. */
  managed_by: "apps" | "ingress" | null;
  /** The project and environment a compose stack belongs to (null for isb's tunnel). */
  project: string | null;
  environment: string | null;
  deployed_at: number;
  deployed_by: string;
}

export const stackKeys = {
  org: (org: string) => ["stacks", org] as const,
  export: (org: string, name: string) => ["stacks", org, "export", name] as const,
  env: (org: string, name: string) => ["stacks", org, "env", name] as const,
  domains: (org: string, name: string) => ["stacks", org, "domains", name] as const,
  deployments: (org: string, name: string) => ["stacks", org, "deployments", name] as const,
  deployment: (org: string, name: string, id: number) => ["stacks", org, "deployment", name, id] as const,
  config: (org: string, name: string) => ["stacks", org, "config", name] as const,
};

/** A starting point for a new stack. */
export const NEW_STACK_TEMPLATE = `services:
  web:
    image: docker:traefik/whoami:latest
    ports: ["127.0.0.1:18080:80"]
    deploy:
      replicas: 2
`;

/** Where a compose stack lives. */
export interface StackOwner {
  project: string;
  environment: string;
}

/** Every compose stack in the org's projects, with its owner. */
export function composeStacks(projects: Project[]): (StackOwner & { name: string; services: string[] })[] {
  return projects.flatMap((p) => p.environments.flatMap((e) => e.compose.map((c) => ({ ...c, project: p.name, environment: e.name }))));
}

/** The project environment a compose stack is deployed into, or null. */
export function ownerOf(projects: Project[], name: string): StackOwner | null {
  const c = composeStacks(projects).find((s) => s.name === name);
  return c ? { project: c.project, environment: c.environment } : null;
}

/** A compose stack's page, under its project environment. */
export function composePath(org: string, owner: StackOwner, name: string, tab?: string): string {
  const base = `/orgs/${encodeURIComponent(org)}/projects/${encodeURIComponent(owner.project)}/${encodeURIComponent(owner.environment)}/compose/${encodeURIComponent(name)}`;
  return tab ? `${base}/${tab}` : base;
}

/**
 * Where an old /orgs/:org/stacks/:stack link goes: the stack's page under its
 * project environment, found in the projects' compose lists or, failing that,
 * in its export. Null when it has no owner (isb's tunnel, a project's own apps
 * stack): that page stays where it is.
 */
export function stackRedirect(org: string, name: string, tab: string | undefined, projects: Project[], exp?: Pick<StackExport, "managed_by" | "project" | "environment">): string | null {
  const owner = ownerOf(projects, name) ?? (exp && !exp.managed_by && exp.project && exp.environment ? { project: exp.project, environment: exp.environment } : null);
  return owner ? composePath(org, owner, name, stackTab(tab)) : null;
}

export function useStackExport(org: string, name: string) {
  return useQuery({
    queryKey: stackKeys.export(org, name),
    queryFn: () => callTool<StackExport>("stack_export", { name }, org),
    retry: (n, e) => !(e instanceof ApiError && e.status === 404) && n < 2,
  });
}

// The day-2 tools below (stack_env_get/set, stack_domains_get/set,
// stack_deployments, stack_deployment_get, stack_rollback's `to`) are newer
// than web/openapi.json: they are called untyped (callTool<R, string>) with
// these hand-written shapes, mirroring the app tools'. Once the snapshot
// has them, the generated argument types take over without other changes.

/** stack_env_get: the stack's .env text, whose variables fill `${VAR}` in its compose file. */
export interface StackEnv {
  env: string;
}

/** stack_env_set {name, env, deploy?}: the stored text, and the deployment when `deploy`. */
export interface StackEnvSet extends DeployResult {
  env: string;
}

/**
 * What a deploy answers (stack_deploy, stack_rollback, stack_env_set and
 * stack_domains_set with `deploy`): the deployment it started, and the
 * secrets it reused from an earlier deploy because no new value was given.
 */
export interface DeployResult {
  deployment?: StackDeployment;
  reused_secrets?: string[];
}

/** The secrets a deploy reused, from its answer or its deployment record. */
export function reusedSecrets(r: DeployResult | null | undefined): string[] {
  return r?.reused_secrets?.length ? r.reused_secrets : (r?.deployment?.reused_secrets ?? []);
}

/** What a stack deployment deployed: a compose file, a rollback, or a changed environment or set of domains. */
export type StackAction = "deploy" | "rollback" | "env" | "domains";

/** One deploy of a compose stack (stack_deployments), as app_deployments has them. */
export interface StackDeployment {
  id: number;
  trigger: Deployment["trigger"];
  status: DeploymentStatus;
  action?: StackAction;
  /** Who started it. */
  actor?: string;
  rollback_of?: number;
  /** Secrets it reused from an earlier deploy: no new value was given. */
  reused_secrets?: string[];
  error?: string;
  /** Unix milliseconds (seconds are taken too, see stackDeploymentMs). */
  created_at: number;
  started_at?: number;
  finished_at?: number;
  /** The services it changed. */
  services?: string[];
}

/** stack_deployments {name, limit?}. */
export interface StackDeployments {
  current?: number | null;
  deployments: StackDeployment[];
}

/** One of the stack's events while a deployment ran (stack_deployment_get's `events`). */
export interface StackDeploymentEvent {
  /** Unix milliseconds. */
  at?: number;
  level?: string;
  service?: string;
  message: string;
}

/** stack_deployment_get {name, id}: the record, the compose file it deployed, and its events. */
export type StackDeploymentReply = (StackDeployment | { deployment: StackDeployment }) & {
  /** The compose file it deployed. */
  source?: string;
  events?: (string | StackDeploymentEvent)[];
  /** The events as text (the same lines), for a reply without them. */
  log?: string | string[];
};

export interface StackDeploymentDetail {
  record: StackDeployment;
  source: string;
  events: StackDeploymentEvent[];
  /** The events as log lines, or the reply's log when it has no events. */
  lines: string[];
}

/** A unix time in milliseconds, from either milliseconds or seconds. */
const ms = (t: number | undefined) => (t === undefined || t === null ? undefined : t < 1e12 ? t * 1000 : t);

/** A stack deployment with its times in milliseconds. */
export function stackDeploymentMs(d: StackDeployment): StackDeployment {
  return { ...d, created_at: ms(d.created_at) ?? 0, started_at: ms(d.started_at), finished_at: ms(d.finished_at) };
}

const pad = (n: number) => String(n).padStart(2, "0");

/**
 * An event as a log line, as the server's `log` writes it: its time (local),
 * its level unless info, its service unless `service` is the one shown.
 */
export function eventLine(e: StackDeploymentEvent, service?: string): string {
  const t = e.at ? new Date(e.at) : null;
  const time = t ? `${pad(t.getHours())}:${pad(t.getMinutes())}:${pad(t.getSeconds())} ` : "";
  const level = e.level && e.level !== "info" && e.level !== "log" ? `[${e.level}] ` : "";
  const svc = e.service && e.service !== service ? `${e.service}: ` : "";
  return `${time}${level}${svc}${e.message}`;
}

/** stack_deployment_get's reply, flat or `{deployment}`, as one shape with its log as lines. */
export function stackDeploymentDetail(r: StackDeploymentReply): StackDeploymentDetail {
  const record = stackDeploymentMs("deployment" in r ? r.deployment : r);
  const events = (r.events ?? []).map((e) => (typeof e === "string" ? { message: e } : e));
  const log = typeof r.log === "string" ? (r.log.trim() ? r.log.replace(/\n$/, "").split("\n") : []) : (r.log ?? []);
  return { record, source: r.source ?? "", events, lines: events.length ? events.map((e) => eventLine(e)) : log };
}

// What the stack's controller says once it rolls out (src/stack/controller.rs):
// "rolling out rev R to N slot(s)", "slot N: creating|replacing ...".
const ROLLOUT = /^(rolling out rev |slot \d+: |scaling down|removing |rollout of rev )/;

/**
 * A stack deployment's stages for the strip an app deployment has (Queued,
 * Pull, Roll out, Live), from its events: Pull once it started (any event),
 * Roll out from the first rollout event. A deployment rolling no
 * slot out yet is drawn at Pull.
 */
export function stackStages(r: Pick<StackDeployment, "status" | "started_at">, events: StackDeploymentEvent[]): { status: DeploymentStatus; reached: { building: boolean; deploying: boolean } } {
  const deploying = events.some((e) => ROLLOUT.test(e.message));
  const building = deploying || !!r.started_at || events.length > 0;
  const status: DeploymentStatus = r.status === "deploying" && !deploying ? "building" : r.status;
  return { status, reached: { building, deploying } };
}

/** The services a deployment's events name, in the order they first speak. */
export function eventServices(events: StackDeploymentEvent[]): string[] {
  return [...new Set(events.map((e) => e.service).filter((s): s is string => !!s))];
}

/**
 * A stack deployment as the app pages' Deployment, so the deployments list
 * and its rows draw it the same way.
 */
export function asDeployment(stack: string, d: StackDeployment): Deployment {
  const x = stackDeploymentMs(d);
  return {
    id: x.id,
    app: stack,
    trigger: x.trigger ?? "api",
    by: x.actor || "someone",
    status: x.status,
    rollback_of: x.rollback_of,
    error: x.error,
    created_at: x.created_at,
    started_at: x.started_at,
    finished_at: x.finished_at,
  };
}

/**
 * One service's domains (stack_domains_get): `managed` are the ones set here,
 * stored beside the compose file and merged into it at deploy; `file` are
 * the ones its `domains:` lists, changed only in the YAML.
 */
export interface ServiceDomains {
  managed: DomainSpec[];
  file: DomainSpec[];
}

/** stack_domains_get {name}: `{services: {[service]: {managed, file}}}`. */
export type StackDomains = Record<string, ServiceDomains>;

/** stack_domains_get's reply, with missing lists as empty ones. */
export function stackDomains(r: { services?: Record<string, Partial<ServiceDomains> | null> } | null | undefined): StackDomains {
  return Object.fromEntries(Object.entries(r?.services ?? {}).map(([svc, d]) => [svc, { managed: d?.managed ?? [], file: d?.file ?? [] }]));
}

export function getStackDomains(org: string, name: string): Promise<StackDomains> {
  return callTool<{ services?: Record<string, Partial<ServiceDomains> | null> }, string>("stack_domains_get", { name }, org).then(stackDomains);
}

/** stack_domains_set {name, service, domains, deploy?}: one service's managed domains, replaced (the file's stay). */
export function setStackDomains(org: string, name: string, service: string, domains: DomainSpec[], deploy: boolean) {
  return callTool<{ domains?: DomainSpec[] } & DeployResult, string>("stack_domains_set", { name, service, domains, ...(deploy ? { deploy: true } : {}) }, org);
}

export function useStackEnv(org: string, name: string) {
  return useQuery({
    queryKey: stackKeys.env(org, name),
    queryFn: () => callTool<StackEnv, string>("stack_env_get", { name }, org).then((r) => r.env ?? ""),
  });
}

export function useStackDomains(org: string, name: string) {
  return useQuery({
    queryKey: stackKeys.domains(org, name),
    queryFn: () => getStackDomains(org, name),
  });
}

export function useStackDeployments(org: string, name: string, limit = 30, refetchInterval?: number | ((latest: StackDeployment | undefined) => number | false)) {
  return useQuery({
    queryKey: [...stackKeys.deployments(org, name), limit],
    refetchInterval: typeof refetchInterval === "function" ? (q) => refetchInterval(q.state.data?.deployments[0]) : refetchInterval,
    queryFn: () => fetchStackDeployments(org, name, limit),
  });
}

/** A stack's last `limit` deployments, times in milliseconds (the query useStackDeployments caches). */
export function fetchStackDeployments(org: string, name: string, limit: number): Promise<StackDeployments> {
  return callTool<StackDeployments, string>("stack_deployments", { name, limit }, org).then((r) => ({
    current: r.current ?? null,
    deployments: (r.deployments ?? []).map(stackDeploymentMs),
  }));
}

/** One deployment, read again every 2 s until it finishes. */
export function useStackDeployment(org: string, name: string, id: number) {
  return useQuery({
    queryKey: stackKeys.deployment(org, name, id),
    refetchInterval: (q) => (q.state.data && finished(q.state.data.record.status) ? false : 2000),
    queryFn: () => callTool<StackDeploymentReply, string>("stack_deployment_get", { name, id }, org).then(stackDeploymentDetail),
  });
}

/** stack_config {name}: the file the stack runs, with managed domains merged in and `${VAR}` filled (secrets as references). */
export interface StackConfig {
  file: unknown;
  source?: string | null;
}

export function useStackConfig(org: string, name: string, enabled: boolean) {
  return useQuery({
    queryKey: stackKeys.config(org, name),
    enabled,
    queryFn: () => callTool<StackConfig>("stack_config", { name }, org),
  });
}

/**
 * Each service's `deploy.replicas` in a compose file's text, 1 where it sets
 * none (or sets it from a variable): what Start scales a stopped stack back
 * to. A line scan, not a YAML parser: block style (`deploy:` then
 * `replicas: 2` under it) and flow style (`deploy: {replicas: 2}`).
 */
export function sourceReplicas(yaml: string, services: string[]): Record<string, number> {
  const found: Record<string, number> = {};
  const lines = yaml.split("\n").map((l) => l.replace(/(^|\s)#.*$/, "").replace(/\s+$/, ""));
  const indent = (l: string) => l.length - l.trimStart().length;
  const key = (l: string) => /^\s*(["']?)([^"'\s:][^"':]*)\1\s*:(?:\s+(.*))?$/.exec(l);
  const flow = (v: string | undefined) => (v ? /\breplicas\s*:\s*(\d+)/.exec(v) : null);
  let inServices = false;
  let svcIndent = -1;
  let svc: string | null = null;
  let deployIndent = -1;
  let childIndent = -1;
  for (const l of lines) {
    if (!l.trim()) continue;
    const ind = indent(l);
    const k = key(l);
    if (ind === 0) {
      inServices = k?.[2] === "services" && !k[3];
      svcIndent = -1;
      svc = null;
      continue;
    }
    if (!inServices) continue;
    if (svcIndent < 0) svcIndent = ind;
    if (ind <= svcIndent) {
      svc = ind === svcIndent && k ? k[2] : null;
      deployIndent = -1;
      const m = svc && k?.[3] ? /\bdeploy\s*:\s*\{([^}]*)\}/.exec(k[3]) : null;
      if (svc && m) found[svc] = Number(flow(m[1])?.[1] ?? 1);
      continue;
    }
    if (!svc) continue;
    if (deployIndent >= 0 && ind <= deployIndent) deployIndent = -1;
    if (deployIndent < 0) {
      if (k?.[2] === "deploy") {
        const f = flow(k[3]);
        if (f) found[svc] = Number(f[1]);
        else if (!k[3]) {
          deployIndent = ind;
          childIndent = -1;
        }
      }
      continue;
    }
    if (childIndent < 0) childIndent = ind;
    if (ind === childIndent && k?.[2] === "replicas" && /^\d+$/.test(k[3] ?? "")) found[svc] = Number(k[3]);
  }
  return Object.fromEntries(services.map((s) => [s, found[s] ?? 1]));
}

/** After a deploy: refresh everything that shows stacks, and put the new file in the editor. */
export async function afterDeploy(qc: QueryClient, org: string, name: string) {
  const fresh = await callTool<StackExport>("stack_export", { name }, org);
  qc.setQueryData(stackKeys.export(org, name), fresh);
  await invalidateOrg(qc, org);
}

/** stack_validate and stack_deploy's project and environment arguments, when the stack has an owner. */
export function ownerArgs(owner: StackOwner | null): { project?: string; environment?: string } {
  return owner ? { project: owner.project, environment: owner.environment } : {};
}
