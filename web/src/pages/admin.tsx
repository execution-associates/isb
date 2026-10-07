import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Boxes,
  Building2,
  FolderOpen,
  MoreHorizontal,
  Network,
  Plus,
  ScrollText,
  Server,
  ShieldCheck,
  ShieldOff,
  Trash2,
  UserCheck,
  UserX,
  Users,
} from "lucide-react";
import { useState } from "react";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { type AdminUser, auth } from "@/api/auth";
import { callTool, type OrgView, type ServerStatus } from "@/api/tools";
import { TabLinks } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel, PersonAvatar } from "@/components/confirm";
import { CopyIconButton, Field, FormError, SubmitButton } from "@/components/form";
import { StatusBadge, StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { limitLabel, orgNameProblem, parseEgress, plural } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { cn } from "@/lib/utils";
import { HistoryPanel } from "@/pages/history";
import { DeleteOrgDialog } from "@/pages/org-settings";
import { RowsSkeleton, Tag } from "@/pages/org-ui";

const TABS = [
  { id: "orgs", label: "Orgs", icon: Building2 },
  { id: "users", label: "Users", icon: Users },
  { id: "server", label: "This host", icon: Server },
  { id: "history", label: "History", icon: ScrollText },
] as const;

/** Platform administration: every org, every user, this host. */
export function AdminPage() {
  const me = useMe().data!;
  const { tab = "orgs" } = useParams();
  if (!me.platform_admin) return <Navigate to="/" replace />;
  if (!TABS.some((t) => t.id === tab)) return <Navigate to="/admin/orgs" replace />;
  return (
    <>
      <PageHeader title="Platform" description="Every org and user on this host. Only platform admins see this." />
      <TabLinks tabs={TABS.map((t) => ({ ...t, to: `/admin/${t.id}` }))} active={tab} />
      {tab === "orgs" && <OrgsTab />}
      {tab === "users" && <UsersTab />}
      {tab === "server" && <ServerTab />}
      {tab === "history" && <HistoryPanel orgs={me.orgs} />}
    </>
  );
}

const Loading = () => <RowsSkeleton />;

/** An org's initial on a stable hue, as the sidebar's org switcher shows it. */
function OrgMark({ name }: { name: string }) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return (
    <span
      aria-hidden
      className="flex size-8 shrink-0 items-center justify-center rounded-lg text-xs font-semibold text-white uppercase shadow-xs"
      style={{ background: `linear-gradient(135deg, oklch(0.62 0.13 ${h}), oklch(0.5 0.13 ${(h + 40) % 360}))` }}
    >
      {name.slice(0, 1)}
    </span>
  );
}

