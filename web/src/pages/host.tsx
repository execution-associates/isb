import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Boxes, Crown, FolderTree, HardDrive, KeyRound, Network, ShieldCheck, SlidersHorizontal, Terminal, Trash2 } from "lucide-react";
import { type ReactNode, useMemo, useState } from "react";
import { Navigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { TabLinks } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { CopyIconButton, FormError } from "@/components/form";
import { StatusBadge, StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { superadminVia, useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { RowsSkeleton, Tag } from "@/pages/org-ui";

// Results of the superadmin-only tools (src/daemon/superadmin.rs).
export interface HostProject {
  name: string;
  description: string | null;
  org: string | null;
  instances: number;
}

export interface HostInstance {
  name: string;
  project: string;
  org: string | null;
  type: "container" | "virtual-machine";
  status: string;
  created_at: string;
  image: string | null;
  addresses: string[];
  stack: string | null;
  owner: string | null;
  managed: boolean;
}

export interface HostPolicy {
  isb: string;
  listen: string[];
  socket: string;
  state_dir: string;
  public_url: string | null;
  access: { team_domain: string; aud: string } | null;
  allow_unauthenticated: boolean;
  tools: { allow: string[]; deny: string[] };
  policy: {
    allow_privileged: boolean;
    allow_raw: boolean;
    bind_roots: string[];
    publish_addresses: string[];
    any_instance: boolean;
  };
  superadmin: {
    socket: string;
    tokens: boolean;
    token_count: number;
    tailnet: string[] | null;
    tailnet_hosts: string[] | null;
    access: string[] | null;
  };
}

export interface SuperadminToken {
  id: number;
  name: string;
  created_at: number;
  last_used: number | null;
  expires_at: number | null;
}

const TABS = [
  { id: "instances", label: "Instances", icon: Boxes },
  { id: "policy", label: "Serve policy", icon: SlidersHorizontal },
  { id: "superadmins", label: "Superadmins", icon: Crown },
] as const;

/** The host itself: what only a superadmin (the unix socket's reach) sees. */
export function HostPage() {
  const me = useMe().data!;
  const { tab = "instances" } = useParams();
  if (!me.superadmin) return <Navigate to="/" replace />;
  if (!TABS.some((t) => t.id === tab)) return <Navigate to="/host" replace />;
  return (
    <>
      <PageHeader
        title={
          <>
            Host
            <StatusBadge tone="warning">Superadmin</StatusBadge>
          </>
        }
        description={<>Every incus instance and project on this host, how the daemon serves, and who has the unix socket's reach. You are here by {superadminVia(me)}.</>}
      />
      <TabLinks tabs={TABS.map((t) => ({ ...t, to: t.id === "instances" ? "/host" : `/host/${t.id}` }))} active={tab} />
      {tab === "instances" && <InstancesTab />}
      {tab === "policy" && <PolicyTab />}
      {tab === "superadmins" && <SuperadminsTab />}
    </>
  );
}

function statusTone(s: string): Tone {
  switch (s.toLowerCase()) {
    case "running":
      return "success";
    case "stopped":
      return "muted";
    case "error":
      return "danger";
    default:
      return "warning";
  }
}

function InstancesTab() {
  const inv = useQuery({
    queryKey: ["tool", "host_inventory"],
    queryFn: () => callTool<{ projects: HostProject[]; instances: HostInstance[] }>("host_inventory"),
    refetchInterval: 15_000,
  });
  const [q, setQ] = useState("");
  const [project, setProject] = useState<string | null>(null);
  const instances = useMemo(() => {
    const all = inv.data?.instances ?? [];
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    return all
      .filter((i) => !project || i.project === project)
      .filter((i) => words.every((w) => [i.name, i.project, i.org, i.stack, i.owner, i.image, i.status, ...i.addresses].some((f) => f?.toLowerCase().includes(w))))
      .sort((a, b) => a.project.localeCompare(b.project) || a.name.localeCompare(b.name));
  }, [inv.data, q, project]);
  if (inv.error)
    return (
      <Panel title="Instances">
        <div className="p-5">
          <FormError>{errorMessage(inv.error)}</FormError>
        </div>
      </Panel>
    );
  const projects = inv.data?.projects ?? [];
  const running = (inv.data?.instances ?? []).filter((i) => i.status === "Running").length;
  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <Stat icon={<FolderTree />} label="Projects" value={inv.data ? projects.length : null} hint={inv.data ? `${projects.filter((p) => p.org).length} are isb orgs` : undefined} />
        <Stat icon={<Boxes />} label="Instances" value={inv.data ? inv.data.instances.length : null} hint={inv.data ? `${running} running` : undefined} />
        <Stat icon={<HardDrive />} label="Not isb's" value={inv.data ? inv.data.instances.filter((i) => !i.org && !i.managed).length : null} hint="outside an org, unlabelled" />
      </div>
      <Panel title="Projects" count={inv.data ? projects.length : undefined} description="Incus projects; an isb org is the project isb-<org> (the default org may be incus' default project).">
        {inv.isLoading ? (
          <RowsSkeleton />
        ) : (
          <div className="flex flex-wrap gap-2 p-4">
            <ProjectChip active={project === null} onClick={() => setProject(null)} label="All" count={inv.data?.instances.length ?? 0} />
            {projects.map((p) => (
              <ProjectChip
                key={p.name}
                active={project === p.name}
                onClick={() => setProject(project === p.name ? null : p.name)}
                label={p.name}
                org={p.org}
                count={(inv.data?.instances ?? []).filter((i) => i.project === p.name).length}
              />
            ))}
          </div>
        )}
      </Panel>
      <Panel
        title="Instances"
        count={inv.data ? instances.length : undefined}
        description="Every container and VM, isb's or not."
        action={<Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Filter by name, project, label, address" className="h-8 w-full sm:w-72" aria-label="Filter instances" />}
      >
        {inv.isLoading ? (
          <RowsSkeleton rows={6} />
        ) : instances.length === 0 ? (
          <Empty icon={<Boxes />} title={q || project ? "Nothing matches" : "No instances"} />
        ) : (
          <Table>
            <TableHeader className="bg-muted/30">
              <TableRow className="hover:bg-transparent">
                <TableHead className="pl-5">Instance</TableHead>
                <TableHead className="hidden sm:table-cell">Project</TableHead>
                <TableHead className="hidden md:table-cell">isb</TableHead>
                <TableHead className="hidden lg:table-cell">Address</TableHead>
                <TableHead className="pr-5 text-right">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {instances.map((i) => (
                <TableRow key={`${i.project}/${i.name}`}>
                  <TableCell className="py-2.5 pl-5">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="truncate font-medium">{i.name}</span>
                      {i.type === "virtual-machine" && <Tag>VM</Tag>}
                    </div>
                    <div className="truncate text-xs text-muted-foreground" title={i.image ?? undefined}>
                      {i.image ?? "no image description"}
                      <span className="sm:hidden"> · {i.project}</span>
                    </div>
                  </TableCell>
                  <TableCell className="hidden sm:table-cell">
                    <span className="font-mono text-[12px]">{i.project}</span>
                    {i.org && <div className="text-xs text-muted-foreground">org {i.org}</div>}
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <div className="flex flex-wrap gap-1">
                      {i.stack && <Tag mono title="user.isb.stack">stack {i.stack}</Tag>}
                      {i.owner && <Tag mono title="user.isb.owner">{i.owner}</Tag>}
                      {!i.stack && !i.owner && <span className="text-xs text-muted-foreground">{i.org ? "in an org" : "not isb's"}</span>}
                    </div>
                  </TableCell>
                  <TableCell className="hidden font-mono text-[12px] lg:table-cell">{i.addresses[0] ?? "-"}</TableCell>
                  <TableCell className="pr-5 text-right">
                    <StatusBadge tone={statusTone(i.status)}>{i.status}</StatusBadge>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </Panel>
    </div>
  );
}

function ProjectChip({ label, count, org, active, onClick }: { label: string; count: number; org?: string | null; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={
        "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-[12px] transition-colors focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none " +
        (active ? "border-foreground/20 bg-foreground text-background" : "bg-background text-foreground/80 hover:bg-muted")
      }
    >
      <span className="font-mono">{label}</span>
      {org && <span className={active ? "text-background/70" : "text-muted-foreground"}>org</span>}
      <span className={"tabular-nums " + (active ? "text-background/70" : "text-muted-foreground")}>{count}</span>
    </button>
  );
}

function Stat({ icon, label, value, hint }: { icon: ReactNode; label: string; value: number | null; hint?: string }) {
  return (
    <div className="flex min-w-0 items-center gap-3 rounded-xl border bg-card px-4 py-3.5 shadow-xs">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground [&_svg]:size-4">{icon}</span>
      <div className="min-w-0">
        <div className="text-xs text-muted-foreground">{label}</div>
        <div className="text-lg leading-tight font-semibold tabular-nums">{value ?? "–"}</div>
        {hint && <div className="truncate text-[11px] text-muted-foreground">{hint}</div>}
      </div>
    </div>
  );
}

function usePolicy() {
  return useQuery({ queryKey: ["tool", "host_policy"], queryFn: () => callTool<HostPolicy>("host_policy") });
}

/** One setting: a label, its value, and what it means. */
function Row({ label, children, hint }: { label: string; children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="grid gap-1 px-5 py-3 sm:grid-cols-[13rem_minmax(0,1fr)] sm:gap-4">
      <div className="text-[13px] font-medium">{label}</div>
      <div className="min-w-0 space-y-1 text-[13px]">
        <div className="flex min-w-0 flex-wrap items-center gap-1.5">{children}</div>
        {hint && <div className="text-xs leading-relaxed text-muted-foreground">{hint}</div>}
      </div>
    </div>
  );
}

function List({ items, empty }: { items: string[]; empty: string }) {
  if (items.length === 0) return <span className="text-muted-foreground">{empty}</span>;
  return (
    <>
      {items.map((x) => (
        <Tag key={x} mono className="h-6 text-[12px] text-foreground/85">
          {x}
        </Tag>
      ))}
    </>
  );
}

function Flag({ on, yes = "Allowed", no = "Refused" }: { on: boolean; yes?: string; no?: string }) {
  return <StatusBadge tone={on ? "warning" : "muted"}>{on ? yes : no}</StatusBadge>;
}

function PolicyTab() {
  const p = usePolicy();
  if (p.isLoading) return <RowsSkeleton rows={6} />;
  if (p.error) return <FormError>{errorMessage(p.error)}</FormError>;
  const d = p.data!;
  return (
    <div className="grid gap-6">
      <Panel title="Listening" icon={<Network />} description="Where the daemon answers. Set by isb serve's flags (docs/serve.md#flags).">
        <div className="divide-y">
          <Row label="Listen addresses" hint="Loopback (behind a tunnel) or a tailnet address.">
            <List items={d.listen} empty="none: the unix socket only" />
          </Row>
          <Row label="Unix socket" hint="The daemon's own user: every tool, no policy.">
            <span className="truncate font-mono text-[12px]">{d.socket}</span>
            <CopyIconButton value={d.socket} label="Copy path" className="size-6" />
          </Row>
          <Row label="Public URL">{d.public_url ? <span className="font-mono text-[12px]">{d.public_url}</span> : <span className="text-muted-foreground">not set</span>}</Row>
          <Row label="Cloudflare Access" hint="Guards the loopback listeners: every request carries a verified assertion.">
            {d.access ? (
              <>
                <StatusBadge tone="success">On</StatusBadge>
                <span className="font-mono text-[12px]">{d.access.team_domain}</span>
              </>
            ) : (
              <StatusBadge tone="muted">Off</StatusBadge>
            )}
          </Row>
          <Row label="Unauthenticated callers" hint="--allow-unauthenticated: for local testing only.">
            <Flag on={d.allow_unauthenticated} />
          </Row>
          <Row label="State directory">
            <span className="truncate font-mono text-[12px]">{d.state_dir}</span>
          </Row>
        </div>
      </Panel>
      <Panel title="What remote callers' specs may ask for" icon={<ShieldCheck />} description="Users and API tokens, platform admins included, are held to this. Superadmins are not.">
        <div className="divide-y">
          <Row label="Privileged containers" hint="--allow-privileged">
            <Flag on={d.policy.allow_privileged} />
          </Row>
          <Row label="Raw config, profiles, idmap maps" hint="--allow-raw: raw_config, raw_devices, incus_profiles, idmap maps, guest-bound ports.">
            <Flag on={d.policy.allow_raw} />
          </Row>
          <Row label="Bind roots" hint="--bind-root: host directories bind mounts may come from.">
            <List items={d.policy.bind_roots} empty="none: no bind mounts" />
          </Row>
          <Row label="Publish addresses" hint="--publish-address: besides loopback.">
            <List items={d.policy.publish_addresses} empty="loopback only" />
          </Row>
          <Row label="Any instance" hint="--any-instance: exec into and remove instances isb did not create.">
            <Flag on={d.policy.any_instance} />
          </Row>
          <Row label="Tools" hint="--allow-tools and --deny-tools (deny wins).">
            {d.tools.allow.length === 0 && d.tools.deny.length === 0 ? (
              <span className="text-muted-foreground">every tool</span>
            ) : (
              <>
                {d.tools.allow.length > 0 && <span className="text-muted-foreground">allow</span>}
                <List items={d.tools.allow} empty="" />
                {d.tools.deny.length > 0 && <span className="text-muted-foreground">deny</span>}
                <List items={d.tools.deny} empty="" />
              </>
            )}
          </Row>
        </div>
      </Panel>
    </div>
  );
}

function SuperadminsTab() {
  const p = usePolicy();
  const tokens = useQuery({
    queryKey: ["tool", "superadmin_token_list"],
    queryFn: () => callTool<{ tokens: SuperadminToken[] }>("superadmin_token_list"),
  });
  const qc = useQueryClient();
  // Kept while the dialog closes, so its title does not blank out.
  const [revoking, setRevoking] = useState<SuperadminToken | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const me = useMe().data!;
  const list = tokens.data?.tokens ?? [];
  return (
    <div className="grid gap-6">
      <Panel title="Who is a superadmin" icon={<Crown />} description="Superadmins have the unix socket's reach: every tool, no remote-spec policy, any instance. Nothing else grants it.">
        {p.isLoading ? (
          <RowsSkeleton />
        ) : p.error ? (
          <div className="p-5">
            <FormError>{errorMessage(p.error)}</FormError>
          </div>
        ) : (
          <div className="divide-y">
            <Row label="Unix socket" hint="The daemon's own user on this host.">
              <StatusDot tone="success" />
              <span className="truncate font-mono text-[12px]">{p.data!.superadmin.socket}</span>
            </Row>
            <Row label="Superadmin tokens" hint="Listed below.">
              <span>{p.data!.superadmin.token_count}</span>
            </Row>
            <Row label="Tailnet identities" hint={p.data!.superadmin.tailnet ? <>Set by --superadmin-tailnet. A request must come from a tailnet peer and name this server ({(p.data!.superadmin.tailnet_hosts ?? []).join(", ")}).</> : "Off: --superadmin-tailnet is not set."}>
              {p.data!.superadmin.tailnet ? <List items={p.data!.superadmin.tailnet} empty="" /> : <StatusBadge tone="muted">Off</StatusBadge>}
            </Row>
            <Row label="Cloudflare Access identities" hint={p.data!.superadmin.access ? "Set by --superadmin-access. Only a verified assertion counts." : "Off: --superadmin-access is not set."}>
              {p.data!.superadmin.access ? <List items={p.data!.superadmin.access} empty="" /> : <StatusBadge tone="muted">Off</StatusBadge>}
            </Row>
          </div>
        )}
      </Panel>
      <Panel
        title="Superadmin tokens"
        icon={<KeyRound />}
        count={tokens.data ? list.length : undefined}
        description={
          <>
            Minted only on the host, never over HTTP:{" "}
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-[12px]">isb token create NAME --superadmin</code>
          </>
        }
      >
        {tokens.isLoading ? (
          <RowsSkeleton />
        ) : tokens.error ? (
          <div className="p-5">
            <FormError>{errorMessage(tokens.error)}</FormError>
          </div>
        ) : list.length === 0 ? (
          <Empty icon={<Terminal />} title="No superadmin tokens">
            Mint one on the host for an agent that needs the socket's reach over HTTP.
          </Empty>
        ) : (
          <Table>
            <TableHeader className="bg-muted/30">
              <TableRow className="hover:bg-transparent">
                <TableHead className="pl-5">Token</TableHead>
                <TableHead className="hidden sm:table-cell">Created</TableHead>
                <TableHead className="hidden sm:table-cell">Last used</TableHead>
                <TableHead>Expires</TableHead>
                <TableHead className="w-12 pr-5" aria-label="Actions" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {list.map((t) => {
                const mine = me.superadmin?.via.kind === "token" && me.superadmin.via.id === t.id;
                return (
                  <TableRow key={t.id}>
                    <TableCell className="py-2.5 pl-5">
                      <div className="flex items-center gap-2">
                        <span className="font-medium">{t.name}</span>
                        <Tag mono>sa-{t.id}</Tag>
                        {mine && <StatusBadge tone="info">This one</StatusBadge>}
                      </div>
                    </TableCell>
                    <TableCell className="hidden text-[13px] text-muted-foreground sm:table-cell" title={dateTime(t.created_at)}>
                      {relativeTime(t.created_at)}
                    </TableCell>
                    <TableCell className="hidden text-[13px] text-muted-foreground sm:table-cell">{t.last_used ? relativeTime(t.last_used) : "never"}</TableCell>
                    <TableCell className="text-[13px] text-muted-foreground">{t.expires_at ? dateTime(t.expires_at) : "never"}</TableCell>
                    <TableCell className="pr-5 text-right">
                      <Button variant="ghost" size="icon-sm" onClick={() => {
                          setRevoking(t);
                          setConfirmOpen(true);
                        }} aria-label={`Revoke ${t.name}`}>
                        <Trash2 />
                      </Button>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </Panel>
      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title={`Revoke superadmin token ${revoking?.name ?? ""}?`}
        description="Whatever holds it loses the socket's reach at once. A new one can only be minted on the host."
        confirm="Revoke"
        onConfirm={async () => {
          if (!revoking) return;
          await callTool("superadmin_token_revoke", { id: revoking.id });
          toast.success(`Revoked ${revoking.name}`);
          await qc.invalidateQueries({ queryKey: ["tool", "superadmin_token_list"] });
          await qc.invalidateQueries({ queryKey: ["tool", "host_policy"] });
        }}
      />
    </div>
  );
}
