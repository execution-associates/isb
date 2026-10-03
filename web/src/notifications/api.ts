// Notification channels (docs/guides/notifications.md). Shapes from
// src/notify/mod.rs and src/notify/provider.rs.
import { useQuery } from "@tanstack/react-query";
import { callTool } from "@/api/tools";

export type Provider =
  | { type: "webhook"; url_secret: string; signing_secret?: string }
  | { type: "slack"; url_secret: string }
  | { type: "discord"; url_secret: string }
  | { type: "telegram"; token_secret: string; chat_id: string }
  | {
      type: "email";
      host: string;
      port?: number;
      tls: "starttls" | "tls" | "none";
      username?: string;
      password_secret?: string;
      from: string;
      to: string[];
    };

export type ProviderType = Provider["type"];

export interface Rule {
  events: string[];
  projects?: string[];
  apps?: string[];
  stacks?: string[];
}

export interface Delivery {
  id: string;
  /** Unix milliseconds. */
  at: number;
  kind: string;
  seq: number;
  summary: string;
  status: "queued" | "retrying" | "sent" | "failed" | "dropped" | "skipped";
  attempts: number;
  http_status?: number;
  error?: string;
  finished_at?: number;
  test: boolean;
}

export interface Channel {
  name: string;
  provider: Provider;
  enabled: boolean;
  rules: Rule[];
  created_at: number;
  updated_at: number;
  last_delivery?: Delivery | null;
}

export const PROVIDERS: { id: ProviderType; label: string; hint: string }[] = [
  { id: "webhook", label: "Webhook", hint: "A signed JSON POST to any URL" },
  { id: "slack", label: "Slack", hint: "An incoming webhook" },
  { id: "discord", label: "Discord", hint: "A channel webhook" },
  { id: "telegram", label: "Telegram", hint: "A bot message to a chat" },
  { id: "email", label: "Email", hint: "Mail through an SMTP server" },
];

/** Event kinds by subject, as `Event::kind` lists them (src/stack/controller.rs). */
export const EVENT_GROUPS: { subject: string; label: string; kinds: { kind: string; label: string }[] }[] = [
  {
    subject: "deploy",
    label: "Deployments",
    kinds: [
      { kind: "deploy.succeeded", label: "Succeeded" },
      { kind: "deploy.failed", label: "Failed" },
    ],
  },
  {
    subject: "health",
    label: "Health",
    kinds: [
      { kind: "health.unhealthy", label: "Unhealthy" },
      { kind: "health.recovered", label: "Recovered" },
    ],
  },
  {
    subject: "backup",
    label: "Backups",
    kinds: [
      { kind: "backup.succeeded", label: "Succeeded" },
      { kind: "backup.failed", label: "Failed" },
    ],
  },
  {
    subject: "restore",
    label: "Restores",
    kinds: [
      { kind: "restore.succeeded", label: "Succeeded" },
      { kind: "restore.failed", label: "Failed" },
    ],
  },
  {
    subject: "job",
    label: "Jobs",
    kinds: [
      { kind: "job.succeeded", label: "Succeeded" },
      { kind: "job.failed", label: "Failed" },
    ],
  },
  {
    subject: "cert",
    label: "Certificates",
    kinds: [
      { kind: "cert.issued", label: "Issued" },
      { kind: "cert.failed", label: "Failed" },
    ],
  },
  {
    subject: "preview",
    label: "Previews",
    kinds: [
      { kind: "preview.created", label: "Created" },
      { kind: "preview.removed", label: "Removed" },
    ],
  },
];

export const ALL_KINDS = EVENT_GROUPS.flatMap((g) => g.kinds.map((k) => k.kind));

/** `*` matches any run of characters, as the daemon's rule globs do. */
export function globMatch(glob: string, s: string): boolean {
  const re = new RegExp(`^${glob.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`);
  return re.test(s);
}

/** The known kinds a rule's event globs select. */
export function selectedKinds(events: string[]): Set<string> {
  return new Set(ALL_KINDS.filter((k) => events.some((g) => globMatch(g, k))));
}

/**
 * The shortest globs for a set of kinds: `*` for all, `<subject>.*` for a
 * whole subject, `*.failed` when every failure (and nothing else of the
 * subject) is chosen, else the kinds themselves. Globs given that match no
 * known kind (a kind added later, say) are kept.
 */
