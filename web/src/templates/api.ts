// Templates: one-click apps (docs/guides/templates.md). Shapes from
// src/template/{mod,catalog,dokploy,coolify}.rs and src/daemon/templates.rs.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";

export interface TemplateSummary {
  ref: string;
  catalog: string;
  id: string;
  name: string;
  description: string;
  version?: string;
  /** The logo's upstream URL. The browser never loads it: see logoSrc. */
  logo?: string;
  tags: string[];
  links: Record<string, string>;
  format: "native" | "dokploy" | "coolify";
}

export type VarKind =
  | "string"
  | "email"
  | "url"
  | "int"
  | "domain"
  | "password"
  | "base64"
  | "hex"
  | "uuid"
  | "port"
  | "username"
  | "timestamp"
  | "jwt";

export interface Variable {
  name: string;
  type?: VarKind;
  label?: string;
  description?: string;
  default?: string;
  required: boolean;
  generated: boolean;
  secret?: boolean;
  length?: number;
  bytes?: number;
  choices?: string[];
  min_length?: number;
  max_length?: number;
  min?: number;
  max?: number;
}

export interface TemplateApp {
  key: string;
  image: string;
  port?: number | null;
  domains: number;
  volumes: string[];
  files: string[];
  depends_on: string[];
}

export interface Compatibility {
  status: "clean" | "notes" | "refused";
  notes?: string[];
  refusals?: string[];
}

export interface TemplateDetail {
  template: TemplateSummary;
  compatibility?: Compatibility;
  variables?: Variable[];
  apps?: TemplateApp[];
  main?: string;
  notes?: string[];
}

export interface PlannedVar {
  name: string;
  value?: string;
  secret: boolean;
  source: "given" | "generated" | "default" | "computed";
}

export interface PlannedApp {
  name: string;
  source: { image?: string; [k: string]: unknown };
  port?: number;
  domains?: { host: string }[];
  volumes?: string[];
  [k: string]: unknown;
}

export interface Plan {
  template: string;
  version?: string;
  instance: string;
  project: string;
  environment: string;
  stack: string;
  apps: PlannedApp[];
  order: string[];
  secrets: { name: string; holds: string }[];
  variables: PlannedVar[];
  urls: string[];
  notes: string[];
}

export interface DeployAnswer {
  plan: Plan;
  ref: string;
  dry_run?: boolean;
  conflicts?: string[];
  error?: string;
  instance?: TemplateInstance;
  /** The apps being deployed, in order (without `wait`). */
  deploying?: string[];
  /** The first app's deployment, queued before the answer (without `wait`). */
  first_deployment?: { app: string; id: number };
}

/**
 * Where a just-started template deploy is followed: the first app's
 * deployment, then the apps after it in deploy order (`?then=`). Null when
 * the answer has no first deployment (an older daemon, or its queueing
 * failed and the background run retries).
 */
export function followOf(r: DeployAnswer): { app: string; id: number; next: string[] } | null {
  const f = r.first_deployment;
  if (!f) return null;
  const order = r.deploying ?? r.plan.order;
  const at = order.indexOf(f.app);
  return { app: f.app, id: f.id, next: at < 0 ? order.filter((a) => a !== f.app) : order.slice(at + 1) };
}

export interface TemplateInstance {
  name: string;
  template: string;
  version?: string;
  project: string;
  environment: string;
  apps: string[];
  secrets: string[];
  variables?: Record<string, string>;
  urls?: string[];
  created_at: number;
  created_by: string;
}

export interface CatalogConfig {
  name: string;
  format: "native" | "dokploy" | "coolify";
  location: string;
}

export const tkeys = {
  list: () => ["templates", "list"] as const,
  get: (ref: string) => ["templates", "get", ref] as const,
  catalogs: () => ["templates", "catalogs"] as const,
  instances: (org: string) => ["apps", org, "template-instances"] as const,
};

export function useTemplates(org: string) {
  return useQuery({
    queryKey: tkeys.list(),
    queryFn: () => callTool<{ templates: TemplateSummary[]; errors: string[] | Record<string, string> }>("template_list", {}, org),
    staleTime: 60_000,
  });
}

export function useTemplate(org: string, ref: string) {
  return useQuery({
    queryKey: tkeys.get(ref),
    queryFn: () => callTool<TemplateDetail>("template_get", { template: ref }, org),
    staleTime: 60_000,
  });
}

export function useCatalogs(org: string) {
  return useQuery({
    queryKey: tkeys.catalogs(),
    queryFn: () => callTool<{ builtin: string; catalogs: CatalogConfig[] }>("template_catalog_list", {}, org),
  });
}

