import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Boxes, Cpu, Globe, HardDrive, Info, MemoryStick, Network, Pencil, ServerCog, ShieldAlert, ShieldCheck, Trash2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool, type OrgView } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, EmptyLine, Panel } from "@/components/confirm";
import { CopyIconButton, Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { limitLabel, parseEgress } from "@/lib/admin";
import { isolationText, placementLabel } from "@/lib/servers";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { useOrgPage } from "@/pages/org-common";
import { AgentIdentitiesPanel } from "@/pages/org-agent-identities";
import { NestingPanel } from "@/pages/org-nesting";

export function SettingsPage() {
  const { org, me, redirect } = useOrgPage();
  const info = useQuery({
    queryKey: ["tool", "org_get", org],
    queryFn: () => callTool<OrgView>("org_get", {}, org),
    enabled: !redirect,
  });
  if (redirect) return redirect;
  const platform = me.platform_admin;
  const o = info.data;
  const isDefault = org === "default";

  return (
    <>
      <PageHeader
        title="Settings"
        description={
          platform
            ? `${org}'s quota, network and egress. As a platform admin you can change them.`
            : `${org}'s quota, network and egress. Platform admins set these, since they're what keeps orgs apart.`
        }
      />
      {info.isLoading ? (
        <div className="grid gap-6">
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-24 rounded-xl" />
            ))}
          </div>
          <Skeleton className="h-56 rounded-xl" />
          <Skeleton className="h-28 rounded-xl" />
        </div>
      ) : info.error ? (
        <FormError>{errorMessage(info.error)}</FormError>
      ) : o ? (
        <div className="grid gap-6">
          {o.placement && !(isDefault && o.placement.kind === "local") && <PlacementPanel o={o} />}
          <LimitsPanel org={org} o={o} editable={platform} />
          <NetworkPanel o={o} />
          <EgressPanel org={org} o={o} editable={platform} />
          <AgentIdentitiesPanel org={org} />
          {o.network && <NestingPanel org={org} o={o} superadmin={!!me.superadmin} />}
          {platform && !isDefault && <DangerPanel org={org} o={o} />}
        </div>
      ) : null}
    </>
  );
}

/** A number a quota limit may cap, as a whole number (incus states counts as strings). */
const count = (v: string | null) => (v && /^\d+$/.test(v.trim()) ? Number(v) : null);

/** One limit's budget: what every instance's own limit adds up to, stopped ones included (bytes for memory and disk). */
type Budget = { limit: number; allocated: number; free: number };
const budgetOf = (o: OrgView, name: string): Budget | undefined => o.allocation?.[name];