function OrgsTab() {
  const list = useQuery({ queryKey: ["tool", "org_list"], queryFn: () => callTool<{ orgs: OrgView[] }>("org_list") });
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<OrgView | null>(null);
  const orgs = list.data?.orgs ?? [];
  return (
    <Panel
      title="Orgs"
      count={list.data ? orgs.length : undefined}
      description="Each org is an isolated incus project with its own network, members and secrets."
      action={
        <Button size="sm" onClick={() => setCreating(true)}>
          <Plus />
          New org
        </Button>
      }
    >
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : orgs.length === 0 ? (
        <Empty icon={<Building2 />} title="No orgs" />
      ) : (
        <Table>
          <TableHeader className="bg-muted/30">
            <TableRow className="hover:bg-transparent">
              <TableHead className="pl-5">Org</TableHead>
              <TableHead className="hidden text-right sm:table-cell">Members</TableHead>
              <TableHead className="hidden text-right sm:table-cell">Stacks</TableHead>
              <TableHead className="hidden w-44 pl-8 lg:table-cell">Instances</TableHead>
              <TableHead className="hidden xl:table-cell">Subnet</TableHead>
              <TableHead className="hidden xl:table-cell">Quota</TableHead>
              <TableHead className="w-12 pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {orgs.map((o) => {
              const lim = o.instances_limit && /^\d+$/.test(o.instances_limit) ? Number(o.instances_limit) : null;
              return (
                <TableRow key={o.name} className="relative">
                  <TableCell className="py-3 pl-5">
                    <div className="flex min-w-0 items-center gap-3">
                      <OrgMark name={o.name} />
                      <div className="min-w-0">
                        <Link
                          to={`/orgs/${encodeURIComponent(o.name)}/settings`}
                          className="font-medium break-all after:absolute after:inset-0 focus-visible:outline-none"
                        >
                          {o.name}
                        </Link>
                        <div className="truncate text-xs text-muted-foreground sm:hidden">
                          {plural(o.members, "member")} · {plural(o.stacks, "stack")}
                        </div>
                      </div>
                    </div>
                  </TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{o.members}</TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{o.stacks}</TableCell>
                  <TableCell className="hidden pl-8 lg:table-cell">
                    <div className="flex items-center gap-2.5">
                      <span className="tabular-nums">
                        {o.instances}
                        {lim && <span className="text-muted-foreground"> / {lim}</span>}
                      </span>
                      {lim && (
                        <span className="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
                          <span className="block h-full rounded-full bg-brand" style={{ width: `${Math.min(100, (o.instances / lim) * 100)}%` }} />
                        </span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="hidden font-mono text-xs text-muted-foreground xl:table-cell">{o.subnet ?? "—"}</TableCell>
                  <TableCell className="hidden text-[13px] text-muted-foreground xl:table-cell">
                    {o.name === "default" ? "—" : !o.cpus && !o.memory ? "No quota" : `${limitLabel(o.cpus)} CPU · ${limitLabel(o.memory)}`}
                  </TableCell>
                  <TableCell className="relative z-10 pr-5 text-right">
                    {o.name !== "default" && (
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="text-muted-foreground hover:text-destructive"
                        aria-label={`Delete ${o.name}`}
                        title="Delete"
                        onClick={() => setDeleting(o)}
                      >
                        <Trash2 />
                      </Button>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      <CreateOrgDialog open={creating} onOpenChange={setCreating} />
      <DeleteOrgDialog
        org={deleting?.name ?? ""}
        o={deleting ?? undefined}
        open={!!deleting}
        onOpenChange={(v) => !v && setDeleting(null)}
        onDeleted={() => setDeleting(null)}
      />
    </Panel>
  );
}

function CreateOrgDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [cpus, setCpus] = useState("");
  const [memory, setMemory] = useState("");
  const [egress, setEgress] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const problem = orgNameProblem(name.trim());
  const close = (v: boolean) => {
    onOpenChange(v);
    if (!v) {
      setName("");
      setCpus("");
      setMemory("");
      setEgress("");
      setTouched(false);
      setError(null);
    }
  };
  const created = async (org: string) => {
    toast.success(`Org ${org} created`);
    await qc.invalidateQueries({ queryKey: ["tool", "org_list"] });
    await qc.invalidateQueries({ queryKey: ["me"] });
    close(false);
    navigate(`/orgs/${encodeURIComponent(org)}/members`);
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (problem) return;
    const args: Record<string, unknown> = { org: name.trim() };
    if (cpus.trim()) {
      const n = Number(cpus);
      if (!Number.isInteger(n) || n < 1) return setError("CPUs must be a whole number of at least 1.");
      args.cpus = n;
    }
    if (memory.trim()) args.memory = memory.trim();
    const e2 = parseEgress(egress);
    if (e2.length) args.egress = e2;
    setPending(true);
    setError(null);
    try {
      const o = await callTool<OrgView>("org_create", args);
      for (const n of o.notes ?? []) if (/service names are off/.test(n)) toast.warning(n);
      await created(o.name);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>New org</DialogTitle>
          <DialogDescription>An isolated incus project with its own network. Invite its first owner from its Members page.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid min-w-0 gap-4">
          <FormError>{error}</FormError>
          <Field label="Name" error={touched ? problem : null} hint="Lowercase letters, digits and -, starting with a letter.">
            {(id, d) => (
              <Input id={id} aria-describedby={d} autoFocus spellCheck={false} value={name} onChange={(e) => setName(e.target.value)} placeholder="acme" className="font-mono" />
            )}
          </Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Org CPUs (optional)" hint="Quota across the org.">
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={cpus} onChange={(e) => setCpus(e.target.value)} placeholder="unlimited" />}
            </Field>
            <Field label="Org memory (optional)" hint="Quota, e.g. 16GiB">
              {(id, d) => <Input id={id} aria-describedby={d} value={memory} onChange={(e) => setMemory(e.target.value)} placeholder="unlimited" />}
            </Field>
          </div>
          <Field label="Egress exceptions (optional)" hint="Private destinations it may reach, one per line: CIDR[:PORTS[/tcp|udp]].">
            {(id, d) => (
              <textarea
                id={id}
                aria-describedby={d}
                rows={2}
                spellCheck={false}
                value={egress}
                onChange={(e) => setEgress(e.target.value)}
                className="w-full min-w-0 rounded-md border border-input bg-transparent px-3 py-2 font-mono text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30"
              />
            )}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Create org</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function UsersTab() {
  const me = useMe().data!;
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["admin-users"], queryFn: auth.adminUsers });
  const [action, setAction] = useState<{ user: AdminUser; change: "disable" | "enable" | "grant" | "revoke" } | null>(null);
  const users = list.data?.users ?? [];
  const words = {
    disable: { title: "Disable", body: "They're signed out everywhere and can't sign in; their tokens stop working until re-enabled.", confirm: "Disable", destructive: true },
    enable: { title: "Enable", body: "They can sign in again, and their tokens work again.", confirm: "Enable", destructive: false },
    grant: { title: "Make platform admin:", body: "They can see and change every org and user on this server.", confirm: "Make platform admin", destructive: false },
    revoke: { title: "Remove platform admin from", body: "They keep their org memberships; their platform tokens stop working.", confirm: "Remove", destructive: true },
  } as const;
  const w = action ? words[action.change] : null;
  return (
    <Panel title="Users" count={list.data ? users.length : undefined} description="Every account on this server and the orgs it belongs to.">

      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : (
        <Table>
          <TableHeader className="bg-muted/30">
            <TableRow className="hover:bg-transparent">
              <TableHead className="pl-5 md:w-2/5">User</TableHead>
              <TableHead className="hidden md:table-cell">Orgs</TableHead>
              <TableHead className="hidden sm:table-cell">Last active</TableHead>
              <TableHead className="w-12 pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {users.map((u) => {
              const self = u.id === me.user.id;
              return (
                <TableRow key={u.id} className={cn(u.disabled && "opacity-70")}>
                  <TableCell className="max-w-0 py-3 pl-5">
                    <div className="flex min-w-0 items-center gap-3">
                      <PersonAvatar name={u.name} email={u.email} />
                      <div className="min-w-0">
                        <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1">
                          <span className="truncate font-medium">{u.name || u.email.split("@")[0]}</span>
                          {self && <span className="rounded bg-muted px-1.5 py-px text-[11px] font-medium text-muted-foreground">you</span>}
                          {u.platform_admin && (
                            <StatusBadge tone="info" className="h-5">
                              Platform admin
                            </StatusBadge>
                          )}
                          {u.disabled && (
                            <StatusBadge tone="danger" className="h-5">
                              Disabled
                            </StatusBadge>
                          )}
                        </div>
                        <div className="truncate text-xs text-muted-foreground">{u.email}</div>
                      </div>
                    </div>
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    {u.memberships.length ? (
                      <div className="flex flex-wrap gap-1">
                        {u.memberships.map((m) => (
                          <Tag key={m.org} className="h-6 gap-1 text-xs">
                            <span className="text-foreground/80">{m.org}</span>
                            <span className="opacity-60">·</span>
                            {m.role}
                          </Tag>
                        ))}
                      </div>
                    ) : (
                      <span className="text-[13px] text-muted-foreground">None</span>
                    )}
                  </TableCell>
                  <TableCell className="hidden text-[13px] text-muted-foreground tabular-nums sm:table-cell" title={dateTime(u.last_active)}>
                    {u.last_active ? relativeTime(u.last_active) : "Never"}
                  </TableCell>
                  <TableCell className="pr-5 text-right">
                    {!self && (
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${u.email}`}>
                            <MoreHorizontal />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          {u.platform_admin ? (
                            <DropdownMenuItem onSelect={() => setAction({ user: u, change: "revoke" })}>
                              <ShieldOff />
                              Remove platform admin
                            </DropdownMenuItem>
                          ) : (
                            <DropdownMenuItem onSelect={() => setAction({ user: u, change: "grant" })}>
                              <ShieldCheck />
                              Make platform admin
                            </DropdownMenuItem>
                          )}
                          {u.disabled ? (
                            <DropdownMenuItem onSelect={() => setAction({ user: u, change: "enable" })}>
                              <UserCheck />
                              Enable
                            </DropdownMenuItem>
                          ) : (
                            <DropdownMenuItem variant="destructive" onSelect={() => setAction({ user: u, change: "disable" })}>
                              <UserX />
                              Disable
                            </DropdownMenuItem>
                          )}
                        </DropdownMenuContent>
                      </DropdownMenu>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      <ConfirmDialog
        open={!!action}
        onOpenChange={(o) => !o && setAction(null)}
        title={w ? `${w.title} ${action!.user.email}?` : ""}
        description={w?.body}
        confirm={w?.confirm ?? ""}
        destructive={w?.destructive}
        onConfirm={async () => {
          const { user, change } = action!;
          const body =
            change === "disable"
              ? { disabled: true }
              : change === "enable"
                ? { disabled: false }
                : { platform_admin: change === "grant" };
          await auth.adminUpdateUser(user.id, body);
          toast.success(`${user.email} updated`);
          await qc.invalidateQueries({ queryKey: ["admin-users"] });
        }}
      />
    </Panel>
  );
}

function ServerTab() {
  const s = useQuery({
    queryKey: ["tool", "server_status"],
    queryFn: () => callTool<ServerStatus>("server_status"),
    refetchInterval: 15_000,
  });
  if (s.isLoading)
    return (
      <div className="grid gap-6">
        <div className="grid gap-3 sm:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-20 rounded-xl" />
          ))}
        </div>
        <Skeleton className="h-40 rounded-xl" />
      </div>
    );
  if (s.error) return <FormError>{errorMessage(s.error)}</FormError>;
  const d = s.data!;
  const down = d.routes.reduce((n, r) => n + r.backends.filter((b) => b.down).length, 0);
  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        {(
          [
            [Server, "isb", d.isb],
            [Boxes, "incus", d.incus],
            [FolderOpen, "State directory", d.state_dir],
          ] as const
        ).map(([Icon, k, v]) => (
          <div key={k} className="flex min-w-0 items-center gap-3 rounded-xl border bg-card px-4 py-3.5 shadow-xs">
            <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground">
              <Icon className="size-4" />
            </span>
            <div className="min-w-0">
              <div className="text-xs text-muted-foreground">{k}</div>
              <div className="flex min-w-0 items-center gap-1">
                <span className="truncate font-mono text-[13px] font-medium" title={v}>
                  {v}
                </span>
                {k === "State directory" && <CopyIconButton value={v} label="Copy path" className="size-6" />}
              </div>
            </div>
          </div>
        ))}
      </div>
      <Panel
        title={
          <>
            Load balancer
            {d.routes.length > 0 &&
              (down ? (
                <StatusBadge tone="warning">{plural(down, "backend")} down</StatusBadge>
              ) : (
                <StatusBadge tone="success">All backends up</StatusBadge>
              ))}
          </>
        }
        description="Published ports and the replicas behind them."
      >
        {d.routes.length === 0 ? (
          <Empty icon={<Network />} title="No published ports">
            Ports a stack publishes show up here with the replicas serving them.
          </Empty>
        ) : (
          <Table>
            <TableHeader className="bg-muted/30">
              <TableRow className="hover:bg-transparent">
                <TableHead className="pl-5">Route</TableHead>
                <TableHead>Listen</TableHead>
                <TableHead className="hidden md:table-cell">Backends</TableHead>
                <TableHead className="hidden pr-5 text-right sm:table-cell">Connections</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {d.routes.map((r) => (
                <TableRow key={r.route}>
                  <TableCell className="max-w-0 truncate py-3 pl-5 font-medium">{r.route}</TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">{r.listen}</TableCell>
                  <TableCell className="hidden md:table-cell">
                    <div className="flex flex-wrap gap-1">
                      {r.backends.map((b) => (
                        <span
                          key={b.addr}
                          title={b.down ? "Down" : `${b.active} active`}
                          className={cn(
                            "inline-flex h-6 items-center gap-1.5 rounded-md border px-2 font-mono text-[11px]",
                            b.down && "border-destructive/30 text-destructive",
                          )}
                        >
                          <StatusDot tone={b.down ? "danger" : "success"} className="size-1.5" />
                          {b.addr}
                          {!b.down && b.active > 0 && <span className="text-muted-foreground tabular-nums">{b.active}</span>}
                        </span>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell className="hidden pr-5 text-right tabular-nums sm:table-cell">
                    {r.accepted.toLocaleString()}
                    {r.failures > 0 && <span className="text-destructive"> · {r.failures} failed</span>}
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