export function useInstances(org: string) {
  return useQuery({
    queryKey: tkeys.instances(org),
    queryFn: () => callTool<{ instances: TemplateInstance[] }>("template_instance_list", {}, org).then((r) => r.instances),
  });
}

/** Templates matching words in the name, description or tags, and a tag. */
export function filterTemplates(all: TemplateSummary[], query: string, tag: string | null): TemplateSummary[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  return all.filter((t) => {
    if (tag && !t.tags.includes(tag)) return false;
    const hay = `${t.name} ${t.id} ${t.description} ${t.tags.join(" ")}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** Tags by how many templates carry them, most first. */
export function tagCounts(all: TemplateSummary[]): [string, number][] {
  const m = new Map<string, number>();
  for (const t of all) for (const g of t.tags) m.set(g, (m.get(g) ?? 0) + 1);
  return [...m.entries()].toSorted((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

export const varLabel = (v: Variable) => v.label || v.name.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

function isHostname(s: string): boolean {
  return (
    s.length <= 253 &&
    s.includes(".") &&
    s.split(".").every((l) => l.length > 0 && l.length <= 63 && !l.startsWith("-") && !l.endsWith("-") && /^[A-Za-z0-9-]+$/.test(l))
  );
}

/**
 * Why `value` is not acceptable for `v`, or null: the checks
 * src/template/mod.rs validate_input makes, so the form stops before the
 * server would. An empty value for a generated or defaulted variable is
 * fine (it is generated, or takes its default).
 */
export function variableProblem(v: Variable, value: string): string | null {
  const kind = v.type ?? "string";
  if (value === "") {
    if (v.required && !v.generated && kind !== "domain") return "Required.";
    return null;
  }
  const n = [...value].length;
  if (v.min_length !== undefined && n < v.min_length) return `At least ${v.min_length} characters.`;
  if (v.max_length !== undefined && n > v.max_length) return `At most ${v.max_length} characters.`;
  if (v.choices?.length && !v.choices.includes(value)) return `One of ${v.choices.join(", ")}.`;
  if (value.includes("\0")) return "No NUL characters.";
  switch (kind) {
    case "email": {
      const at = value.indexOf("@");
      const d = at > 0 ? value.slice(at + 1) : "";
      if (at <= 0 || !d.includes(".") || d.startsWith(".") || d.endsWith(".") || /\s/.test(value)) return "An email address.";
      return null;
    }
    case "url":
      if (!/^https?:\/\//.test(value) || /\s/.test(value)) return "An http(s) URL.";
      return null;
    case "int":
    case "port":
    case "timestamp": {
      if (!/^-?\d+$/.test(value)) return "A whole number.";
      const i = Number(value);
      const lo = kind === "port" ? 1 : v.min;
      const hi = kind === "port" ? 65535 : v.max;
      if ((lo !== undefined && i < lo) || (hi !== undefined && i > hi)) return `Between ${lo ?? "-"} and ${hi ?? "-"}.`;
      return null;
    }
    case "domain":
      if (!(value === "auto" || isHostname(value))) return "A hostname (example.com), or empty for a generated one.";
      return null;
    default:
      return null;
  }
}

/** Problems by variable name for a whole form. */
export function formProblems(vars: Variable[], values: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const v of vars) {
    const p = variableProblem(v, values[v.name] ?? "");
    if (p) out[v.name] = p;
  }
  return out;
}

/** Only what the person typed: empty fields are left to the server (generated or default). */
export function valuesToSend(values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(values).filter(([, v]) => v !== ""));
}

/** What an empty field becomes, in words, for its placeholder. */
export function emptyMeans(v: Variable): string {
  const kind = v.type ?? "string";
  if (kind === "domain") return "generated name";
  if (v.default !== undefined && v.default !== "") return v.default.includes("${") ? "computed default" : v.default;
  if (v.generated) {
    if (kind === "password") return `generated: ${v.length ?? 32} letters and digits`;
    if (kind === "hex" || kind === "base64") return `generated: ${v.bytes ?? 32} random bytes, ${kind}`;
    if (kind === "username") return "generated";
    return `generated ${kind}`;
  }
  return v.required ? "" : "optional";
}

export const initialsOf = (name: string) =>
  name
    .split(/[\s-]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("") || "?";

/** isb's cached copy of a template's logo, when it has one. */
export const logoSrc = (t: Pick<TemplateSummary, "catalog" | "id" | "logo">) =>
  t.logo ? `/api/v1/templates/${encodeURIComponent(t.catalog)}/${encodeURIComponent(t.id)}/logo` : undefined;
