// Connecting an agent to the daemon's MCP server (docs/guides/agents.md): the
// endpoint URLs, who may make an org token, and the install snippets per
// client. Pure, so the snippets are tested as text.
import type { AgentIdentity, Me } from "@/api/auth";

/** The org-bound endpoint: `org` is filled in, any other is refused. */
export const orgMcpUrl = (origin: string, org: string) => `${trimSlash(origin)}/orgs/${encodeURIComponent(org)}/mcp`;

/** The unbound endpoint: every tool takes `org`; a superadmin's reach. */
export const rootMcpUrl = (origin: string) => `${trimSlash(origin)}/mcp`;

const trimSlash = (s: string) => s.replace(/\/+$/, "");

/** The MCP page's two endpoints, a tab each for a superadmin. */
export type McpEndpoint = "superadmin" | "org";

/**
 * Which endpoint the MCP page shows, from `?endpoint=`: a superadmin gets
 * tabs, the org one first and chosen unless the URL says `superadmin`;
 * anyone else gets the org's alone, with no tabs (null).
 */
export function mcpEndpoint(superadmin: boolean, param: string | null): McpEndpoint | null {
  if (!superadmin) return null;
  return param === "superadmin" ? "superadmin" : "org";
}

/** A superadmin's tabs on the MCP page, in order, each linked by its `?endpoint=`. */
export function mcpEndpointTabs(org: string): { id: McpEndpoint; label: string; to: string }[] {
  const page = `/orgs/${encodeURIComponent(org)}/agents`;
  return [
    { id: "org", label: "Organization", to: `${page}?endpoint=org` },
    { id: "superadmin", label: "Superadmin", to: `${page}?endpoint=superadmin` },
  ];
}

/**
 * Whether an address is one only this machine or the tailnet reaches
 * (loopback, 100.64.0.0/10, fd7a:115c:a1e0::/48, *.ts.net), so no
 * Cloudflare Access is in front of it.
 */
export function isDirectHost(hostname: string): boolean {
  const h = hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (h === "localhost" || h === "::1" || h.endsWith(".localhost") || h.endsWith(".ts.net")) return true;
  if (h.startsWith("fd7a:115c:a1e0:")) return true;
  const m = h.match(/^(\d+)\.(\d+)\.\d+\.\d+$/);
  if (!m) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  return a === 127 || (a === 100 && b >= 64 && b <= 127);
}

/** A tailnet `--listen` address (`100.86.22.100:8092`), as host_policy lists them. */
export function isTailnetListen(addr: string): boolean {
  const host = addr.replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
  if (host.toLowerCase().startsWith("fd7a:115c:a1e0:")) return true;
  const m = host.match(/^100\.(\d+)\.\d+\.\d+$/);
  return !!m && Number(m[1]) >= 64 && Number(m[1]) <= 127;
}

/** `http://ADDR` for a listen address (IPv6 bracketed). */
export function listenOrigin(addr: string): string {
  const i = addr.lastIndexOf(":");
  const host = addr.slice(0, i);
  return host.includes(":") && !host.startsWith("[") ? `http://[${host}]${addr.slice(i)}` : `http://${addr}`;
}

/**
 * Why `me` cannot make an API token for `org` here, or null when it can
 * (the server's rule: an account, and a member of the org or a platform
 * admin; a narrowed token cannot mint).
 */
export function orgTokenBlocker(me: Me, org: string): string | null {
  if (me.superadmin && !me.superadmin.account) {
    return "You are signed in as a superadmin with no isb account, so you have no API tokens of your own. An org member makes one, or give the agent the superadmin MCP below.";
  }
  if (!me.platform_admin && !me.memberships.some((m) => m.org === org)) {
    return `You aren't a member of ${org}, so you can't make a token for it. Ask one of its admins to invite you, or to make the token.`;
  }
  if (me.auth.kind === "api_token") {
    const s = me.auth.scopes ?? [];
    if (s.length && !s.includes("admin")) return "You are signed in with a narrowed API token, which can't make tokens. Sign in to the web UI to make one.";
    if (me.auth.org && me.auth.org !== org) return `You are signed in with a token for ${me.auth.org}, which can't make one for ${org}.`;
  }
  return null;
}

