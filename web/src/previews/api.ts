// Preview deployments per pull request (docs/previews.md). Shapes from
// src/app/preview.rs and src/daemon/previews.rs.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";
import type { Deployment } from "@/apps/api";
import { splitStack } from "@/apps/live";
import { durationSeconds } from "@/jobs/api";

export interface PreviewSettings {
  enabled?: boolean;
  branches?: string[];
  max?: number;
  /** `.env` text. */
  env?: string;
  inherit_env?: boolean;
  domain?: string;
  port?: number;
  replicas?: number;
  resources?: { cpus?: string; memory?: string };
  ttl?: string;
  forks?: boolean;
  fork_secrets?: string[];
  status?: { token_secret: string; kind?: "github" | "gitea"; api_url?: string };
}

export interface Preview {
  app: string;
  number: number;
  stack: string;
  provider: string;
  fork: boolean;
  title: string;
  head_ref: string;
  base_ref: string;
  head_sha?: string;
  sha?: string;
  image?: string;
  url?: string;
  created_at: number;
  updated_at: number;
  current?: number;
  removing?: boolean;
  /** new, queued, building, deploying, done, failed, superseded, removing */
  status: string;
  last_deployment: Deployment | null;
}

/** What a preview event's message starts with (src/app/preview.rs pevent). */
export const previewPrefix = (app: string, n?: number) => (n === undefined ? `app ${app} preview #` : `app ${app} preview #${n}: `);

/** Whether a feed event is about `app`'s previews (or preview `n`) in `org`. */
export function isPreviewEvent(e: { stack: string; service?: string; message: string }, org: string, app: string, n?: number): boolean {
  return splitStack(e.stack).org === org && e.service === app && e.message.startsWith(previewPrefix(app, n));
}

export const pkeys = {
  list: (org: string, app: string) => ["apps", org, "previews", app] as const,
};

export function usePreviews(org: string, app: string, refetchInterval?: number | false) {
  return useQuery({
    queryKey: pkeys.list(org, app),
    queryFn: () => callTool<{ previews: Preview[] }>("preview_list", { name: app }, org).then((r) => r.previews),
    refetchInterval,
  });
}

/** The settings form's state, from saved settings. */
export interface PreviewForm {
  enabled: boolean;
  branches: string;
  max: string;
  replicas: string;
  domain: string;
  port: string;
  ttl: string;
  inherit_env: boolean;
  env: string;
  forks: boolean;
  fork_secrets: string[];
  status_secret: string;
  status_kind: "" | "github" | "gitea";
}

export function formOf(s: PreviewSettings | undefined): PreviewForm {
  return {
    enabled: !!s?.enabled,
    branches: (s?.branches ?? []).join(", "),
    max: String(s?.max ?? 3),
    replicas: String(s?.replicas ?? 1),
    domain: s?.domain ?? "auto",
    port: s?.port ? String(s.port) : "",
    ttl: s?.ttl ?? "",
    inherit_env: !!s?.inherit_env,
    env: s?.env ?? "",
    forks: !!s?.forks,
    fork_secrets: s?.fork_secrets ?? [],
    status_secret: s?.status?.token_secret ?? "",
    status_kind: s?.status?.kind ?? "",
  };
}

/** Form problems by field, as src/app/preview.rs validates. */
export function formProblems(f: PreviewForm, appPort?: number | null): Partial<Record<keyof PreviewForm, string>> {
  const p: Partial<Record<keyof PreviewForm, string>> = {};
  const int = (s: string) => (/^\d+$/.test(s.trim()) ? Number(s) : NaN);
  const max = int(f.max);
  if (!(max >= 1 && max <= 50)) p.max = "1 to 50.";
  const rep = int(f.replicas);
  if (!(rep >= 1 && rep <= 10)) p.replicas = "1 to 10.";
  const d = f.domain.trim();
  if (d && d !== "auto" && !/^\*\.[a-z0-9-]+(\.[a-z0-9-]+)+$/i.test(d)) p.domain = "auto, or *.preview.example.com.";
  if (f.port.trim()) {
    const n = int(f.port);
    if (!(n >= 1 && n <= 65535)) p.port = "1 to 65535.";
  }
  if (f.enabled && !f.port.trim() && !appPort) p.port = "Previews are served on a port: set this or the app's.";
  if (f.ttl.trim()) {
    const t = durationSeconds(f.ttl);
    if (t === null || t < 600) p.ttl = "Such as 36h or 7d (at least 10m).";
  }
  return p;
}

/** The settings to save, from the form (undefined fields are left out). */
export function settingsOf(f: PreviewForm): PreviewSettings {
  const branches = f.branches
    .split(/[\s,]+/)
    .map((b) => b.trim())
    .filter(Boolean);
  const s: PreviewSettings = {
    enabled: f.enabled,
    max: Number(f.max),
    replicas: Number(f.replicas),
    domain: f.domain.trim() || "auto",
    inherit_env: f.inherit_env,
    forks: f.forks,
  };
  if (branches.length) s.branches = branches;
  if (f.port.trim()) s.port = Number(f.port);
  if (f.ttl.trim()) s.ttl = f.ttl.trim();
  if (f.env.trim()) s.env = f.env;
  if (f.forks && f.fork_secrets.length) s.fork_secrets = f.fork_secrets;
  if (f.status_secret.trim()) s.status = { token_secret: f.status_secret.trim(), ...(f.status_kind ? { kind: f.status_kind } : {}) };
  return s;
}