export function compactEvents(kinds: Set<string>, keep: string[] = []): string[] {
  const extra = keep.filter((g) => !ALL_KINDS.some((k) => globMatch(g, k)));
  if (ALL_KINDS.every((k) => kinds.has(k))) return ["*"];
  const failed = ALL_KINDS.filter((k) => k.endsWith(".failed"));
  const allFailed = failed.every((k) => kinds.has(k));
  const out: string[] = [];
  if (allFailed && failed.length > 0) out.push("*.failed");
  for (const g of EVENT_GROUPS) {
    const ks = g.kinds.map((k) => k.kind);
    if (ks.every((k) => kinds.has(k))) {
      out.push(`${g.subject}.*`);
      continue;
    }
    for (const k of ks) if (kinds.has(k) && !(allFailed && k.endsWith(".failed"))) out.push(k);
  }
  return [...out, ...extra];
}

/** A comma or whitespace separated list, trimmed, without empties. */
export const splitList = (s: string) =>
  s
    .split(/[\s,]+/)
    .map((x) => x.trim())
    .filter(Boolean);

/** Build a rule from the form: kinds plus optional filters (empty = any). */
export function buildRule(kinds: Set<string>, filters: { projects?: string; apps?: string; stacks?: string }, keep: string[] = []): Rule {
  const r: Rule = { events: compactEvents(kinds, keep) };
  const p = splitList(filters.projects ?? "");
  const a = splitList(filters.apps ?? "");
  const s = splitList(filters.stacks ?? "");
  if (p.length) r.projects = p;
  if (a.length) r.apps = a;
  if (s.length) r.stacks = s;
  return r;
}

/** A rule in a few words, for listings. */
export function describeRule(r: Rule): string {
  const ev = r.events.length === 0 || r.events.includes("*") ? "every event" : r.events.join(", ");
  const f: string[] = [];
  if (r.projects?.length) f.push(`project ${r.projects.join(", ")}`);
  if (r.apps?.length) f.push(`app ${r.apps.join(", ")}`);
  if (r.stacks?.length) f.push(`stack ${r.stacks.join(", ")}`);
  return f.length ? `${ev} · ${f.join(" · ")}` : ev;
}

/** The secrets a provider names (for checking they exist). */
export function providerSecrets(p: Provider): string[] {
  switch (p.type) {
    case "webhook":
      return [p.url_secret, p.signing_secret].filter(Boolean) as string[];
    case "slack":
    case "discord":
      return [p.url_secret];
    case "telegram":
      return [p.token_secret];
    case "email":
      return p.password_secret ? [p.password_secret] : [];
  }
}

export function providerSummary(p: Provider): string {
  switch (p.type) {
    case "webhook":
      return `URL in ${p.url_secret}${p.signing_secret ? `, signed with ${p.signing_secret}` : ""}`;
    case "slack":
    case "discord":
      return `URL in ${p.url_secret}`;
    case "telegram":
      return `chat ${p.chat_id}, token in ${p.token_secret}`;
    case "email":
      return `${p.to.join(", ")} via ${p.host}${p.port ? `:${p.port}` : ""}`;
  }
}

export const nkeys = {
  channels: (org: string) => ["apps", org, "notify-channels"] as const,
  deliveries: (org: string, name: string) => ["apps", org, "notify-deliveries", name] as const,
};

export function useChannels(org: string) {
  return useQuery({
    queryKey: nkeys.channels(org),
    queryFn: () => callTool<{ channels: Channel[] }>("notification_channel_list", {}, org).then((r) => r.channels),
  });
}

export function useDeliveries(org: string, name: string | null) {
  return useQuery({
    queryKey: nkeys.deliveries(org, name ?? ""),
    enabled: !!name,
    refetchInterval: 5000,
    queryFn: () => callTool<{ deliveries: Delivery[] }>("notification_deliveries", { name: name ?? "" }, org).then((r) => r.deliveries),
  });
}

export function channelNameProblem(s: string): string | null {
  if (!s) return "Give the channel a name.";
  if (s.length > 40 || !/^[a-z][a-z0-9-]*$/.test(s)) return "Up to 40 characters of a-z, 0-9 and -, starting with a letter.";
  return null;
}