/** What an org token made by `me` reaches, in a sentence. */
export function orgTokenReach(me: Me, org: string): string {
  const role = me.memberships.find((m) => m.org === org)?.role ?? (me.platform_admin ? "owner" : null);
  if (role === "viewer") return "You are a viewer here, so a token you make only reads: an agent with it can list and inspect, not deploy, exec or read secret values.";
  if (role === "owner" || role === "admin") return `It acts as you (${role}): it administers ${org}'s apps and secrets, and can manage its members and tokens. Narrow it with Access if the agent needs less.`;
  return `It acts as you (${role ?? "member"}): it administers ${org}'s apps and secrets, and nothing outside ${org}.`;
}

// ---- how an org's agent signs in ----

/** Who reaches every org with no mapping: a count for any member, names for owners and admins. */
export interface Reach {
  count: number;
  who: string[];
  /** The caller is one of them (absent for platform admins: `Me` says). */
  you?: boolean;
}

/** `GET orgs/ORG/agent-identities`: the org's mappings and which front doors the server has. */
export interface OrgAgentIdentities {
  identities: AgentIdentity[];
  available: {
    tailnet_listen: string[];
    access: boolean;
    /** The server's `--public-url`, null when unset. */
    public_url: string | null;
    reach: { platform_admins: Reach; access_superadmins: Reach; tailnet_superadmins: Reach };
  };
}

export type Way = "token" | "tailnet" | "access";

export interface WayState {
  id: Way;
  label: string;
  on: boolean;
  /** When off: what is missing, in a sentence. */
  why: string | null;
  /** When on: who gets in, in a sentence (no other user's email unless the caller manages the org). */
  works: string | null;
}

/** What `orgWays` needs to know of the viewer. */
export interface Viewer {
  platformAdmin: boolean;
  /** A member of the org. */
  member: boolean;
}

/** The roles an agent identity may have (never owner). */
export const AGENT_ROLES = ["viewer", "member", "admin"] as const;
export type AgentRole = (typeof AGENT_ROLES)[number];

export const ROLE_REACH: Record<AgentRole, string> = {
  viewer: "reads: lists and inspects, no secret values, no deploys, no exec",
  member: "administers the org's apps, stacks, sandboxes and secrets",
  admin: "member, and manages the org's members, invitations and tokens",
};

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const named = (r: Reach | undefined, n: number, one: string, many?: string) => (r?.who.length ? `${plural(n, one, many)} (${r.who.join(", ")})` : plural(n, one, many));

/**
 * The three ways an org's agent can sign in, with whether each is on. The
 * org token is always on. A tailnet identity needs a tailnet `--listen`
 * address and someone it admits: a tailnet mapping in the org, or a
 * `--superadmin-tailnet` login (which reaches every org). An Access identity
 * needs Access on a listener and someone it admits: a mapping (a service
 * token, or the email of someone who is not an isb user), a member user
 * (their email acts as them), a platform admin (who reaches every org) or a
 * `--superadmin-access` identity. `data` is undefined while it loads.
 */
