// MCP (/orgs/ORG/agents): how to connect an agent to the daemon's MCP
// server. The org endpoint for everyone in the org, with a token made here;
// the unbound /mcp endpoint for superadmins, whose credentials are made on
// the host only (docs/guides/agents.md, docs/concepts/access.md#superadmins).
// A superadmin sees the two as tabs (?endpoint=), the superadmin one first.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Bot, ChevronRight, Cloud, Crown, ExternalLink, KeyRound, Network, Plug, ShieldAlert, Terminal, Wrench } from "lucide-react";
import { type ReactNode, useId, useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";
import { type AgentIdentity, type ApiToken, auth, type Me } from "@/api/auth";
import { get } from "@/api/client";
import { callTool } from "@/api/tools";
import { TabLinks } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { Panel } from "@/components/confirm";
import { CopyField, CopyIconButton, Field, FormError, SubmitButton } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ACCESS, type Access, describeScopes, EXPIRY, maxGrant, scopesFor } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import {
  ACCESS_ID_VAR,
  ACCESS_SECRET_VAR,
  CLIENTS,
  type Client,
  exportLine,
  firstSentence,
  isDirectHost,
  isHostTool,
  isTailnetListen,
  listenOrigin,
  mcpEndpoint,
  mcpEndpointTabs,
  orgMcpUrl,
  orgWays,
  publicMcpUrl,
  type OrgAgentIdentities,
  ROLE_REACH,
  subjectKind,
  type AgentRole,
  type Way,
  type WayState,
  orgTokenBlocker,
  orgTokenReach,
  rootMcpUrl,
  snippet,
  type SnippetOptions,
  type ToolInfo,
} from "@/lib/mcp";
import { defaultOrg, superadminVia, useMe } from "@/lib/session";
import { cn } from "@/lib/utils";
import type { HostPolicy } from "@/pages/host";
import { useOrgPage } from "@/pages/org-common";
import { RolePill, RowsSkeleton, Tag } from "@/pages/org-ui";
import { agentIdentitiesKey } from "@/pages/org-agent-identities";

const DOCS = "https://github.com/execution-associates/isb/blob/main/docs";
const TOKEN_VAR = "ISB_TOKEN";
const SA_TOKEN_VAR = "ISB_SUPERADMIN_TOKEN";

/** Where this page was opened: the address the agent should use too. */
const origin = () => window.location.origin;

export function McpPage() {
  const { org, me, redirect } = useOrgPage();
  const [params] = useSearchParams();
  if (redirect) return redirect;
  // A superadmin picks an endpoint with tabs, the org one first; the rest
  // see the org's alone.
  const endpoint = mcpEndpoint(!!me.superadmin, params.get("endpoint"));
  return (
    <>
      <PageHeader
        title="MCP"
        description={
          endpoint ? (
            <>Connect an agent (Claude Code, Codex, Cursor, anything that speaks MCP) to isb: the unbound superadmin endpoint, or the endpoint for {org} with a token for the agent, and the lines that install it.</>
          ) : (
            <>Connect an agent (Claude Code, Codex, Cursor, anything that speaks MCP) to isb: the endpoint for {org}, a token for the agent, and the lines that install it.</>
          )
        }
      />
      {endpoint && <TabLinks tabs={mcpEndpointTabs(org)} active={endpoint} />}
      <div className="grid gap-6">
        {endpoint === "superadmin" ? (
          <SuperadminMcp me={me} />
        ) : (
          <>
            <OrgMcp me={me} org={org} />
            <ClaudeApps org={org} />
          </>
        )}
      </div>
    </>
  );
}

/** `/agents` with no org: the remembered org's page, or the superadmin part alone. */
export function McpHome() {
  const me = useMe().data!;
  const org = defaultOrg(me);
  if (org) return <Navigate to={`/orgs/${encodeURIComponent(org)}/agents`} replace />;
  if (!me.superadmin) return <PageHeader title="No orgs yet" description="You aren't a member of any org. Ask an org admin to invite you; its MCP page shows how to connect an agent." />;
  return (
    <>
      <PageHeader title="MCP" description="Connect an agent to isb. There are no orgs yet, so only the superadmin endpoint applies." />
      <SuperadminMcp me={me} />
    </>
  );
}