/** Bytes as incus writes sizes: 512MiB, 3.5GiB. */
function size(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${Number.isInteger(v) ? v : v.toFixed(1)}${units[u]}`;
}

/** Used against limit as a bar; the tone warms as it fills. */
function Meter({ used, limit }: { used: number; limit: number }) {
  const pct = Math.min(100, Math.round((used / Math.max(limit, 1)) * 100));
  const tone = pct >= 90 ? "bg-destructive" : pct >= 75 ? "bg-warning" : "bg-brand";
  return (
    <div className="h-1.5 overflow-hidden rounded-full bg-muted" role="meter" aria-valuemin={0} aria-valuemax={limit} aria-valuenow={used}>
      <div className={cn("h-full rounded-full transition-[width]", tone)} style={{ width: `${Math.max(pct, used > 0 ? 3 : 0)}%` }} />
    </div>
  );
}

function QuotaTile({
  icon: Icon,
  label,
  value,
  limit,
  used,
  sub,
}: {
  icon: typeof Cpu;
  label: string;
  value: ReactNode;
  limit?: number | null;
  used?: number;
  sub?: ReactNode;
}) {
  const metered = used !== undefined && limit;
  return (
    <div className="flex min-w-0 flex-col gap-2.5 rounded-lg border bg-background/60 px-4 py-3.5">
      <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
        <Icon className="size-3.5" />
        {label}
      </div>
      <div className="truncate text-xl font-semibold tracking-tight tabular-nums">{value}</div>
      {metered ? (
        <Meter used={used} limit={limit} />
      ) : (
        <div className="h-1.5 rounded-full border border-dashed border-border" aria-hidden />
      )}
      {sub && <div className="truncate text-xs text-muted-foreground">{sub}</div>}
    </div>
  );
}

function LimitsPanel({ org, o, editable }: { org: string; o: OrgView; editable: boolean }) {
  const [open, setOpen] = useState(false);
  const instLimit = count(o.instances_limit);
  const unlimited = <span className="text-muted-foreground">Unlimited</span>;
  return (
    <Panel
      title="Quota"
      description="Totals across the org's instances: each caps the sum of every instance's own limit, stopped ones included. Below, what an instance gets when its spec sets no limits."
      action={
        editable && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <Pencil />
            Edit quota
          </Button>
        )
      }
    >
      <div className="grid grid-cols-2 gap-3 p-5 lg:grid-cols-4">
        <QuotaTile
          icon={Boxes}
          label="Instances"
          value={
            instLimit ? (
              <>
                {o.instances}
                <span className="text-base font-normal text-muted-foreground"> / {instLimit}</span>
              </>
            ) : (
              o.instances
            )
          }
          used={o.instances}
          limit={instLimit}
          sub={instLimit ? `${Math.max(instLimit - o.instances, 0)} left` : "No limit"}
        />
        {(
          [
            [Cpu, "CPUs", o.cpus, "cpu", String],
            [MemoryStick, "Memory", o.memory, "memory", size],
            [HardDrive, "Disk", o.disk, "disk", size],
          ] as [typeof Cpu, string, string | null, string, (n: number) => string][]
        ).map(([icon, label, limit, name, fmt]) => {
          const b = budgetOf(o, name);
          return (
            <QuotaTile
              key={name}
              icon={icon}
              label={label}
              value={limit ? limitLabel(limit) : unlimited}
              used={b?.allocated}
              limit={b?.limit}
              sub={b ? `${fmt(b.allocated)} allocated, ${fmt(b.free)} free` : limit ? "Across the org" : "No limit"}
            />
          );
        })}
      </div>
      <dl className="grid grid-cols-2 border-t text-sm sm:grid-cols-4 sm:divide-x">
        {(
          [
            ["Default CPUs", o.default_cpus ?? "None", "per instance"],
            ["Default memory", o.default_memory ?? "None", "per instance"],
            ["Stacks", o.stacks, "deployed"],
            ["Members", o.members, "people"],
          ] as [string, ReactNode, string][]
        ).map(([k, v, unit]) => (
          <div key={k} className="min-w-0 px-5 py-3">
            <dt className="text-xs text-muted-foreground">{k}</dt>
            <dd className="mt-0.5 truncate">
              <span className="font-medium tabular-nums">{v}</span> <span className="text-xs text-muted-foreground">{unit}</span>
            </dd>
          </div>
        ))}
      </dl>
      <LimitsDialog org={org} o={o} open={open} onOpenChange={setOpen} />
    </Panel>
  );
}

const LIMIT_FIELDS: { key: keyof OrgView & string; arg: string; label: string; hint: string; int?: boolean; lift?: boolean }[] = [
  { key: "cpus", arg: "cpus", label: "CPUs", hint: "Across the org, or none.", int: true, lift: true },
  { key: "memory", arg: "memory", label: "Memory", hint: "Across the org, e.g. 16GiB, or none.", lift: true },
  { key: "disk", arg: "disk", label: "Disk", hint: "Across the org, e.g. 100GiB, or none.", lift: true },
  { key: "instances_limit", arg: "instances", label: "Instances", hint: "Containers and VMs, or none.", int: true, lift: true },
  { key: "default_cpus", arg: "default_cpus", label: "Default CPUs", hint: "Per instance.", int: true },
  { key: "default_memory", arg: "default_memory", label: "Default memory", hint: "Per instance, e.g. 512MiB." },
];

function LimitsDialog({ org, o, open, onOpenChange }: { org: string; o: OrgView; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [vals, setVals] = useState<Record<string, string>>({});
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = (v: boolean) => {
    onOpenChange(v);
    if (!v) {
      setVals({});
      setError(null);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const args: Record<string, string | number> = {};
    for (const f of LIMIT_FIELDS) {
      const v = (vals[f.arg] ?? "").trim();
      if (!v) continue;
      if (f.lift && v.toLowerCase() === "none") {
        args[f.arg] = "none";
        continue;
      }
      if (f.int) {
        const n = Number(v);
        if (!Number.isInteger(n) || n < 1) {
          setError(`${f.label} must be a whole number of at least 1.`);
          return;
        }
        args[f.arg] = n;
      } else args[f.arg] = v;
    }
    if (!Object.keys(args).length) return close(false);
    setPending(true);
    setError(null);
    try {
      await callTool("org_update", { ...args, org });
      toast.success(`${org}'s quota updated`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_get", org] });
      await qc.invalidateQueries({ queryKey: ["tool", "org_list"] });
      close(false);
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
          <DialogTitle>Quota for {org}</DialogTitle>
          <DialogDescription>
            Leave a field empty to keep it, or enter none to lift a limit. A limit caps the sum of every instance&apos;s own
            limit, stopped ones included.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <div className="grid gap-4 sm:grid-cols-2">
            {LIMIT_FIELDS.map((f) => (
              <Field key={f.arg} label={f.label} hint={f.hint}>
                {(id, d) => (
                  <Input
                    id={id}
                    aria-describedby={d}
                    inputMode={f.int ? "numeric" : undefined}
                    placeholder={(o[f.key] as string | null) ?? "unlimited"}
                    value={vals[f.arg] ?? ""}
                    onChange={(e) => setVals((v) => ({ ...v, [f.arg]: e.target.value }))}
                  />
                )}
              </Field>
            ))}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Save</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** A key and a value with a copy button: the value is mono when it is an identifier. */
function KeyValue({ label, value, children, hint }: { label: string; value?: string | null; children?: ReactNode; hint?: ReactNode }) {
  return (
    <div className="grid gap-1 px-5 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:items-center sm:gap-4">
      <dt className="text-[13px] text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-sm">
        {children ?? (
          <div className="flex min-w-0 items-center gap-1">
            {value ? (
              <>
                <code className="truncate font-mono text-[13px]">{value}</code>
                <CopyIconButton value={value} label={`Copy ${label.toLowerCase()}`} />
              </>
            ) : (
              <span className="text-muted-foreground">None</span>
            )}
          </div>
        )}
        {hint && <div className="mt-0.5 text-xs text-muted-foreground">{hint}</div>}
      </dd>
    </div>
  );
}

const MOVING_DOCS = "https://github.com/execution-associates/isb/blob/main/docs/concepts/placement.md#moving-an-org";

/** Where the org runs and what keeps it apart from the others. */
function PlacementPanel({ o }: { o: OrgView }) {
  const p = o.placement;
  const { where, isolation } = placementLabel(p);
  return (
    <Panel icon={<ServerCog />} title="Placement" description="Where this org's workloads run, and how they're kept apart from other orgs'.">
      <dl className="divide-y">
        <KeyValue label="Runs on">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="font-medium">{p?.kind === "vm" ? `Dedicated VM ${p.vm?.instance ?? p.server}` : where}</span>
            <span className="inline-flex h-5 items-center rounded-md border bg-muted/50 px-1.5 text-[11px] font-medium text-muted-foreground">{isolation}</span>
          </div>
          {p?.kind === "vm" && p.vm && (
            <div className="mt-0.5 text-xs text-muted-foreground">
              Server {p.server} · {p.vm.cpus} CPU{p.vm.cpus === 1 ? "" : "s"}, {p.vm.memory} memory, {p.vm.disk} disk
            </div>
          )}
        </KeyValue>
        <KeyValue label="Isolation">
          <p className="text-[13px] leading-relaxed">{isolationText(p)}</p>
        </KeyValue>
      </dl>
      <div className="flex items-start gap-2.5 border-t bg-muted/30 px-5 py-3 text-[13px] leading-relaxed text-muted-foreground">
        <Info className="mt-0.5 size-4 shrink-0" />
        <p>
          Moving an org to another placement isn't supported yet. To move it by hand: back up its data, remove its workloads, delete it and create it
          where it should run.{" "}
          <a href={MOVING_DOCS} target="_blank" rel="noreferrer" className="inline-flex items-center gap-0.5 font-medium text-foreground underline-offset-4 hover:underline">
            How to move an org
            <ArrowUpRight className="size-3.5" />
          </a>
        </p>
      </div>
    </Panel>
  );
}

function NetworkPanel({ o }: { o: OrgView }) {
  const svc = `<service>.<stack>.${o.domain}`;
  return (
    <Panel
      icon={<Network />}
      title="Network"
      description="Each org has its own bridge. Instances reach each other and the internet, and no other private network."
    >
      <dl className="divide-y">
        <KeyValue label="Bridge" value={o.network} />
        <KeyValue label="Subnet" value={o.subnet} />
        <KeyValue label="Incus project" value={o.project} />
        <KeyValue
          label="Service names"
          hint={
            o.service_names ? (
              <>
                or <code className="font-mono">{"<service>.<stack>"}</code> from inside the org
              </>
            ) : undefined
          }
        >
          {o.service_names ? (
            <div className="flex min-w-0 items-center gap-1">
              <code className="truncate font-mono text-[13px]">{svc}</code>
              <CopyIconButton value={svc} label="Copy service name pattern" />
            </div>
          ) : (
            <span className="text-[13px] text-muted-foreground">
              Off: instances only, as <code className="font-mono text-xs">{`<instance>.${o.domain}`}</code>. The host
              needs <code className="font-mono text-xs">sudo isb host setup</code>.
            </span>
          )}
        </KeyValue>
        <KeyValue label="Host directories" hint={<>Set on the host with <code className="font-mono">isb org create --bind-root</code>.</>}>
          {o.bind_roots.length ? (
            <div className="flex flex-wrap gap-1">
              {o.bind_roots.map((r) => (
                <code key={r} className="rounded border bg-muted/40 px-1.5 py-0.5 font-mono text-xs">
                  {r}
                </code>
              ))}
            </div>
          ) : (
            <span className="text-[13px] text-muted-foreground">None: managed volumes only</span>
          )}
        </KeyValue>
      </dl>
    </Panel>
  );
}

/** What an egress rule lets through, in words. */
function egressWhat(rule: string): string {
  const m = rule.match(/^([^:]+)(?::([^/]+))?(?:\/(tcp|udp))?$/);
  if (!m) return "";
  const [, dest, ports, proto] = m;
  const whole = dest.includes("/") && !dest.endsWith("/32") ? "network" : "address";
  if (!ports) return `Everything to that ${whole}`;
  const p = proto?.toUpperCase() ?? "TCP";
  return ports.includes(",") || ports.includes("-") ? `${p} ports ${ports}` : `${p} port ${ports}`;
}

function EgressPanel({ org, o, editable }: { org: string; o: OrgView; editable: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      icon={<Globe />}
      title="Egress exceptions"
      count={o.egress.length}
      description="Private destinations this org may reach despite the default deny: a tailnet host, another org's published service."
      action={
        editable && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <Pencil />
            Edit
          </Button>
        )
      }
    >
      {o.egress.length ? (
        <ul className="divide-y">
          {o.egress.map((e) => (
            <li key={e} className="flex items-center gap-3 px-5 py-2.5">
              <ArrowUpRight className="size-3.5 shrink-0 text-muted-foreground" />
              <code className="min-w-0 truncate font-mono text-[13px]">{e}</code>
              <span className="ml-auto hidden shrink-0 text-xs text-muted-foreground sm:inline">{egressWhat(e)}</span>
              <CopyIconButton value={e} label="Copy rule" className="ml-auto sm:ml-0" />
            </li>
          ))}
        </ul>
      ) : (
        <EmptyLine icon={<ShieldCheck />}>None. The org reaches the internet and its own subnet only.</EmptyLine>
      )}
      <EgressDialog org={org} o={o} open={open} onOpenChange={setOpen} />
    </Panel>
  );
}