export function orgWays(data: OrgAgentIdentities | undefined, memberCount: number, viewer: Viewer = { platformAdmin: false, member: false }): WayState[] {
  const maps = (k: AgentIdentity["kind"]) => (data?.identities ?? []).filter((i) => i.kind === k).length;
  const listens = data?.available.tailnet_listen ?? [];
  const accessOn = !!data?.available.access;
  const reach = data?.available.reach;
  const accessSuper = reach?.access_superadmins;
  const tailSuper = reach?.tailnet_superadmins;
  const admins = reach?.platform_admins;

  const tailnetWho: string[] = [];
  if (tailSuper?.you) tailnetWho.push("you (superadmin via the tailnet)");
  const otherTail = (tailSuper?.count ?? 0) - (tailSuper?.you ? 1 : 0);
  if (otherTail > 0) tailnetWho.push(`${named(tailSuper, otherTail, "other tailnet superadmin")}, who reach every org`);
  if (maps("tailnet") > 0) tailnetWho.push(plural(maps("tailnet"), "mapped login or tag", "mapped logins or tags"));
  const tailnetWhy: string[] = [];
  if (listens.length === 0) tailnetWhy.push("The server has no tailnet --listen address (start isb serve with --listen 100.x.y.z:PORT), so no tailnet peer can reach it.");
  if (tailnetWho.length === 0) tailnetWhy.push("No tailnet login or tag is mapped to a role in this org and no --superadmin-tailnet login is set: an owner or admin adds a mapping in Settings, Agent identities.");
  const tailnetOn = tailnetWhy.length === 0;

  const accessWho: string[] = [];
  if (accessSuper?.you) accessWho.push("you (superadmin via Access)");
  else if (viewer.platformAdmin) accessWho.push("you (platform admin)");
  else if (viewer.member) accessWho.push("you (a member)");
  const otherMembers = memberCount - (viewer.member ? 1 : 0);
  if (otherMembers > 0) accessWho.push(`${plural(otherMembers, "other member user")}, as themselves`);
  const otherAdmins = (admins?.count ?? 0) - (viewer.platformAdmin ? 1 : 0);
  if (otherAdmins > 0) accessWho.push(`${named(admins, otherAdmins, "other platform admin")}, who reach every org`);
  const otherSuper = (accessSuper?.count ?? 0) - (accessSuper?.you ? 1 : 0);
  if (otherSuper > 0) accessWho.push(`${named(accessSuper, otherSuper, "other Access superadmin")}, who reach every org`);
  if (maps("access") > 0) accessWho.push(plural(maps("access"), "mapped service token or email", "mapped service tokens or emails"));
  const accessWhy: string[] = [];
  if (!accessOn) accessWhy.push("Cloudflare Access does not guard a listener on this server (set CF_ACCESS_TEAM_DOMAIN and CF_ACCESS_AUD).");
  if (accessWho.length === 0) accessWhy.push("This org has no member users, no platform admin, no --superadmin-access identity and no Access service token or email mapped: an owner or admin adds one in Settings, Agent identities.");
  const accessReady = accessWhy.length === 0;

  return [
    { id: "token", label: "Org token", on: true, why: null, works: null },
    {
      id: "tailnet",
      label: "Tailnet identity",
      on: tailnetOn,
      why: tailnetWhy.join(" ") || null,
      works: tailnetOn ? `Tailnet sign-in works for: ${tailnetWho.join("; ")}. Any other node needs its login or tag mapped.` : null,
    },
    {
      id: "access",
      label: "Access identity",
      on: accessReady,
      why: accessWhy.join(" ") || null,
      works: accessReady ? `Access sign-in works for: ${accessWho.join("; ")}. Anyone else needs their email or service token mapped, or to be added as a member.` : null,
    },
  ];
}

/**
 * The MCP URL behind Cloudflare Access: the server's public URL, never the
 * address the page is open at (which may be a tailnet or loopback one Access
 * does not guard). Null when the server has no `--public-url`.
 */
export function publicMcpUrl(publicUrl: string | null | undefined, org: string): string | null {
  return publicUrl ? orgMcpUrl(publicUrl, org) : null;
}

/** The inline offer when a mapping's email is an isb user's: add them to the org instead. */
export interface MemberOffer {
  userId: number;
  email: string;
  label: string;
}

/**
 * `PUT agent-identities` answers 409 `is_user` (with the user's id and email)
 * for an Access email that is an isb user's. Offer to add that user to the
 * org as `role`, to owners and admins only; null otherwise.
 */
export function memberOffer(e: unknown, org: string, role: AgentRole, canManage: boolean): MemberOffer | null {
  if (!canManage || !e || typeof e !== "object") return null;
  const { code, data } = e as { code?: string; data?: { user_id?: unknown; email?: unknown } };
  if (code !== "is_user" || typeof data?.user_id !== "number" || typeof data.email !== "string") return null;
  return { userId: data.user_id, email: data.email, label: `Add ${data.email} to ${org} as ${role}` };
}