// ---- the org endpoint ----

function OrgMcp({ me, org }: { me: Me; org: string }) {
  const url = orgMcpUrl(origin(), org);
  const [way, setWay] = useState<Way>("token");
  const [access, setAccess] = useState(() => !isDirectHost(window.location.hostname));
  const [created, setCreated] = useState<{ token: string; info: ApiToken } | null>(null);
  const ids = useQuery({ queryKey: agentIdentitiesKey(org), queryFn: () => auth.agentIdentities(org) });
  const members = useQuery({ queryKey: ["members", org], queryFn: () => auth.members(org) });
  const ways = orgWays(ids.data, members.data?.members.length ?? 0, { platformAdmin: me.platform_admin, member: me.memberships.some((m) => m.org === org) });
  const opts: SnippetOptions = { name: `isb-${org}`, url, tokenVar: TOKEN_VAR, access };
  return (
    <>
      <Panel
        icon={<Plug />}
        title={
          <>
            Org MCP
            <Tag mono>{org}</Tag>
          </>
        }
        description={
          <>
            For an agent working in {org}: it administers {org}'s apps, deployments and secrets as its role allows, and reaches nothing outside {org}. Every tool's <code className="font-mono text-xs">org</code> is filled in.
          </>
        }
      >
        <div className="grid gap-5 p-5">
          <WayUrl way={way} org={org} pageUrl={url} data={ids.data} />
          <div className="grid gap-2">
            <div className="text-[13px] font-medium">How the agent signs in</div>
            <SourceTabs label="Agent sign-in" items={ways} value={way} onChange={setWay} loading={ids.isLoading} />
          </div>
          {ids.error && <FormError>{errorMessage(ids.error)}</FormError>}
        </div>
      </Panel>
      {way === "token" && (
        <>
          <Panel icon={<Plug />} title="Behind Cloudflare Access?">
            <div className="p-5">
              <AccessSwitch on={access} onChange={setAccess} />
            </div>
          </Panel>
          <TokenPanel me={me} org={org} created={created} onCreated={setCreated} />
          <Panel icon={<Terminal />} title="Install it" description="Pick the agent's client. Each reads the token from an environment variable, so no file holds it.">
            <div className="p-5">
              <Install opts={opts} token={created?.token} where={orgTokenBlocker(me, org) ? "a token an org member made" : "make one above"} />
            </div>
          </Panel>
        </>
      )}
      {way === "tailnet" && <OrgTailnet me={me} org={org} state={ways[1]} data={ids.data} loading={ids.isLoading} />}
      {way === "access" && <OrgAccess me={me} org={org} state={ways[2]} data={ids.data} loading={ids.isLoading} memberCount={members.data?.members.length ?? 0} />}
      <ToolList filter={(t) => !isHostTool(t)} title={`Tools at /orgs/${org}/mcp`} hint="What the server lists. A call is still judged by the caller's role and scopes: a viewer's or a read-only token's writes are refused. An org connector is scoped to this org even for a superadmin or platform admin, who act as an admin of the org here: host, superadmin and platform tools stay on the unbound /mcp." />
    </>
  );
}

/** The MCP URL for the chosen way in: the tailnet address for a tailnet agent, the public URL for an Access one, else the address this page is open at. */
function WayUrl({ way, org, pageUrl, data }: { way: Way; org: string; pageUrl: string; data: OrgAgentIdentities | undefined }) {
  const listens = data?.available.tailnet_listen ?? [];
  if (way === "tailnet" && listens[0]) {
    return <UrlRow url={orgMcpUrl(listenOrigin(listens[0]), org)} label="MCP URL on the tailnet" hint="The server's tailnet address: reachable from any node on the tailnet, and no Cloudflare Access in front." />;
  }
  const pub = publicMcpUrl(data?.available.public_url, org);
  if (way === "access" && pub) {
    return <UrlRow url={pub} label="MCP URL (the public URL)" hint="The server's public URL, where Cloudflare Access sits in front of it. An agent connects here, not at the address this page is open at." />;
  }
  return <UrlRow url={pageUrl} />;
}

