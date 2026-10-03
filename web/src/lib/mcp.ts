// Connecting an agent to the daemon's MCP server (docs/guides/agents.md): the
// endpoint URLs, who may make an org token, and the install snippets per
// client. Pure, so the snippets are tested as text.
import type { Me } from "@/api/auth";

/** The org-bound endpoint: `org` is filled in, any other is refused. */
export const orgMcpUrl = (origin: string, org: string) => `${trimSlash(origin)}/orgs/${encodeURIComponent(org)}/mcp`;

/** The unbound endpoint: every tool takes `org`; a superadmin's reach. */
export const rootMcpUrl = (origin: string) => `${trimSlash(origin)}/mcp`;

const trimSlash = (s: string) => s.replace(/\/+$/, "");

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
}

/** Tools only a superadmin gets (docs/concepts/access.md). */
export const isHostTool = (name: string) => name.startsWith("host_") || name.startsWith("superadmin_");

/** A description's first sentence, for a one-line list. */
export function firstSentence(s: string | undefined): string {
  if (!s) return "";
  const t = s.trim().split(/\n\s*\n/)[0].replace(/\s+/g, " ");
  const m = t.match(/^.*?[.!?](?=\s|$)/);
  return m ? m[0] : t;
}