/** What a mapping's subject is: a login, a node tag, an email or a service token. */
export function subjectKind(i: Pick<AgentIdentity, "kind" | "subject">): string {
  if (i.kind === "tailnet") return i.subject.startsWith("tag:") ? "node tag" : "login";
  return i.subject.includes("@") ? "email" : "service token";
}

/** What a new mapping's subject looks like, as the form's placeholder. */
export const SUBJECT_EXAMPLE = { tailnet: "tag:agents or me@example.com", access: "abc123.access or bob@example.com" } as const;

/** What the form refuses before the server does, or null. */
export function subjectProblem(kind: AgentIdentity["kind"], raw: string): string | null {
  const s = raw.trim();
  if (!s) return "Name a login, tag, email or client id.";
  if (/[\s*?,]/.test(s)) return "Exact names only: no spaces, commas or wildcards.";
  if (kind === "tailnet" && !/^tag:[A-Za-z0-9_-]+$/i.test(s) && !/^[^@]+@[^@]+$/.test(s)) return "A tailnet login (someone@example.com) or a tag (tag:name).";
  if (kind === "access" && s.includes("@") && !/^[^@]+@[^@]+$/.test(s)) return "A whole email address, or a service token's client id.";
  return null;
}

// ---- install snippets ----

export type Client = "claude" | "codex" | "cursor" | "curl";

export const CLIENTS: { id: Client; label: string }[] = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "cursor", label: "Cursor / JSON" },
  { id: "curl", label: "curl test" },
];

export interface SnippetOptions {
  /** The name the client lists the server under, e.g. `isb-acme`. */
  name: string;
  url: string;
  /** The environment variable holding the bearer token; null sends none (a tailnet superadmin). */
  tokenVar: string | null;
  /** Add Cloudflare Access service token headers, read from CF_ACCESS_CLIENT_ID / CF_ACCESS_CLIENT_SECRET. */
  access: boolean;
}

export interface Block {
  /** A caption above the code: what the block is, or the file it goes in. */
  title: string;
  lang: "sh" | "json" | "toml";
  code: string;
}

export interface Snippet {
  blocks: Block[];
  /** A sentence under the blocks. */
  note: string;
}

export const ACCESS_ID_VAR = "CF_ACCESS_CLIENT_ID";
export const ACCESS_SECRET_VAR = "CF_ACCESS_CLIENT_SECRET";

/** Header name to value, with `ref(VAR)` standing for a variable reference in the client's syntax. */
function headers(o: SnippetOptions, ref: (v: string) => string): [string, string][] {
  const h: [string, string][] = [];
  if (o.tokenVar) h.push(["Authorization", `Bearer ${ref(o.tokenVar)}`]);
  if (o.access) {
    h.push(["CF-Access-Client-Id", ref(ACCESS_ID_VAR)]);
    h.push(["CF-Access-Client-Secret", ref(ACCESS_SECRET_VAR)]);
  }
  return h;
}

const json = (v: unknown) => JSON.stringify(v, null, 2);
const tomlStr = (s: string) => JSON.stringify(s);
/** A TOML key: bare when it can be, quoted otherwise. */
const tomlKey = (s: string) => (/^[A-Za-z0-9_-]+$/.test(s) ? s : tomlStr(s));

function serverJson(o: SnippetOptions, ref: (v: string) => string, type?: string) {
  const h = headers(o, ref);
  return {
    mcpServers: {
      [o.name]: {
        ...(type ? { type } : {}),
        url: o.url,
        ...(h.length ? { headers: Object.fromEntries(h) } : {}),
      },
    },
  };
}

export function claudeCode(o: SnippetOptions): Snippet {
  const h = headers(o, (v) => `$${v}`);
  const cmd = [`claude mcp add --transport http --scope user ${o.name} ${o.url}`, ...h.map(([k, v]) => `  --header "${k}: ${v}"`)].join(" \\\n");
  const vars = [o.tokenVar, ...(o.access ? [ACCESS_ID_VAR, ACCESS_SECRET_VAR] : [])].filter(Boolean).join(", ");
  return {
    blocks: [
      { title: "For you, in every project", lang: "sh", code: cmd },
      { title: "Or for a repository, in .mcp.json", lang: "json", code: json(serverJson(o, (v) => `\${${v}}`, "http")) },
    ],
    note: vars
      ? `The command reads ${vars} from your shell now and keeps the values in ~/.claude.json, outside any repository. .mcp.json keeps only the \${...} references, which Claude Code fills from its environment when it starts, so that file can be committed; the token itself never should be.`
      : "No token: the agent's machine is what signs it in. Check it with /mcp in Claude Code.",
  };
}