function EgressDialog({ org, o, open, onOpenChange }: { org: string; o: OrgView; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [text, setText] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const value = text ?? o.egress.join("\n");
  const close = (v: boolean) => {
    onOpenChange(v);
    if (!v) {
      setText(null);
      setError(null);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      const r = await callTool<OrgView>("org_update", { org, egress: parseEgress(value) });
      toast.success(r.egress.length ? `${org} may reach ${r.egress.length} private destination${r.egress.length === 1 ? "" : "s"}` : `${org}'s egress exceptions cleared`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_get", org] });
      close(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Egress exceptions for {org}</DialogTitle>
          <DialogDescription>One per line. Saving replaces the list; an empty list clears it.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid min-w-0 gap-4">
          <FormError>{error}</FormError>
          <textarea
            aria-label="Egress exceptions"
            rows={5}
            spellCheck={false}
            value={value}
            onChange={(e) => setText(e.target.value)}
            placeholder={"100.79.171.47/32:1080/tcp\n10.20.0.0/16"}
            className="min-h-28 w-full min-w-0 rounded-md border border-input bg-transparent px-3 py-2 font-mono text-sm shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30"
          />
          <div className="overflow-x-auto rounded-md border text-xs">
            <table className="w-full">
              <tbody className="divide-y">
                {[
                  ["10.20.0.0/16", "everything to that network"],
                  ["100.79.171.47", "everything to one address"],
                  ["100.79.171.47/32:1080/tcp", "TCP port 1080 only"],
                  ["10.1.2.3:53/udp", "UDP port 53 only"],
                  ["10.1.2.3:8000-8100,9000", "those TCP ports"],
                ].map(([k, v]) => (
                  <tr key={k}>
                    <td className="px-3 py-1.5 font-mono whitespace-nowrap">{k}</td>
                    <td className="px-3 py-1.5 text-muted-foreground">{v}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p className="text-xs text-muted-foreground">
            This lifts the org's own network rules only; the host's firewall still applies.
          </p>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Save</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function DeleteOrgDialog({
  org,
  o,
  open,
  onOpenChange,
  onDeleted,
}: {
  org: string;
  o?: Pick<OrgView, "instances" | "stacks" | "placement">;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onDeleted: () => void;
}) {
  const qc = useQueryClient();
  const [deleteVm, setDeleteVm] = useState(true);
  const vm = o?.placement?.kind === "vm" ? o.placement : null;
  const what = [
    o?.stacks ? `${o.stacks} stack${o.stacks === 1 ? "" : "s"}` : "",
    o?.instances ? `${o.instances} instance${o.instances === 1 ? "" : "s"}` : "",
  ].filter(Boolean);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) setDeleteVm(true);
      }}
      title={`Delete ${org}?`}
      description={
        <>
          Its project, volumes, network and service names go, with its members, invitations and API tokens.
          {what.length > 0 && ` Everything running in it is deleted first: its apps and ${what.join(" across ")}.`}
        </>
      }
      confirm="Delete org"
      typed={org}
      onConfirm={async () => {
        // The typed name is the confirmation: everything in it goes too.
        const r = await callTool<{ notes?: string[]; deleted_vm?: string }>("org_delete", vm ? { org, force: true, delete_vm: deleteVm } : { org, force: true });
        toast.success(`Org ${org} deleted`, { description: r?.deleted_vm ? `Its VM ${r.deleted_vm} was deleted too.` : undefined });
        for (const n of r?.notes ?? []) toast.info(n);
        // Leave the org's pages before they learn it is gone.
        onDeleted();
        for (const k of [["apps", org], ["stacks", org], ["workspace", org], ["workspace-sandboxes", org]]) qc.removeQueries({ queryKey: k });
        await qc.invalidateQueries({ queryKey: ["me"] });
        await qc.invalidateQueries({ queryKey: ["tool"] });
      }}
    >
      {vm && (
        <label className="flex items-start gap-3 rounded-md border p-3 text-sm">
          <input type="checkbox" className="mt-0.5 size-4 accent-destructive" checked={deleteVm} onChange={(e) => setDeleteVm(e.target.checked)} />
          <span>
            Also delete its VM ({vm.vm?.instance ?? vm.server}) and server registration
            <span className="block text-xs text-muted-foreground">Without this, the VM keeps running as an empty server you can remove later.</span>
          </span>
        </label>
      )}
    </ConfirmDialog>
  );
}

function DangerPanel({ org, o }: { org: string; o: OrgView }) {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  return (
    <Panel tone="danger" icon={<ShieldAlert />} title="Danger zone" description="Actions here can't be undone.">
      <div className="flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0 text-sm">
          <p className="font-medium">Delete this org</p>
          <p className="text-[13px] text-muted-foreground">
            Its project, volumes, network and members go. Its secrets stay on the host's disk.
          </p>
        </div>
        <Button variant="destructive" className="shrink-0" onClick={() => setOpen(true)}>
          <Trash2 />
          Delete org
        </Button>
      </div>
      <DeleteOrgDialog org={org} o={o} open={open} onOpenChange={setOpen} onDeleted={() => navigate("/admin/orgs", { replace: true })} />
    </Panel>
  );
}
