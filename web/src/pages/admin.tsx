import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Building2, MoreHorizontal, Plus, Server, ShieldCheck, ShieldOff, Trash2, UserCheck, UserX, Users } from "lucide-react";
import { useState } from "react";
import { Link, Navigate, NavLink, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { type AdminUser, auth } from "@/api/auth";
import { callTool, type OrgView, type ServerStatus } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
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
import { dateTime, initials, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { cn } from "@/lib/utils";
import { DeleteOrgDialog } from "@/pages/org-settings";

const TABS = [
  { id: "orgs", label: "Orgs", icon: Building2 },
  { id: "users", label: "Users", icon: Users },
  { id: "server", label: "Server", icon: Server },
] as const;

/** Platform administration: every org, every user, the server. */
export function AdminPage() {
  const me = useMe().data!;
  const { tab = "orgs" } = useParams();
  if (!me.platform_admin) return <Navigate to="/" replace />;
  if (!TABS.some((t) => t.id === tab)) return <Navigate to="/admin/orgs" replace />;
  return (
    <>
      <PageHeader title="Platform" description="Every org and user on this server. Only platform admins see this." />
      <nav className="mb-6 flex gap-1 overflow-x-auto border-b" aria-label="Platform sections">
        {TABS.map((t) => (
          <NavLink
            key={t.id}
            to={`/admin/${t.id}`}
            className={({ isActive }) =>
              cn(
                "-mb-px flex items-center gap-2 border-b-2 border-transparent px-3 py-2 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground",
                isActive && "border-foreground text-foreground",
              )
            }
          >
            <t.icon className="size-4" />
            {t.label}
          </NavLink>
        ))}
      </nav>
      {tab === "orgs" && <OrgsTab />}
      {tab === "users" && <UsersTab />}
      {tab === "server" && <ServerTab />}
    </>
  );
}

const Loading = () => (
  <div className="space-y-2 p-5">
    <Skeleton className="h-9" />
    <Skeleton className="h-9" />
  </div>
);

function OrgsTab() {
  const list = useQuery({ queryKey: ["tool", "org_list"], queryFn: () => callTool<{ orgs: OrgView[] }>("org_list") });
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<OrgView | null>(null);
  const orgs = list.data?.orgs ?? [];
  return (
    <Panel
      title="Orgs"
      description={list.data ? `${orgs.length} on this server` : undefined}
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
          <TableHeader>
            <TableRow>
              <TableHead className="pl-5">Org</TableHead>
              <TableHead className="hidden sm:table-cell">Members</TableHead>
              <TableHead className="hidden sm:table-cell">Stacks</TableHead>
              <TableHead className="hidden md:table-cell">Instances</TableHead>
              <TableHead className="hidden lg:table-cell">Subnet</TableHead>
              <TableHead className="hidden lg:table-cell">Quota</TableHead>
              <TableHead className="w-12 pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {orgs.map((o) => (
              <TableRow key={o.name}>
                <TableCell className="pl-5">
                  <Link to={`/orgs/${encodeURIComponent(o.name)}/settings`} className="font-medium break-all hover:underline">
                    {o.name}
                  </Link>
                  <div className="truncate text-xs text-muted-foreground sm:hidden">
                    {plural(o.members, "member")} · {plural(o.stacks, "stack")}
                  </div>
                </TableCell>
                <TableCell className="hidden tabular-nums sm:table-cell">{o.members}</TableCell>
                <TableCell className="hidden tabular-nums sm:table-cell">{o.stacks}</TableCell>
                <TableCell className="hidden tabular-nums md:table-cell">
                  {o.instances}
                  {o.instances_limit && <span className="text-muted-foreground"> / {o.instances_limit}</span>}
                </TableCell>
                <TableCell className="hidden font-mono text-xs lg:table-cell">{o.subnet ?? "—"}</TableCell>
                <TableCell className="hidden text-muted-foreground lg:table-cell">
                  {o.name === "default" ? "—" : !o.cpus && !o.memory ? "No quota" : `${limitLabel(o.cpus)} CPU · ${limitLabel(o.memory)}`}
                </TableCell>
                <TableCell className="pr-5 text-right">
                  {o.name !== "default" && (
                    <Button variant="ghost" size="icon-sm" aria-label={`Delete ${o.name}`} title="Delete" onClick={() => setDeleting(o)}>
                      <Trash2 />
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
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
      toast.success(`Org ${o.name} created`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_list"] });
      await qc.invalidateQueries({ queryKey: ["me"] });
      close(false);
      navigate(`/orgs/${encodeURIComponent(o.name)}/members`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New org</DialogTitle>
          <DialogDescription>
            An isolated incus project with its own network. Invite its first owner from its Members page.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid min-w-0 gap-4">
          <FormError>{error}</FormError>
          <Field label="Name" error={touched ? problem : null} hint="Lowercase letters, digits and -, starting with a letter.">
            {(id, d) => (
              <Input id={id} aria-describedby={d} autoFocus spellCheck={false} value={name} onChange={(e) => setName(e.target.value)} placeholder="acme" className="font-mono" />
            )}
          </Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="CPUs (optional)" hint="Across the org.">
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={cpus} onChange={(e) => setCpus(e.target.value)} placeholder="unlimited" />}
            </Field>
            <Field label="Memory (optional)" hint="e.g. 16GiB">
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
    <Panel title="Users" description={list.data ? `${users.length} account${users.length === 1 ? "" : "s"}` : undefined}>
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
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
                  <TableCell className="max-w-0 pl-5">
                    <div className="flex min-w-0 items-center gap-3">
                      <Avatar className="hidden size-8 shrink-0 rounded-md sm:flex">
                        <AvatarFallback className="rounded-md text-xs">{initials(u.name, u.email)}</AvatarFallback>
                      </Avatar>
                      <div className="min-w-0">
                        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                          <span className="truncate font-medium">{u.name || u.email.split("@")[0]}</span>
                          {self && (
                            <Badge variant="secondary" className="font-normal">
                              you
                            </Badge>
                          )}
                          {u.platform_admin && <Badge className="font-normal">platform admin</Badge>}
                          {u.disabled && (
                            <Badge variant="outline" className="border-destructive/40 font-normal text-destructive">
                              disabled
                            </Badge>
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
                          <Badge key={m.org} variant="outline" className="font-normal">
                            {m.org} · {m.role}
                          </Badge>
                        ))}
                      </div>
                    ) : (
                      <span className="text-muted-foreground">None</span>
                    )}
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground sm:table-cell" title={dateTime(u.last_active)}>
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
  if (s.isLoading) return <Loading />;
  if (s.error) return <FormError>{errorMessage(s.error)}</FormError>;
  const d = s.data!;
  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        {[
          ["isb", d.isb],
          ["incus", d.incus],
          ["State directory", d.state_dir],
        ].map(([k, v]) => (
          <div key={k} className="min-w-0 rounded-xl border bg-card px-5 py-4 shadow-sm">
            <div className="text-xs text-muted-foreground">{k}</div>
            <div className="mt-1 truncate font-mono text-sm font-medium" title={v}>
              {v}
            </div>
          </div>
        ))}
      </div>
      <Panel title="Load balancer" description="Published ports and the replicas behind them.">
        {d.routes.length === 0 ? (
          <Empty title="No published ports" />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="pl-5">Route</TableHead>
                <TableHead>Listen</TableHead>
                <TableHead className="hidden md:table-cell">Backends</TableHead>
                <TableHead className="hidden pr-5 sm:table-cell">Accepted</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {d.routes.map((r) => (
                <TableRow key={r.route}>
                  <TableCell className="max-w-0 truncate pl-5 font-medium">{r.route}</TableCell>
                  <TableCell className="font-mono text-xs">{r.listen}</TableCell>
                  <TableCell className="hidden md:table-cell">
                    <div className="flex flex-wrap gap-1">
                      {r.backends.map((b) => (
                        <Badge key={b.addr} variant="outline" className={cn("font-mono text-[11px] font-normal", b.down && "border-destructive/40 text-destructive")}>
                          {b.addr}
                        </Badge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell className="hidden pr-5 tabular-nums sm:table-cell">
                    {r.accepted}
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