export function codex(o: SnippetOptions): Snippet {
  const lines = [`[mcp_servers.${tomlKey(o.name)}]`, `url = ${tomlStr(o.url)}`];
  if (o.tokenVar) lines.push(`bearer_token_env_var = ${tomlStr(o.tokenVar)}`);
  if (o.access) lines.push(`env_http_headers = { "CF-Access-Client-Id" = ${tomlStr(ACCESS_ID_VAR)}, "CF-Access-Client-Secret" = ${tomlStr(ACCESS_SECRET_VAR)} }`);
  const blocks: Block[] = [];
  if (!o.access) {
    blocks.push({
      title: "Add it",
      lang: "sh",
      code: `codex mcp add ${o.name} --url ${o.url}${o.tokenVar ? ` \\\n  --bearer-token-env-var ${o.tokenVar}` : ""}`,
    });
  }
  blocks.push({ title: o.access ? "In ~/.codex/config.toml" : "Or in ~/.codex/config.toml", lang: "toml", code: lines.join("\n") });
  return {
    blocks,
    note: o.tokenVar || o.access
      ? "Codex reads the variables from the environment it starts in on each launch; the config names them and holds no secret."
      : "No token: the agent's machine is what signs it in.",
  };
}

export function cursor(o: SnippetOptions): Snippet {
  return {
    blocks: [{ title: "In ~/.cursor/mcp.json (or .cursor/mcp.json in a project)", lang: "json", code: json(serverJson(o, (v) => `\${env:${v}}`)) }],
    note: "Other clients that take an mcpServers map use the same shape: a Streamable HTTP url and headers. Write the token as the client's environment reference where it has one, never as the value in a file you commit.",
  };
}

export function curl(o: SnippetOptions): Snippet {
  const h = [...headers(o, (v) => `$${v}`), ["Content-Type", "application/json"]];
  const code = [`curl -sS ${o.url}`, ...h.map(([k, v]) => `  -H "${k}: ${v}"`), `  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'`].join(" \\\n");
  return {
    blocks: [{ title: "List the tools", lang: "sh", code }],
    note: 'A JSON answer with "tools" means the URL and the credentials work. 401: the token is wrong, revoked or expired; 403 from Cloudflare: Access stopped it before isb.',
  };
}

export function snippet(client: Client, o: SnippetOptions): Snippet {
  switch (client) {
    case "claude":
      return claudeCode(o);
    case "codex":
      return codex(o);
    case "cursor":
      return cursor(o);
    case "curl":
      return curl(o);
  }
}

/** The line that puts a token in the environment (single-quoted; tokens are URL-safe). */
export const exportLine = (tokenVar: string, token: string) => `export ${tokenVar}='${token.replace(/'/g, `'\\''`)}'`;

// ---- the tool list (GET /api/v1/tools) ----

export interface ToolInfo {
  name: string;
  title?: string;
  description?: string;
  annotations?: { readOnlyHint?: boolean; destructiveHint?: boolean };
  /** Whether an org's /orgs/ORG/mcp lists the tool; false for host, superadmin and platform tools. */
  org_endpoint?: boolean;
}

/** Tools only /mcp lists, not an org's endpoint: host, superadmin and platform tools (docs/concepts/access.md). */
export const isHostTool = (t: ToolInfo) => (t.org_endpoint === undefined ? t.name.startsWith("host_") || t.name.startsWith("superadmin_") : !t.org_endpoint);

/** A description's first sentence, for a one-line list. */
export function firstSentence(s: string | undefined): string {
  if (!s) return "";
  const t = s.trim().split(/\n\s*\n/)[0].replace(/\s+/g, " ");
  const m = t.match(/^.*?[.!?](?=\s|$)/);
  return m ? m[0] : t;
}