/** The sign-in cards: each a way in, with On or Off; selecting one shows how. */
function SourceTabs<T extends string>({ label, items, value, onChange, loading }: { label: string; items: { id: T; label: string; on: boolean }[]; value: T; onChange: (v: T) => void; loading?: boolean }) {
  const icons: Record<string, typeof KeyRound> = { token: KeyRound, tailnet: Network, access: Cloud };
  return (
    <div role="tablist" aria-label={label} className="grid gap-2 sm:grid-cols-3">
      {items.map((s) => {
        const Icon = icons[s.id] ?? KeyRound;
        return (
          <button
            key={s.id}
            type="button"
            role="tab"
            aria-selected={value === s.id}
            onClick={() => onChange(s.id)}
            className={cn(
              "flex items-center gap-2.5 rounded-lg border px-3 py-2.5 text-left text-[13px] transition-colors hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
              value === s.id && "border-foreground/25 bg-muted/60 shadow-xs",
            )}
          >
            <Icon className="size-4 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 font-medium">{s.label}</span>
            {loading ? null : s.on ? <StatusBadge tone="success">On</StatusBadge> : <StatusBadge tone="muted">Off</StatusBadge>}
          </button>
        );
      })}
    </div>
  );
}

/** The org's mappings of one kind, and where to change them. */
function Mappings({ org, me, kind, data }: { org: string; me: Me; kind: AgentIdentity["kind"]; data: OrgAgentIdentities | undefined }) {
  const items = (data?.identities ?? []).filter((i) => i.kind === kind);
  const canManage = maxGrant(me, org) !== null;
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-1.5 text-[13px]">
        <span className="mr-1 text-muted-foreground">Mapped in {org}:</span>
        {items.length === 0 && <span className="text-muted-foreground">none</span>}
        {items.map((i) => (
          <span key={i.id} className="inline-flex items-center gap-1" title={`${subjectKind(i)}; ${ROLE_REACH[i.role as AgentRole]}`}>
            <Tag mono className="h-6 text-[12px] text-foreground/85">
              {i.subject}
            </Tag>
            <RolePill role={i.role} />
          </span>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">
        {canManage ? "Add or remove them in " : "The org's owners and admins add or remove them in "}
        <Link to={`/orgs/${encodeURIComponent(org)}/settings#agent-identities`} className="text-foreground underline-offset-4 hover:underline">
          Settings, Agent identities
        </Link>
        .
      </p>
    </div>
  );
}

function OrgTailnet({ me, org, state, data, loading }: { me: Me; org: string; state: WayState; data: OrgAgentIdentities | undefined; loading: boolean }) {
  const listens = data?.available.tailnet_listen ?? [];
  const url = listens[0] ? orgMcpUrl(listenOrigin(listens[0]), org) : null;
  return (
    <Panel icon={<Network />} title="Tailnet identity" description={`No token: an agent on a tailnet node that ${org} maps to a role is signed in by where it connects from.`}>
      <div className="grid gap-4 p-5">
        {loading ? (
          <RowsSkeleton rows={2} />
        ) : (
          <>
            <SourceHead>
              The node's login, or one of its tags, is mapped to a role in {org} only; a tagged node is its tags, never its owner's login. The server asks tailscaled who is connecting, so nothing the client sends can claim an identity. The client must send <code className="font-mono text-xs">Content-Type: application/json</code> and no foreign <code className="font-mono text-xs">Origin</code>, as MCP clients do.
            </SourceHead>
            <Mappings org={org} me={me} kind="tailnet" data={data} />
            <WayStatus state={state} />
            {url && (
              <>
                <UrlRow url={url} label="MCP URL on the tailnet" hint="The server's tailnet address: reachable from any node on the tailnet, and no Cloudflare Access in front." />
                <Install opts={{ name: `isb-${org}`, url, tokenVar: null, access: false }} envStep={false} />
              </>
            )}
          </>
        )}
      </div>
    </Panel>
  );
}

function OrgAccess({ me, org, state, data, loading, memberCount }: { me: Me; org: string; state: WayState; data: OrgAgentIdentities | undefined; loading: boolean; memberCount: number }) {
  const url = publicMcpUrl(data?.available.public_url, org);
  return (
    <Panel icon={<Cloud />} title="Access identity" description="No isb token: Cloudflare Access signs the identity, and isb trusts only a verified assertion.">
      <div className="grid gap-4 p-5">
        {loading ? (
          <RowsSkeleton rows={2} />
        ) : (
          <>
            <SourceHead>
              A headless agent uses an Access service token that {org} maps to a role (below), or one of Access's own policies for a person: someone whose email is an isb user acts as that user with their real roles ({memberCount} member{memberCount === 1 ? "" : "s"} here), and anyone else needs their email mapped. Put the service token's client id in Settings and its secret in the agent's environment.
            </SourceHead>
            <Mappings org={org} me={me} kind="access" data={data} />
            <WayStatus state={state} />
            {url ? (
              <>
                <UrlRow url={url} label="MCP URL (the public URL)" hint="The server's public URL, where Cloudflare Access sits in front of it. An agent connects here, not at the address this page is open at." />
                <Install opts={{ name: `isb-${org}`, url, tokenVar: null, access: true }} />
              </>
            ) : (
              <WhyOff>
                This server has no public URL, so there is no address Cloudflare Access sits in front of. Start <code className="font-mono text-xs">isb serve</code> with <code className="font-mono text-xs">--public-url https://your-host</code> (the address Access guards); the MCP URL is then <code className="font-mono text-xs">{"<public URL>"}/orgs/{org}/mcp</code>.
              </WhyOff>
            )}
          </>
        )}
      </div>
    </Panel>
  );
}

function WhyOff({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-dashed px-4 py-3 text-[13px] leading-relaxed text-muted-foreground">
      <StatusBadge tone="muted" className="mr-2">
        Off
      </StatusBadge>
      {children}
    </div>
  );
}

/** Why a way is off, or who it works for when it is on. */
function WayStatus({ state }: { state: WayState }) {
  if (!state.on) return <WhyOff>{state.why}</WhyOff>;
  if (!state.works) return null;
  return (
    <div className="rounded-lg border px-4 py-3 text-[13px] leading-relaxed text-muted-foreground">
      <StatusBadge tone="success" className="mr-2">
        On
      </StatusBadge>
      {state.works}
    </div>
  );
}

function UrlRow({ url, label = "MCP URL", hint }: { url: string; label?: string; hint?: string }) {
  return (
    <div className="grid gap-1.5">
      <div className="text-[13px] font-medium">{label}</div>
      <div className="flex min-w-0 items-center gap-1 rounded-md border bg-muted/40 py-1 pr-1 pl-3">
        <code className="min-w-0 flex-1 truncate font-mono text-[13px]" title={url}>
          {url}
        </code>
        <CopyIconButton value={url} label="Copy URL" />
      </div>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {hint ??
          "The address this page is open at. An agent on another machine needs an address that reaches this server from there: the public URL behind the tunnel, or the tailnet address."}
      </p>
    </div>
  );
}

function AccessSwitch({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  const id = useId();
  return (
    <div className="flex items-start gap-3 rounded-lg border px-4 py-3">
      <Switch id={id} checked={on} onCheckedChange={onChange} className="mt-0.5" />
      <label htmlFor={id} className="grid gap-0.5 text-[13px]">
        <span className="font-medium">Cloudflare Access is in front of this address</span>
        <span className="leading-relaxed text-muted-foreground">
          A headless agent must get through Access before isb sees it, with an Access service token the application's policy allows: the snippets add its <code className="font-mono text-xs">CF-Access-Client-Id</code> and <code className="font-mono text-xs">CF-Access-Client-Secret</code> headers from {ACCESS_ID_VAR} and {ACCESS_SECRET_VAR}. Off for localhost and tailnet addresses.
        </span>
      </label>
    </div>
  );
}

function TokenPanel({ me, org, created, onCreated }: { me: Me; org: string; created: { token: string; info: ApiToken } | null; onCreated: (c: { token: string; info: ApiToken } | null) => void }) {
  const qc = useQueryClient();
  const blocker = orgTokenBlocker(me, org);
  const [name, setName] = useState("agent");
  const [expires, setExpires] = useState("90d");
  const [access, setAccess] = useState<Access>("full");
  const [tools, setTools] = useState("");
  const [error, setError] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: () => {
      const s = scopesFor(access, tools);
      if ("error" in s) throw new Error(s.error);
      return auth.createToken({ name: name.trim(), org, expires: expires === "never" ? undefined : expires, scopes: s.scopes.length ? s.scopes : undefined });
    },
    onSuccess: (r) => {
      onCreated(r);
      setError(null);
      void qc.invalidateQueries({ queryKey: ["tokens"] });
      void qc.invalidateQueries({ queryKey: ["org-tokens"] });
    },
    onError: (e) => setError(errorMessage(e)),
  });
  return (
    <Panel
      icon={<KeyRound />}
      title="A token for the agent"
      description={
        <>
          One per agent, so you can revoke it alone. Yours are listed under{" "}
          <Link to="/account#tokens" className="text-foreground underline-offset-4 hover:underline">
            Account
          </Link>
          ; the org's owners and admins see every token in the org on Members.
        </>
      }
    >
      <div className="grid gap-4 p-5">
        {blocker ? (
          <p className="text-[13px] leading-relaxed text-muted-foreground">{blocker}</p>
        ) : created ? (
          <div className="grid gap-3">
            <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
              <StatusBadge tone="success">Created</StatusBadge>
              <span>{created.info.name}</span>
              <Tag>{created.info.org}</Tag>
              <Tag>{describeScopes(created.info.scopes)}</Tag>
            </div>
            <p className="text-[13px] text-muted-foreground">This is the only time it's shown. Put it in the agent's environment or a secret manager; never commit it. The snippets below use it.</p>
            <CopyField value={created.token} label="Copy token" />
            <button type="button" className="justify-self-start text-[13px] text-muted-foreground underline-offset-4 hover:text-foreground hover:underline" onClick={() => onCreated(null)}>
              Make another
            </button>
          </div>
        ) : (
          <form
            className="grid gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              setError(null);
              create.mutate();
            }}
          >
            <p className="text-[13px] leading-relaxed text-muted-foreground">{orgTokenReach(me, org)}</p>
            <FormError>{error}</FormError>
            <div className="grid gap-4 sm:grid-cols-3">
              <Field label="Name">
                {(id) => <Input id={id} required maxLength={100} placeholder="What uses it" value={name} onChange={(e) => setName(e.target.value)} />}
              </Field>
              <Field label="Access">
                {(id) => (
                  <Select value={access} onValueChange={(v) => setAccess(v as Access)}>
                    <SelectTrigger id={id} className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {ACCESS.map((a) => (
                        <SelectItem key={a.value} value={a.value}>
                          {a.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </Field>
              <Field label="Expires">
                {(id) => (
                  <Select value={expires} onValueChange={setExpires}>
                    <SelectTrigger id={id} className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {EXPIRY.map((x) => (
                        <SelectItem key={x.value} value={x.value}>
                          {x.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </Field>
            </div>
            <p className="-mt-2 text-xs text-muted-foreground">{ACCESS.find((a) => a.value === access)?.hint}</p>
            {access === "tools" && (
              <Field label="Tools" hint="Names or globs, separated by spaces.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono text-sm" placeholder="app_* stack_status" value={tools} onChange={(e) => setTools(e.target.value)} />}
              </Field>
            )}
            <div>
              <SubmitButton pending={create.isPending} disabled={!name.trim()}>
                <KeyRound />
                Create token for {org}
              </SubmitButton>
            </div>
          </form>
        )}
      </div>
    </Panel>
  );
}

// ---- snippets ----

export function CodeBlock({ title, code, className }: { title: ReactNode; code: string; className?: string }) {
  return (
    <div className={cn("grid min-w-0 gap-1.5", className)}>
      <div className="text-xs font-medium text-muted-foreground">{title}</div>
      <div className="relative min-w-0 rounded-lg border bg-muted/40">
        <pre className="overflow-x-auto py-3 pr-11 pl-3.5 font-mono text-[12px] leading-relaxed whitespace-pre text-foreground/90">{code}</pre>
        <CopyIconButton value={code} label="Copy" className="absolute top-1.5 right-1.5 bg-card/90 shadow-xs backdrop-blur-sm" />
      </div>
    </div>
  );
}

/** The client tabs and their blocks, after the environment step. */
export function Install({ opts, token, placeholder = "isb_tok_...", where, envStep = true }: { opts: SnippetOptions; token?: string; placeholder?: string; where?: string; envStep?: boolean }) {
  const [client, setClient] = useState<Client>("claude");
  const s = snippet(client, opts);
  const env = [
    ...(opts.tokenVar ? [exportLine(opts.tokenVar, token ?? placeholder)] : []),
    ...(opts.access ? [`export ${ACCESS_ID_VAR}='....access'`, `export ${ACCESS_SECRET_VAR}='...'`] : []),
  ];
  return (
    <div className="grid min-w-0 gap-4">
      {envStep && env.length > 0 && (
        <CodeBlock
          title={token ? "1. Put the token in the agent's environment (this is the token you just made)" : `1. Put the token in the agent's environment${where ? ` (${where})` : ""}`}
          code={env.join("\n")}
        />
      )}
      <div className="grid min-w-0 gap-3">
        {envStep && env.length > 0 && <div className="text-xs font-medium text-muted-foreground">2. Add the server to the client</div>}
        <div role="tablist" aria-label="Client" className="grid grid-cols-2 gap-1 rounded-lg border bg-muted/40 p-1 sm:flex sm:w-fit">
          {CLIENTS.map((c) => (
            <button
              key={c.id}
              type="button"
              role="tab"
              aria-selected={client === c.id}
              onClick={() => setClient(c.id)}
              className={cn(
                "h-7 rounded-md px-3 text-[13px] font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none sm:flex-none",
                client === c.id && "bg-background text-foreground shadow-xs",
              )}
            >
              {c.label}
            </button>
          ))}
        </div>
        <div role="tabpanel" className="grid min-w-0 gap-3">
          {s.blocks.map((b) => (
            <CodeBlock key={b.title} title={b.title} code={b.code} />
          ))}
          <p className="text-xs leading-relaxed text-muted-foreground">{s.note}</p>
        </div>
      </div>
    </div>
  );
}

// ---- the tools ----

function ToolList({ filter, title, hint }: { filter: (t: ToolInfo) => boolean; title: string; hint: string }) {
  const q = useQuery({ queryKey: ["mcp-tools"], queryFn: () => get<{ tools: ToolInfo[] }>("/api/v1/tools").then((r) => r.tools), staleTime: 5 * 60_000 });
  const tools = (q.data ?? []).filter(filter);
  return (
    <details className="group min-w-0 overflow-hidden rounded-xl border bg-card shadow-xs">
      <summary className="flex cursor-pointer list-none items-center gap-3 px-5 py-4 select-none [&::-webkit-details-marker]:hidden">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground [&_svg]:size-4">
          <Wrench />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-2 text-[15px] font-semibold tracking-tight">
            {title}
            {q.data && <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-muted px-1.5 text-[11px] font-medium text-muted-foreground tabular-nums">{tools.length}</span>}
          </span>
          <span className="block text-[13px] leading-relaxed text-muted-foreground">{hint}</span>
        </span>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground transition-transform group-open:rotate-90" />
      </summary>
      <div className="border-t">
        {q.isLoading ? (
          <RowsSkeleton />
        ) : q.error ? (
          <div className="p-5">
            <FormError>{errorMessage(q.error)}</FormError>
          </div>
        ) : (
          <ul className="divide-y">
            {tools.map((t) => (
              <li key={t.name} className="flex min-w-0 flex-col gap-0.5 px-5 py-2.5 sm:flex-row sm:items-baseline sm:gap-4">
                <span className="flex shrink-0 items-center gap-2 sm:w-64">
                  <code className="truncate font-mono text-[12.5px] font-medium">{t.name}</code>
                  {t.annotations?.readOnlyHint && <Tag>read</Tag>}
                </span>
                <span className="min-w-0 text-[13px] text-muted-foreground">{firstSentence(t.description) || t.title}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </details>
  );
}

// ---- superadmins ----

type Source = "token" | "tailnet" | "access";

function SuperadminMcp({ me }: { me: Me }) {
  const policy = useQuery({ queryKey: ["tool", "host_policy"], queryFn: () => callTool<HostPolicy>("host_policy") });
  const [source, setSource] = useState<Source>("token");
  const p = policy.data;
  const tailnetListens = (p?.listen ?? []).filter(isTailnetListen);
  const behindAccess = !isDirectHost(window.location.hostname);
  const sources: { id: Source; label: string; on: boolean }[] = [
    { id: "token", label: "Superadmin token", on: true },
    { id: "tailnet", label: "Tailnet identity", on: !!p?.superadmin.tailnet },
    { id: "access", label: "Access identity", on: !!p?.superadmin.access },
  ];
  return (
    <>
      <Panel
        icon={<Crown />}
        title={
          <>
            Superadmin MCP
            <StatusBadge tone="warning">Superadmin</StatusBadge>
          </>
        }
        description={<>Shown because you are a superadmin ({superadminVia(me)}). The unbound endpoint: every tool, every org (each call names its org), no remote-spec policy, any instance, and the host tools.</>}
      >
        <div className="grid gap-5 p-5">
          <Alert className="border-warning/40 bg-warning/[0.06]">
            <ShieldAlert className="text-warning" />
            <AlertTitle>Root on this host, in effect</AlertTitle>
            <AlertDescription className="leading-relaxed">
              A superadmin has what the daemon's unix socket has: privileged containers, raw incus config and host bind mounts if it asks. Give it only to an agent you would hand a root shell on this machine, and prefer an org token for everything else.
            </AlertDescription>
          </Alert>
          <div className="grid gap-2">
            <div className="text-[13px] font-medium">How the agent becomes a superadmin</div>
            <SourceTabs label="Superadmin source" items={sources} value={source} onChange={setSource} loading={policy.isLoading} />
          </div>
          {policy.error && <FormError>{errorMessage(policy.error)}</FormError>}
          {source === "token" && <TokenSource behindAccess={behindAccess} />}
          {source === "tailnet" && (policy.isLoading ? <RowsSkeleton rows={2} /> : <TailnetSource allow={p?.superadmin.tailnet ?? null} listens={tailnetListens} />)}
          {source === "access" && (policy.isLoading ? <RowsSkeleton rows={2} /> : <AccessSource allow={p?.superadmin.access ?? null} publicUrl={p?.public_url ?? null} />)}
        </div>
      </Panel>
      <ToolList filter={(t) => isHostTool(t)} title="Host and platform tools, /mcp only" hint="On top of every tool an org endpoint lists, at /mcp only: the host and superadmin tools for a superadmin, the platform tools (orgs, users) for a platform admin." />
    </>
  );
}

function SourceHead({ children }: { children: ReactNode }) {
  return <p className="text-[13px] leading-relaxed text-muted-foreground">{children}</p>;
}

function TokenSource({ behindAccess }: { behindAccess: boolean }) {
  const [access, setAccess] = useState(behindAccess);
  const url = rootMcpUrl(origin());
  return (
    <div className="grid gap-4">
      <SourceHead>
        A superadmin token belongs to nobody and is minted only on the host, as the daemon's user; nothing on the web can make one, a superadmin included. Revoke it on{" "}
        <Link to="/host/superadmins" className="text-foreground underline-offset-4 hover:underline">
          Host
        </Link>
        .
      </SourceHead>
      <CodeBlock title="On the host" code={`isb token create NAME --superadmin --expires 30d`} />
      <UrlRow url={url} />
      <AccessSwitch on={access} onChange={setAccess} />
      <Install opts={{ name: "isb", url, tokenVar: SA_TOKEN_VAR, access }} placeholder="isb_sa_..." where="the one minted on the host" />
    </div>
  );
}

function Off({ flag, children }: { flag: string; children: ReactNode }) {
  return (
    <div className="rounded-lg border border-dashed px-4 py-3 text-[13px] leading-relaxed text-muted-foreground">
      <StatusBadge tone="muted" className="mr-2">
        Off
      </StatusBadge>
      {children} It is turned on with <code className="font-mono text-xs">{flag}</code> on <code className="font-mono text-xs">isb serve</code> (docs/reference/configuration.md).
    </div>
  );
}

function TailnetSource({ allow, listens }: { allow: string[] | null; listens: string[] }) {
  if (!allow) return <Off flag="--superadmin-tailnet">No tailnet identity is a superadmin.</Off>;
  return (
    <div className="grid gap-4">
      <SourceHead>
        No token: an agent running on a tailnet node whose login, or one of whose tags, is on the list is a superadmin by where it connects from. A tagged node is its tags, never its owner's login. The client must send <code className="font-mono text-xs">Content-Type: application/json</code> and no foreign <code className="font-mono text-xs">Origin</code>, as MCP clients do.
      </SourceHead>
      <div className="flex flex-wrap items-center gap-1.5 text-[13px]">
        <span className="mr-1 text-muted-foreground">Allowed:</span>
        {allow.map((a) => (
          <Tag key={a} mono className="h-6 text-[12px] text-foreground/85">
            {a}
          </Tag>
        ))}
      </div>
      {listens.length === 0 ? (
        <Off flag="--listen 100.x.y.z:PORT">The daemon has no tailnet listen address, so no tailnet peer reaches it.</Off>
      ) : (
        <>
          <UrlRow url={rootMcpUrl(listenOrigin(listens[0]))} label="MCP URL on the tailnet" />
          <Install opts={{ name: "isb", url: rootMcpUrl(listenOrigin(listens[0])), tokenVar: null, access: false }} envStep={false} />
        </>
      )}
    </div>
  );
}

function AccessSource({ allow, publicUrl }: { allow: string[] | null; publicUrl: string | null }) {
  if (!allow) return <Off flag="--superadmin-access">No Cloudflare Access identity is a superadmin.</Off>;
  const url = rootMcpUrl(publicUrl ?? origin());
  return (
    <div className="grid gap-4">
      <SourceHead>
        No isb token: Cloudflare Access signs the identity, and isb trusts only a verified assertion. People on the list sign in through Access (an OAuth client, below); a headless agent uses one of the listed service tokens.
      </SourceHead>
      <div className="flex flex-wrap items-center gap-1.5 text-[13px]">
        <span className="mr-1 text-muted-foreground">Allowed:</span>
        {allow.map((a) => (
          <Tag key={a} mono className="h-6 text-[12px] text-foreground/85">
            {a}
          </Tag>
        ))}
      </div>
      <UrlRow url={url} label="MCP URL (the public URL)" />
      <Install opts={{ name: "isb", url, tokenVar: null, access: true }} />
    </div>
  );
}

// ---- claude.ai and Claude Desktop ----

function ClaudeApps({ org }: { org: string }) {
  return (
    <Panel
      icon={<Bot />}
      title="claude.ai, Claude Desktop and ChatGPT"
      description={
        <>
          Their custom connectors sign in with OAuth and can't send a bearer token. isb has no OAuth server of its own: it relies on Cloudflare Access Managed OAuth in front of the public URL. With that set up, add <code className="font-mono text-xs">{"<public URL>"}/orgs/{org}/mcp</code> as a connector; the first connection opens the Access login, and the connector acts as the isb user with that email.{" "}
          <a href={`${DOCS}/guides/remote-access.md#cloudflare-tunnel-and-access`} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-foreground underline-offset-4 hover:underline">
            Setting it up
            <ExternalLink className="size-3" />
          </a>
        </>
      }
    />
  );
}
